//! Newline-delimited JSON frame IO with strict bounded line buffering.

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, ReadHalf, WriteHalf},
    net::UnixStream,
};

use crate::protocol::{ClientFrame, ServerFrame};

pub type FrameReader = BufReader<ReadHalf<UnixStream>>;
pub type FrameWriter = BufWriter<WriteHalf<UnixStream>>;

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub fn split(stream: UnixStream) -> (FrameReader, FrameWriter) {
    let (read, write) = tokio::io::split(stream);
    (BufReader::new(read), BufWriter::new(write))
}

async fn read_bounded_line(
    reader: &mut FrameReader,
    max_bytes: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            return Ok(Some(line));
        }
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            if line.len().saturating_add(take) > max_bytes {
                reader.consume(take);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "frame exceeds size limit",
                ));
            }
            line.extend_from_slice(&buf[..take]);
            reader.consume(take);
            return Ok(Some(line));
        }
        if line.len().saturating_add(buf.len()) > max_bytes {
            let len = buf.len();
            reader.consume(len);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame exceeds size limit",
            ));
        }
        let len = buf.len();
        line.extend_from_slice(buf);
        reader.consume(len);
    }
}

pub async fn read_server_frame(reader: &mut FrameReader) -> std::io::Result<Option<ServerFrame>> {
    let Some(line) = read_bounded_line(reader, MAX_FRAME_BYTES).await? else {
        return Ok(None);
    };
    serde_json::from_slice::<ServerFrame>(&line)
        .map(Some)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

pub async fn read_client_frame(reader: &mut FrameReader) -> std::io::Result<Option<ClientFrame>> {
    let Some(line) = read_bounded_line(reader, MAX_FRAME_BYTES).await? else {
        return Ok(None);
    };
    serde_json::from_slice::<ClientFrame>(&line)
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
    use serde_json::json;
    use tokio::net::UnixListener;

    async fn connected_pair() -> (
        tempfile::TempDir,
        FrameReader,
        FrameWriter,
        FrameReader,
        FrameWriter,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });
        let client = UnixStream::connect(&path).await.unwrap();
        let server = accept.await.unwrap();
        let (cr, cw) = split(client);
        let (sr, sw) = split(server);
        (dir, cr, cw, sr, sw)
    }

    #[tokio::test]
    async fn client_frame_round_trip() {
        let (_dir, _cr, mut cw, mut sr, _sw) = connected_pair().await;
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
        let (_dir, mut cr, _cw, _sr, mut sw) = connected_pair().await;
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
    async fn oversized_unterminated_frame_rejects_without_unbounded_buffering() {
        let (_dir, mut cr, _cw, _sr, mut sw) = connected_pair().await;
        let writer = tokio::spawn(async move {
            let chunk = vec![b'a'; 64 * 1024];
            let mut written = 0usize;
            while written <= MAX_FRAME_BYTES + 128 * 1024 {
                if sw.write_all(&chunk).await.is_err() {
                    break;
                }
                written += chunk.len();
            }
            let _ = sw.flush().await;
        });
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_server_frame(&mut cr),
        )
        .await
        .expect("must not hang waiting for newline");
        let err = res.expect_err("oversized stream must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        drop(_cw);
        drop(cr);
        let _ = writer.await;
    }

    #[tokio::test]
    async fn clean_close_yields_none() {
        let (_dir, mut cr, _cw, sr, sw) = connected_pair().await;
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
