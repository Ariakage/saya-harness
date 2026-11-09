use saya_core::boundary::Cancellation;
use saya_core::mcp::{McpClient, McpContent, McpHttpConfig, McpLimits};
use serde_json::{Value, json};
use std::future::{Future, pending};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};

struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}

struct Cancel(watch::Sender<bool>);
impl Cancel {
    fn new() -> Self {
        Self(watch::channel(false).0)
    }
    fn cancel(&self) {
        self.0.send_replace(true);
    }
}
impl Cancellation for Cancel {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let mut receiver = self.0.subscribe();
        Box::pin(async move {
            while !*receiver.borrow_and_update() {
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}

#[derive(Clone, Copy)]
enum Era {
    Modern,
    Legacy,
}

#[derive(Clone)]
struct Scenario {
    era: Era,
    list_changed: bool,
    repeating_cursor: bool,
    input_required: bool,
    hang_tool: bool,
    malformed_schema: bool,
    sse: bool,
    drop_tool: bool,
    tool_error: bool,
    padding_method: Option<&'static str>,
    bad_output: bool,
    changing_catalog: bool,
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            era: Era::Modern,
            list_changed: false,
            repeating_cursor: false,
            input_required: false,
            hang_tool: false,
            malformed_schema: false,
            sse: false,
            drop_tool: false,
            tool_error: false,
            padding_method: None,
            bad_output: false,
            changing_catalog: false,
        }
    }
}

fn response(request: &Value, scenario: &Scenario) -> Option<Value> {
    let id = request.get("id")?.clone();
    let mut result = match request["method"].as_str()? {
        "server/discover" => match scenario.era {
            Era::Modern => {
                json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{"listChanged":scenario.list_changed}},"ttlMs":0,"cacheScope":"private"})
            }
            Era::Legacy => {
                return Some(
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"legacy fixture"}}),
                );
            }
        },
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":scenario.list_changed}},"serverInfo":{"name":"fixture","version":"1"}})
        }
        "tools/list" => {
            let schema = if scenario.malformed_schema {
                json!({"type":"invalid-type"})
            } else {
                json!({"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false})
            };
            let mut result = json!({"resultType":"complete","tools":[{"name":"echo","description":"Untrusted fixture tool", "inputSchema":schema}],"ttlMs":0,"cacheScope":"private"});
            if scenario.bad_output {
                result["tools"][0]["outputSchema"] = json!({"type":"object","properties":{"ok":{"type":"string"}},"required":["ok"]});
            }
            if scenario.repeating_cursor {
                result["tools"] = json!([]);
                result["nextCursor"] = json!("repeated");
            }
            result
        }
        "tools/call" if scenario.hang_tool => return None,
        "tools/call" if scenario.input_required => {
            json!({"resultType":"input_required","inputRequests":{"more":{"method":"elicitation/create","params":{"message":"need input","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}}}}}},"requestState":"opaque"})
        }
        "tools/call" => {
            json!({"resultType":"complete","content":[{"type":"text","text":request["params"]["arguments"]["message"]}],"structuredContent":{"ok":true},"isError":scenario.tool_error})
        }
        _ => {
            return Some(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unsupported fixture method"}}),
            );
        }
    };
    if scenario
        .padding_method
        .is_some_and(|method| request["method"] == method)
    {
        result["_meta"] = json!({"padding":"x".repeat(4096)});
    }
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

struct StdioFixture {
    requests: mpsc::Receiver<Value>,
    notify: mpsc::Sender<Value>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for StdioFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn stdio_fixture(scenario: Scenario, limits: McpLimits) -> (McpClient, StdioFixture) {
    let (client, server) = tokio::io::duplex(4 * 1024 * 1024);
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, mut server_write) = tokio::io::split(server);
    let (requests_tx, requests) = mpsc::channel(64);
    let (notify, mut notifications) = mpsc::channel::<Value>(16);
    let task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let mut subscription = None;
        let mut list_count = 0;
        loop {
            let outgoing = tokio::select! {
                line = lines.next_line() => {
                    let Ok(Some(line)) = line else { break; };
                    let request:Value = serde_json::from_str(&line).unwrap();
                    requests_tx.send(request.clone()).await.unwrap();
                    if request["method"]=="tools/call" && scenario.drop_tool {break;}
                    if request["method"] == "subscriptions/listen" {
                        subscription = Some(request["id"].clone());
                        Some(json!({"jsonrpc":"2.0","method":"notifications/subscriptions/acknowledged","params":{"_meta":{"io.modelcontextprotocol/subscriptionId":request["id"]},"notifications":{"toolsListChanged":true}}}))
                    } else {
                        let mut response=response(&request, &scenario);
                        if request["method"]=="tools/list" && scenario.changing_catalog {
                            list_count+=1;
                            if let Some(response)=&mut response {response["result"]["tools"][0]["description"]=json!(format!("catalogue-{list_count}"));}
                        }
                        response
                    }
                }
                notification = notifications.recv() => {
                    let Some(mut notification) = notification else { break; };
                    if matches!(scenario.era,Era::Modern) && let Some(id) = &subscription {
                        notification["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] = id.clone();
                    }
                    Some(notification)
                }
            };
            if let Some(outgoing) = outgoing
                && server_write
                    .write_all(format!("{outgoing}\n").as_bytes())
                    .await
                    .is_err()
            {
                break;
            }
        }
    });
    let client = McpClient::connect_stdio("fixture", client_read, client_write, limits, &Never)
        .await
        .unwrap();
    (
        client,
        StdioFixture {
            requests,
            notify,
            task,
        },
    )
}

fn fast_limits() -> McpLimits {
    McpLimits {
        connect_timeout: Duration::from_secs(3),
        request_timeout: Duration::from_secs(3),
        ..McpLimits::default()
    }
}

#[tokio::test]
async fn stdio_modern_and_legacy_discover_validate_and_execute_once() {
    for era in [Era::Modern, Era::Legacy] {
        let (client, mut fixture) = stdio_fixture(
            Scenario {
                era,
                ..Scenario::default()
            },
            fast_limits(),
        )
        .await;
        let catalog = client.list_tools(&Never).await.unwrap();
        let result = client
            .call_tool(
                "echo",
                json!({"message":"hello"}),
                catalog.generation,
                &Never,
            )
            .await
            .unwrap();
        assert!(matches!(&result.content[0], McpContent::Text {text} if text == "hello"));
        assert_eq!(result.structured_content, Some(json!({"ok":true})));
        let invalid = client
            .call_tool("echo", json!({"message":7}), catalog.generation, &Never)
            .await
            .unwrap_err();
        assert_eq!(invalid.0.code, "invalid_args");
        let mut methods = Vec::new();
        while let Ok(request) = fixture.requests.try_recv() {
            if request["method"] == "server/discover" {
                let capabilities =
                    &request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"];
                assert!(capabilities.get("sampling").is_none());
                assert!(capabilities.get("roots").is_none());
                assert!(capabilities.get("elicitation").is_none());
            }
            methods.push(request["method"].as_str().unwrap().to_owned());
        }
        assert_eq!(
            methods
                .iter()
                .filter(|method| method.as_str() == "tools/call")
                .count(),
            1
        );
        assert_eq!(
            methods.iter().any(|method| method == "initialize"),
            matches!(era, Era::Legacy)
        );
        client.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn stdio_list_changes_invalidate_approvals_in_both_eras() {
    for era in [Era::Modern, Era::Legacy] {
        let (client, fixture) = stdio_fixture(
            Scenario {
                era,
                list_changed: true,
                ..Scenario::default()
            },
            fast_limits(),
        )
        .await;
        let catalog = client.list_tools(&Never).await.unwrap();
        fixture
            .notify
            .send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{}}))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while client.generation() == catalog.generation {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let result = client
            .call_tool(
                "echo",
                json!({"message":"stale"}),
                catalog.generation,
                &Never,
            )
            .await
            .unwrap_err();
        assert_eq!(result.0.code, "mcp_stale_catalog");
        let fresh = client.list_tools(&Never).await.unwrap();
        assert!(fresh.generation > catalog.generation);
        client.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn stdio_discovery_and_output_limits_fail_closed() {
    let (client, _fixture) = stdio_fixture(
        Scenario {
            repeating_cursor: true,
            ..Scenario::default()
        },
        fast_limits(),
    )
    .await;
    assert_eq!(
        client.list_tools(&Never).await.unwrap_err().0.code,
        "mcp_protocol"
    );
    client.shutdown().await.unwrap();
    let limits = McpLimits {
        max_catalog_bytes: 16,
        ..fast_limits()
    };
    let (client, _fixture) = stdio_fixture(Scenario::default(), limits).await;
    assert_eq!(
        client.list_tools(&Never).await.unwrap_err().0.code,
        "mcp_limit"
    );
    client.shutdown().await.unwrap();
    let (client, _fixture) = stdio_fixture(
        Scenario {
            malformed_schema: true,
            ..Scenario::default()
        },
        fast_limits(),
    )
    .await;
    assert_eq!(
        client.list_tools(&Never).await.unwrap_err().0.code,
        "mcp_schema"
    );
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn input_required_is_returned_without_automatic_follow_up() {
    let (client, mut fixture) = stdio_fixture(
        Scenario {
            input_required: true,
            ..Scenario::default()
        },
        fast_limits(),
    )
    .await;
    let catalog = client.list_tools(&Never).await.unwrap();
    let error = client
        .call_tool("echo", json!({"message":"one"}), catalog.generation, &Never)
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "needs_input");
    let mut calls = 0;
    while let Ok(request) = fixture.requests.try_recv() {
        calls += usize::from(request["method"] == "tools/call");
    }
    assert_eq!(calls, 1);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn stdio_cancellation_sends_cancel_without_replaying_tool() {
    let (client, mut fixture) = stdio_fixture(
        Scenario {
            hang_tool: true,
            ..Scenario::default()
        },
        fast_limits(),
    )
    .await;
    let catalog = client.list_tools(&Never).await.unwrap();
    while fixture.requests.try_recv().is_ok() {}
    let cancel = Cancel::new();
    {
        let call = client.call_tool(
            "echo",
            json!({"message":"one"}),
            catalog.generation,
            &cancel,
        );
        tokio::pin!(call);
        let observed = tokio::select! { result=&mut call => panic!("tool completed unexpectedly {result:?}"), request=fixture.requests.recv()=>request.unwrap() };
        assert_eq!(observed["method"], "tools/call");
        cancel.cancel();
        assert_eq!((&mut call).await.unwrap_err().0.code, "outcome_unknown");
        let notification = tokio::time::timeout(Duration::from_secs(1), fixture.requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(notification["method"], "notifications/cancelled");
        assert!(fixture.requests.try_recv().is_err());
    }
    client.shutdown().await.unwrap();
}

struct HttpFixture {
    endpoint: String,
    requests: Arc<std::sync::Mutex<Vec<(Value, String)>>>,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    refreshes: Arc<AtomicUsize>,
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn http_fixture(scenario: Scenario, tool_status: u16) -> HttpFixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = requests.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let call_count = calls.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let refreshes = Arc::new(AtomicUsize::new(0));
    let refreshed = refreshes.clone();
    let resource = endpoint.clone();
    let origin = endpoint.trim_end_matches("/mcp").to_string();
    let cancelled = Arc::new(AtomicBool::new(false));
    let disconnected = cancelled.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            };
            // BSD sockets may inherit nonblocking mode from the listener.
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let (body_start, headers) = loop {
                let Ok(length) = socket.read(&mut buffer) else {
                    break (0, String::new());
                };
                if length == 0 {
                    break (0, String::new());
                }
                bytes.extend_from_slice(&buffer[..length]);
                if let Some(start) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..start]).into_owned();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|text| text.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= start + 4 + length {
                        break (start + 4, headers);
                    }
                }
            };
            if body_start == 0 {
                continue;
            }
            let (status, body) = if headers.starts_with("GET ")
                && headers.contains("oauth-protected-resource")
            {
                (200,json!({"resource":resource,"authorization_servers":[format!("{origin}/")],"scopes_supported":["read"]}).to_string())
            } else if headers.starts_with("GET ")
                && (headers.contains("oauth-authorization-server")
                    || headers.contains("openid-configuration"))
            {
                (200,json!({"issuer":format!("{origin}/"),"authorization_endpoint":format!("{origin}/authorize"),"token_endpoint":format!("{origin}/token"),"response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"code_challenge_methods_supported":["S256"]}).to_string())
            } else if headers.starts_with("POST /token ") {
                refreshed.fetch_add(1, Ordering::AcqRel);
                (200,json!({"access_token":"refreshed-fixture-token","refresh_token":"next-refresh-token","token_type":"Bearer","expires_in":3600,"scope":"read"}).to_string())
            } else if headers.starts_with("GET ") {
                (405, String::new())
            } else if let Ok(request) = serde_json::from_slice::<Value>(&bytes[body_start..]) {
                captured.lock().unwrap().push((request.clone(), headers));
                let tool = request["method"] == "tools/call";
                if tool {
                    call_count.fetch_add(1, Ordering::AcqRel);
                }
                if tool && scenario.drop_tool {
                    continue;
                }
                if tool && scenario.hang_tool {
                    let _=socket.write_all(b"HTTP/1.1 200 Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: 10000000\r\nConnection: close\r\n\r\n");
                    let _ = socket.set_read_timeout(Some(Duration::from_millis(20)));
                    while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
                        if matches!(socket.read(&mut buffer), Ok(0)) {
                            disconnected.store(true, Ordering::Release);
                            break;
                        }
                    }
                    continue;
                }
                if tool && tool_status != 200 {
                    (tool_status, "{}".into())
                } else {
                    let result = response(&request, &scenario);
                    (
                        if result.is_some() { 200 } else { 202 },
                        result.map(|result| result.to_string()).unwrap_or_default(),
                    )
                }
            } else {
                (400, String::new())
            };
            let challenge = if status == 401 {
                "WWW-Authenticate: Bearer resource_metadata=\"https://auth.invalid/meta\"\r\n"
            } else {
                ""
            };
            let is_sse = scenario.sse && status == 200;
            let body = if is_sse {
                format!("data: {body}\n\n")
            } else {
                body
            };
            let content_type = if is_sse {
                "text/event-stream"
            } else {
                "application/json"
            };
            let reply = format!(
                "HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{challenge}\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes());
        }
    });
    HttpFixture {
        endpoint,
        requests,
        calls,
        stop,
        cancelled,
        refreshes,
    }
}

#[tokio::test]
async fn http_sse_response_is_supported_in_both_protocol_eras() {
    for era in [Era::Modern, Era::Legacy] {
        let fixture = http_fixture(
            Scenario {
                era,
                sse: true,
                ..Scenario::default()
            },
            200,
        );
        let mut config = McpHttpConfig::new(&fixture.endpoint);
        config.allow_insecure_http = true;
        let client = McpClient::connect_http("remote", config, fast_limits(), &Never)
            .await
            .unwrap();
        let catalog = client.list_tools(&Never).await.unwrap();
        let result = client
            .call_tool("echo", json!({"message":"sse"}), catalog.generation, &Never)
            .await
            .unwrap();
        assert!(matches!(&result.content[0],McpContent::Text{text} if text=="sse"));
        assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
        client.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn modern_http_cancellation_closes_only_the_request_stream_without_replay() {
    let fixture = http_fixture(
        Scenario {
            hang_tool: true,
            ..Scenario::default()
        },
        200,
    );
    let mut config = McpHttpConfig::new(&fixture.endpoint);
    config.allow_insecure_http = true;
    let client = McpClient::connect_http("remote", config, fast_limits(), &Never)
        .await
        .unwrap();
    let catalog = client.list_tools(&Never).await.unwrap();
    let cancel = Cancel::new();
    {
        let call = client.call_tool(
            "echo",
            json!({"message":"cancel"}),
            catalog.generation,
            &cancel,
        );
        tokio::pin!(call);
        tokio::select! {
            result=&mut call=>panic!("unexpected completion {result:?}"),
            _=async {while fixture.calls.load(Ordering::Acquire)==0 {tokio::time::sleep(Duration::from_millis(1)).await;}} =>{}
        }
        cancel.cancel();
        assert_eq!((&mut call).await.unwrap_err().0.code, "outcome_unknown");
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while !fixture.cancelled.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request["method"] == "notifications/cancelled")
    );
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn http_modern_and_legacy_json_protocols_include_metadata_and_transient_auth() {
    for era in [Era::Modern, Era::Legacy] {
        let fixture = http_fixture(
            Scenario {
                era,
                ..Scenario::default()
            },
            200,
        );
        let mut config = McpHttpConfig::new(&fixture.endpoint);
        config.allow_insecure_http = true;
        config.bearer_token = Some("fixture-token".into());
        assert!(!format!("{config:?}").contains("fixture-token"));
        let client = McpClient::connect_http("remote", config, fast_limits(), &Never)
            .await
            .unwrap();
        let catalog = client.list_tools(&Never).await.unwrap();
        client
            .call_tool(
                "echo",
                json!({"message":"hello"}),
                catalog.generation,
                &Never,
            )
            .await
            .unwrap();
        assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
        for (request, headers) in fixture.requests.lock().unwrap().iter() {
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-token")
            );
            if matches!(era, Era::Modern) {
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("mcp-protocol-version: 2026-07-28")
                );
                assert_eq!(
                    request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                    "2026-07-28"
                );
            }
        }
        client.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn http_401_and_404_stop_without_reinitializing_or_replaying() {
    for status in [401, 404, 429, 500] {
        let fixture = http_fixture(Scenario::default(), status);
        let mut config = McpHttpConfig::new(&fixture.endpoint);
        config.allow_insecure_http = true;
        let client = McpClient::connect_http("remote", config, fast_limits(), &Never)
            .await
            .unwrap();
        let catalog = client.list_tools(&Never).await.unwrap();
        assert!(
            client
                .call_tool("echo", json!({"message":"one"}), catalog.generation, &Never)
                .await
                .is_err()
        );
        assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
        assert_eq!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(request, _)| request["method"] == "server/discover")
                .count(),
            1
        );
        client.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn http_rejects_embedded_credentials_and_reserved_header_overrides() {
    for endpoint in [
        "http://example.invalid/mcp",
        "https://user:secret@example.invalid/mcp",
        "https://example.invalid/mcp?token=secret",
        "https://example.invalid/mcp#token",
    ] {
        let error = McpClient::connect_http(
            "remote",
            McpHttpConfig::new(endpoint),
            fast_limits(),
            &Never,
        )
        .await
        .unwrap_err();
        assert_eq!(error.0.code, "invalid_config");
        assert!(!error.0.message.contains("secret"));
    }
    let mut config = McpHttpConfig::new("https://example.invalid/mcp");
    config
        .headers
        .insert("authorization".into(), "secret".into());
    assert_eq!(
        McpClient::connect_http("remote", config, fast_limits(), &Never)
            .await
            .unwrap_err()
            .0
            .code,
        "invalid_config"
    );
}

#[tokio::test]
async fn lost_stdio_and_http_tool_responses_are_unknown_and_never_replayed() {
    let (client, mut fixture) = stdio_fixture(
        Scenario {
            drop_tool: true,
            ..Scenario::default()
        },
        fast_limits(),
    )
    .await;
    let catalog = client.list_tools(&Never).await.unwrap();
    assert_eq!(
        client
            .call_tool("echo", json!({"message":"one"}), catalog.generation, &Never)
            .await
            .unwrap_err()
            .0
            .code,
        "outcome_unknown"
    );
    let mut calls = 0;
    while let Ok(request) = fixture.requests.try_recv() {
        calls += usize::from(request["method"] == "tools/call");
    }
    assert_eq!(calls, 1);
    client.shutdown().await.unwrap();
    let fixture = http_fixture(
        Scenario {
            drop_tool: true,
            ..Scenario::default()
        },
        200,
    );
    let mut config = McpHttpConfig::new(&fixture.endpoint);
    config.allow_insecure_http = true;
    let client = McpClient::connect_http("remote", config, fast_limits(), &Never)
        .await
        .unwrap();
    let catalog = client.list_tools(&Never).await.unwrap();
    assert_eq!(
        client
            .call_tool("echo", json!({"message":"one"}), catalog.generation, &Never)
            .await
            .unwrap_err()
            .0
            .code,
        "outcome_unknown"
    );
    assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn tool_timeout_is_unknown_but_cancellation_before_dispatch_is_known() {
    let limits = McpLimits {
        request_timeout: Duration::from_millis(30),
        ..fast_limits()
    };
    let (client, mut fixture) = stdio_fixture(
        Scenario {
            hang_tool: true,
            ..Scenario::default()
        },
        limits,
    )
    .await;
    let catalog = client.list_tools(&Never).await.unwrap();
    while fixture.requests.try_recv().is_ok() {}
    let cancel = Cancel::new();
    cancel.cancel();
    assert_eq!(
        client
            .call_tool(
                "echo",
                json!({"message":"not sent"}),
                catalog.generation,
                &cancel
            )
            .await
            .unwrap_err()
            .0
            .code,
        "cancelled"
    );
    assert!(fixture.requests.try_recv().is_err());
    assert_eq!(
        client
            .call_tool(
                "echo",
                json!({"message":"timeout"}),
                catalog.generation,
                &Never
            )
            .await
            .unwrap_err()
            .0
            .code,
        "outcome_unknown"
    );
    let sent = tokio::time::timeout(Duration::from_secs(1), fixture.requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sent["method"], "tools/call");
    let cancelled = tokio::time::timeout(Duration::from_secs(1), fixture.requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled["method"], "notifications/cancelled");
    assert!(fixture.requests.try_recv().is_err());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn adapter_preserves_remote_error_content_and_propagates_input_and_unknown() {
    use saya_core::boundary::{Tool, ToolContext};
    use saya_core::contract::{PermissionMode, Run, RunId, SessionId, ToolCall, WorkMode};
    use saya_core::mcp::McpToolAdapter;
    for (scenario, expected) in [
        (
            Scenario {
                tool_error: true,
                ..Scenario::default()
            },
            "mcp_tool_error",
        ),
        (
            Scenario {
                input_required: true,
                ..Scenario::default()
            },
            "needs_input",
        ),
        (
            Scenario {
                drop_tool: true,
                ..Scenario::default()
            },
            "outcome_unknown",
        ),
    ] {
        let (client, _fixture) = stdio_fixture(scenario, fast_limits()).await;
        let catalog = client.list_tools(&Never).await.unwrap();
        let client = Arc::new(client);
        let adapter =
            McpToolAdapter::new(client.clone(), catalog.tools[0].clone(), catalog.generation);
        assert_eq!(adapter.definition().name, "echo");
        let context = ToolContext {
            run: Run {
                id: RunId::new(),
                session_id: SessionId::new(),
                work_mode: WorkMode::Work,
                permissions: PermissionMode::Manual,
            },
            workspace: std::env::temp_dir(),
        };
        let call = ToolCall {
            call_id: "one".into(),
            name: "echo".into(),
            arguments: json!({"message":"error detail"}).to_string(),
        };
        match adapter.execute(&call, &context, &Never).await {
            Ok(result) => {
                assert!(!result.ok);
                assert_eq!(result.error.unwrap().code, expected);
                assert!(result.data.unwrap().to_string().contains("error detail"));
            }
            Err(error) => assert_eq!(error.0.code, expected),
        }
        client.cancel_connection();
    }
}

#[tokio::test]
async fn connection_revisions_do_not_repeat_after_reconnect_with_the_same_catalogue() {
    let (first, _first_fixture) = stdio_fixture(Scenario::default(), fast_limits()).await;
    let (second, _second_fixture) = stdio_fixture(Scenario::default(), fast_limits()).await;
    assert_eq!(
        first.list_tools(&Never).await.unwrap().generation,
        second.list_tools(&Never).await.unwrap().generation
    );
    assert_ne!(first.revision(), second.revision());
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
}

#[tokio::test]
async fn http_json_and_sse_enforce_the_same_early_limits_for_every_response_kind() {
    for sse in [false, true] {
        for method in ["server/discover", "tools/list", "tools/call"] {
            let fixture = http_fixture(
                Scenario {
                    sse,
                    padding_method: Some(method),
                    ..Scenario::default()
                },
                200,
            );
            let limits = McpLimits {
                max_wire_bytes: 1024,
                ..fast_limits()
            };
            let mut config = McpHttpConfig::new(&fixture.endpoint);
            config.allow_insecure_http = true;
            let connected = McpClient::connect_http("remote", config, limits, &Never).await;
            if method == "server/discover" {
                assert!(connected.is_err());
                continue;
            }
            let client = connected.unwrap();
            let listed = client.list_tools(&Never).await;
            if method == "tools/list" {
                assert!(listed.is_err());
                client.shutdown().await.unwrap();
                continue;
            }
            let catalog = listed.unwrap();
            assert_eq!(
                client
                    .call_tool("echo", json!({"message":"one"}), catalog.generation, &Never)
                    .await
                    .unwrap_err()
                    .0
                    .code,
                "outcome_unknown"
            );
            assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
            client.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn output_schema_and_projection_failures_after_dispatch_are_unknown() {
    for (scenario, limits) in [
        (
            Scenario {
                bad_output: true,
                ..Scenario::default()
            },
            fast_limits(),
        ),
        (
            Scenario {
                padding_method: Some("tools/call"),
                ..Scenario::default()
            },
            McpLimits {
                max_wire_bytes: 1024,
                ..fast_limits()
            },
        ),
    ] {
        let (client, mut fixture) = stdio_fixture(scenario, limits).await;
        let catalog = client.list_tools(&Never).await.unwrap();
        assert_eq!(
            client
                .call_tool("echo", json!({"message":"one"}), catalog.generation, &Never)
                .await
                .unwrap_err()
                .0
                .code,
            "outcome_unknown"
        );
        let mut calls = 0;
        while let Ok(request) = fixture.requests.try_recv() {
            calls += usize::from(request["method"] == "tools/call");
        }
        assert_eq!(calls, 1);
        client.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn catalog_change_while_publication_waits_cannot_stamp_old_data_as_fresh() {
    let (client, mut fixture) = stdio_fixture(
        Scenario {
            list_changed: true,
            changing_catalog: true,
            hang_tool: true,
            ..Scenario::default()
        },
        fast_limits(),
    )
    .await;
    let catalog = client.list_tools(&Never).await.unwrap();
    while fixture.requests.try_recv().is_ok() {}
    let cancel = Cancel::new();
    {
        let call = client.call_tool(
            "echo",
            json!({"message":"holds read lease"}),
            catalog.generation,
            &cancel,
        );
        tokio::pin!(call);
        let observed = tokio::select! {result=&mut call=>panic!("unexpected result {result:?}"),request=fixture.requests.recv()=>request.unwrap()};
        assert_eq!(observed["method"], "tools/call");
        let discovery = client.list_tools(&Never);
        tokio::pin!(discovery);
        let observed = tokio::select! {result=&mut discovery=>panic!("unexpected discovery {result:?}"),request=fixture.requests.recv()=>request.unwrap()};
        assert_eq!(observed["method"], "tools/list");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(futures::poll!(&mut discovery).is_pending());
        fixture
            .notify
            .send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{}}))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while client.generation() == catalog.generation {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cancel.cancel();
        assert_eq!((&mut call).await.unwrap_err().0.code, "outcome_unknown");
        assert_eq!(
            (&mut discovery).await.unwrap_err().0.code,
            "mcp_stale_catalog"
        );
    }
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn oauth_preflight_refresh_is_allowed_but_401_never_replays_the_tool() {
    use rmcp::transport::auth::{AuthorizationManager, OAuthClientConfig, StoredCredentials};
    use saya_core::credentials::{
        CredentialScope, CredentialValue, FileCredentialStore, SecretRegistry,
    };
    use saya_core::mcp::McpHttpClient;
    use std::collections::BTreeSet;
    let fixture = http_fixture(Scenario::default(), 401);
    let origin = format!("{}/", fixture.endpoint.trim_end_matches("/mcp"));
    let directory = tempfile::tempdir().unwrap();
    let store = FileCredentialStore::open(
        directory.path().join("credentials"),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    let scope =
        CredentialScope::for_mcp("remote", &fixture.endpoint, BTreeSet::from(["read".into()]))
            .unwrap();
    let token=serde_json::from_value(json!({"access_token":"expired-fixture-token","refresh_token":"valid-fixture-refresh","token_type":"Bearer","expires_in":1,"scope":"read"})).unwrap();
    let credential = StoredCredentials::new(
        "fixture-client".into(),
        Some(token),
        vec!["read".into()],
        Some(0),
    )
    .with_issuer(Some(origin));
    let reference = store
        .create(scope.clone(), CredentialValue::oauth(credential))
        .await
        .unwrap();
    let mut manager = AuthorizationManager::new(&fixture.endpoint).await.unwrap();
    manager.set_credential_store(store.oauth(reference, scope).unwrap());
    let resolved = manager.resolve_metadata().await.unwrap();
    manager.set_metadata(resolved.metadata);
    manager
        .configure_client(OAuthClientConfig::new(
            "fixture-client",
            "http://127.0.0.1/callback",
        ))
        .unwrap();
    let limits = fast_limits();
    let transport = McpHttpClient::new(&limits)
        .unwrap()
        .with_auth_manager(manager, &fixture.endpoint)
        .unwrap();
    let mut config = McpHttpConfig::new(&fixture.endpoint);
    config.allow_insecure_http = true;
    let client = McpClient::connect_http_with_client("remote", transport, config, limits, &Never)
        .await
        .unwrap();
    let catalog = client.list_tools(&Never).await.unwrap();
    assert_eq!(
        client
            .call_tool("echo", json!({"message":"one"}), catalog.generation, &Never)
            .await
            .unwrap_err()
            .0
            .code,
        "needs_auth"
    );
    assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
    assert_eq!(fixture.refreshes.load(Ordering::Acquire), 1);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn authorized_http_client_rejects_foreign_targets_and_full_oversized_envelopes_before_io() {
    use rmcp::model::ClientJsonRpcMessage;
    use rmcp::transport::auth::AuthorizationManager;
    use rmcp::transport::streamable_http_client::{StreamableHttpClient, StreamableHttpError};
    use saya_core::mcp::McpHttpClient;
    use std::collections::HashMap;
    let fixture = http_fixture(Scenario::default(), 200);
    let manager = AuthorizationManager::new("https://expected.invalid/mcp")
        .await
        .unwrap();
    let client = McpHttpClient::new(&fast_limits())
        .unwrap()
        .with_auth_manager(manager, "https://expected.invalid/mcp")
        .unwrap();
    let message:ClientJsonRpcMessage=serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":{"message":"one"}}})).unwrap();
    let failure = client
        .post_message(
            Arc::from(fixture.endpoint.clone()),
            message,
            None,
            None,
            HashMap::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(failure,StreamableHttpError::Io(error) if error.kind()==std::io::ErrorKind::PermissionDenied)
    );
    let limits = McpLimits {
        max_wire_bytes: 128,
        ..fast_limits()
    };
    let client = McpHttpClient::new(&limits).unwrap();
    let params = json!({"name":"echo","arguments":{"message":"x".repeat(40)}});
    assert!(serde_json::to_vec(&params).unwrap().len() < 128);
    let message: ClientJsonRpcMessage = serde_json::from_value(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params}),
    )
    .unwrap();
    assert!(serde_json::to_vec(&message).unwrap().len() > 128);
    let failure = client
        .post_message(
            Arc::from(fixture.endpoint.clone()),
            message,
            None,
            None,
            HashMap::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(failure,StreamableHttpError::Io(error) if error.kind()==std::io::ErrorKind::InvalidInput)
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stored_bearer_revocation_stops_an_already_connected_client_before_another_post() {
    use saya_core::credentials::{
        CredentialScope, CredentialValue, FileCredentialStore, SecretRegistry, SecretValue,
    };
    use saya_core::mcp::McpHttpClient;
    use std::collections::BTreeSet;
    let fixture = http_fixture(Scenario::default(), 200);
    let root = tempfile::tempdir().unwrap();
    let store = FileCredentialStore::open(
        root.path().join("credentials"),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    let scope = CredentialScope::for_mcp("remote", &fixture.endpoint, BTreeSet::new()).unwrap();
    let reference = store
        .create(
            scope.clone(),
            CredentialValue::Secret(SecretValue::new("stored-fixture-bearer").unwrap()),
        )
        .await
        .unwrap();
    let limits = fast_limits();
    let transport = McpHttpClient::new(&limits)
        .unwrap()
        .with_stored_bearer(store.clone(), reference, scope.clone())
        .unwrap();
    let mut config = McpHttpConfig::new(&fixture.endpoint);
    config.allow_insecure_http = true;
    let client = McpClient::connect_http_with_client("remote", transport, config, limits, &Never)
        .await
        .unwrap();
    let catalog = client.list_tools(&Never).await.unwrap();
    client
        .call_tool(
            "echo",
            json!({"message":"first"}),
            catalog.generation,
            &Never,
        )
        .await
        .unwrap();
    store.revoke(reference, &scope).await.unwrap();
    assert_eq!(
        client
            .call_tool(
                "echo",
                json!({"message":"second"}),
                catalog.generation,
                &Never
            )
            .await
            .unwrap_err()
            .0
            .code,
        "needs_auth"
    );
    assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn captured_adapter_retains_text_before_projection_caps_and_redacts_known_secrets() {
    use saya_core::{
        boundary::{Tool, ToolContext},
        contract::{PermissionMode, Run, RunId, SessionId, ToolCall, WorkMode},
        credentials::SecretRegistry,
        mcp::McpToolAdapter,
        output_store::{OutputLimits, OutputStore, SearchOptions},
    };
    for era in [Era::Modern, Era::Legacy] {
        let root = tempfile::tempdir().unwrap();
        let registry = Arc::new(SecretRegistry::default());
        registry
            .register(&["fixture-private-credential".into()])
            .unwrap();
        let outputs = OutputStore::open(
            root.path().join("outputs"),
            OutputLimits::default(),
            registry,
        )
        .unwrap();
        let (client, _fixture) = stdio_fixture(
            Scenario {
                era,
                ..Scenario::default()
            },
            McpLimits {
                max_result_bytes: 512,
                ..fast_limits()
            },
        )
        .await;
        let catalogue = client.list_tools(&Never).await.unwrap();
        let client = Arc::new(client);
        let adapter = McpToolAdapter::new(
            client.clone(),
            catalogue.tools[0].clone(),
            catalogue.generation,
        );
        let context = ToolContext {
            run: Run {
                id: RunId::new(),
                session_id: SessionId::new(),
                work_mode: WorkMode::Work,
                permissions: PermissionMode::Auto,
            },
            workspace: root.path().into(),
        };
        let message = format!(
            "{}\nfixture-private-credential\nneedle-after-projection",
            "line of retained evidence\n".repeat(2000)
        );
        let call = ToolCall {
            call_id: "captured-mcp".into(),
            name: "echo".into(),
            arguments: json!({"message":message}).to_string(),
        };
        let captured = adapter
            .execute_captured(&call, &context, &Never, Some(&outputs))
            .await
            .unwrap();
        assert!(captured.result.truncated);
        assert_eq!(captured.outputs.len(), 1);
        assert!(captured.capture_errors.is_empty());
        let metadata = &captured.outputs[0].metadata;
        assert!(metadata.artifact_complete);
        assert_eq!(metadata.scope.call_id, call.call_id);
        outputs
            .publish(context.run.session_id, &metadata.output_ref)
            .unwrap();
        let found = outputs
            .search(
                context.run.session_id,
                &metadata.output_ref,
                SearchOptions {
                    pattern: "needle-after-projection".into(),
                    ..SearchOptions::default()
                },
            )
            .unwrap();
        assert_eq!(found.matches.len(), 1);
        for entry in std::fs::read_dir(outputs.protected_path()).unwrap() {
            let entry = entry.unwrap();
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "out")
            {
                assert!(
                    !String::from_utf8_lossy(&std::fs::read(entry.path()).unwrap())
                        .contains("fixture-private-credential")
                );
            }
        }
        drop(adapter);
        Arc::try_unwrap(client)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    }
}
