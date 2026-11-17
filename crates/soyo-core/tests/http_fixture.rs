#[path = "fixtures/http_support/mod.rs"]
mod fixture;

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::fd::AsRawFd,
    sync::mpsc,
    time::Duration,
};

fn send_request(socket: &mut TcpStream) -> std::io::Result<()> {
    socket.write_all(
        b"POST /v1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
    )
}

#[test]
fn dynamic_server_accepts_next_request_after_client_disconnects()
-> Result<(), Box<dyn std::error::Error>> {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (url, requests) = fixture::server_dynamic(2, move |index, _| {
        if index == 0 {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        ("application/json", format!("{{\"index\":{index}}}"))
    })?;
    let address: SocketAddr = url
        .trim_start_matches("http://")
        .trim_end_matches("/v1")
        .parse()?;
    let mut first = TcpStream::connect(address)?;
    send_request(&mut first)?;
    entered_rx.recv_timeout(Duration::from_secs(5))?;
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: the socket is live and linger points to a valid value for setsockopt.
    let configured = unsafe {
        libc::setsockopt(
            first.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&linger as *const libc::linger).cast(),
            std::mem::size_of_val(&linger) as libc::socklen_t,
        )
    };
    assert_eq!(configured, 0, "{}", std::io::Error::last_os_error());
    drop(first);
    release_tx.send(())?;
    let mut second = TcpStream::connect(address)?;
    second.set_read_timeout(Some(Duration::from_secs(5)))?;
    send_request(&mut second)?;
    let mut response = String::new();
    second.read_to_string(&mut response)?;
    assert!(response.contains("{\"index\":1}"), "{response}");
    assert!(requests.recv_timeout(Duration::from_secs(5))?.is_object());
    assert!(requests.recv_timeout(Duration::from_secs(5))?.is_object());
    Ok(())
}
