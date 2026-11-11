mod support;
use saya_core::{
    boundary::Tool,
    tools::read::{ReadLimits, ReadTool, ReadToolKind, ReadTools},
};
use std::{fs, path::Path, sync::Arc};

#[test]
fn search_definitions_require_directory_roots_and_describe_patterns()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let files = Arc::new(tools(temp.path())?);
    for kind in [ReadToolKind::FindFiles, ReadToolKind::SearchText] {
        let definition = ReadTool::new(files.clone(), kind).definition();
        let path = &definition.input_schema["properties"]["path"]["description"];
        assert!(
            path.as_str()
                .is_some_and(|text| text.contains("Directory root"))
        );
    }
    let read = ReadTool::new(files.clone(), ReadToolKind::ReadFile).definition();
    assert!(
        read.description
            .contains("use search_text before read_file")
    );
    assert!(
        read.input_schema["properties"]["line_count"]["description"]
            .as_str()
            .is_some_and(|text| text.contains("do not use large pages"))
    );
    let search = ReadTool::new(files, ReadToolKind::SearchText).definition();
    let properties = &search.input_schema["properties"];
    assert!(
        properties["pattern"]["description"]
            .as_str()
            .is_some_and(|text| text.contains("Literal text"))
    );
    Ok(())
}

fn tools(root: &Path) -> Result<ReadTools, Box<dyn std::error::Error>> {
    Ok(ReadTools::new(root, &[], false, ReadLimits::default())?)
}

#[test]
fn read_file_has_complete_hash_pagination_and_unicode_safe_truncation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    fs::write(temp.path().join("text.txt"), "first\n你好\nlast\n")?;
    let files = tools(temp.path())?;
    let page = files.read_file(Path::new("text.txt"), 2, 1)?;
    assert_eq!(page.content, "你好\n");
    assert_eq!(
        (page.start_line, page.end_line, page.truncated),
        (2, 2, true)
    );
    assert_eq!(
        page.version,
        files.read_file(Path::new("text.txt"), 1, 99)?.version
    );
    let limits = ReadLimits {
        read_return_bytes: 4,
        ..ReadLimits::default()
    };
    let capped =
        ReadTools::new(temp.path(), &[], false, limits)?.read_file(Path::new("text.txt"), 2, 10)?;
    assert_eq!(capped.content, "你");
    assert!(capped.truncated);
    assert_eq!(
        files
            .read_file(Path::new("text.txt"), 0, 1)
            .expect_err("zero line")
            .code,
        "invalid_args"
    );
    Ok(())
}

#[test]
fn rejects_binary_oversize_and_outside_roots() -> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    let outside = support::temp_workspace()?;
    fs::write(root.path().join("binary"), [0xff, 0])?;
    fs::write(root.path().join("large"), "12345")?;
    fs::write(outside.path().join("secret"), "secret")?;
    let limits = ReadLimits {
        file_bytes: 4,
        ..ReadLimits::default()
    };
    let files = ReadTools::new(root.path(), &[], false, limits)?;
    assert_eq!(
        files
            .read_file(Path::new("binary"), 1, 1)
            .expect_err("binary")
            .code,
        "invalid_text"
    );
    assert_eq!(
        files
            .read_file(Path::new("large"), 1, 1)
            .expect_err("oversize")
            .code,
        "file_too_large"
    );
    assert_eq!(
        files
            .read_file(&outside.path().join("secret"), 1, 1)
            .expect_err("outside")
            .code,
        "path_denied"
    );
    let extended = ReadTools::new(
        root.path(),
        &[outside.path().to_path_buf()],
        false,
        ReadLimits::default(),
    )?;
    assert_eq!(
        extended
            .read_file(&outside.path().join("secret"), 1, 1)?
            .content,
        "secret"
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("escape"))?;
        assert_eq!(
            files
                .read_file(Path::new("escape"), 1, 1)
                .expect_err("symlink escape")
                .code,
            "path_denied"
        );
    }
    Ok(())
}

#[test]
fn search_obeys_ignore_hidden_limits_and_stable_order() -> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    fs::write(root.path().join(".gitignore"), "ignored.txt\n")?;
    fs::write(root.path().join("ignored.txt"), "needle ignored")?;
    fs::write(root.path().join(".hidden"), "needle hidden")?;
    fs::create_dir(root.path().join("target"))?;
    fs::write(root.path().join("target/generated.txt"), "needle generated")?;
    fs::write(root.path().join("b.txt"), "needle b\n")?;
    fs::write(root.path().join("a.txt"), "needle a\n")?;
    let files = tools(root.path())?;
    let found = files.find_files(Path::new("."), "*.txt", false)?;
    let names: Vec<_> = found
        .items
        .iter()
        .filter_map(|p| p.file_name().and_then(|s| s.to_str()))
        .collect();
    assert_eq!(names, ["a.txt", "b.txt"]);
    let hits = files.search_text(Path::new("."), "needle [ab]", true, false)?;
    assert_eq!(hits.items.len(), 2);
    assert!(hits.items[0].path.ends_with("a.txt"));
    assert!(hits.items[1].path.ends_with("b.txt"));
    assert!(files.find_files(Path::new("."), "*.txt", true)?.items.len() >= 4);
    let limits = ReadLimits {
        search_results: 1,
        list_entries: 1,
        ..ReadLimits::default()
    };
    let capped = ReadTools::new(root.path(), &[], false, limits)?;
    assert!(
        capped
            .search_text(Path::new("."), "needle", false, false)?
            .truncated
    );
    let list = capped.list_dir(Path::new("."))?;
    assert_eq!(list.items.len(), 1);
    assert!(list.truncated);
    Ok(())
}

#[tokio::test]
async fn adapter_enforces_context_and_returns_structured_results()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{
        boundary::{Cancellation, Tool, ToolContext},
        contract::{PermissionMode, Run, RunId, SessionId, ToolCall, WorkMode},
        tools::read::{ReadTool, ReadToolKind},
    };
    use std::{
        future::{Future, pending},
        pin::Pin,
        sync::Arc,
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
    let root = support::temp_workspace()?;
    fs::write(root.path().join("a.txt"), "hello\n")?;
    let files = Arc::new(tools(root.path())?);
    let tool = ReadTool::new(files, ReadToolKind::ReadFile);
    let run = Run {
        id: RunId::new(),
        session_id: SessionId::new(),
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    let context = ToolContext {
        run: run.clone(),
        workspace: root.path().to_path_buf(),
    };
    let mut call = ToolCall {
        call_id: "1".into(),
        name: "read_file".into(),
        arguments: r#"{"path":"a.txt","start_line":1,"line_count":1}"#.into(),
    };
    let result = tool.execute(&call, &context, &NeverCancel).await?;
    assert!(result.ok);
    assert_eq!(
        result
            .data
            .as_ref()
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_str()),
        Some("hello\n")
    );
    call.arguments = r#"{"path":"a.txt","start_line":1,"line_count":1,"bypass":true}"#.into();
    let rejected = tool.execute(&call, &context, &NeverCancel).await?;
    assert!(!rejected.ok);
    assert_eq!(
        rejected.error.as_ref().map(|e| e.code.as_str()),
        Some("invalid_args")
    );
    let unrestricted = Arc::new(ReadTools::new(
        root.path(),
        &[],
        true,
        ReadLimits::default(),
    )?);
    let tool = ReadTool::new(unrestricted, ReadToolKind::ReadFile);
    let rejected = tool.execute(&call, &context, &NeverCancel).await?;
    assert_eq!(
        rejected.error.as_ref().map(|e| e.code.as_str()),
        Some("path_denied")
    );
    Ok(())
}

#[test]
fn search_does_not_follow_symlinks_or_return_overlong_lines()
-> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    let external = support::temp_workspace()?;
    fs::write(external.path().join("secret.txt"), "needle secret")?;
    fs::write(
        root.path().join("large.txt"),
        format!("needle {}", "x".repeat(1024)),
    )?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(external.path(), root.path().join("link"))?;
    let files = ReadTools::new(
        root.path(),
        &[],
        false,
        ReadLimits {
            search_bytes: 64,
            ..ReadLimits::default()
        },
    )?;
    let result = files.search_text(Path::new("."), "needle", false, true)?;
    assert!(result.truncated);
    assert!(result.items.is_empty());
    assert!(
        !files
            .find_files(Path::new("."), "*.txt", true)?
            .items
            .iter()
            .any(|p| p.starts_with(external.path()))
    );
    assert_eq!(
        files
            .search_text(Path::new("."), "[", true, false)
            .expect_err("invalid regex")
            .code,
        "invalid_args"
    );
    Ok(())
}

#[tokio::test]
async fn adapter_stops_when_cancellation_arrives() -> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{
        boundary::{Cancellation, Tool, ToolContext},
        contract::{PermissionMode, Run, RunId, SessionId, ToolCall, WorkMode},
        tools::read::{ReadTool, ReadToolKind},
    };
    use std::{future::Future, pin::Pin, sync::Arc};
    struct CancelImmediately;
    impl Cancellation for CancelImmediately {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async {})
        }
    }
    let root = support::temp_workspace()?;
    fs::write(root.path().join("a.txt"), "hello")?;
    let tool = ReadTool::new(Arc::new(tools(root.path())?), ReadToolKind::ReadFile);
    let context = ToolContext {
        run: Run {
            id: RunId::new(),
            session_id: SessionId::new(),
            work_mode: WorkMode::Work,
            permissions: PermissionMode::Auto,
        },
        workspace: root.path().to_path_buf(),
    };
    let call = ToolCall {
        call_id: "1".into(),
        name: "read_file".into(),
        arguments: r#"{"path":"a.txt","start_line":1,"line_count":1}"#.into(),
    };
    assert_eq!(
        tool.execute(&call, &context, &CancelImmediately)
            .await
            .expect_err("must cancel")
            .0
            .code,
        "cancelled"
    );
    Ok(())
}

#[test]
fn bypass_can_read_outside_the_workspace_and_list_limit_is_stable()
-> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    let outside = support::temp_workspace()?;
    fs::write(outside.path().join("item"), "hello\n")?;
    for name in ["z", "b", "x", "a"] {
        fs::write(root.path().join(name), name)?;
    }
    let limits = ReadLimits {
        list_entries: 2,
        ..ReadLimits::default()
    };
    let files = ReadTools::new(root.path(), &[], true, limits)?;
    let value = files.read_file(&outside.path().join("item"), 1, 1)?;
    assert_eq!(
        value.version,
        "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
    );
    let list = files.list_dir(Path::new("."))?;
    assert_eq!(
        list.items
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(list.truncated);
    Ok(())
}

#[test]
fn nested_gitignore_negation_and_ignored_parent_are_respected()
-> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    fs::write(root.path().join(".gitignore"), "*.txt\nignored/\n")?;
    fs::create_dir(root.path().join("src"))?;
    fs::write(root.path().join("src/.gitignore"), "!keep.txt\n")?;
    fs::write(root.path().join("src/keep.txt"), "keep")?;
    fs::write(root.path().join("src/drop.txt"), "drop")?;
    fs::create_dir(root.path().join("ignored"))?;
    fs::write(root.path().join("ignored/.gitignore"), "!secret.txt\n")?;
    fs::write(root.path().join("ignored/secret.txt"), "secret")?;
    let result = tools(root.path())?.find_files(Path::new("."), "*.txt", false)?;
    assert_eq!(
        result.items,
        vec![root.path().join("src/keep.txt").canonicalize()?]
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn gitignore_symlink_cannot_read_outside_allowed_roots() -> Result<(), Box<dyn std::error::Error>> {
    let root = support::temp_workspace()?;
    let external = support::temp_workspace()?;
    fs::write(external.path().join("private"), "secret.txt\n")?;
    std::os::unix::fs::symlink(
        external.path().join("private"),
        root.path().join(".gitignore"),
    )?;
    fs::write(root.path().join("visible.txt"), "text")?;
    let error = tools(root.path())?
        .find_files(Path::new("."), "*.txt", false)
        .expect_err("external gitignore must not be read");
    assert_eq!(error.code, "path_denied");
    Ok(())
}
