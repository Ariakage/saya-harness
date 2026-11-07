#![allow(dead_code)]
use serde_json::Value;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    time::Duration,
};

/// Minimal controlled HTTP/1.1 fixture. No credentials or remote requests.
pub fn server(
    responses: Vec<(&'static str, String)>,
) -> Result<(String, mpsc::Receiver<Value>), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for (content_type, body) in responses {
            let Ok((mut socket, _)) = listener.accept() else {
                break;
            };
            let _ = socket.set_read_timeout(Some(Duration::from_secs(3)));
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            let body_pos = loop {
                match socket.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
                if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..pos]);
                    let size = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= pos + 4 + size {
                        break pos + 4;
                    }
                }
            };
            if let Ok(json) = serde_json::from_slice(&request[body_pos..]) {
                let _ = sender.send(json);
            }
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            if socket.write_all(header.as_bytes()).is_err() {
                break;
            }
            for chunk in body.as_bytes().chunks(13) {
                if socket.write_all(chunk).is_err() {
                    return;
                }
            }
        }
    });
    Ok((format!("http://{address}/v1"), receiver))
}
pub fn sse(events: &[Value]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

type CapturedRequests = mpsc::Receiver<(Value, String)>;
pub fn server_with_headers(
    responses: Vec<(&'static str, String)>,
) -> Result<(String, CapturedRequests), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for (content_type, body) in responses {
            let Ok((mut socket, _)) = listener.accept() else {
                break;
            };
            let _ = socket.set_read_timeout(Some(Duration::from_secs(3)));
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            let (body_pos, headers) = loop {
                match socket.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
                if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..pos]).into_owned();
                    let size = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= pos + 4 + size {
                        break (pos + 4, headers);
                    }
                }
            };
            if let Ok(json) = serde_json::from_slice(&request[body_pos..]) {
                let _ = sender.send((json, headers));
            }
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            if socket.write_all(header.as_bytes()).is_err() {
                break;
            }
            for chunk in body.as_bytes().chunks(13) {
                if socket.write_all(chunk).is_err() {
                    return;
                }
            }
        }
    });
    Ok((format!("http://{address}/v1"), receiver))
}

/// A fixed number of requests are answered from their actual JSON bodies.
pub fn server_dynamic<F>(
    count: usize,
    mut respond: F,
) -> Result<(String, mpsc::Receiver<Value>), Box<dyn std::error::Error>>
where
    F: FnMut(usize, &Value) -> (&'static str, String) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        'requests: for index in 0..count {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            let body_pos = loop {
                match socket.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
                if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..pos]);
                    let len = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= pos + 4 + len {
                        break pos + 4;
                    }
                }
            };
            let Ok(json) = serde_json::from_slice::<Value>(&request[body_pos..]) else {
                return;
            };
            let (content_type, body) = respond(index, &json);
            let _ = sender.send(json);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            if socket.write_all(header.as_bytes()).is_err() {
                continue;
            }
            for chunk in body.as_bytes().chunks(17) {
                if socket.write_all(chunk).is_err() {
                    continue 'requests;
                }
            }
        }
    });
    Ok((format!("http://{address}/v1"), receiver))
}
