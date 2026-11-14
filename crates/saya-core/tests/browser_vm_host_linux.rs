#![cfg(target_os = "linux")]

use saya_core::{
    boundary::Cancellation,
    browser::vm::{BrowserVmSettings, SshVmCommandRunner, VmCommandRunner, VmCommandSpec},
    execution::{self, BackendKind, NetworkPolicy, SandboxPolicy},
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

struct Canary {
    endpoint: SocketAddr,
    request: &'static [u8],
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Canary {
    fn start(address: &str, request: &'static [u8], reply: &'static [u8]) -> Self {
        let listener = TcpListener::bind(address).expect("loopback canary");
        let endpoint = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let count = hits.clone();
        let cancelled = stop.clone();
        let thread = thread::spawn(move || {
            while !cancelled.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut peer, _)) => {
                        count.fetch_add(1, Ordering::AcqRel);
                        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                        peer.set_write_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut received = vec![0; request.len()];
                        if peer.read_exact(&mut received).is_err() {
                            continue;
                        }
                        assert_eq!(received, request);
                        let _ = peer.write_all(reply);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("canary accept: {error}"),
                }
            }
        });
        Self {
            endpoint,
            request,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    fn positive_control(&self, expected: &[u8]) {
        let mut stream =
            TcpStream::connect_timeout(&self.endpoint, Duration::from_secs(1)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        stream.write_all(self.request).unwrap();
        let mut received = Vec::new();
        stream.read_to_end(&mut received).unwrap();
        assert_eq!(received, expected);
    }
}

impl Drop for Canary {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn pin_launcher() {
    let binary =
        std::env::var_os("SAYA_TEST_EXECUTABLE").expect("explicit current Linux executable");
    execution::pin_launcher_image_from(Path::new(&binary)).expect("pin current Linux executable");
}

#[test]
#[ignore = "requires a supported Linux kernel, pinned native tools, Python and SAYA_TEST_EXECUTABLE"]
fn private_vm_channel_preserves_literals_and_denies_other_host_access() {
    pin_launcher();
    for host in ["127.0.0.2", "::1"] {
        let workspace = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        let key = private.path().join("key");
        let known = private.path().join("known_hosts");
        let hidden = private.path().join("hidden");
        fs::write(&key, b"SYNTHETIC_KEY").unwrap();
        fs::write(&known, b"SYNTHETIC_HOST").unwrap();
        fs::write(&hidden, b"MUST_STAY_HIDDEN").unwrap();
        let address = SocketAddr::new(host.parse().unwrap(), 0);
        let ssh = Canary::start(&address.to_string(), b"", b"SSH_OK");
        let broker = Canary::start(
            "127.0.0.1:0",
            b"GET /n5-channel HTTP/1.0\r\n\r\n",
            b"BROKER_OK",
        );
        let forbidden = Canary::start(
            &format!("127.0.0.1:{}", ssh.endpoint.port()),
            b"",
            b"FORBIDDEN",
        );
        ssh.positive_control(b"SSH_OK");
        broker.positive_control(b"BROKER_OK");
        forbidden.positive_control(b"FORBIDDEN");
        let forbidden_baseline = forbidden.hits.load(Ordering::Acquire);
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let udp_peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp_peer
            .send_to(b"CONTROL", udp.local_addr().unwrap())
            .unwrap();
        let mut packet = [0; 32];
        assert_eq!(udp.recv_from(&mut packet).unwrap().0, 7);
        // SAFETY: duplicate an owned socket without CLOEXEC to exercise child sanitization.
        let inherited_raw = unsafe { libc::fcntl(udp_peer.as_raw_fd(), libc::F_DUPFD, 512) };
        assert!(inherited_raw >= 512);
        // SAFETY: the successful duplicate is a new, uniquely owned descriptor.
        let inherited = unsafe { OwnedFd::from_raw_fd(inherited_raw) };
        let mut policy = SandboxPolicy::workspace(workspace.path().into());
        policy.read_roots.clear();
        policy.read_files = vec![key.clone(), known.clone()];
        policy.network = NetworkPolicy::LoopbackPort {
            host: host.parse::<IpAddr>().unwrap(),
            port: ssh.endpoint.port(),
            proxy_port: Some(broker.endpoint.port()),
        };
        let environment = execution::prepare(&policy).expect("private VM host channel");
        assert_eq!(environment.identity().backend, BackendKind::Bubblewrap);
        let script = r#"import errno,fcntl,json,os,pwd,socket,sys
host,port,broker,blocked,udp,key,known,hidden,high_fd=json.loads(sys.argv[1])
for target,request,expected in [((host,port),b'',b'SSH_OK'),(('127.0.0.1',broker),b'GET /n5-channel HTTP/1.0\r\n\r\n',b'BROKER_OK')]:
 with socket.create_connection(target,timeout=2) as peer:
  peer.sendall(request)
  assert peer.recv(64)==expected
try:
 socket.create_connection(('127.0.0.1',blocked),timeout=0.3)
except OSError:
 pass
else:
 raise AssertionError('unapproved host TCP reached')
with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as peer:
 peer.sendto(b'PAYLOAD',('127.0.0.1',udp))
try:
 socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
except OSError as error:
 assert error.errno==errno.EPERM
else:
 raise AssertionError('named Unix socket allowed')
assert open(key,'rb').read()==b'SYNTHETIC_KEY'
assert open(known,'rb').read()==b'SYNTHETIC_HOST'
for target in [key,known]:
 try:
  open(target,'wb')
 except OSError:
  pass
 else:
  raise AssertionError('credential file writable')
assert not os.path.exists(hidden)
assert not os.path.exists('/etc/shadow')
assert not os.path.exists('/proc')
for fd in [*range(3,256),high_fd]:
 try:
  fcntl.fcntl(fd,fcntl.F_GETFD)
 except OSError as error:
  assert error.errno==errno.EBADF
 else:
  raise AssertionError('undeclared descriptor inherited')
assert pwd.getpwuid(os.getuid()).pw_name=='saya'
print('N5_PRIVATE_HOST_CHANNEL_OK')
"#;
        let arguments = serde_json::json!([
            host,
            ssh.endpoint.port(),
            broker.endpoint.port(),
            forbidden.endpoint.port(),
            udp.local_addr().unwrap().port(),
            key,
            known,
            hidden,
            inherited.as_raw_fd(),
        ])
        .to_string();
        let argv = ["/usr/bin/python3", "-I", "-S", "-c", script, &arguments].map(OsString::from);
        let prepared = environment
            .prepare_command(&argv, workspace.path(), &BTreeMap::new())
            .unwrap();
        let mut command = Command::new(&prepared.program);
        prepared.configure(&mut command).unwrap();
        let output = command.stdin(Stdio::null()).output().unwrap();
        assert!(
            output.status.success(),
            "native host channel failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"N5_PRIVATE_HOST_CHANNEL_OK\n");
        assert_eq!(forbidden.hits.load(Ordering::Acquire), forbidden_baseline);
        assert!(
            matches!(udp.recv_from(&mut packet), Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
        );
        let runtime = environment.runtime_directory().to_path_buf();
        environment.cleanup().unwrap();
        assert!(!runtime.exists());
    }
}

struct SshFixture {
    directory: tempfile::TempDir,
    daemon: Child,
    settings: BrowserVmSettings,
}

impl SshFixture {
    fn new(host: &str) -> Self {
        let directory = tempfile::tempdir_in("/root").unwrap();
        let root = directory.path();
        for name in ["server", "client"] {
            assert!(
                Command::new("/usr/bin/ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                    .arg(root.join(name))
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fs::write(
            root.join("authorized_keys"),
            fs::read(root.join("client.pub")).unwrap(),
        )
        .unwrap();
        fs::set_permissions(
            root.join("authorized_keys"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let public = fs::read_to_string(root.join("server.pub")).unwrap();
        fs::write(
            root.join("known_hosts"),
            format!("lima-colima-n5-host {public}"),
        )
        .unwrap();
        let endpoint = TcpListener::bind(SocketAddr::new(host.parse().unwrap(), 0))
            .unwrap()
            .local_addr()
            .unwrap();
        let configuration = format!(
            "Port {}\nListenAddress {host}\nHostKey {}\nAuthorizedKeysFile {}\nPidFile {}\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\nAuthenticationMethods publickey\nPermitRootLogin prohibit-password\nUsePAM yes\nAllowUsers root\nAllowTcpForwarding no\nAllowAgentForwarding no\nX11Forwarding no\nPermitTTY no\n",
            endpoint.port(),
            root.join("server").display(),
            root.join("authorized_keys").display(),
            root.join("sshd.pid").display()
        );
        fs::write(root.join("sshd_config"), configuration).unwrap();
        let daemon = Command::new("/usr/sbin/sshd")
            .args(["-D", "-e", "-f"])
            .arg(root.join("sshd_config"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(root.join("sshd.log")).unwrap())
            .process_group(0)
            .spawn()
            .unwrap();
        let settings = BrowserVmSettings {
            enabled: true,
            colima: Some("/usr/bin/ssh".into()),
            ssh: Some("/usr/bin/ssh".into()),
            profile: Some("n5-host".into()),
            host: Some(host.into()),
            user: Some("root".into()),
            port: Some(endpoint.port()),
            identity_file: Some(root.join("client")),
            known_hosts: Some(root.join("known_hosts")),
            node: Some("/opt/saya/node".into()),
            worker_root: Some("/opt/saya/worker".into()),
            chromium: Some("/opt/saya/chromium".into()),
            identity_manifest: Some("/opt/saya/identity.json".into()),
            identity_sha256: Some("a".repeat(64)),
            remote_temp_root: Some("/tmp".into()),
            ..Default::default()
        };
        let mut fixture = Self {
            directory,
            daemon,
            settings,
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while TcpStream::connect_timeout(&endpoint, Duration::from_millis(50)).is_err() {
            assert!(
                fixture.daemon.try_wait().unwrap().is_none(),
                "SSH fixture exited before readiness"
            );
            assert!(Instant::now() < deadline, "SSH fixture readiness deadline");
            thread::sleep(Duration::from_millis(10));
        }
        fixture
    }

    fn positive_control(&self) {
        let output = Command::new("/usr/bin/ssh")
            .args([
                "-F",
                "/dev/null",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "HostKeyAlias=lima-colima-n5-host",
                "-o",
            ])
            .arg(format!(
                "UserKnownHostsFile={}",
                self.directory.path().join("known_hosts").display()
            ))
            .arg("-i")
            .arg(self.directory.path().join("client"))
            .arg("-p")
            .arg(self.settings.port.unwrap().to_string())
            .arg(format!("root@{}", self.settings.host.as_ref().unwrap()))
            .arg("printf SSH_CONTROL")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "SSH positive control: {}; fixture: {}",
            String::from_utf8_lossy(&output.stderr),
            fs::read_to_string(self.directory.path().join("sshd.log")).unwrap(),
        );
        assert_eq!(output.stdout, b"SSH_CONTROL");
    }
}

impl Drop for SshFixture {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

struct NeverCancelled;
impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
#[ignore = "requires the dedicated root Linux SSH fixture, native tools and SAYA_TEST_EXECUTABLE"]
async fn native_ssh_vm_runner_authenticates_and_rejects_a_different_host_key() {
    pin_launcher();
    for host in ["127.0.0.2", "::1"] {
        let fixture = SshFixture::new(host);
        fixture.positive_control();
        let runner =
            SshVmCommandRunner::new(fixture.settings.clone()).expect("Linux VM host runner");
        let request = || VmCommandSpec {
            script: "printf N5_VM_HOST_SSH_OK",
            args: &[],
            stdin: None,
            timeout: Duration::from_secs(8),
            stderr_limit: 4096,
            stdout_limit: 4096,
        };
        let output = runner.run(request(), &NeverCancelled).await.unwrap();
        assert_eq!(
            output.status,
            Some(0),
            "native SSH: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"N5_VM_HOST_SSH_OK");
        let public = fs::read_to_string(fixture.directory.path().join("client.pub")).unwrap();
        fs::write(
            fixture.directory.path().join("known_hosts"),
            format!("lima-colima-n5-host {public}"),
        )
        .unwrap();
        let denied = runner.run(request(), &NeverCancelled).await.unwrap();
        assert_ne!(denied.status, Some(0));
        assert!(denied.stdout.is_empty());
        assert!(String::from_utf8_lossy(&denied.stderr).contains("Host key verification failed"));
    }
}
