use saya_core::{
    boundary::Cancellation,
    credentials::{
        CredentialScope, CredentialValue, FileCredentialStore, McpOAuthLogin, OAuthLoginOptions,
        SecretRegistry,
    },
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::{Future, pending},
    io::{Read, Write},
    net::TcpListener,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct Cancelled;
impl Cancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}
struct Fixture {
    endpoint: String,
    token_calls: Arc<AtomicUsize>,
    exchange: Arc<Mutex<BTreeMap<String, String>>>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    resource: Arc<Mutex<String>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}
fn fixture(grant: &str) -> Fixture {
    fixture_resource(grant, false)
}
fn fixture_resource(grant: &str, ancestor: bool) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let origin = endpoint.trim_end_matches("/mcp").to_owned();
    let resource = Arc::new(Mutex::new(if ancestor {
        format!("{origin}/")
    } else {
        endpoint.clone()
    }));
    let canonical = resource.clone();
    let token_calls = Arc::new(AtomicUsize::new(0));
    let calls = token_calls.clone();
    let exchange = Arc::new(Mutex::new(BTreeMap::new()));
    let sent = exchange.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let grant = grant.to_owned();
    let join = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 2048];
            let body_start = loop {
                let Ok(length) = socket.read(&mut buffer) else {
                    break None;
                };
                if length == 0 {
                    break None;
                }
                bytes.extend_from_slice(&buffer[..length]);
                if let Some(start) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..start]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= start + 4 + length {
                        break Some(start + 4);
                    }
                }
            };
            let Some(body_start) = body_start else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..body_start]);
            let (status, body) = if headers.starts_with("GET ")
                && headers.contains("oauth-protected-resource")
            {
                (
                    200,
                    json!({"resource":canonical.lock().unwrap().clone(),"authorization_servers":[format!("{origin}/")],"scopes_supported":["read","admin","offline_access"]}),
                )
            } else if headers.starts_with("GET ")
                && (headers.contains("oauth-authorization-server")
                    || headers.contains("openid-configuration"))
            {
                (
                    200,
                    json!({"issuer":format!("{origin}/"),"authorization_endpoint":format!("{origin}/authorize"),"token_endpoint":format!("{origin}/token"),"registration_endpoint":format!("{origin}/register"),"response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"scopes_supported":["read","admin","offline_access"],"code_challenge_methods_supported":["S256"],"authorization_response_iss_parameter_supported":true}),
                )
            } else if headers.starts_with("POST /token ") {
                calls.fetch_add(1, Ordering::AcqRel);
                let form = reqwest_mcp::Url::parse(&format!(
                    "http://fixture.invalid/?{}",
                    String::from_utf8_lossy(&bytes[body_start..])
                ))
                .unwrap();
                *sent.lock().unwrap() = form
                    .query_pairs()
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect();
                (
                    200,
                    json!({"access_token":"login-fixture-access-token","refresh_token":"login-fixture-refresh-token","token_type":"Bearer","expires_in":3600,"scope":grant}),
                )
            } else if headers.starts_with("POST /register ") {
                let registration: serde_json::Value =
                    serde_json::from_slice(&bytes[body_start..]).unwrap();
                (
                    200,
                    json!({"client_id":"dynamic-saya-fixture","redirect_uris":registration["redirect_uris"]}),
                )
            } else {
                (404, json!({"error":"missing"}))
            };
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
        }
    });
    Fixture {
        endpoint,
        token_calls,
        exchange,
        stop,
        join: Some(join),
        resource,
    }
}
fn store() -> (tempfile::TempDir, FileCredentialStore, Arc<SecretRegistry>) {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretRegistry::default());
    let store =
        FileCredentialStore::open(root.path().join("credentials"), secrets.clone()).unwrap();
    (root, store, secrets)
}
fn scope(fixture: &Fixture) -> CredentialScope {
    CredentialScope::for_mcp(
        "fixture",
        &fixture.endpoint,
        BTreeSet::from(["read".into()]),
    )
    .unwrap()
}
fn options() -> OAuthLoginOptions {
    OAuthLoginOptions {
        interactive: true,
        client_id: Some("pre-registered-saya".into()),
        timeout: Duration::from_secs(5),
        callback_port: 0,
    }
}
fn callback(login: &McpOAuthLogin, issuer: &str) -> String {
    let url = reqwest_mcp::Url::parse(login.authorization_url()).unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let mut callback = reqwest_mcp::Url::parse(login.redirect_uri()).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("code", "login-fixture-authorization-code")
        .append_pair("state", &state)
        .append_pair("iss", issuer);
    callback.to_string()
}
fn base64_url(input: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let value = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | (chunk.get(2).copied().unwrap_or(0) as u32);
        out.push(alphabet[((value >> 18) & 63) as usize] as char);
        out.push(alphabet[((value >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(alphabet[((value >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(alphabet[(value & 63) as usize] as char);
        }
    }
    out
}

#[tokio::test]
async fn manual_callback_uses_pkce_exact_scopes_issuer_and_a_fresh_private_reference() {
    let fixture = fixture("read");
    let (_root, store, secrets) = store();
    let scope = scope(&fixture);
    let login = McpOAuthLogin::begin(store.clone(), scope.clone(), options(), &Never)
        .await
        .unwrap();
    let auth = reqwest_mcp::Url::parse(login.authorization_url()).unwrap();
    let params = auth
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(params["scope"], "read");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["resource"], fixture.endpoint);
    assert!(!format!("{login:?}").contains(&params["state"]));
    let callback = callback(
        &login,
        &format!("{}/", fixture.endpoint.trim_end_matches("/mcp")),
    );
    let reference = login
        .complete_callback_url(&callback, &Never)
        .await
        .unwrap();
    assert_eq!(fixture.token_calls.load(Ordering::Acquire), 1);
    {
        let sent = fixture.exchange.lock().unwrap();
        assert_eq!(sent["grant_type"], "authorization_code");
        assert_eq!(sent["resource"], fixture.endpoint);
        assert_eq!(
            base64_url(&Sha256::digest(sent["code_verifier"].as_bytes())),
            params["code_challenge"]
        );
    }
    assert!(matches!(
        store.resolve(reference, &scope).await.unwrap().value,
        CredentialValue::OAuth(_)
    ));
    let known = secrets.snapshot().unwrap();
    assert!(known.contains(&"login-fixture-access-token".into()));
    assert!(known.contains(&"login-fixture-refresh-token".into()));
    assert_eq!(store.list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn forged_callback_state_issuer_destination_and_duplicate_keys_never_exchange_a_code() {
    let fixture = fixture("read");
    let (_root, store, _) = store();
    for defect in ["state", "issuer", "destination", "duplicate"] {
        let login = McpOAuthLogin::begin(store.clone(), scope(&fixture), options(), &Never)
            .await
            .unwrap();
        let mut url = reqwest_mcp::Url::parse(&callback(
            &login,
            &format!("{}/", fixture.endpoint.trim_end_matches("/mcp")),
        ))
        .unwrap();
        match defect {
            "state" => {
                let pairs = url
                    .query_pairs()
                    .filter(|(key, _)| key != "state")
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect::<Vec<_>>();
                url.set_query(None);
                url.query_pairs_mut()
                    .extend_pairs(pairs)
                    .append_pair("state", "forged-state");
            }
            "issuer" => {
                let pairs = url
                    .query_pairs()
                    .filter(|(key, _)| key != "iss")
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect::<Vec<_>>();
                url.set_query(None);
                url.query_pairs_mut()
                    .extend_pairs(pairs)
                    .append_pair("iss", "https://other.invalid/");
            }
            "destination" => url.set_path("/other"),
            _ => {
                url.query_pairs_mut().append_pair("code", "duplicate");
            }
        }
        assert_eq!(
            login
                .complete_callback_url(url.as_str(), &Never)
                .await
                .unwrap_err()
                .0
                .code,
            "needs_auth"
        );
    }
    assert_eq!(fixture.token_calls.load(Ordering::Acquire), 0);
    assert!(store.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn loopback_callback_supports_dynamic_public_registration_and_closes_its_listener() {
    let fixture = fixture("read");
    let (_root, store, _) = store();
    let mut opts = options();
    opts.client_id = None;
    let login = McpOAuthLogin::begin(store.clone(), scope(&fixture), opts, &Never)
        .await
        .unwrap();
    let callback = callback(
        &login,
        &format!("{}/", fixture.endpoint.trim_end_matches("/mcp")),
    );
    let task = tokio::spawn(async move {
        reqwest_mcp::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(callback)
            .send()
            .await
            .unwrap()
            .status()
    });
    login.wait_for_callback(&Never).await.unwrap();
    assert_eq!(task.await.unwrap(), 200);
    assert_eq!(
        fixture.exchange.lock().unwrap()["client_id"],
        "dynamic-saya-fixture"
    );
}

#[tokio::test]
async fn expanded_grants_are_observed_but_never_persisted_under_a_smaller_scope() {
    let fixture = fixture("read admin");
    let (_root, store, secrets) = store();
    let login = McpOAuthLogin::begin(store.clone(), scope(&fixture), options(), &Never)
        .await
        .unwrap();
    let callback = callback(
        &login,
        &format!("{}/", fixture.endpoint.trim_end_matches("/mcp")),
    );
    assert_eq!(
        login
            .complete_callback_url(&callback, &Never)
            .await
            .unwrap_err()
            .0
            .code,
        "needs_auth"
    );
    assert_eq!(fixture.token_calls.load(Ordering::Acquire), 1);
    assert!(store.list().await.unwrap().is_empty());
    assert!(
        secrets
            .snapshot()
            .unwrap()
            .contains(&"login-fixture-access-token".into())
    );
}

#[tokio::test]
async fn noninteractive_cancelled_and_expired_flows_do_not_create_credentials() {
    let fixture = fixture("read");
    let (_root, store, _) = store();
    assert_eq!(
        McpOAuthLogin::begin(
            store.clone(),
            scope(&fixture),
            OAuthLoginOptions::default(),
            &Never
        )
        .await
        .unwrap_err()
        .0
        .code,
        "needs_input"
    );
    assert_eq!(
        McpOAuthLogin::begin(store.clone(), scope(&fixture), options(), &Cancelled)
            .await
            .unwrap_err()
            .0
            .code,
        "cancelled"
    );
    let login = McpOAuthLogin::begin(store.clone(), scope(&fixture), options(), &Never)
        .await
        .unwrap();
    let callback = callback(
        &login,
        &format!("{}/", fixture.endpoint.trim_end_matches("/mcp")),
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    assert_eq!(
        login
            .complete_callback_url(&callback, &Never)
            .await
            .unwrap_err()
            .0
            .code,
        "oauth_timeout"
    );
    assert_eq!(fixture.token_calls.load(Ordering::Acquire), 0);
    assert!(store.list().await.unwrap().is_empty());
}

struct FlagCancel(AtomicBool);
impl Cancellation for FlagCancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            while !self.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    }
}
#[tokio::test]
async fn cancellation_after_private_commit_begins_waits_for_the_actual_saved_reference() {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let fixture = fixture("read");
    let (_root, store, secrets) = store();
    let login = McpOAuthLogin::begin(store.clone(), scope(&fixture), options(), &Never)
        .await
        .unwrap();
    let callback = callback(
        &login,
        &format!("{}/", fixture.endpoint.trim_end_matches("/mcp")),
    );
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(store.protected_path().join("index.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let cancel = Arc::new(FlagCancel(AtomicBool::new(false)));
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        login
            .complete_callback_url(&callback, task_cancel.as_ref())
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !secrets
            .snapshot()
            .unwrap()
            .contains(&"login-fixture-access-token".into())
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Let the successful exchange reach create(), which is held at the index lock.
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.0.store(true, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!task.is_finished());
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    let reference = task.await.unwrap().unwrap();
    assert_eq!(store.list().await.unwrap()[0].reference, reference);
}

#[tokio::test]
async fn canonical_ancestor_resource_is_recorded_without_generalizing_the_transport_endpoint() {
    let fixture = fixture_resource("read", true);
    let (_root, store, _) = store();
    let scope = scope(&fixture);
    let login = McpOAuthLogin::begin(store.clone(), scope.clone(), options(), &Never)
        .await
        .unwrap();
    let callback = callback(
        &login,
        &format!("{}/", fixture.endpoint.trim_end_matches("/mcp")),
    );
    let canonical = fixture.resource.lock().unwrap().clone();
    let reference = login
        .complete_callback_url(&callback, &Never)
        .await
        .unwrap();
    let resolved = store.resolve(reference, &scope).await.unwrap();
    assert_eq!(resolved.metadata.scope.audience, fixture.endpoint);
    assert_eq!(
        resolved.metadata.oauth_resource.as_deref(),
        Some(canonical.as_str())
    );
    assert_eq!(fixture.exchange.lock().unwrap()["resource"], canonical);
    let other = CredentialScope::for_mcp(
        "fixture",
        &format!("{}/other", fixture.endpoint.trim_end_matches("/mcp")),
        scope.scopes.clone(),
    )
    .unwrap();
    assert_eq!(
        store.resolve(reference, &other).await.unwrap_err().0.code,
        "credential_scope"
    );
    let bridge = store.oauth(reference, scope.clone()).unwrap();
    saya_core::mcp::McpHttpClient::new(&saya_core::mcp::McpLimits::default())
        .unwrap()
        .with_oauth_store(&fixture.endpoint, bridge)
        .await
        .unwrap();
    *fixture.resource.lock().unwrap() = fixture.endpoint.clone();
    let bridge = store.oauth(reference, scope).unwrap();
    assert_eq!(
        saya_core::mcp::McpHttpClient::new(&saya_core::mcp::McpLimits::default())
            .unwrap()
            .with_oauth_store(&fixture.endpoint, bridge)
            .await
            .unwrap_err()
            .0
            .code,
        "needs_auth"
    );
    assert_eq!(fixture.token_calls.load(Ordering::Acquire), 1);
}
