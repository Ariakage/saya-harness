use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_saya"));
    command
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root);
    command
}

#[test]
fn implicit_tui_rejects_nonterminal_input_before_opening_configuration_or_storage() {
    let root = tempfile::tempdir().unwrap();
    for piped in [false, true] {
        let mut child = command(root.path())
            .env("SAYA_LOG_LEVEL", "invalid-level")
            .args(["--config", "missing.toml"])
            .stdin(if piped { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(mut input) = child.stdin.take()
            && let Err(error) = input.write_all(b"this must not become a task\n")
        {
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("TUI requires terminal stdin and stdout"));
        assert!(error.contains("saya run --stdin"));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[test]
fn implicit_machine_output_requires_an_explicit_subcommand_without_stdout_pollution() {
    let root = tempfile::tempdir().unwrap();
    for output in ["json", "jsonl"] {
        let result = command(root.path())
            .env("SAYA_LOG_LEVEL", "invalid-level")
            .args(["--output", output, "--config", "missing.toml"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        assert!(
            String::from_utf8(result.stderr)
                .unwrap()
                .contains("Choose an explicit CLI subcommand")
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[test]
fn help_and_version_are_available_without_a_terminal_or_session_side_effects() {
    let root = tempfile::tempdir().unwrap();
    for argument in ["--help", "--version"] {
        let result = command(root.path())
            .arg(argument)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        assert!(!result.stdout.is_empty());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
