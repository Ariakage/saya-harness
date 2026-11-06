use saya_core::{
    boundary::Cancellation,
    provider::transport::{Authentication, HttpConfig, HttpTransport},
};
use std::{
    collections::BTreeMap,
    future::{Future, pending},
    io::{Read, Write},
    net::TcpListener,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
#[derive(Default)]
struct Signal {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
}
impl Signal {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}
impl Cancellation for Signal {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            loop {
                let notified = self.notify.notified();
                if self.is_cancelled() {
                    return;
                }
                notified.await;
            }
        })
    }
}

fn config(url: String) -> HttpConfig {
    HttpConfig {
        base_url: url,
        allow_remote_http: false,
        authentication: Authentication::Bearer("test-secret".into()),
        headers_env: BTreeMap::new(),
        connect_timeout: Duration::from_secs(1),
        idle_timeout: Duration::from_secs(1),
        total_timeout: Duration::from_secs(3),
    }
}
fn server(
    parts: Vec<(Duration, Vec<u8>)>,
) -> Result<(String, mpsc::Receiver<String>), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut socket, _)) = listener.accept() else {
            return;
        };
        let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
        let mut request = Vec::new();
        let mut buf = [0u8; 2048];
        loop {
            match socket.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => request.extend_from_slice(&buf[..n]),
            }
            if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..pos]);
                let len = headers
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= pos + 4 + len {
                    break;
                }
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&request).into_owned());
        for (delay, bytes) in parts {
            std::thread::sleep(delay);
            if socket.write_all(&bytes).is_err() {
                break;
            }
            let _ = socket.flush();
        }
    });
    Ok((format!("http://{address}/custom/v1"), rx))
}
fn chunk(s: &str) -> (Duration, Vec<u8>) {
    (Duration::ZERO, s.as_bytes().to_vec())
}

#[tokio::test]
async fn posts_to_prefixed_endpoint_and_decodes_fragmented_sse()
-> Result<(), Box<dyn std::error::Error>> {
    let (url, rx) = server(vec![
        chunk(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nConnection: close\r\n\r\n\u{feff}: comment\r",
        ),
        chunk("\nevent: delta\rdata: hel"),
        (Duration::ZERO, b"lo\r\ndata: \xe4".to_vec()),
        (
            Duration::from_millis(5),
            b"\xb8\x96\xe7\x95\x8c\r\n\r\ndata:\n\n".to_vec(),
        ),
    ])?;
    let transport = HttpTransport::new(config(url))?;
    let response = transport
        .post(
            "chat/completions",
            &serde_json::json!({"stream":true}),
            None,
            &NeverCancel,
        )
        .await?;
    let mut events = response.events()?;
    let event = events
        .next(&NeverCancel)
        .await?
        .ok_or("missing first event")?;
    assert_eq!(
        (event.event.as_str(), event.data.as_str()),
        ("delta", "hello\n世界")
    );
    assert_eq!(
        events
            .next(&NeverCancel)
            .await?
            .ok_or("missing empty event")?
            .data,
        ""
    );
    assert!(events.next(&NeverCancel).await?.is_none());
    let request = rx.recv_timeout(Duration::from_secs(1))?;
    assert!(request.starts_with("POST /custom/v1/chat/completions HTTP/1.1\r\n"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-secret\r\n")
    );
    assert!(request.contains("\"stream\":true"));
    Ok(())
}

#[tokio::test]
async fn status_errors_have_safe_messages_retry_metadata_and_no_redirect()
-> Result<(), Box<dyn std::error::Error>> {
    for (status, expected) in [
        ("429 Too Many Requests", "rate_limited"),
        ("503 Service Unavailable", "server_error"),
        ("401 Unauthorized", "authentication_error"),
        ("302 Found", "provider_error"),
    ] {
        let (url, _rx) = server(vec![chunk(&format!(
            "HTTP/1.1 {status}\r\nLocation: http://example.invalid/leak\r\nRetry-After: 12\r\nContent-Length: 11\r\n\r\ntest-secret"
        ))])?;
        let transport = HttpTransport::new(config(url))?;
        let error = match transport
            .post("responses", &serde_json::json!({}), None, &NeverCancel)
            .await
        {
            Ok(_) => return Err("unexpected success".into()),
            Err(e) => e,
        };
        assert_eq!(error.0.code, expected);
        assert_eq!(error.1, Some(Duration::from_secs(12)));
        assert!(!error.to_string().contains("test-secret"));
        assert!(!error.to_string().contains("example.invalid"));
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_and_idle_timeout_interrupt_stalled_stream()
-> Result<(), Box<dyn std::error::Error>> {
    for cancel_request in [true, false] {
        let (url, _rx) = server(vec![
            chunk(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            ),
            (Duration::from_millis(300), b"data: late\n\n".to_vec()),
        ])?;
        let mut cfg = config(url);
        cfg.idle_timeout = Duration::from_millis(80);
        let transport = HttpTransport::new(cfg)?;
        let signal = Arc::new(Signal::default());
        let mut events = transport
            .post("responses", &serde_json::json!({}), None, signal.as_ref())
            .await?
            .events()?;
        if cancel_request {
            let signal2 = signal.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                signal2.cancel();
            });
        }
        let error = events
            .next(signal.as_ref())
            .await
            .expect_err("stream should stop");
        assert_eq!(
            error.0.code,
            if cancel_request {
                "cancelled"
            } else {
                "model_timeout"
            }
        );
    }
    Ok(())
}

#[test]
fn rejects_unsafe_base_urls_and_reserved_headers_without_leaking_values() {
    for url in [
        "http://example.com/v1",
        "ftp://127.0.0.1/v1",
        "https://secret@example.com/v1",
        "https://example.com/v1?token=secret",
    ] {
        let error = match HttpTransport::new(config(url.into())) {
            Ok(_) => panic!("unsafe URL accepted"),
            Err(e) => e,
        };
        assert_eq!(error.0.code, "invalid_config");
        assert!(!error.to_string().contains("secret"));
    }
    let mut cfg = config("http://localhost/v1".into());
    cfg.headers_env
        .insert("Authorization".into(), "SECRET_NAME".into());
    let error = match HttpTransport::new(cfg) {
        Ok(_) => panic!("reserved header accepted"),
        Err(e) => e,
    };
    assert_eq!(error.0.code, "invalid_config");
}

#[tokio::test]
async fn malformed_sse_and_wrong_content_type_are_rejected()
-> Result<(), Box<dyn std::error::Error>> {
    let (url, _rx) = server(vec![(Duration::ZERO, b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: \xff\n\n".to_vec())])?;
    let transport = HttpTransport::new(config(url))?;
    let mut events = transport
        .post("responses", &serde_json::json!({}), None, &NeverCancel)
        .await?
        .events()?;
    assert_eq!(
        events
            .next(&NeverCancel)
            .await
            .expect_err("invalid UTF-8")
            .0
            .code,
        "invalid_model_response"
    );
    let (url, _rx) = server(vec![chunk(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
    )])?;
    let transport = HttpTransport::new(config(url))?;
    let response = transport
        .post("responses", &serde_json::json!({}), None, &NeverCancel)
        .await?;
    assert!(response.events().is_err());
    Ok(())
}

#[tokio::test]
async fn yields_complete_event_before_a_later_invalid_event()
-> Result<(), Box<dyn std::error::Error>> {
    let (url, _rx) = server(vec![(Duration::ZERO, b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: first\n\ndata: \xff\n\n".to_vec())])?;
    let transport = HttpTransport::new(config(url))?;
    let mut events = transport
        .post("responses", &serde_json::json!({}), None, &NeverCancel)
        .await?
        .events()?;
    assert_eq!(
        events
            .next(&NeverCancel)
            .await?
            .ok_or("missing event")?
            .data,
        "first"
    );
    assert_eq!(
        events
            .next(&NeverCancel)
            .await
            .expect_err("invalid UTF-8")
            .0
            .code,
        "invalid_model_response"
    );
    Ok(())
}

#[tokio::test]
async fn reads_json_and_rejects_oversized_events() -> Result<(), Box<dyn std::error::Error>> {
    let (url, _rx) = server(vec![chunk(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 7\r\n\r\n{\"x\":1}",
    )])?;
    let transport = HttpTransport::new(config(url))?;
    let response = transport
        .post("responses", &serde_json::json!({}), None, &NeverCancel)
        .await?;
    assert_eq!(response.json(&NeverCancel).await?["x"], 1);
    let mut response =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: "
            .to_vec();
    response.extend(std::iter::repeat_n(b'x', 1024 * 1024 + 1));
    let (url, _rx) = server(vec![(Duration::ZERO, response)])?;
    let transport = HttpTransport::new(config(url))?;
    let mut events = transport
        .post("responses", &serde_json::json!({}), None, &NeverCancel)
        .await?
        .events()?;
    assert_eq!(
        events
            .next(&NeverCancel)
            .await
            .expect_err("oversized event")
            .0
            .code,
        "invalid_model_response"
    );
    Ok(())
}

#[test]
fn system_proxy_environment_is_respected() -> Result<(), Box<dyn std::error::Error>> {
    const FLAG: &str = "SAYA_TRANSPORT_PROXY_TEST_CHILD";
    if std::env::var_os(FLAG).is_some() {
        let mut cfg = config("http://destination.invalid/v1".into());
        cfg.allow_remote_http = true;
        let transport = HttpTransport::new(cfg)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let response = transport
                .post("responses", &serde_json::json!({}), None, &NeverCancel)
                .await?;
            assert_eq!(response.json(&NeverCancel).await?["proxy"], true);
            Ok::<_, saya_core::contract::BoundaryError>(())
        })?;
        return Ok(());
    }
    let (url, rx) = server(vec![chunk(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"proxy\":true}",
    )])?;
    let proxy = url.trim_end_matches("/custom/v1");
    let result = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "system_proxy_environment_is_respected",
            "--nocapture",
        ])
        .env(FLAG, "1")
        .env("HTTP_PROXY", proxy)
        .env("http_proxy", proxy)
        .env("ALL_PROXY", proxy)
        .env("all_proxy", proxy)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()?;
    assert!(result.status.success(), "proxy child test failed");
    let request = rx.recv_timeout(Duration::from_secs(1))?;
    assert!(request.starts_with("POST http://destination.invalid/v1/responses HTTP/1.1\r\n"));
    Ok(())
}

#[tokio::test]
async fn anthropic_key_version_and_environment_headers_are_separate()
-> Result<(), Box<dyn std::error::Error>> {
    let (url, rx) = server(vec![chunk(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
    )])?;
    let mut cfg = config(url);
    cfg.authentication = Authentication::ApiKey("test-api-key".into());
    cfg.headers_env
        .insert("x-fixture-environment".into(), "PATH".into());
    let transport = HttpTransport::new(cfg)?;
    transport
        .post(
            "messages",
            &serde_json::json!({}),
            Some("2023-06-01"),
            &NeverCancel,
        )
        .await?
        .json(&NeverCancel)
        .await?;
    let request = rx
        .recv_timeout(Duration::from_secs(1))?
        .to_ascii_lowercase();
    assert!(request.contains("x-api-key: test-api-key\r\n"));
    assert!(request.contains("anthropic-version: 2023-06-01\r\n"));
    assert!(request.contains("x-fixture-environment:"));
    assert!(!request.contains("authorization:"));
    Ok(())
}

#[tokio::test]
async fn total_timeout_stops_a_stream_even_if_bytes_keep_arriving()
-> Result<(), Box<dyn std::error::Error>> {
    let mut parts = vec![chunk(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    )];
    for _ in 0..12 {
        parts.push((Duration::from_millis(25), b": keepalive\n\n".to_vec()));
    }
    let (url, _) = server(parts)?;
    let mut cfg = config(url);
    cfg.idle_timeout = Duration::from_millis(200);
    cfg.total_timeout = Duration::from_millis(100);
    let transport = HttpTransport::new(cfg)?;
    let mut events = transport
        .post("responses", &serde_json::json!({}), None, &NeverCancel)
        .await?
        .events()?;
    let error = events
        .next(&NeverCancel)
        .await
        .expect_err("total timeout should stop the stream");
    assert_eq!(error.0.code, "model_timeout");
    Ok(())
}
