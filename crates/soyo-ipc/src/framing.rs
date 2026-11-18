//! Newline-delimited JSON frame IO.

use interprocess::local_socket::tokio::Stream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, ReadHalf, WriteHalf};

use crate::protocol::{ClientFrame, ServerFrame};

pub type FrameReader = BufReader<ReadHalf<Stream>>;
pub type FrameWriter = BufWriter<WriteHalf<Stream>>;

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub fn split(stream: Stream) -> (FrameReader, FrameWriter) {
    let (read, write) = tokio::io::split(stream);
    (BufReader::new(read), BufWriter::new(write))
}

pub async fn read_server_frame(reader: &mut FrameReader) -> std::io::Result<Option<ServerFrame>> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    if line.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds size limit",
        ));
    }
    serde_json::from_str::<ServerFrame>(&line)
        .map(Some)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

pub async fn read_client_frame(reader: &mut FrameReader) -> std::io::Result<Option<ClientFrame>> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    if line.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds size limit",
        ));
    }
    serde_json::from_str::<ClientFrame>(&line)
        .map(Some)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

pub async fn write_frame<T: serde::Serialize>(
    writer: &mut FrameWriter,
    frame: &T,
) -> std::io::Result<()> {
    let mut buf = serde_json::to_vec(frame).map_err(std::io::Error::other)?;
    buf.push(b'\n');
    writer.write_all(&buf).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ClientFrame, ServerFrame};
    use interprocess::local_socket::{
        ListenerOptions,
        tokio::Stream,
        traits::tokio::{Listener as _, Stream as _},
    };
    use serde_json::json;

    async fn connected_pair() -> (FrameReader, FrameWriter, FrameReader, FrameWriter) {
        let dir = tempfile::tempdir().unwrap();
        let name = crate::socket::helper_socket_name_at(dir.path().join("t.sock"));
        let listener = ListenerOptions::new()
            .name(name.clone())
            .create_tokio()
            .unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap() });
        let client = Stream::connect(name).await.unwrap();
        let server = accept.await.unwrap();
        let (cr, cw) = split(client);
        let (sr, sw) = split(server);
        // keep dir alive for the duration of the test
        std::mem::forget(dir);
        (cr, cw, sr, sw)
    }

    #[tokio::test]
    async fn client_frame_round_trip() {
        let (_cr, mut cw, mut sr, _sw) = connected_pair().await;
        write_frame(
            &mut cw,
            &ClientFrame::Request {
                id: 7,
                method: "get_bootstrap_state".into(),
                params: json!({"page": 2}),
            },
        )
        .await
        .unwrap();
        match read_client_frame(&mut sr).await.unwrap() {
            Some(ClientFrame::Request { id, method, params }) => {
                assert_eq!(id, 7);
                assert_eq!(method, "get_bootstrap_state");
                assert_eq!(params["page"], 2);
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    #[tokio::test]
    async fn server_frame_round_trip_and_malformed() {
        let (mut cr, _cw, _sr, mut sw) = connected_pair().await;
        write_frame(
            &mut sw,
            &ServerFrame::Hello {
                version: 1,
                pid: 42,
            },
        )
        .await
        .unwrap();
        match read_server_frame(&mut cr).await.unwrap() {
            Some(ServerFrame::Hello { version, pid }) => {
                assert_eq!(version, 1);
                assert_eq!(pid, 42);
            }
            other => panic!("unexpected: {other:?}"),
        }
        // Malformed JSON line surfaces as an error, not a hang.
        sw.write_all(b"{not json\n").await.unwrap();
        sw.flush().await.unwrap();
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_server_frame(&mut cr),
        )
        .await;
        assert!(
            matches!(res, Ok(Err(_))),
            "malformed frame must error: {res:?}"
        );
    }

    #[tokio::test]
    async fn clean_close_yields_none() {
        let (mut cr, _cw, sr, sw) = connected_pair().await;
        // tokio::io::split keeps the socket open until BOTH halves drop.
        drop(sw);
        drop(sr);
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_server_frame(&mut cr),
        )
        .await;
        assert!(
            matches!(res, Ok(Ok(None))),
            "EOF must read as None: {res:?}"
        );
    }
}
