use saya_core::execution::{
    self, BackendKind, CapabilityLevel, NetworkPolicy, ProxyEndpoint, ResourceLimits, SandboxPolicy,
};
#[cfg(target_os = "macos")]
use std::net::TcpListener;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::Path,
    process::{Command, Output, Stdio},
};

#[cfg(target_os = "macos")]
fn native_policy(workspace: std::path::PathBuf) -> SandboxPolicy {
    static BOOTSTRAP: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    BOOTSTRAP.get_or_init(|| {
        let directory = tempfile::tempdir().expect("reference bootstrap fixture");
        let image = if let Some(path) = std::env::var_os("SAYA_TEST_EXECUTABLE") {
            std::path::PathBuf::from(path)
        } else {
            let source = directory.path().join("bootstrap.c");
            let image = directory.path().join("bootstrap");
            fs::write(
                &source,
                r###"#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int fill(int fd, void *target, size_t remaining) {
    char *position = target;
    while (remaining > 0) {
        ssize_t count = read(fd, position, remaining);
        if (count <= 0) return -1;
        position += count;
        remaining -= (size_t)count;
    }
    return 0;
}

static int number(int fd, uint32_t *value) {
    unsigned char bytes[4];
    if (fill(fd, bytes, sizeof(bytes))) return -1;
    *value = ((uint32_t)bytes[0] << 24) | ((uint32_t)bytes[1] << 16)
           | ((uint32_t)bytes[2] << 8) | bytes[3];
    return 0;
}

int main(int argc, char **argv) {
    int fd = -1;
    int separator = -1;
    if (argc < 6 || strcmp(argv[1], "__saya-sandbox-launch")) return 64;
    for (int index = 2; index < argc; index++) {
        if (!strcmp(argv[index], "--env-fd") && index + 1 < argc) {
            fd = atoi(argv[++index]);
            continue;
        }
        if (!strcmp(argv[index], "--")) {
            separator = index;
            break;
        }
        return 64;
    }
    if (fd < 3 || separator < 0 || separator + 1 >= argc) return 64;
    char magic[8];
    uint32_t count = 0;
    if (fill(fd, magic, sizeof(magic)) || memcmp(magic, "SAYAENV1", 8)
        || number(fd, &count) || count > 1048576) return 74;
    char **environment = calloc((size_t)count + 1, sizeof(char *));
    if (!environment) return 74;
    size_t total = 12;
    const size_t limit = 8 * 1024 * 1024;
    for (uint32_t index = 0; index < count; index++) {
        uint32_t key_length;
        uint32_t value_length;
        if (number(fd, &key_length) || number(fd, &value_length)
            || key_length == 0 || key_length > limit || value_length > limit
            || total + 8 + key_length + value_length > limit) return 74;
        char *entry = malloc((size_t)key_length + value_length + 2);
        if (!entry) return 74;
        if (fill(fd, entry, key_length)
            || fill(fd, entry + key_length + 1, value_length)) return 74;
        entry[key_length] = '=';
        entry[key_length + value_length + 1] = 0;
        environment[index] = entry;
        total += 8 + key_length + value_length;
    }
    char extra;
    if (read(fd, &extra, 1) != 0) return 74;
    close(fd);
    execve(argv[separator + 1], argv + separator + 1, environment);
    return errno == 0 ? 74 : errno;
}
"###,
            )
            .expect("reference bootstrap source");
            assert!(
                Command::new("/usr/bin/cc")
                    .args(["-Wall", "-Wextra", "-Werror"])
                    .arg(&source)
                    .arg("-o")
                    .arg(&image)
                    .status()
                    .expect("compile bootstrap reference fixture")
                    .success()
            );
            image
        };
        execution::pin_launcher_image_from(&image).expect("pin trusted fixture image");
        directory
    });
    SandboxPolicy::workspace(workspace)
}
fn launch(environment: &execution::PreparedEnvironment, cwd: &Path, argv: &[&str]) -> Output {
    let argv = argv.iter().map(OsString::from).collect::<Vec<_>>();
    let prepared = environment
        .prepare_command(&argv, cwd, &BTreeMap::new())
        .expect("prepare fixture command");
    let mut command = Command::new(&prepared.program);
    prepared
        .configure(&mut command)
        .expect("configure fixture command");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("launch fixture command")
}
#[test]
fn invalid_paths_proxy_and_resource_limits_fail_before_launch() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = SandboxPolicy::workspace(workspace.path().to_path_buf());
    policy.resources.open_files = Some(0);
    assert_eq!(
        execution::prepare(&policy)
            .expect_err("zero is invalid")
            .0
            .code,
        "invalid_sandbox_policy"
    );
    policy.resources = ResourceLimits::default();
    policy.network = NetworkPolicy::Proxy(ProxyEndpoint {
        endpoint: "192.0.2.1:1234".parse().expect("synthetic address"),
        unix_socket: None,
    });
    assert_eq!(
        execution::prepare(&policy)
            .expect_err("external proxy is invalid")
            .0
            .code,
        "invalid_sandbox_policy"
    );
    policy.network = NetworkPolicy::Deny;
    policy.read_roots.push("/".into());
    assert_eq!(
        execution::prepare(&policy)
            .expect_err("root view is invalid")
            .0
            .code,
        "unsupported_fs_policy"
    );
}
#[test]
fn aggregate_resources_require_real_delegation() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = SandboxPolicy::workspace(workspace.path().to_path_buf());
    policy.backend = BackendKind::Host;
    policy.resources.total_memory_bytes = Some(1024 * 1024);
    assert_eq!(
        execution::prepare(&policy)
            .expect_err("no delegated root")
            .0
            .code,
        "sandbox_capability_unavailable"
    );
    policy.cgroup_root = Some(workspace.path().into());
    assert!(execution::prepare(&policy).is_err());
}
#[test]
fn explicit_host_backend_reports_absent_isolation() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = SandboxPolicy::workspace(workspace.path().to_path_buf());
    policy.backend = BackendKind::Host;
    let environment = execution::prepare(&policy).expect("explicit host backend");
    assert_eq!(
        environment.capabilities().filesystem,
        CapabilityLevel::Unsupported
    );
    assert_eq!(
        environment.capabilities().network,
        CapabilityLevel::Unsupported
    );
    assert_eq!(environment.identity().backend, BackendKind::Host);
    assert!(environment.cleanup().expect("cleanup").complete);
    assert!(environment.cleanup().expect("idempotent cleanup").complete);
}
#[test]
fn unavailable_platform_backend_never_falls_back_to_host() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = SandboxPolicy::workspace(workspace.path().to_path_buf());
    policy.backend = if cfg!(target_os = "macos") {
        BackendKind::Bubblewrap
    } else {
        BackendKind::Seatbelt
    };
    assert_eq!(
        execution::prepare(&policy)
            .expect_err("wrong platform")
            .0
            .code,
        "sandbox_capability_unavailable"
    );
}
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_enforces_reads_writes_links_and_child_inheritance() {
    use std::os::unix::fs::symlink;
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = scratch.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");
    let outside = scratch.path().join("outside.txt");
    fs::write(&outside, "SYNTHETIC").expect("outside fixture");
    let protected = workspace.join("protected.txt");
    fs::write(&protected, "SYNTHETIC").expect("protected fixture");
    symlink(&outside, workspace.join("link.txt")).expect("fixture symlink");
    let mut policy = native_policy(workspace.clone());
    policy.protected_paths.push(protected.clone());
    let environment = execution::prepare(&policy).expect("native Seatbelt availability");
    assert_eq!(environment.identity().backend, BackendKind::Seatbelt);
    assert_eq!(environment.capabilities().filesystem, CapabilityLevel::Hard);
    assert_eq!(
        environment.capabilities().process_cleanup,
        CapabilityLevel::BestEffort
    );
    assert!(
        launch(
            &environment,
            &workspace,
            &["/bin/sh", "-c", "printf allowed > normal.txt"]
        )
        .status
        .success()
    );
    assert_eq!(
        fs::read_to_string(workspace.join("normal.txt")).expect("created fixture"),
        "allowed"
    );
    assert!(
        launch(&environment, &workspace, &["/bin/cat", "normal.txt"])
            .status
            .success()
    );
    for path in [&outside, &protected, &workspace.join("link.txt")] {
        assert!(
            !launch(
                &environment,
                &workspace,
                &["/bin/cat", path.to_str().expect("fixture path")]
            )
            .status
            .success()
        );
        let script = "printf forbidden > \"$1\"";
        assert!(
            !launch(
                &environment,
                &workspace,
                &[
                    "/bin/sh",
                    "-c",
                    script,
                    "fixture",
                    path.to_str().expect("fixture path")
                ]
            )
            .status
            .success()
        );
    }
    assert_eq!(
        fs::read_to_string(&outside).expect("unchanged fixture"),
        "SYNTHETIC"
    );
    assert_eq!(
        fs::read_to_string(&protected).expect("unchanged fixture"),
        "SYNTHETIC"
    );
    assert!(
        !launch(
            &environment,
            &workspace,
            &[
                "/bin/sh",
                "-c",
                "/bin/sh -c 'printf forbidden > protected.txt'"
            ]
        )
        .status
        .success()
    );
    environment.cleanup().expect("cleanup");
}
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_read_only_and_future_protected_paths_remain_enforced() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = native_policy(workspace.path().to_path_buf());
    policy.write_roots.clear();
    let read_only = execution::prepare(&policy).expect("read-only environment");
    assert!(
        !launch(
            &read_only,
            workspace.path(),
            &["/bin/sh", "-c", "printf forbidden > created.txt"]
        )
        .status
        .success()
    );
    assert!(!workspace.path().join("created.txt").exists());
    policy.write_roots.push(workspace.path().into());
    policy.protected_paths.push(workspace.path().join("future"));
    let environment = execution::prepare(&policy).expect("future deny environment");
    fs::create_dir(workspace.path().join("future")).expect("external fixture creation");
    assert!(
        !launch(
            &environment,
            workspace.path(),
            &["/bin/sh", "-c", "printf forbidden > future/created.txt"]
        )
        .status
        .success()
    );
    assert!(!workspace.path().join("future/created.txt").exists());
}
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_network_permits_only_the_declared_proxy_port() {
    let workspace = tempfile::tempdir().expect("workspace");
    let proxy = TcpListener::bind("127.0.0.1:0").expect("loopback fixture");
    let other = TcpListener::bind("127.0.0.1:0").expect("other loopback fixture");
    let mut policy = native_policy(workspace.path().into());
    let denied = execution::prepare(&policy).expect("deny network environment");
    let proxy_port = proxy
        .local_addr()
        .expect("proxy address")
        .port()
        .to_string();
    let other_port = other
        .local_addr()
        .expect("other address")
        .port()
        .to_string();
    assert!(
        !launch(
            &denied,
            workspace.path(),
            &["/usr/bin/nc", "-z", "-w", "1", "127.0.0.1", &proxy_port]
        )
        .status
        .success()
    );
    policy.network = NetworkPolicy::Proxy(ProxyEndpoint {
        endpoint: proxy.local_addr().expect("proxy address"),
        unix_socket: None,
    });
    let environment = execution::prepare(&policy).expect("proxy environment");
    assert!(
        launch(
            &environment,
            workspace.path(),
            &["/usr/bin/nc", "-z", "-w", "1", "127.0.0.1", &proxy_port]
        )
        .status
        .success()
    );
    assert!(
        !launch(
            &environment,
            workspace.path(),
            &["/usr/bin/nc", "-z", "-w", "1", "127.0.0.1", &other_port]
        )
        .status
        .success()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn bubblewrap_prepares_private_views_and_payload_launcher() {
    let workspace = tempfile::tempdir().expect("workspace");
    let policy = SandboxPolicy::workspace(workspace.path().into());
    let environment =
        execution::prepare(&policy).expect("validated Bubblewrap dependency and user namespaces");
    assert_eq!(environment.identity().backend, BackendKind::Bubblewrap);
    assert_eq!(
        environment.capabilities().syscall_filter,
        CapabilityLevel::Hard
    );
    let argv = [OsString::from("/usr/bin/true")];
    let prepared = environment
        .prepare_command(&argv, workspace.path(), &BTreeMap::new())
        .expect("prepared launcher");
    let args = prepared
        .args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>();
    for required in [
        "--unshare-user",
        "--unshare-pid",
        "--unshare-net",
        "--disable-userns",
        "--seccomp",
        "--ro-bind-fd",
        "--bind-fd",
        "__saya-sandbox-launch",
    ] {
        assert!(args.iter().any(|arg| arg == required), "missing {required}");
    }
    environment.cleanup().expect("cleanup");
}
#[cfg(target_os = "linux")]
#[test]
fn requested_cgroup_limits_use_only_the_explicitly_delegated_tree() {
    let Some(root) = std::env::var_os("SAYA_TEST_CGROUP_ROOT") else {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut policy = SandboxPolicy::workspace(workspace.path().into());
        policy.backend = BackendKind::Host;
        policy.resources.total_processes = Some(16);
        assert_eq!(
            execution::prepare(&policy)
                .expect_err("missing delegation")
                .0
                .code,
            "sandbox_capability_unavailable"
        );
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = SandboxPolicy::workspace(workspace.path().into());
    policy.backend = BackendKind::Host;
    policy.cgroup_root = Some(root.into());
    policy.resources.total_memory_bytes = Some(256 * 1024 * 1024);
    policy.resources.total_processes = Some(16);
    policy.resources.cpu_quota = Some(execution::CpuQuota {
        quota_micros: 10_000,
        period_micros: 100_000,
    });
    let environment = execution::prepare(&policy).expect("explicit real delegation");
    assert_eq!(
        environment.capabilities().total_memory,
        CapabilityLevel::Hard
    );
    assert_eq!(
        environment.capabilities().total_processes,
        CapabilityLevel::Hard
    );
    assert_eq!(
        environment.capabilities().cpu_bandwidth,
        CapabilityLevel::Hard
    );
    let output = launch(
        &environment,
        workspace.path(),
        &["/bin/sh", "-c", "cat /proc/self/cgroup"],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("/saya-"));
    environment
        .cleanup()
        .expect("actual cgroup kill and removal");
}

#[test]
fn closed_environment_rejects_new_and_prepared_launches() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = SandboxPolicy::workspace(workspace.path().into());
    policy.backend = BackendKind::Host;
    let environment = execution::prepare(&policy).expect("environment");
    let args = [OsString::from("/usr/bin/true")];
    let prepared = environment
        .prepare_command(&args, workspace.path(), &BTreeMap::new())
        .expect("prepared before close");
    let mut configured = Command::new(&prepared.program);
    prepared
        .configure(&mut configured)
        .expect("configure before close");
    environment.cleanup().expect("cleanup");
    assert!(
        configured.spawn().is_err(),
        "closed environment launched a configured command"
    );
    assert_eq!(
        environment
            .prepare_command(&args, workspace.path(), &BTreeMap::new())
            .err()
            .expect("closed environment")
            .0
            .code,
        "sandbox_environment_closed"
    );
    assert!(
        prepared
            .configure(&mut Command::new(&prepared.program))
            .is_err()
    );
}
#[test]
fn per_process_file_and_descriptor_limits_are_applied_before_exec() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = SandboxPolicy::workspace(workspace.path().into());
    policy.backend = BackendKind::Host;
    policy.resources.file_size_bytes = Some(8);
    policy.resources.open_files = Some(32);
    let environment = execution::prepare(&policy).expect("resource environment");
    let output = launch(
        &environment,
        workspace.path(),
        &["/bin/sh", "-c", "ulimit -n"],
    );
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "32");
    let output = launch(
        &environment,
        workspace.path(),
        &["/bin/sh", "-c", "printf 12345678901234567890 > limited.txt"],
    );
    assert!(!output.status.success());
    assert!(
        fs::metadata(workspace.path().join("limited.txt"))
            .expect("partial file")
            .len()
            <= 8
    );
}

#[cfg(target_os = "macos")]
#[test]
fn inherited_undeclared_descriptors_cannot_bypass_seatbelt_paths() {
    use std::os::fd::AsRawFd;
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = scratch.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");
    let outside = scratch.path().join("outside.txt");
    fs::write(&outside, "SYNTHETIC-INHERITED-FD").expect("outside fixture");
    let file = fs::File::open(&outside).expect("open synthetic fixture");
    let descriptor = file.as_raw_fd();
    // SAFETY: this test owns the open fixture descriptor. Restore CLOEXEC before Drop.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
        0
    );
    let policy = native_policy(workspace.clone());
    let environment = execution::prepare(&policy).expect("Seatbelt environment");
    let script = format!("/bin/cat <&{descriptor}");
    let output = launch(&environment, &workspace, &["/bin/sh", "-c", &script]);
    assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags) }, 0);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SYNTHETIC-INHERITED-FD"));
}

#[cfg(target_os = "linux")]
#[test]
fn payload_seccomp_denies_unix_socketpair_datagram_retargeting() {
    use std::os::unix::process::CommandExt;
    let filter = execution::prepare_linux_payload_filter().expect("payload filter");
    let mut command = Command::new("/usr/bin/python3");
    command.args(["-I", "-S", "-c", "import errno,socket,sys\ntry:\n socket.socketpair(socket.AF_UNIX,socket.SOCK_DGRAM)\nexcept OSError as e:\n sys.exit(0 if e.errno == errno.EPERM else 3)\nsys.exit(4)"]).env_clear().stdin(Stdio::null());
    // SAFETY: filter was prepared before fork; install performs only prctl.
    unsafe {
        command.pre_exec(move || filter.install());
    }
    assert!(command.status().expect("filtered Python fixture").success());
}

fn file_request(
    workspace: &Path,
    name: &str,
    arguments: serde_json::Value,
    target: &Path,
) -> execution::FileWorkerRequest {
    use saya_core::{
        boundary::ToolContext,
        contract::{PermissionMode, Run, RunId, SessionId, ToolCall, WorkMode},
        tools::read::ReadLimits,
    };
    execution::FileWorkerRequest {
        schema_version: 1,
        call: ToolCall {
            call_id: "worker-fixture-call".into(),
            name: name.into(),
            arguments: arguments.to_string(),
        },
        context: ToolContext {
            run: Run {
                id: RunId::new(),
                session_id: SessionId::new(),
                work_mode: WorkMode::Work,
                permissions: PermissionMode::Auto,
            },
            workspace: workspace.into(),
        },
        approved_target: target.into(),
        read_roots: Vec::new(),
        read_limits: ReadLimits::default(),
        write_max_bytes: 1024 * 1024,
    }
}
#[tokio::test]
async fn file_worker_reuses_native_read_write_patch_and_discovery_tools() {
    use serde_json::json;
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = fs::canonicalize(scratch.path()).expect("canonical workspace");
    let path = workspace.join("example.txt");
    let write = file_request(
        &workspace,
        "write_file",
        json!({"path":"example.txt","content":"first\n","expected_version":null}),
        &path,
    );
    assert!(
        execution::execute_file_worker(write)
            .await
            .expect("worker write")
            .ok
    );
    let read = file_request(
        &workspace,
        "read_file",
        json!({"path":"example.txt","start_line":1,"line_count":10}),
        &path,
    );
    let read = execution::execute_file_worker(read)
        .await
        .expect("worker read");
    assert!(read.ok);
    let data = read.data.expect("read payload");
    assert_eq!(data["content"], "first\n");
    let patch = file_request(
        &workspace,
        "apply_patch",
        json!({"path":"example.txt","patch":"--- a/example.txt\n+++ b/example.txt\n@@ -1 +1 @@\n-first\n+second\n","expected_version":data["version"]}),
        &path,
    );
    assert!(
        execution::execute_file_worker(patch)
            .await
            .expect("worker patch")
            .ok
    );
    assert_eq!(fs::read_to_string(&path).expect("patched file"), "second\n");
    for (name, arguments) in [
        ("list_dir", json!({"path":"."})),
        ("find_files", json!({"path":".","glob":"*.txt"})),
        ("search_text", json!({"path":".","pattern":"second"})),
    ] {
        let result =
            execution::execute_file_worker(file_request(&workspace, name, arguments, &workspace))
                .await
                .expect("worker discovery");
        assert!(result.ok, "{name} {:?}", result.error);
    }
}
#[tokio::test]
async fn file_worker_rechecks_plan_target_and_supported_tool_boundaries() {
    use saya_core::contract::WorkMode;
    use serde_json::json;
    use std::os::unix::fs::symlink;
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = fs::canonicalize(scratch.path()).expect("canonical workspace");
    let path = workspace.join("first.txt");
    let other = workspace.join("second.txt");
    let mut plan = file_request(
        &workspace,
        "write_file",
        json!({"path":"first.txt","content":"forbidden","expected_version":null}),
        &path,
    );
    plan.context.run.work_mode = WorkMode::Plan;
    assert_eq!(
        execution::execute_file_worker(plan)
            .await
            .expect("PLAN result")
            .error
            .expect("PLAN refusal")
            .code,
        "mode_denied"
    );
    assert!(!path.exists());
    fs::write(&other, "SYNTHETIC").expect("other fixture");
    symlink(&other, &path).expect("retargeted fixture");
    let read = file_request(
        &workspace,
        "read_file",
        json!({"path":"first.txt","start_line":1,"line_count":1}),
        &path,
    );
    assert_eq!(
        execution::execute_file_worker(read)
            .await
            .expect("gate result")
            .error
            .expect("stale target refusal")
            .code,
        "permission_denied"
    );
    let exec = file_request(&workspace, "exec", json!({"argv":["/bin/sh"]}), &workspace);
    assert_eq!(
        execution::execute_file_worker(exec)
            .await
            .expect_err("unsupported tool")
            .0
            .code,
        "unsupported_worker_tool"
    );
    let mut invalid = file_request(&workspace, "list_dir", json!({"path":"."}), &workspace);
    invalid.read_limits.list_bytes = 0;
    assert_eq!(
        execution::execute_file_worker(invalid)
            .await
            .expect_err("zero worker budget")
            .0
            .code,
        "invalid_worker_request"
    );
}

#[tokio::test]
async fn file_worker_scan_budget_is_independent_of_return_frame_budget() {
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = fs::canonicalize(scratch.path()).expect("workspace");
    let mut request = file_request(
        &workspace,
        "list_dir",
        serde_json::json!({"path":"."}),
        &workspace,
    );
    request.read_limits.file_bytes = 64 * 1024 * 1024;
    request.read_limits.read_return_bytes = 64 * 1024 * 1024;
    request.read_limits.search_bytes = 64 * 1024 * 1024;
    request.read_limits.list_bytes = 64 * 1024 * 1024;
    request.write_max_bytes = 64 * 1024 * 1024;
    assert!(
        execution::execute_file_worker(request)
            .await
            .expect("large scan small return")
            .ok
    );
}

#[cfg(target_os = "macos")]
#[test]
fn seatbelt_allows_runtime_sysctls_and_denies_process_argument_queries() {
    let workspace = tempfile::tempdir().expect("workspace");
    let source = workspace.path().join("sysctl-probe.c");
    let program = workspace.path().join("sysctl-probe");
    fs::write(
        &source,
        r#"
#include <sys/types.h>
#include <sys/sysctl.h>
#include <unistd.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    char buffer[262144];
    size_t length = sizeof(buffer);
    int operating_system[] = {CTL_KERN, KERN_OSRELEASE};
    int allowed = sysctl(operating_system, 2, buffer, &length, NULL, 0);
    length = sizeof(buffer);
    int arguments[] = {CTL_KERN, KERN_PROCARGS2, atoi(argv[1])};
    int process_arguments = sysctl(arguments, 3, buffer, &length, NULL, 0);
    int error = errno;
    printf("os=%d process_arguments=%d error=%d\n", allowed, process_arguments, error);
    return 0;
}
"#,
    )
    .expect("probe source");
    assert!(
        Command::new("/usr/bin/cc")
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .status()
            .expect("compile sysctl probe")
            .success()
    );
    let program = program.canonicalize().expect("canonical probe path");
    let parent = std::process::id().to_string();
    let control = Command::new(&program)
        .arg(&parent)
        .output()
        .expect("control probe");
    assert!(String::from_utf8_lossy(&control.stdout).contains("os=0 process_arguments=0"));
    let environment =
        execution::prepare(&native_policy(workspace.path().into())).expect("Seatbelt");
    let result = launch(
        &environment,
        workspace.path(),
        &[program.to_str().expect("probe path"), &parent],
    );
    assert!(
        result.status.success(),
        "status={:?} stdout={} stderr={}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.stdout),
        "os=0 process_arguments=-1 error=1\n"
    );
    let alias = workspace.path().join("sysctl-probe");
    let through_alias = launch(
        &environment,
        workspace.path(),
        &[alias.to_str().expect("alias path"), &parent],
    );
    assert!(
        through_alias.status.success(),
        "root symlink alias did not resolve"
    );
    assert_eq!(
        String::from_utf8_lossy(&through_alias.stdout),
        "os=0 process_arguments=-1 error=1\n"
    );
    environment.cleanup().expect("cleanup");
}

#[cfg(target_os = "linux")]
#[test]
fn anonymous_stream_pairs_cannot_retarget_or_deliver_rights_to_host_unix_peers() {
    use std::os::unix::{
        net::{UnixDatagram, UnixListener},
        process::CommandExt,
    };
    let root = tempfile::tempdir().unwrap();
    let stream_path = root.path().join("host-stream.sock");
    let datagram_path = root.path().join("host-datagram.sock");
    let fixture_path = root.path().join("fixture.txt");
    fs::write(&fixture_path, "SYNTHETIC-PAIR-LOCAL-RIGHTS").unwrap();
    let stream = UnixListener::bind(&stream_path).unwrap();
    let datagram = UnixDatagram::bind(&datagram_path).unwrap();
    stream.set_nonblocking(true).unwrap();
    datagram.set_nonblocking(true).unwrap();
    let program = r#"
import array
import ctypes
import errno
import os
import socket
import sys

stream_path, datagram_path, fixture_path = sys.argv[1:]
for kind in (socket.SOCK_DGRAM, socket.SOCK_SEQPACKET):
    try:
        socket.socketpair(socket.AF_UNIX, kind)
    except OSError as error:
        assert error.errno == errno.EPERM
    else:
        raise AssertionError('non-stream pair was allowed')
try:
    socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
except OSError as error:
    assert error.errno == errno.EPERM
else:
    raise AssertionError('named Unix socket was allowed')

left, right = socket.socketpair(
    socket.AF_UNIX, socket.SOCK_STREAM | socket.SOCK_CLOEXEC | socket.SOCK_NONBLOCK
)
for endpoint in (left, right):
    try:
        endpoint.connect(stream_path)
    except OSError:
        pass
    else:
        raise AssertionError('connected stream was retargeted')

libc = ctypes.CDLL(None, use_errno=True)
class Address(ctypes.Structure):
    _fields_ = [('family', ctypes.c_ushort), ('data', ctypes.c_ubyte * 14)]
unspecified = Address(socket.AF_UNSPEC)
assert libc.connect(left.fileno(), ctypes.byref(unspecified), ctypes.sizeof(unspecified)) == -1
try:
    left.connect(stream_path)
except OSError:
    pass
else:
    raise AssertionError('AF_UNSPEC permitted stream retargeting')
left.send(b'local')
assert right.recv(16) == b'local'
for destination in (stream_path, datagram_path):
    try:
        length = left.sendto(b'addressed', destination)
    except OSError:
        pass
    else:
        assert length == 9 and right.recv(16) == b'addressed'

fd = os.open(fixture_path, os.O_RDONLY)
def receive_local_rights():
    data, ancillary, flags, address = left.recvmsg(1, socket.CMSG_SPACE(array.array('i').itemsize))
    assert data == b'f' and not flags and not address
    assert len(ancillary) == 1 and ancillary[0][:2] == (socket.SOL_SOCKET, socket.SCM_RIGHTS)
    received = array.array('i')
    received.frombytes(ancillary[0][2])
    assert len(received) == 1
    try:
        assert os.fstat(received[0]).st_ino == os.fstat(fd).st_ino
    finally:
        os.close(received[0])
try:
    rights = [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [fd]))]
    right.sendmsg([b'f'], rights)
    receive_local_rights()
    for destination in (stream_path, datagram_path):
        try:
            length = right.sendmsg([b'f'], rights, 0, destination)
        except OSError:
            pass
        else:
            assert length == 1
            receive_local_rights()
finally:
    os.close(fd)
left.close()
right.close()
"#;
    let mut command = Command::new("/usr/bin/python3");
    command
        .args(["-I", "-S", "-c", program])
        .arg(&stream_path)
        .arg(&datagram_path)
        .arg(&fixture_path)
        .env_clear()
        .stdin(Stdio::null());
    let filter = execution::prepare_linux_payload_filter().unwrap();
    // SAFETY: the program was prepared before fork; installation only invokes prctl.
    unsafe {
        command.pre_exec(move || filter.install());
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "filtered synthetic pair probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(matches!(stream.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock));
    let mut bytes = [0u8; 128];
    assert!(
        matches!(datagram.recv(&mut bytes),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock)
    );
}
