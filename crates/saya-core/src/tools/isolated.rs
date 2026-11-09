//! Parent-side, single-request file-worker IPC. Authorization and persistence belong to dispatch.

use crate::{
    boundary::Cancellation,
    contract::{BoundaryError, ErrorInfo, ToolResult, WorkMode},
    execution::{FileWorkerRequest, FileWorkerResponse, PreparedEnvironment, WORKER_RPC_MAX_BYTES},
    permissions::Operation,
    tools::exec::{WorkerCapture, run_worker},
};
use serde_json::Value;
use std::{io::Write, path::Path, time::Duration};

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

fn unknown(observed: Option<ToolResult>) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "outcome_unknown".into(),
            message: "Isolated file-worker completion could not be confirmed. The operation was not replayed".into(),
        },
        None,
        observed.map(Box::new),
    )
}

/// Execute one already-approved file request in the prepared native environment.
///
/// Sends one bounded JSON packet and consumes one bounded response through the shared process
/// supervisor. No Store, model configuration or logging is opened here. Pre-launch validation and
/// cancellation return known errors. Malformed or missing output and abnormal exit return
/// `outcome_unknown`. After successful process cleanup, a complete valid result is retained even
/// when cancellation or timeout occurred. The caller separately terminates the Run. A complete
/// worker result remains in `BoundaryError.2` when an abnormal lifecycle prevents acceptance.
/// Callers must supervise the environment lease and persist intent before invoking this function.
pub async fn execute_isolated_file(
    environment: &PreparedEnvironment,
    request: FileWorkerRequest,
    cancel: &dyn Cancellation,
    timeout: Duration,
) -> Result<ToolResult, BoundaryError> {
    if cancel.is_cancelled() {
        return Err(failure(
            "cancelled",
            "File operation cancelled before worker launch",
        ));
    }
    if timeout.is_zero() || timeout > Duration::from_secs(600) {
        return Err(failure(
            "invalid_args",
            "File worker timeout must be positive and at most 600 seconds",
        ));
    }
    let input = encode_request(&request)?;
    validate_request(&request)?;
    if cancel.is_cancelled() {
        return Err(failure(
            "cancelled",
            "File operation cancelled before worker launch",
        ));
    }
    let argv = environment.file_worker_argv()?;
    let capture = run_worker(
        environment,
        &argv,
        &request.context.workspace,
        input,
        cancel,
        timeout,
    )
    .await
    .map_err(|error| {
        // The supervisor's diagnostics and any raw captured stderr stay outside tool data.
        // Known setup failures occur before spawn. Other failures can follow worker effects.
        if error.2.is_none()
            && matches!(
                error.0.code.as_str(),
                "cancelled"
                    | "invalid_args"
                    | "invalid_path"
                    | "path_denied"
                    | "sandbox_environment_closed"
                    | "sandbox_capability_unavailable"
                    | "unsupported_fs_policy"
                    | "sandbox_launch_failed"
                    | "spawn_error"
            )
        {
            failure(&error.0.code, "Isolated file worker could not be launched")
        } else {
            unknown(error.2.map(|result| *result).filter(valid_result))
        }
    })?;
    decode_capture(capture)
}

fn validate_request(request: &FileWorkerRequest) -> Result<(), BoundaryError> {
    if request.schema_version != 1 {
        return Err(failure(
            "unsupported_worker_schema",
            "File worker schema must be 1",
        ));
    }
    if !matches!(
        request.call.name.as_str(),
        "read_file" | "list_dir" | "find_files" | "search_text" | "write_file" | "apply_patch"
    ) {
        return Err(failure(
            "unsupported_worker_tool",
            "File workers accept only local file tools",
        ));
    }
    if request.context.run.work_mode == WorkMode::Plan
        && matches!(request.call.name.as_str(), "write_file" | "apply_patch")
    {
        return Err(failure(
            "mode_denied",
            "PLAN permits read-only file operations",
        ));
    }
    if !request.context.workspace.is_absolute()
        || !request.approved_target.is_absolute()
        || request.read_roots.len() > 128
        || request.read_roots.iter().any(|root| !root.is_absolute())
    {
        return Err(failure(
            "invalid_worker_request",
            "File worker paths and roots are invalid",
        ));
    }
    let limits = &request.read_limits;
    if [
        limits.read_return_bytes,
        limits.file_bytes,
        limits.search_bytes,
        limits.list_bytes,
        request.write_max_bytes,
    ]
    .contains(&0)
        || limits.search_results == 0
        || limits.search_results > 100_000
        || limits.list_entries == 0
        || limits.list_entries > 100_000
    {
        return Err(failure(
            "invalid_worker_request",
            "File worker limits are invalid",
        ));
    }
    let arguments: Value = serde_json::from_str(&request.call.arguments)
        .map_err(|_| failure("invalid_args", "File worker arguments must be valid JSON"))?;
    let path = arguments
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("invalid_args", "File worker arguments require a path"))?;
    let normalized = Operation::file(
        &request.context.workspace,
        &request.call.name,
        Path::new(path),
    )?
    .normalized_arguments(arguments)?;
    let approved = serde_json::to_value(&request.approved_target).map_err(|_| {
        failure(
            "invalid_path",
            "File worker approved target cannot be encoded",
        )
    })?;
    if normalized.get("path") != Some(&approved) {
        return Err(failure(
            "stale_approval",
            "File worker target differs from its approval",
        ));
    }
    Ok(())
}

fn encode_request(request: &FileWorkerRequest) -> Result<Vec<u8>, BoundaryError> {
    let mut output = RequestBuffer(Vec::new());
    serde_json::to_writer(&mut output, request).map_err(|_| {
        failure(
            "worker_request_too_large",
            "File worker request cannot fit its IPC byte limit",
        )
    })?;
    output.0.push(b'\n');
    Ok(output.0)
}

struct RequestBuffer(Vec<u8>);
impl Write for RequestBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > (WORKER_RPC_MAX_BYTES - 1).saturating_sub(self.0.len()) {
            return Err(std::io::Error::from_raw_os_error(libc::EFBIG));
        }
        self.0
            .try_reserve(bytes.len())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::ENOMEM))?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn valid_result(result: &ToolResult) -> bool {
    result.ok == result.error.is_none()
        && result
            .error
            .as_ref()
            .is_none_or(|error| !error.code.is_empty() && error.code.len() <= 128)
}

fn decode_capture(capture: WorkerCapture) -> Result<ToolResult, BoundaryError> {
    if capture.process.stdout_truncated || capture.stdout.len() > WORKER_RPC_MAX_BYTES {
        return Err(unknown(None));
    }
    let response: FileWorkerResponse =
        serde_json::from_slice(&capture.stdout).map_err(|_| unknown(None))?;
    if response.schema_version != 1
        || !valid_result(&response.result)
        || response
            .observed_result
            .as_ref()
            .is_some_and(|result| !valid_result(result))
    {
        return Err(unknown(None));
    }
    if let Some(error) = &response.boundary_error {
        if response.result.ok
            || response.result.error.as_ref() != Some(error)
            || response.result.data.is_some()
            || response.result.truncated
        {
            return Err(unknown(None));
        }
    } else if response.observed_result.is_some() {
        return Err(unknown(None));
    }
    if response.boundary_error.is_none()
        && response.result.error.as_ref().is_some_and(|error| {
            matches!(
                error.code.as_str(),
                "invalid_worker_request"
                    | "unsupported_worker_schema"
                    | "unsupported_worker_tool"
                    | "worker_rpc_too_large"
                    | "worker_io_error"
            )
        })
    {
        return Err(unknown(None));
    }
    if !capture.process.cancelled
        && !capture.process.timed_out
        && capture.process.exit_code != Some(0)
    {
        let observed = response.observed_result.unwrap_or(response.result);
        return Err(unknown(Some(observed)));
    }
    if let Some(error) = response.boundary_error {
        return Err(BoundaryError(
            error,
            None,
            response.observed_result.map(Box::new),
        ));
    }
    Ok(response.result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        boundary::ToolContext,
        contract::{PermissionMode, Run, RunId, SessionId, ToolCall},
        tools::{exec::ExecResult, read::ReadLimits},
    };
    use serde_json::json;

    fn result() -> ToolResult {
        ToolResult {
            ok: true,
            data: Some(json!({"content":"observed"})),
            error: None,
            truncated: false,
        }
    }
    fn capture(response: &FileWorkerResponse) -> WorkerCapture {
        WorkerCapture {
            process: ExecResult {
                exit_code: Some(0),
                stdout: String::new(),
                stderr: "SYNTHETIC-RAW-STDERR".into(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                cancelled: false,
            },
            stdout: serde_json::to_vec(response).unwrap(),
        }
    }
    fn response() -> FileWorkerResponse {
        FileWorkerResponse {
            schema_version: 1,
            result: result(),
            boundary_error: None,
            observed_result: None,
        }
    }
    fn request(workspace: &Path) -> FileWorkerRequest {
        FileWorkerRequest {
            schema_version: 1,
            call: ToolCall {
                call_id: "file-fixture".into(),
                name: "read_file".into(),
                arguments: json!({"path":"fixture.txt"}).to_string(),
            },
            context: ToolContext {
                workspace: workspace.into(),
                run: Run {
                    id: RunId::new(),
                    session_id: SessionId::new(),
                    work_mode: WorkMode::Work,
                    permissions: PermissionMode::Manual,
                },
            },
            approved_target: workspace.join("fixture.txt"),
            read_roots: Vec::new(),
            read_limits: ReadLimits::default(),
            write_max_bytes: 1024,
        }
    }

    #[test]
    fn one_complete_response_is_accepted_without_raw_stderr() {
        assert_eq!(decode_capture(capture(&response())).unwrap(), result());
        let mut encoded = capture(&response());
        encoded.stdout.extend_from_slice(b"\n{}");
        assert_eq!(
            decode_capture(encoded).unwrap_err().0.code,
            "outcome_unknown"
        );
    }

    #[test]
    fn partial_invalid_utf8_schema_and_result_shapes_are_unknown() {
        for wire in [b"{".as_slice(), b"\xff", b"null", b"{}"] {
            let mut invalid = capture(&response());
            invalid.stdout = wire.into();
            let error = decode_capture(invalid).unwrap_err();
            assert_eq!(error.0.code, "outcome_unknown");
            assert!(!error.to_string().contains("SYNTHETIC-RAW-STDERR"));
            assert!(error.2.is_none());
        }
        let mut invalid = response();
        invalid.schema_version = 2;
        assert!(decode_capture(capture(&invalid)).is_err());
        invalid = response();
        invalid.result.error = Some(ErrorInfo {
            code: "bad".into(),
            message: "bad".into(),
        });
        assert!(decode_capture(capture(&invalid)).is_err());
        invalid = response();
        invalid.observed_result = Some(result());
        assert!(decode_capture(capture(&invalid)).is_err());
    }

    #[test]
    fn process_failures_preserve_complete_observations_and_never_accept_truncated_wire() {
        for variant in 0..2 {
            let mut interrupted = capture(&response());
            if variant == 0 {
                interrupted.process.cancelled = true;
            } else {
                interrupted.process.timed_out = true;
            }
            interrupted.process.exit_code = None;
            assert_eq!(decode_capture(interrupted).unwrap(), result());
        }
        let mut abnormal = capture(&response());
        abnormal.process.exit_code = Some(74);
        let error = decode_capture(abnormal).unwrap_err();
        assert_eq!(error.0.code, "outcome_unknown");
        assert_eq!(error.2.as_deref(), Some(&result()));
        let mut truncated = capture(&response());
        truncated.process.stdout_truncated = true;
        assert!(decode_capture(truncated).unwrap_err().2.is_none());
    }

    #[test]
    fn worker_boundary_failures_preserve_only_typed_observed_results() {
        let error = ErrorInfo {
            code: "io_error".into(),
            message: "directory sync failed".into(),
        };
        let response = FileWorkerResponse {
            schema_version: 1,
            result: ToolResult {
                ok: false,
                data: None,
                error: Some(error.clone()),
                truncated: false,
            },
            boundary_error: Some(error.clone()),
            observed_result: Some(result()),
        };
        let decoded = decode_capture(capture(&response)).unwrap_err();
        assert_eq!(decoded.0, error);
        assert_eq!(decoded.2.as_deref(), Some(&result()));
    }

    #[test]
    fn mismatched_boundary_and_fallback_responses_never_claim_observed_completion() {
        let error = ErrorInfo {
            code: "io_error".into(),
            message: "file synchronization failed".into(),
        };
        let mut invalid = response();
        invalid.boundary_error = Some(error.clone());
        assert!(decode_capture(capture(&invalid)).unwrap_err().2.is_none());
        invalid.result = ToolResult {
            ok: false,
            data: None,
            error: Some(error),
            truncated: false,
        };
        invalid.boundary_error.as_mut().unwrap().code = "different".into();
        assert!(decode_capture(capture(&invalid)).unwrap_err().2.is_none());
        for code in [
            "invalid_worker_request",
            "unsupported_worker_schema",
            "unsupported_worker_tool",
            "worker_rpc_too_large",
            "worker_io_error",
        ] {
            let mut fallback = response();
            fallback.result = ToolResult {
                ok: false,
                data: None,
                error: Some(ErrorInfo {
                    code: code.into(),
                    message: "unconfirmed worker response".into(),
                }),
                truncated: false,
            };
            let failure = decode_capture(capture(&fallback)).unwrap_err();
            assert_eq!(failure.0.code, "outcome_unknown");
            assert!(failure.2.is_none());
        }
    }

    #[test]
    fn prelaunch_target_schema_and_mode_are_checked_and_wire_is_bounded() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("fixture.txt"), "fixture").unwrap();
        let mut request = request(&workspace.path().canonicalize().unwrap());
        validate_request(&request).unwrap();
        request.read_limits.read_return_bytes = 64 * 1024 * 1024;
        request.read_limits.file_bytes = 64 * 1024 * 1024;
        request.read_limits.search_bytes = 64 * 1024 * 1024;
        request.read_limits.list_bytes = 64 * 1024 * 1024;
        request.write_max_bytes = 64 * 1024 * 1024;
        validate_request(&request).unwrap();
        let encoded = encode_request(&request).unwrap();
        assert!(encoded.ends_with(b"\n"));
        let decoded: FileWorkerRequest = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.call, request.call);
        request.approved_target = request.context.workspace.join("other");
        assert_eq!(
            validate_request(&request).unwrap_err().0.code,
            "stale_approval"
        );
        request.schema_version = 2;
        assert_eq!(
            validate_request(&request).unwrap_err().0.code,
            "unsupported_worker_schema"
        );
        request.schema_version = 1;
        request.call.name = "write_file".into();
        request.context.run.work_mode = WorkMode::Plan;
        assert_eq!(
            validate_request(&request).unwrap_err().0.code,
            "mode_denied"
        );
        let mut buffer = RequestBuffer(vec![b' '; WORKER_RPC_MAX_BYTES - 2]);
        assert!(buffer.write_all(b"xx").is_err());
        assert_eq!(buffer.0.len(), WORKER_RPC_MAX_BYTES - 2);
    }
}
