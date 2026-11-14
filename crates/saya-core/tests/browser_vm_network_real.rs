//! Real dedicated-VM browser network acceptance entry point.
//!
//! Ignored by default. Run it explicitly on a macOS or Linux host with a provisioned
//! Colima profile (created by `scripts/n5/browser_vm.py`) and the
//! `SAYA_TEST_VM_*` inputs described in `docs/browser.md`:
//!
//! ```text
//! cargo test -p saya-core --test browser_vm_network_real -- --ignored --nocapture
//! ```
//!
//! The test starts only local host fixtures. It never contacts the public
//! internet. Chromium inside the VM reaches the host fixture exclusively
//! through the host broker, the SSH reverse streamlocal forward and the
//! namespace-confined `socat` proxy.
//!
//! Bypass canaries live on the *browser's own private network namespace*
//! loopback, not the VM default namespace. After the worker PID is known the
//! test starts the TCP/UDP canaries with `sudo -n nsenter -t <worker> -n`, then
//! proves with a positive control executed in the same namespace that both
//! canaries are reachable. Only then does the dynamic page attempt them. If
//! Chromium ignored the proxy and connected directly, the attempt would hit
//! the canary; therefore a zero count is a meaningful negative control.

use saya_core::{
    boundary::Cancellation,
    browser::{
        BrowserProvider,
        network::BrowserNetworkSettings,
        vm::{BrowserVmSettings, SshVmCommandRunner, VmCommandRunner, VmCommandSpec},
    },
    tools::exec::ExecConfig,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

struct SwitchCancel(Arc<AtomicBool>);
impl Cancellation for SwitchCancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        let flag = self.0.clone();
        Box::pin(async move {
            while !flag.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    }
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing explicit test input {name}"))
}

fn path(name: &str) -> PathBuf {
    PathBuf::from(required(name))
}

fn settings() -> BrowserVmSettings {
    BrowserVmSettings {
        enabled: true,
        colima: Some(path("SAYA_TEST_VM_COLIMA")),
        ssh: Some(path("SAYA_TEST_VM_SSH")),
        profile: Some(required("SAYA_TEST_VM_PROFILE")),
        host: Some(required("SAYA_TEST_VM_HOST")),
        user: Some(required("SAYA_TEST_VM_USER")),
        port: Some(
            required("SAYA_TEST_VM_PORT")
                .parse()
                .expect("valid SAYA_TEST_VM_PORT"),
        ),
        identity_file: Some(path("SAYA_TEST_VM_IDENTITY_FILE")),
        known_hosts: Some(path("SAYA_TEST_VM_KNOWN_HOSTS")),
        node: Some(required("SAYA_TEST_VM_NODE")),
        worker_root: Some(required("SAYA_TEST_VM_WORKER_ROOT")),
        chromium: Some(required("SAYA_TEST_VM_CHROMIUM")),
        identity_manifest: Some(required("SAYA_TEST_VM_MANIFEST")),
        identity_sha256: Some(required("SAYA_TEST_VM_MANIFEST_SHA256")),
        remote_temp_root: Some(required("SAYA_TEST_VM_TEMP_ROOT")),
        ..Default::default()
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut buffer = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") && head.len() < 16 * 1024 {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => head.extend_from_slice(&buffer[..count]),
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// The dynamic page records the STUN lifecycle in a visible element so a
/// snapshot proves the inline script actually ran. The TCP canary is attempted
/// by both an image and a fetch; the UDP canary by a real RTCPeerConnection.
const DYNAMIC_TEMPLATE: &str = "<title>dynamic-ok</title><body>DYNAMIC_MARKER\
     <div id=\"stun\">STUN_PENDING</div>\
     <img src=\"http://127.0.0.1:__TCP__/image\">\
     <script>\
     var marker=document.getElementById('stun');\
     marker.textContent='STUN_STARTED';\
     fetch('http://127.0.0.1:__TCP__/script').catch(function(){});\
     try {\
       var pc=new RTCPeerConnection({iceServers:[{urls:'stun:127.0.0.1:__UDP__'}]});\
       pc.createDataChannel('canary');\
       pc.createOffer()\
         .then(function(offer){return pc.setLocalDescription(offer);})\
         .then(function(){setTimeout(function(){marker.textContent='STUN_DONE';pc.close();},1200);})\
         .catch(function(){marker.textContent='STUN_FAILED';});\
     } catch (e) { marker.textContent='STUN_FAILED'; }\
     </script></body>";

/// Host fixture: static, redirect, dynamic (with netns canary subresources and
/// a real RTCPeerConnection STUN attempt), slow, and a provider 403 for
/// anything not explicitly routed.
async fn spawn_fixture(
    canary: Arc<Mutex<Option<(u16, u16)>>>,
) -> (u16, Arc<AtomicU64>, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicU64::new(0));
    let counter = hits.clone();
    let (slow_started, slow_received) = oneshot::channel();
    tokio::spawn(async move {
        let mut slow_started = Some(slow_started);
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let request = read_request(&mut stream).await;
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split(' ').nth(1))
                .unwrap_or("/");
            let response = match path {
                "/" => "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: 50\r\n\r\n<title>static-ok</title><body>STATIC_MARKER</body>".to_owned(),
                "/redirect" => "HTTP/1.1 302 Found\r\nLocation: /dynamic\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                "/dynamic" => {
                    let ports = *canary.lock().unwrap();
                    match ports {
                        Some((tcp, udp)) => {
                            let body = DYNAMIC_TEMPLATE
                                .replace("__TCP__", &tcp.to_string())
                                .replace("__UDP__", &udp.to_string());
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                                body.len()
                            )
                        }
                        None => "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                    }
                }
                "/slow" => {
                    if let Some(started) = slow_started.take() {
                        let _ = started.send(());
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nslow".to_owned()
                }
                _ => "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
            };
            counter.fetch_add(1, Ordering::AcqRel);
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (port, hits, slow_received)
}

// The canary nodes are started inside the worker's private network namespace
// with `sudo -n nsenter`. Node is already a pinned worker input, so the canary
// adds no runtime dependency beyond nsenter, which provision/check verifies.
const CANARY_START_SCRIPT: &str = r#"set -eu
dir=$1
node=$2
target=$3
mkdir -p -- "$dir"
chmod 700 -- "$dir"
rm -f -- "$dir/tcp.port" "$dir/udp.port" "$dir/tcp.hits" "$dir/udp.hits" "$dir/tcp.pid" "$dir/udp.pid"
uid=$(id -u)
gid=$(id -g)
enter() {
  sudo -n nsenter -t "$target" -n -- \
    setpriv --reuid="$uid" --regid="$gid" --init-groups -- "$@"
}
enter setsid -f "$node" -e 'const fs=require("fs");const net=require("net");const dir=process.argv[1];const server=net.createServer(c=>{fs.appendFileSync(dir+"/tcp.hits","tcp\n");c.end();});server.listen(0,"127.0.0.1",()=>{fs.writeFileSync(dir+"/tcp.port",String(server.address().port));fs.writeFileSync(dir+"/tcp.pid",String(process.pid));});' "$dir" >/dev/null 2>&1
enter setsid -f "$node" -e 'const fs=require("fs");const dgram=require("dgram");const dir=process.argv[1];const socket=dgram.createSocket("udp4");socket.on("message",()=>fs.appendFileSync(dir+"/udp.hits","udp\n"));socket.bind(0,"127.0.0.1",()=>{fs.writeFileSync(dir+"/udp.port",String(socket.address().port));fs.writeFileSync(dir+"/udp.pid",String(process.pid));});' "$dir" >/dev/null 2>&1
i=0
while { [ ! -s "$dir/tcp.port" ] || [ ! -s "$dir/udp.port" ]; } && [ "$i" -lt 100 ]; do
  i=$((i+1))
  sleep 0.1
done
[ -s "$dir/tcp.port" ] && [ -s "$dir/udp.port" ] || { echo "canary=start-failed" >&2; exit 1; }
printf '%s\n' "$(cat -- "$dir/tcp.port")"
printf '%s\n' "$(cat -- "$dir/udp.port")"
"#;

// Positive controls execute in the *same* private netns as the browser. They
// must observe an actual canary hit, otherwise the later zero would prove
// nothing.
const CANARY_POSITIVE_SCRIPT: &str = r#"set -eu
dir=$1
node=$2
target=$3
uid=$(id -u)
gid=$(id -g)
sudo -n nsenter -t "$target" -n -- \
  setpriv --reuid="$uid" --regid="$gid" --init-groups -- \
  "$node" -e 'const fs=require("fs");const net=require("net");const dgram=require("dgram");const dir=process.argv[1];const lines=p=>{try{return fs.readFileSync(p,"utf8").split("\n").filter(Boolean).length}catch(e){return 0}};const tcpPort=Number(fs.readFileSync(dir+"/tcp.port","utf8"));const udpPort=Number(fs.readFileSync(dir+"/udp.port","utf8"));const tcpBefore=lines(dir+"/tcp.hits");const udpBefore=lines(dir+"/udp.hits");const stream=net.connect(tcpPort,"127.0.0.1",()=>stream.destroy());stream.on("error",()=>{});const socket=dgram.createSocket("udp4");socket.send(Buffer.from("saya-positive"),udpPort,"127.0.0.1",()=>socket.close());setTimeout(()=>{const tcpAfter=lines(dir+"/tcp.hits");const udpAfter=lines(dir+"/udp.hits");if(tcpAfter<=tcpBefore){console.error("positive=tcp-canary-unreachable");process.exit(1);}if(udpAfter<=udpBefore){console.error("positive=udp-canary-unreachable");process.exit(1);}console.log("positive=ok");process.exit(0);},3000);' "$dir"
"#;

const CANARY_RESET_SCRIPT: &str = r#"set -eu
dir=$1
: > "$dir/tcp.hits"
: > "$dir/udp.hits"
echo "canary=reset"
"#;

const CANARY_READ_SCRIPT: &str = r#"set -eu
dir=$1
tcp=$(wc -l < "$dir/tcp.hits" 2>/dev/null || echo 0)
udp=$(wc -l < "$dir/udp.hits" 2>/dev/null || echo 0)
printf 'tcp=%s\nudp=%s\n' "$tcp" "$udp"
"#;

const CANARY_STOP_SCRIPT: &str = r#"set -eu
dir=$1
for name in tcp udp; do
  if [ -s "$dir/$name.pid" ]; then
    pid=$(cat -- "$dir/$name.pid")
    case "$pid" in
      ''|*[!0-9]*) ;;
      *) kill -TERM "$pid" 2>/dev/null || true ;;
    esac
  fi
done
i=0
for name in tcp udp; do
  if [ -s "$dir/$name.pid" ]; then
    pid=$(cat -- "$dir/$name.pid")
    case "$pid" in
      ''|*[!0-9]*) ;;
      *)
        while [ "$i" -lt 20 ] && kill -0 "$pid" 2>/dev/null; do
          i=$((i+1))
          sleep 0.1
        done
        kill -KILL "$pid" 2>/dev/null || true
        ;;
    esac
  fi
done
rm -rf -- "$dir"
echo "canary=gone"
"#;

async fn remote_result(
    runner: &SshVmCommandRunner,
    script: &str,
    args: &[String],
    cancel: &dyn Cancellation,
) -> Result<saya_core::browser::vm::VmCommandOutput, String> {
    runner
        .run(
            VmCommandSpec {
                script,
                args,
                stdin: None,
                timeout: Duration::from_secs(30),
                stderr_limit: 16 * 1024,
                stdout_limit: 512 * 1024,
            },
            cancel,
        )
        .await
        .map_err(|failure| format!("{}: {}", failure.code, failure.message))
}

fn count(text: &str, prefix: &str) -> u64 {
    text.lines()
        .find_map(|line| {
            line.strip_prefix(prefix)
                .and_then(|value| value.trim().parse().ok())
        })
        .unwrap_or(u64::MAX)
}

const LEFTOVER_SCRIPT: &str = r#"set -eu
runs="$1"
canary="$2"
if [ -d "$runs" ]; then
  for entry in "$runs"/saya-browser-vm-*; do
    [ -e "$entry" ] && echo "leftover-root:$entry"
  done
fi
[ -e "$canary" ] && echo "leftover-canary-dir:$canary"
for file in /proc/[0-9]*/cmdline; do
  [ -r "$file" ] || continue
  pid=${file%/cmdline}
  pid=${pid##*/}
  [ "$pid" = "$$" ] && continue
  command=$(tr '\0' ' ' < "$file" 2>/dev/null || true)
  case "$command" in
    *saya-browser-vm""-*) echo "leftover-worker:$command" ;;
  esac
  case "$command" in
    *socat" TCP-LISTEN:"*) echo "leftover-socat:$command" ;;
  esac
  case "$command" in
    *saya-network-canary"-"*) echo "leftover-canary:$command" ;;
  esac
done
echo "scan=done"
"#;

fn worker_pid(
    observations: &[saya_core::browser::network::BrowserNetworkObservation],
) -> Option<u32> {
    observations.iter().find_map(|entry| {
        entry
            .worker_id
            .strip_prefix("vm-")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|pid| *pid > 1)
    })
}

#[tokio::test]
#[ignore = "requires a provisioned Colima browser VM and SAYA_TEST_VM_* inputs"]
async fn vm_browser_network_is_broker_only_and_cleans_everything() {
    let launcher = std::fs::canonicalize(required("SAYA_TEST_EXECUTABLE"))
        .expect("explicit Saya launcher path");
    saya_core::execution::pin_launcher_image_from(&launcher)
        .expect("pin the explicit Saya CLI instead of the libtest executable");
    let settings = settings();

    let temp_root = settings
        .remote_temp_root
        .clone()
        .expect("explicit SAYA_TEST_VM_TEMP_ROOT");
    let unique = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default()
    );
    let canary_dir = format!(
        "{}/saya-network-canary-{}",
        temp_root.trim_end_matches('/'),
        unique
    );
    let runner = SshVmCommandRunner::new(settings.clone()).expect("SSH runner");
    let node = settings
        .node
        .clone()
        .expect("SAYA_TEST_VM_NODE is required");

    let canary_ports = Arc::new(Mutex::new(None));
    let (fixture_port, fixture_hits, slow_received) = spawn_fixture(canary_ports.clone()).await;
    let origin = format!("http://127.0.0.1:{fixture_port}");
    let network = BrowserNetworkSettings {
        enabled: true,
        // Explicitly allow only the host fixture. Everything else is denied by
        // the broker, including other loopback ports and metadata addresses.
        allowed_origins: vec![origin.clone()],
        public_web: false,
        localhost_development_origin: Some(origin.clone()),
        ..Default::default()
    };
    let provider = Arc::new(
        saya_core::browser::vm::assemble_vm_browser_provider(
            settings.clone(),
            network,
            "vm-network-real",
            ExecConfig::default(),
        )
        .expect("VM provider assembly"),
    );
    let opened = provider
        .open(None, &NeverCancel)
        .await
        .expect("VM worker open");
    let session = opened.session.clone();
    let page = opened.page.clone();

    let static_url = format!("{origin}/");
    let action = provider
        .navigate(&session, &page, &static_url, &NeverCancel)
        .await
        .expect("static navigation through the broker");
    assert_eq!(action.url.as_deref(), Some(static_url.as_str()));
    let snapshot = provider
        .snapshot(&session, &page, 262_144, &NeverCancel)
        .await
        .expect("static snapshot");
    assert!(
        snapshot.text.contains("STATIC_MARKER"),
        "static body is missing: {}",
        snapshot.text
    );
    // The first broker observation carries the worker PID. The canaries must
    // live in that worker's private network namespace, not the VM default.
    let mut worker = None;
    for _ in 0..100 {
        worker = worker_pid(&provider.network_observations());
        if worker.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let worker_pid = worker.expect("worker PID from a broker observation");

    let mut canary_started = false;
    let mut tcp_canary_port = 0u16;
    let mut udp_canary_port = 0u16;
    let canary_phase: Result<(), String> = async {
        let args = [canary_dir.clone(), node.clone(), worker_pid.to_string()];
        let start = remote_result(&runner, CANARY_START_SCRIPT, &args, &NeverCancel)
            .await
            .map_err(|error| format!("canary start: {error}"))?;
        if !start.succeeded() {
            return Err(format!(
                "canary start failed: {}",
                String::from_utf8_lossy(&start.stderr)
            ));
        }
        canary_started = true;
        let text = String::from_utf8_lossy(&start.stdout).into_owned();
        let mut lines = text.lines();
        tcp_canary_port = lines
            .next()
            .and_then(|value| value.trim().parse().ok())
            .ok_or_else(|| format!("VM netns TCP canary port missing: {text}"))?;
        udp_canary_port = lines
            .next()
            .and_then(|value| value.trim().parse().ok())
            .ok_or_else(|| format!("VM netns UDP canary port missing: {text}"))?;

        // Positive control in the same private netns. A zero count later only
        // means something because these attempts must actually hit.
        let positive_args = [canary_dir.clone(), node.clone(), worker_pid.to_string()];
        let positive = remote_result(
            &runner,
            CANARY_POSITIVE_SCRIPT,
            &positive_args,
            &NeverCancel,
        )
        .await
        .map_err(|error| format!("canary positive control: {error}"))?;
        if !positive.succeeded()
            || !String::from_utf8_lossy(&positive.stdout).contains("positive=ok")
        {
            return Err(format!(
                "canary positive control failed: {} {}",
                String::from_utf8_lossy(&positive.stdout),
                String::from_utf8_lossy(&positive.stderr)
            ));
        }
        let reset = remote_result(
            &runner,
            CANARY_RESET_SCRIPT,
            std::slice::from_ref(&canary_dir),
            &NeverCancel,
        )
        .await
        .map_err(|error| format!("canary reset: {error}"))?;
        if !reset.succeeded() || !String::from_utf8_lossy(&reset.stdout).contains("canary=reset") {
            return Err("canary counters could not be reset".into());
        }
        *canary_ports.lock().unwrap() = Some((tcp_canary_port, udp_canary_port));

        // Dynamic page attempts the netns canaries. If Chromium ignored the
        // proxy, these requests would reach them directly.
        let action = provider
            .navigate(&session, &page, &format!("{origin}/redirect"), &NeverCancel)
            .await
            .map_err(|failure| format!("{}: {}", failure.code, failure.message))?;
        if action.url.as_deref() != Some(format!("{origin}/dynamic").as_str()) {
            return Err(format!(
                "redirect navigation ended at an unexpected URL: {:?}",
                action.url
            ));
        }
        let mut saw_started = false;
        let mut saw_done = false;
        let mut saw_failed = false;
        for _ in 0..40 {
            let snapshot = provider
                .snapshot(&session, &page, 262_144, &NeverCancel)
                .await
                .map_err(|failure| format!("dynamic snapshot: {}", failure.message))?;
            if !snapshot.text.contains("DYNAMIC_MARKER") {
                return Err(format!("dynamic body is missing: {}", snapshot.text));
            }
            saw_started |= snapshot.text.contains("STUN_STARTED");
            saw_done |= snapshot.text.contains("STUN_DONE");
            saw_failed |= snapshot.text.contains("STUN_FAILED");
            if saw_started && (saw_done || saw_failed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !saw_started {
            return Err("the dynamic page script never reported STUN_STARTED".into());
        }
        if !(saw_done || saw_failed) {
            return Err("the dynamic page never reported STUN_DONE or STUN_FAILED".into());
        }
        // Give the image/fetch/STUN attempts time to complete or be denied.
        tokio::time::sleep(Duration::from_secs(2)).await;
        Ok(())
    }
    .await;

    // Read and remove the canaries before asserting so a failed count cannot
    // leak a listener in the worker namespace.
    let counts = if canary_started {
        remote_result(
            &runner,
            CANARY_READ_SCRIPT,
            std::slice::from_ref(&canary_dir),
            &NeverCancel,
        )
        .await
        .ok()
        .filter(|output| output.succeeded())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
    } else {
        String::new()
    };
    if canary_started {
        let stop_args = [canary_dir.clone()];
        match remote_result(&runner, CANARY_STOP_SCRIPT, &stop_args, &NeverCancel).await {
            Ok(output)
                if output.succeeded()
                    && String::from_utf8_lossy(&output.stdout).contains("canary=gone") => {}
            Ok(output) => {
                eprintln!(
                    "canary stop was not confirmed: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(error) => eprintln!("canary stop failed: {error}"),
        }
    }
    if let Err(error) = canary_phase {
        panic!("{error}");
    }
    assert_eq!(
        count(&counts, "tcp="),
        0,
        "a browser attempt reached the private-netns TCP canary: {counts}"
    );
    assert_eq!(
        count(&counts, "udp="),
        0,
        "a browser attempt reached the private-netns UDP canary: {counts}"
    );

    // A metadata target is not an allowed origin. Chromium may still report a
    // completed navigation to its own error page, so the authoritative check is
    // the broker observation: the connection must have been denied and no
    // metadata body is involved.
    let _ = provider
        .navigate(
            &session,
            &page,
            "http://169.254.169.254/latest/meta-data/",
            &NeverCancel,
        )
        .await;

    let cancel = Arc::new(AtomicBool::new(false));
    let token = SwitchCancel(cancel.clone());
    let slow_url = format!("{origin}/slow");
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (result, ()) = tokio::join!(
            provider.navigate(&session, &page, &slow_url, &token),
            async {
                tokio::time::timeout(Duration::from_secs(10), slow_received)
                    .await
                    .expect("the slow request must reach the fixture before cancellation")
                    .expect("slow fixture closed before receiving the request");
                cancel.store(true, Ordering::Release);
            }
        );
        result
    })
    .await
    .expect("slow navigation must terminate after cancellation");
    let failure =
        result.expect_err("the slow page must not complete successfully after cancellation");
    assert_eq!(failure.code, "outcome_unknown", "{failure:?}");

    let observed = provider.network_observations();
    assert!(
        observed.iter().any(|entry| entry.outcome == "allowed"
            && entry.origin.as_deref() == Some(origin.as_str())
            && entry.bytes > 0
            && entry.run_id == "vm-network-real"),
        "no allowed observation for the fixture: {observed:?}"
    );
    assert!(
        observed.iter().any(|entry| entry.outcome == "denied"
            && entry
                .origin
                .as_deref()
                .is_some_and(|origin| origin.contains("169.254.169.254"))),
        "the metadata denial was not observed: {observed:?}"
    );
    // The dynamic page's canary subresources must actually reach the broker.
    // Without this, a zero canary count could simply mean the page never
    // attempted the connection.
    let canary_origin = format!("http://127.0.0.1:{tcp_canary_port}");
    assert!(
        observed.iter().any(|entry| entry.outcome == "denied"
            && entry.origin.as_deref() == Some(canary_origin.as_str())),
        "the dynamic page never attempted the netns TCP canary through the broker: {observed:?}"
    );
    for entry in &observed {
        let origin = entry.origin.as_deref().unwrap_or_default();
        assert!(!origin.contains("/latest"));
        assert!(!origin.contains("marker"));
    }
    assert!(fixture_hits.load(Ordering::Acquire) >= 2);

    let _ = provider.close(&session, &NeverCancel).await;
    provider
        .shutdown(&NeverCancel)
        .await
        .expect("VM worker shutdown");

    // Everything tied to the Run, including the broker listener, the SSH
    // reverse forward, the in-VM socat bridge and the private-netns canaries,
    // must be gone.
    let args = [temp_root, canary_dir.clone()];
    let output = runner
        .run(
            VmCommandSpec {
                script: LEFTOVER_SCRIPT,
                args: &args,
                stdin: None,
                timeout: Duration::from_secs(30),
                stderr_limit: 16 * 1024,
                stdout_limit: 512 * 1024,
            },
            &NeverCancel,
        )
        .await
        .expect("remote leftover scan");
    assert!(output.succeeded(), "leftover scan failed");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("scan=done"), "{text}");
    assert!(!text.contains("leftover-root:"), "{text}");
    assert!(!text.contains("leftover-worker:"), "{text}");
    assert!(!text.contains("leftover-socat:"), "{text}");
    assert!(!text.contains("leftover-canary"), "{text}");
    eprintln!(
        "vm network acceptance observations={} tcp_canary_port={} udp_canary_port={}",
        observed.len(),
        tcp_canary_port,
        udp_canary_port
    );
}
