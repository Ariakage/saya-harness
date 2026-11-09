use saya_core::{
    boundary::ToolContext,
    contract::{PermissionMode, Run, RunId, SessionId, ToolCall, WorkMode},
    execution::{FileWorkerRequest, FileWorkerResponse},
    tools::read::ReadLimits,
};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn request(
    workspace: &Path,
    name: &str,
    arguments: serde_json::Value,
    target: &Path,
) -> FileWorkerRequest {
    FileWorkerRequest {
        schema_version: 1,
        call: ToolCall {
            call_id: "isolated-fixture".into(),
            name: name.into(),
            arguments: arguments.to_string(),
        },
        context: ToolContext {
            workspace: workspace.into(),
            run: Run {
                id: RunId::new(),
                session_id: SessionId::new(),
                work_mode: WorkMode::Code,
                permissions: PermissionMode::Manual,
            },
        },
        approved_target: target.into(),
        read_roots: Vec::new(),
        read_limits: ReadLimits::default(),
        write_max_bytes: 4096,
        protected_paths: Default::default(),
    }
}

fn run_worker(workspace: &Path, input: &[u8], args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    let poisoned = home.path().join("invalid-config.toml");
    fs::write(&poisoned, "SYNTHETIC-CONFIG-MUST-NOT-BE-READ").unwrap();
    let state = home.path().join("state");
    let mut child = Command::new(env!("CARGO_BIN_EXE_saya"))
        .args(args)
        .current_dir(workspace)
        .env_clear()
        .env("HOME", home.path())
        .env("SAYA_CONFIG", poisoned)
        .env("XDG_STATE_HOME", &state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        !state.exists(),
        "internal worker opened app state or logging"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SYNTHETIC-CONFIG"));
    output
}

fn call_worker(request: &FileWorkerRequest) -> FileWorkerResponse {
    let output = run_worker(
        &request.context.workspace,
        &serde_json::to_vec(request).unwrap(),
        &["__saya-file-worker"],
    );
    assert!(
        output.status.success(),
        "worker failed: {:?}",
        output.status.code()
    );
    assert!(output.stderr.is_empty());
    let response: FileWorkerResponse = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response.schema_version, 1);
    response
}

#[test]
fn private_cli_worker_routes_before_configuration_and_returns_a_single_response() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let path = workspace.join("fixture.txt");
    fs::write(&path, "fixture 你好\n").unwrap();
    let response = call_worker(&request(
        &workspace,
        "read_file",
        json!({"path":"fixture.txt","start_line":1,"line_count":10}),
        &path,
    ));
    assert!(response.result.ok);
    assert_eq!(response.result.data.unwrap()["content"], "fixture 你好\n");
    assert!(response.boundary_error.is_none());
    assert!(response.observed_result.is_none());
}

#[test]
fn private_cli_worker_rechecks_plan_and_the_exact_approved_target() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let path = workspace.join("new.txt");
    let mut write = request(
        &workspace,
        "write_file",
        json!({"path":"new.txt","content":"blocked","expected_version":null}),
        &path,
    );
    write.context.run.work_mode = WorkMode::Plan;
    let response = call_worker(&write);
    assert!(!response.result.ok);
    assert_eq!(response.result.error.unwrap().code, "mode_denied");
    assert!(!path.exists());
    write.context.run.work_mode = WorkMode::Code;
    write.approved_target = workspace.join("different.txt");
    let response = call_worker(&write);
    assert!(!response.result.ok);
    assert!(!path.exists());
    assert!(!write.approved_target.exists());
    write.approved_target = path.clone();
    let response = call_worker(&write);
    assert!(response.result.ok);
    assert_eq!(fs::read_to_string(path).unwrap(), "blocked");
}

#[test]
fn private_cli_worker_rejects_malformed_multiple_or_unsupported_requests() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    for input in [b"{".as_slice(), b"{}\n{}", b"null"] {
        let output = run_worker(&workspace, input, &["__saya-file-worker"]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let response: FileWorkerResponse = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!response.result.ok);
        assert_eq!(
            response.result.error.unwrap().code,
            "invalid_worker_request"
        );
    }
    let mut unsupported = request(
        &workspace,
        "exec",
        json!({"argv":["/bin/sh","-c","touch forbidden"]}),
        &workspace.join("forbidden"),
    );
    assert!(!call_worker(&unsupported).result.ok);
    assert!(!workspace.join("forbidden").exists());
    unsupported.schema_version = 2;
    assert!(!call_worker(&unsupported).result.ok);
    let output = run_worker(&workspace, b"", &["__saya-file-worker", "unexpected"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn private_cli_worker_receives_host_private_inode_snapshot_and_preserves_normal_hardlinks() {
    use saya_core::tools::read::ReadTools;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let private = workspace.join("private-memory");
    fs::create_dir(&private).unwrap();
    let memory = private.join("MEMORY.md");
    fs::write(&memory, "SYNTHETIC-PRIVATE-MEMORY").unwrap();
    fs::hard_link(&memory, workspace.join("private-alias.txt")).unwrap();
    fs::write(workspace.join("public.txt"), "public needle").unwrap();
    fs::hard_link(
        workspace.join("public.txt"),
        workspace.join("normal-link.txt"),
    )
    .unwrap();
    let host = ReadTools::new(&workspace, &[], false, ReadLimits::default()).unwrap();
    host.protect_paths(vec![private.clone()]).unwrap();
    let snapshot = host
        .worker_protection_snapshot("search_text", &workspace)
        .unwrap();
    for name in ["read_file", "write_file", "apply_patch"] {
        let args = match name {
            "read_file" => json!({"path":"private-alias.txt","start_line":1,"line_count":10}),
            "write_file" => {
                json!({"path":"private-alias.txt","content":"blocked","expected_version":"0".repeat(64)})
            }
            _ => {
                json!({"path":"private-alias.txt","patch":"--- a/private-alias.txt\n+++ b/private-alias.txt\n@@ -1 +1 @@\n-SYNTHETIC-PRIVATE-MEMORY\n+blocked\n","expected_version":"invalid"})
            }
        };
        let mut input = request(&workspace, name, args, &workspace.join("private-alias.txt"));
        input.protected_paths = snapshot.clone();
        let response = call_worker(&input);
        assert!(!response.result.ok);
        assert_eq!(response.result.error.unwrap().code, "path_denied");
    }
    for (name, args) in [
        ("list_dir", json!({"path":"."})),
        (
            "find_files",
            json!({"path":".","glob":"*.txt","include_ignored":true}),
        ),
        (
            "search_text",
            json!({"path":".","pattern":"SYNTHETIC-PRIVATE-MEMORY","include_ignored":true}),
        ),
    ] {
        let mut input = request(&workspace, name, args, &workspace);
        input.protected_paths = snapshot.clone();
        let response = call_worker(&input);
        assert!(response.result.ok, "{name}: {:?}", response.result);
        let data = response.result.data.unwrap();
        assert!(!data.to_string().contains("private-alias.txt"));
        assert!(!data.to_string().contains("SYNTHETIC-PRIVATE-MEMORY"));
    }
    fs::rename(&private, workspace.join("moved-private-memory")).unwrap();
    let mut input = request(
        &workspace,
        "read_file",
        json!({"path":"normal-link.txt","start_line":1,"line_count":10}),
        &workspace.join("normal-link.txt"),
    );
    input.protected_paths = snapshot;
    let response = call_worker(&input);
    assert!(response.result.ok);
    assert_eq!(response.result.data.unwrap()["content"], "public needle");
    assert_eq!(
        fs::read_to_string(workspace.join("moved-private-memory/MEMORY.md")).unwrap(),
        "SYNTHETIC-PRIVATE-MEMORY"
    );
    let mut legacy = serde_json::to_value(request(
        &workspace,
        "read_file",
        json!({"path":"public.txt","start_line":1,"line_count":10}),
        &workspace.join("public.txt"),
    ))
    .unwrap();
    legacy.as_object_mut().unwrap().remove("protected_paths");
    let output = run_worker(
        &workspace,
        &serde_json::to_vec(&legacy).unwrap(),
        &["__saya-file-worker"],
    );
    let response: FileWorkerResponse = serde_json::from_slice(&output.stdout).unwrap();
    assert!(response.result.ok);
}

#[tokio::test]
async fn native_file_worker_uses_snapshot_without_accessing_os_hidden_private_root() {
    use saya_core::tools::{isolated::execute_isolated_file, read::ReadTools};
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let private = workspace.join("private-memory");
    fs::create_dir(&private).unwrap();
    fs::write(private.join("MEMORY.md"), "SYNTHETIC-NATIVE-PRIVATE").unwrap();
    fs::hard_link(
        private.join("MEMORY.md"),
        workspace.join("private-alias.txt"),
    )
    .unwrap();
    fs::write(workspace.join("plain.txt"), "public needle").unwrap();
    fs::hard_link(
        workspace.join("plain.txt"),
        workspace.join("normal-link.txt"),
    )
    .unwrap();
    let host = ReadTools::new(&workspace, &[], false, ReadLimits::default()).unwrap();
    host.protect_paths(vec![private.clone()]).unwrap();
    let snapshot = host
        .worker_protection_snapshot("search_text", &workspace)
        .unwrap();
    let environment = native_environment(&workspace, vec![private.clone()]);
    for (name, args, target) in [
        (
            "read_file",
            json!({"path":"normal-link.txt","start_line":1,"line_count":10}),
            workspace.join("normal-link.txt"),
        ),
        ("list_dir", json!({"path":"."}), workspace.clone()),
        (
            "find_files",
            json!({"path":".","glob":"*.txt","include_ignored":true}),
            workspace.clone(),
        ),
        (
            "search_text",
            json!({"path":".","pattern":"needle","include_ignored":true}),
            workspace.clone(),
        ),
    ] {
        let mut input = request(&workspace, name, args, &target);
        input.protected_paths = snapshot.clone();
        let result = execute_isolated_file(&environment, input, &Never, Duration::from_secs(10))
            .await
            .unwrap();
        assert!(result.ok, "{name}: {result:?}");
        let data = result.data.unwrap();
        assert!(!data.to_string().contains("private-alias.txt"));
        assert!(!data.to_string().contains("SYNTHETIC-NATIVE-PRIVATE"));
        assert!(
            data.to_string().contains("plain.txt") || data.to_string().contains("public needle")
        );
    }
    let mut input = request(
        &workspace,
        "read_file",
        json!({"path":"private-alias.txt","start_line":1,"line_count":10}),
        &workspace.join("private-alias.txt"),
    );
    input.protected_paths = snapshot;
    assert_eq!(
        execute_isolated_file(&environment, input, &Never, Duration::from_secs(10))
            .await
            .unwrap_err()
            .0
            .code,
        "path_denied"
    );
    assert_eq!(
        fs::read_to_string(private.join("MEMORY.md")).unwrap(),
        "SYNTHETIC-NATIVE-PRIVATE"
    );
    assert!(environment.cleanup().unwrap().complete);
}

struct Never;
impl saya_core::boundary::Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

fn native_environment(
    workspace: &Path,
    protected: Vec<std::path::PathBuf>,
) -> saya_core::execution::PreparedEnvironment {
    saya_core::execution::pin_launcher_image_from(Path::new(env!("CARGO_BIN_EXE_saya"))).unwrap();
    let mut policy = saya_core::execution::SandboxPolicy::workspace(workspace.into());
    policy.protected_paths = protected;
    saya_core::execution::prepare(&policy).unwrap()
}

#[tokio::test]
async fn native_parent_rpc_uses_the_real_cli_image_for_all_six_file_tools() {
    use saya_core::tools::isolated::execute_isolated_file;
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let environment = native_environment(&workspace, Vec::new());
    let path = workspace.join("file.txt");
    let write = execute_isolated_file(
        &environment,
        request(
            &workspace,
            "write_file",
            json!({"path":"file.txt","content":"seed\n","expected_version":null}),
            &path,
        ),
        &Never,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert!(write.ok, "{write:?}");
    let version = write.data.as_ref().unwrap()["version"].as_str().unwrap();
    let patch = execute_isolated_file(
        &environment,
        request(
            &workspace,
            "apply_patch",
            json!({
                "path":"file.txt",
                "patch":"--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-seed\n+revised\n",
                "expected_version":version,
            }),
            &path,
        ),
        &Never,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert!(patch.ok, "{patch:?}");
    let read = execute_isolated_file(
        &environment,
        request(
            &workspace,
            "read_file",
            json!({"path":"file.txt","start_line":1,"line_count":10}),
            &path,
        ),
        &Never,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert!(read.ok, "{read:?}");
    assert_eq!(read.data.unwrap()["content"], "revised\n");
    for (name, arguments) in [
        ("list_dir", json!({"path":"."})),
        ("find_files", json!({"path":".","glob":"*.txt"})),
        ("search_text", json!({"path":".","pattern":"revised"})),
    ] {
        let result = execute_isolated_file(
            &environment,
            request(&workspace, name, arguments, &workspace),
            &Never,
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert!(result.ok, "{name}: {result:?}");
        assert!(!result.data.unwrap()["items"].as_array().unwrap().is_empty());
    }
    assert_eq!(fs::read_to_string(path).unwrap(), "revised\n");
    assert!(environment.cleanup().unwrap().complete);
}

#[tokio::test]
async fn native_parent_rpc_enforces_filesystem_policy_beyond_request_metadata() {
    use saya_core::tools::isolated::execute_isolated_file;
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let protected = workspace.join("protected.txt");
    fs::write(&protected, "SYNTHETIC-PROTECTED-DATA").unwrap();
    let outside = root.path().join("outside.txt");
    fs::write(&outside, "SYNTHETIC-OUTSIDE-DATA").unwrap();
    let outside = outside.canonicalize().unwrap();
    let environment = native_environment(&workspace, vec![protected.clone()]);
    let denied = execute_isolated_file(
        &environment,
        request(
            &workspace,
            "read_file",
            json!({"path":"protected.txt","start_line":1,"line_count":10}),
            &protected,
        ),
        &Never,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert!(!denied.ok);
    let mut outer = request(
        &workspace,
        "read_file",
        json!({"path":outside,"start_line":1,"line_count":10}),
        &outside,
    );
    outer.read_roots = vec![root.path().canonicalize().unwrap()];
    match execute_isolated_file(&environment, outer, &Never, Duration::from_secs(10)).await {
        Ok(result) => assert!(!result.ok),
        Err(error) => assert!(!error.to_string().contains("SYNTHETIC-OUTSIDE-DATA")),
    }
    assert_eq!(
        fs::read_to_string(&protected).unwrap(),
        "SYNTHETIC-PROTECTED-DATA"
    );
    assert_eq!(
        fs::read_to_string(&outside).unwrap(),
        "SYNTHETIC-OUTSIDE-DATA"
    );
    environment.cleanup().unwrap();
}

#[tokio::test]
async fn native_parent_rpc_accepts_large_limits_when_actual_packets_fit() {
    use saya_core::tools::isolated::execute_isolated_file;
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let environment = native_environment(&workspace, Vec::new());
    let path = workspace.join("small.txt");
    let mut write = request(
        &workspace,
        "write_file",
        json!({"path":"small.txt","content":"small result\n","expected_version":null}),
        &path,
    );
    write.read_limits = ReadLimits {
        read_return_bytes: 64 * 1024 * 1024,
        file_bytes: 64 * 1024 * 1024,
        search_bytes: 64 * 1024 * 1024,
        list_bytes: 64 * 1024 * 1024,
        ..ReadLimits::default()
    };
    write.write_max_bytes = 64 * 1024 * 1024;
    let read_limits = write.read_limits.clone();
    let result = execute_isolated_file(&environment, write, &Never, Duration::from_secs(10))
        .await
        .unwrap();
    assert!(result.ok);
    let mut read = request(
        &workspace,
        "read_file",
        json!({"path":"small.txt","start_line":1,"line_count":10}),
        &path,
    );
    read.read_limits = read_limits;
    read.write_max_bytes = 64 * 1024 * 1024;
    let result = execute_isolated_file(&environment, read, &Never, Duration::from_secs(10))
        .await
        .unwrap();
    assert!(result.ok);
    assert_eq!(result.data.unwrap()["content"], "small result\n");
    assert_eq!(fs::read_to_string(path).unwrap(), "small result\n");
    assert!(environment.cleanup().unwrap().complete);
}

#[test]
fn native_direct_prepared_worker_returns_a_valid_response_before_parent_supervision() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let path = workspace.join("direct-worker.txt");
    fs::write(&path, "SYNTHETIC-DIRECT-WORKER\n").unwrap();
    let environment = native_environment(&workspace, Vec::new());
    let argv = environment.file_worker_argv().unwrap();
    let prepared = environment
        .prepare_command(&argv, &workspace, &std::collections::BTreeMap::new())
        .unwrap();
    let mut command = Command::new(&prepared.program);
    prepared.configure(&mut command).unwrap();
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut bytes = serde_json::to_vec(&request(
        &workspace,
        "read_file",
        json!({"path":"direct-worker.txt","start_line":1,"line_count":1}),
        &path,
    ))
    .unwrap();
    bytes.push(b'\n');
    child.stdin.take().unwrap().write_all(&bytes).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut expired = false;
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            expired = true;
            let _ = child.kill();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout[..output.stdout.len().min(8192)]);
    let stderr = String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(8192)]);
    assert!(
        !expired && output.status.success(),
        "synthetic direct worker status={} expired={expired} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    let response: FileWorkerResponse =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "synthetic direct worker invalid JSON {error}: stdout={stdout:?} stderr={stderr:?}"
            )
        });
    assert!(response.result.ok, "{:?}", response.result.error);
    assert_eq!(
        response.result.data.unwrap()["content"],
        "SYNTHETIC-DIRECT-WORKER\n"
    );
    assert!(environment.cleanup().unwrap().complete);
}
