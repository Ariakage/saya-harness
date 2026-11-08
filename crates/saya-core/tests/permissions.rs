mod support;
use saya_core::{
    contract::{PermissionMode, WorkMode},
    permissions::{Decision, Operation, PermissionConfig, Policy},
    tools::exec::ExecPlan,
};
use std::{path::Path, time::Duration};

fn empty(root: &Path) -> Result<Policy, Box<dyn std::error::Error>> {
    Ok(Policy::new(root, PermissionConfig::from_toml("")?)?)
}
fn exec(root: &Path) -> Result<Operation, Box<dyn std::error::Error>> {
    let exe = std::fs::canonicalize("/bin/echo")?;
    Ok(Operation::exec(&ExecPlan {
        argv: vec![exe.to_string_lossy().into_owned(), "hello".into()],
        cwd: root.to_path_buf(),
        timeout: Duration::from_secs(10),
        environment_digest: "test-environment".into(),
    })?)
}
#[test]
fn normalized_arguments_sort_objects_recursively_independent_of_json_features()
-> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    let operation = Operation::file(root.path(), "write_file", Path::new("out.txt"))?;
    let first = serde_json::from_str(
        r#"{"path":"out.txt","content":"hello","metadata":{"z":1,"a":[{"y":2,"b":3}]}}"#,
    )?;
    let second = serde_json::from_str(
        r#"{"metadata":{"a":[{"b":3,"y":2}],"z":1},"content":"hello","path":"out.txt"}"#,
    )?;
    assert_eq!(
        operation.normalized_arguments(first)?.to_string(),
        operation.normalized_arguments(second)?.to_string()
    );
    Ok(())
}

#[test]
fn exec_environment_overrides_retain_hard_restrictions_and_rule_precedence()
-> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    let operation = exec(root.path())?;
    let arguments =
        serde_json::json!({"env":{"SAYA_APPROVAL_TEST":{"source":"literal","value":"different"}}});
    for effect in ["allow", "ask", "deny"] {
        let rules = format!("[[rules]]\neffect='{effect}'\ntool='exec'\n");
        let policy = Policy::new(root.path(), PermissionConfig::from_toml(&rules)?)?;
        for permissions in [
            PermissionMode::Auto,
            PermissionMode::Manual,
            PermissionMode::Bypass,
        ] {
            assert_eq!(
                policy
                    .evaluate_with_arguments(WorkMode::Plan, permissions, &operation, &arguments)
                    .decision,
                Decision::Deny
            );
            let expected = match (effect, permissions) {
                ("deny", _) => Decision::Deny,
                (_, PermissionMode::Bypass) => Decision::Allow,
                ("ask", _) | (_, PermissionMode::Manual) => Decision::Ask,
                _ => Decision::ModelReview,
            };
            assert_eq!(
                policy
                    .evaluate_with_arguments(WorkMode::Work, permissions, &operation, &arguments)
                    .decision,
                expected,
                "{effect}: {permissions:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn all_modes_and_permissions_enforce_plan_and_defaults() -> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    std::fs::write(root.path().join("a.txt"), "text")?;
    let policy = empty(root.path())?;
    let read = Operation::file(root.path(), "read_file", Path::new("a.txt"))?;
    let write = Operation::file(root.path(), "write_file", Path::new("new.txt"))?;
    let command = exec(root.path())?;
    let input = Operation::ask_user();
    for mode in [
        WorkMode::Plan,
        WorkMode::Build,
        WorkMode::Code,
        WorkMode::Work,
    ] {
        for permission in [
            PermissionMode::Auto,
            PermissionMode::Bypass,
            PermissionMode::Manual,
        ] {
            assert_eq!(
                policy.evaluate(mode, permission, &read).decision,
                if permission == PermissionMode::Manual {
                    Decision::Ask
                } else {
                    Decision::Allow
                }
            );
            assert_eq!(
                policy.evaluate(mode, permission, &input).decision,
                if permission == PermissionMode::Manual {
                    Decision::Ask
                } else {
                    Decision::Allow
                }
            );
            let expected_write = if mode == WorkMode::Plan {
                Decision::Deny
            } else if permission == PermissionMode::Manual {
                Decision::Ask
            } else {
                Decision::Allow
            };
            let expected_exec = if mode == WorkMode::Plan {
                Decision::Deny
            } else {
                match permission {
                    PermissionMode::Auto => Decision::ModelReview,
                    PermissionMode::Manual => Decision::Ask,
                    PermissionMode::Bypass => Decision::Allow,
                }
            };
            assert_eq!(
                policy.evaluate(mode, permission, &write).decision,
                expected_write
            );
            assert_eq!(
                policy.evaluate(mode, permission, &command).decision,
                expected_exec
            );
        }
    }
    Ok(())
}

#[test]
fn rules_have_order_independent_deny_ask_allow_precedence() -> Result<(), Box<dyn std::error::Error>>
{
    let root = support::temp_workspace()?;
    let path = root.path().canonicalize()?.join("*.txt");
    let text = format!(
        r#"
[[rules]]
effect="allow"
tool="write_file"
path_glob="{}"
[[rules]]
effect="deny"
tool="write_file"
path_glob="{}"
[[rules]]
effect="ask"
tool="write_file"
path_glob="{}"
"#,
        path.display(),
        path.display(),
        path.display()
    );
    let policy = Policy::new(root.path(), PermissionConfig::from_toml(&text)?)?;
    let operation = Operation::file(root.path(), "write_file", Path::new("a.txt"))?;
    for permission in [
        PermissionMode::Auto,
        PermissionMode::Manual,
        PermissionMode::Bypass,
    ] {
        let result = policy.evaluate(WorkMode::Work, permission, &operation);
        assert_eq!(result.decision, Decision::Deny);
        assert_eq!(result.reason, "explicit_deny");
    }
    let text = format!(
        "[[rules]]\neffect=\"allow\"\ntool=\"write_file\"\npath_glob=\"{}\"\n[[rules]]\neffect=\"ask\"\ntool=\"write_file\"\npath_glob=\"{}\"\n",
        path.display(),
        path.display()
    );
    let policy = Policy::new(root.path(), PermissionConfig::from_toml(&text)?)?;
    assert_eq!(
        policy
            .evaluate(WorkMode::Work, PermissionMode::Auto, &operation)
            .decision,
        Decision::Ask
    );
    assert_eq!(
        policy
            .evaluate(WorkMode::Work, PermissionMode::Bypass, &operation)
            .decision,
        Decision::Allow
    );
    Ok(())
}

#[test]
fn path_roots_precede_allow_and_bypass_still_obeys_deny() -> Result<(), Box<dyn std::error::Error>>
{
    let root = support::temp_workspace()?;
    let other = support::temp_workspace()?;
    let target = other.path().join("data.txt");
    std::fs::write(&target, "text")?;
    let pattern = other.path().canonicalize()?.join("*.txt");
    let text = format!(
        "[[rules]]\neffect=\"allow\"\ntool=\"read_file\"\npath_glob=\"{}\"\n",
        pattern.display()
    );
    let policy = Policy::new(root.path(), PermissionConfig::from_toml(&text)?)?;
    let operation = Operation::file(root.path(), "read_file", &target)?;
    let denied = policy.evaluate(WorkMode::Work, PermissionMode::Auto, &operation);
    assert_eq!(
        (denied.decision, denied.reason),
        (Decision::Deny, "outside_roots")
    );
    assert_eq!(
        policy
            .evaluate(WorkMode::Work, PermissionMode::Bypass, &operation)
            .decision,
        Decision::Allow
    );
    let text = format!("additional_roots=[\"{}\"]\n", other.path().display());
    let policy = Policy::new(root.path(), PermissionConfig::from_toml(&text)?)?;
    assert_eq!(
        policy
            .evaluate(WorkMode::Work, PermissionMode::Auto, &operation)
            .decision,
        Decision::Allow
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, root.path().join("link"))?;
        let operation = Operation::file(root.path(), "read_file", Path::new("link"))?;
        assert_eq!(
            empty(root.path())?
                .evaluate(WorkMode::Work, PermissionMode::Auto, &operation)
                .reason,
            "outside_roots"
        );
    }
    Ok(())
}

#[test]
fn rule_globs_resolve_symlinked_static_prefixes_and_future_directories()
-> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    let links = support::temp_workspace()?;
    let alias = links.path().join("workspace-alias");
    std::os::unix::fs::symlink(root.path(), &alias)?;
    let pattern = alias.join("future/*.txt");
    let quoted = serde_json::to_string(pattern.to_str().ok_or("glob UTF-8")?)?;
    for effect in ["deny", "ask"] {
        let config = PermissionConfig::from_toml(&format!(
            "[[rules]]\neffect='{effect}'\ntool='read_file'\npath_glob={quoted}\n"
        ))?;
        let policy = Policy::new(&alias, config)?;
        std::fs::create_dir_all(root.path().join("future"))?;
        std::fs::write(root.path().join("future/private.txt"), "private")?;
        let operation = Operation::file(&alias, "read_file", Path::new("future/private.txt"))?;
        assert_eq!(
            policy
                .evaluate(WorkMode::Work, PermissionMode::Auto, &operation)
                .decision,
            if effect == "deny" {
                Decision::Deny
            } else {
                Decision::Ask
            }
        );
        if effect == "deny" {
            assert_eq!(
                policy
                    .evaluate(WorkMode::Work, PermissionMode::Bypass, &operation)
                    .decision,
                Decision::Deny
            );
        }
        std::fs::remove_dir_all(root.path().join("future"))?;
    }
    Ok(())
}

#[test]
fn canonical_glob_prefixes_keep_literal_metacharacters_and_reject_parent_components()
-> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    let links = support::temp_workspace()?;
    for (index, directory) in ["literal[dir]", "literal\\dir"].into_iter().enumerate() {
        let real = root.path().join(directory);
        std::fs::create_dir(&real)?;
        std::fs::write(real.join("secret.txt"), "private")?;
        let alias = links.path().join(format!("alias-{index}"));
        std::os::unix::fs::symlink(&real, &alias)?;
        let quoted = serde_json::to_string(alias.join("**").to_str().ok_or("glob UTF-8")?)?;
        let policy = Policy::new(
            &alias,
            PermissionConfig::from_toml(&format!(
                "[[rules]]\neffect='deny'\npath_glob={quoted}\n"
            ))?,
        )?;
        let operation = Operation::file(&alias, "read_file", Path::new("secret.txt"))?;
        assert_eq!(
            policy
                .evaluate(WorkMode::Work, PermissionMode::Bypass, &operation)
                .decision,
            Decision::Deny
        );
    }
    let quoted = serde_json::to_string(
        root.path()
            .join("*/../secret.txt")
            .to_str()
            .ok_or("glob UTF-8")?,
    )?;
    assert!(
        Policy::new(
            root.path(),
            PermissionConfig::from_toml(&format!(
                "[[rules]]\neffect='deny'\npath_glob={quoted}\n"
            ))?
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn executable_and_argv_prefix_must_all_match_and_review_still_asks()
-> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    let exe = std::fs::canonicalize("/bin/echo")?;
    let text = format!(
        "[[rules]]\neffect=\"allow\"\ntool=\"exec\"\nexecutable=\"{}\"\nargs_prefix=[\"hello\"]\n",
        exe.display()
    );
    let policy = Policy::new(root.path(), PermissionConfig::from_toml(&text)?)?;
    let op = exec(root.path())?;
    assert_eq!(
        policy
            .evaluate(WorkMode::Work, PermissionMode::Auto, &op)
            .decision,
        Decision::Allow
    );
    assert_eq!(
        policy
            .evaluate(WorkMode::Work, PermissionMode::Manual, &op)
            .decision,
        Decision::Ask
    );
    assert_eq!(
        policy
            .evaluate(WorkMode::Plan, PermissionMode::Bypass, &op)
            .decision,
        Decision::Deny
    );
    Ok(())
}

#[test]
fn invalid_config_or_unknown_tool_never_falls_through() -> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    assert!(PermissionConfig::from_toml("unknown=true").is_err());
    assert!(
        Policy::new(
            root.path(),
            PermissionConfig::from_toml(
                "[[rules]]\neffect='allow'\ntool='write_file'\nargs_prefix=['x']"
            )?
        )
        .is_err()
    );
    assert!(
        Policy::new(
            root.path(),
            PermissionConfig::from_toml("[[rules]]\neffect='allow'\npath_glob='relative/*'")?
        )
        .is_err()
    );
    assert!(Operation::file(root.path(), "unknown", Path::new("x")).is_err());
    Ok(())
}

#[test]
fn gate_never_treats_ask_or_model_review_as_authorization() -> Result<(), Box<dyn std::error::Error>>
{
    use saya_core::{
        boundary::ToolContext,
        contract::{Run, RunId, SessionId, ToolCall},
        permissions::PolicyGate,
        tools::{exec::ExecGate, write::WriteGate},
    };
    use std::sync::Arc;
    let root = support::temp_workspace()?;
    let gate = PolicyGate::new(Arc::new(empty(root.path())?));
    let run = Run {
        id: RunId::new(),
        session_id: SessionId::new(),
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    let mut context = ToolContext {
        run,
        workspace: root.path().into(),
    };
    let file = ToolCall {
        call_id: "w".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path":"a.txt","content":"x","expected_version":null})
            .to_string(),
    };
    assert!(WriteGate::authorize(
        &gate,
        &file,
        &context,
        &root.path().join("a.txt")
    )?);
    context.run.permissions = PermissionMode::Manual;
    assert!(!WriteGate::authorize(
        &gate,
        &file,
        &context,
        &root.path().join("a.txt")
    )?);
    context.run.permissions = PermissionMode::Bypass;
    context.run.work_mode = WorkMode::Plan;
    assert!(!WriteGate::authorize(
        &gate,
        &file,
        &context,
        &root.path().join("a.txt")
    )?);
    context.run.work_mode = WorkMode::Work;
    context.run.permissions = PermissionMode::Auto;
    let exe = std::fs::canonicalize("/bin/echo")?;
    let plan = ExecPlan {
        argv: vec![exe.to_string_lossy().into_owned(), "hello".into()],
        cwd: root.path().canonicalize()?,
        timeout: Duration::from_secs(1),
        environment_digest: "test-environment".into(),
    };
    let command = ToolCall {
        call_id: "c".into(),
        name: "exec".into(),
        arguments: serde_json::json!({"argv":["echo","hello"]}).to_string(),
    };
    assert!(!ExecGate::authorize(&gate, &plan, &command, &context)?);
    context.run.permissions = PermissionMode::Bypass;
    assert!(ExecGate::authorize(&gate, &plan, &command, &context)?);
    Ok(())
}
