use saya_core::project_context::ProjectContext;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

fn write(root: &Path, rel: &str, text: &str) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}
fn skill(name: &str, description: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\nallowed-tools: all\n---\n{body}\n")
}
#[test]
fn instruction_scope_and_skill_precedence_are_deterministic() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let ws = tmp.path().join("workspace");
    fs::create_dir_all(ws.join("src/nested")).unwrap();
    fs::create_dir_all(&home).unwrap();
    write(&home, ".agents/AGENTS.md", "global agents");
    write(&home, ".saya/AGENTS.md", "global saya");
    write(&ws, "AGENTS.md", "root");
    write(&ws, ".agents/AGENTS.md", "workspace agents");
    write(&ws, ".saya/AGENTS.md", "workspace saya");
    write(&ws, "src/AGENTS.md", "src rules");
    write(&ws, "src/nested/AGENTS.md", "nested rules");
    write(
        &home,
        ".agents/skills/example/SKILL.md",
        &skill("example", "global agent", "global body"),
    );
    write(
        &home,
        ".saya/skills/example/SKILL.md",
        &skill("example", "global saya", "global body"),
    );
    write(
        &ws,
        ".agents/skills/example/SKILL.md",
        &skill("example", "workspace agent", "workspace body"),
    );
    write(
        &ws,
        ".saya/skills/example/SKILL.md",
        &skill("example", "workspace saya", "workspace body"),
    );
    write(
        &home,
        ".agents/skills/other/SKILL.md",
        &skill("other", "another skill", "do not inject this body"),
    );
    write(
        &ws,
        ".agents/skills/bad/SKILL.md",
        &skill("wrong", "bad name", "body"),
    );
    let context = ProjectContext::new(&ws, Some(&home)).unwrap();
    let snapshot = context
        .snapshot(Some(Path::new("src/nested/new.rs")))
        .unwrap();
    assert_eq!(
        snapshot
            .instructions
            .iter()
            .map(|v| v.content.as_str())
            .collect::<Vec<_>>(),
        [
            "global agents",
            "global saya",
            "root",
            "workspace agents",
            "workspace saya",
            "src rules",
            "nested rules"
        ]
    );
    assert_eq!(snapshot.skills.len(), 2);
    assert_eq!(snapshot.skills[0].name, "example");
    assert_eq!(snapshot.skills[0].description, "workspace saya");
    assert_eq!(snapshot.skills[0].scope, "workspace");
    assert_eq!(snapshot.skills[1].name, "other");
    assert_eq!(snapshot.skills[1].scope, "user");
    let serialized = serde_json::to_string(&snapshot).unwrap();
    assert!(!serialized.contains("do not inject this body"));
    assert!(!serialized.contains("allowed-tools"));
    let root = context.snapshot(None).unwrap();
    assert_eq!(root.instructions.len(), 5);
    assert_ne!(root.fingerprint().unwrap(), snapshot.fingerprint().unwrap());
}
#[test]
fn changed_and_deleted_instructions_are_refreshed_and_hashed() {
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path();
    let p = write(ws, "AGENTS.md", "before");
    let ctx = ProjectContext::new(ws, None).unwrap();
    let first = ctx.snapshot(None).unwrap();
    fs::write(&p, "after").unwrap();
    let second = ctx.snapshot(None).unwrap();
    assert_ne!(first.fingerprint().unwrap(), second.fingerprint().unwrap());
    assert_ne!(first.instructions[0].sha256, second.instructions[0].sha256);
    fs::remove_file(p).unwrap();
    assert!(ctx.snapshot(None).unwrap().instructions.is_empty());
}
#[test]
fn outside_target_does_not_scan_ancestor_instructions() {
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("work");
    fs::create_dir_all(&ws).unwrap();
    write(tmp.path(), "AGENTS.md", "outside");
    write(&ws, "AGENTS.md", "inside");
    let ctx = ProjectContext::new(&ws, None).unwrap();
    let snap = ctx.snapshot(Some(&tmp.path().join("outside.txt"))).unwrap();
    assert_eq!(snap.instructions.len(), 1);
    assert_eq!(snap.instructions[0].content, "inside");
}
#[cfg(unix)]
#[test]
fn symlinks_cannot_escape_workspace_or_home_capabilities() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("work");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&ws).unwrap();
    fs::create_dir_all(&outside).unwrap();
    write(&outside, "AGENTS.md", "must not leak");
    symlink(outside.join("AGENTS.md"), ws.join("AGENTS.md")).unwrap();
    let error = ProjectContext::new(&ws, None)
        .unwrap()
        .snapshot(None)
        .unwrap_err();
    assert!(!error.0.message.contains("must not leak"));
}
#[cfg(unix)]
#[test]
fn nonregular_instruction_fails_without_blocking() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("AGENTS.md");
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is a valid NUL-terminated temporary pathname.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let error = ProjectContext::new(tmp.path(), None)
        .unwrap()
        .snapshot(None)
        .unwrap_err();
    assert_eq!(error.0.code, "project_context_error");
}

#[test]
fn multiline_yaml_metadata_is_supported_and_aliases_are_skipped() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        ".agents/skills/multi/SKILL.md",
        "---\nname: multi\ndescription: >-\n  Multi line\n  description.\n---\nbody\n",
    );
    write(
        root,
        ".agents/skills/alias/SKILL.md",
        "---\nname: alias\ndescription: &shared hello\nmetadata:\n  nested: *shared\n---\nbody\n",
    );
    let snap = ProjectContext::new(root, None)
        .unwrap()
        .snapshot(None)
        .unwrap();
    assert_eq!(snap.skills.len(), 1);
    assert_eq!(snap.skills[0].name, "multi");
    assert_eq!(snap.skills[0].description, "Multi line description.");
}
#[cfg(unix)]
#[test]
fn skill_entry_cannot_follow_symlink_outside_its_discovery_root() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join(".agents/skills")).unwrap();
    write(
        root,
        "other/project/SKILL.md",
        &skill("linked", "must not read", "body"),
    );
    symlink(
        root.join("other/project"),
        root.join(".agents/skills/linked"),
    )
    .unwrap();
    assert!(
        ProjectContext::new(root, None)
            .unwrap()
            .snapshot(None)
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn opened_roots_remain_capability_bound_when_path_is_replaced() {
    use std::os::unix::fs::symlink;
    let temp = TempDir::new().unwrap();
    let workspace = temp.path().join("work");
    let original = temp.path().join("original");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();
    write(&workspace, "AGENTS.md", "ORIGINAL");
    write(&outside, "AGENTS.md", "UNRELATED");
    let context = ProjectContext::new(&workspace, None).unwrap();
    fs::rename(&workspace, &original).unwrap();
    symlink(&outside, &workspace).unwrap();
    let snapshot = context.snapshot(None).unwrap();
    assert_eq!(snapshot.instructions.len(), 1);
    assert_eq!(snapshot.instructions[0].content, "ORIGINAL");
}

#[test]
fn workspace_equal_to_home_loads_each_source_once() {
    let temp = TempDir::new().unwrap();
    write(temp.path(), ".agents/AGENTS.md", "rule");
    let snapshot = ProjectContext::new(temp.path(), Some(temp.path()))
        .unwrap()
        .snapshot(None)
        .unwrap();
    assert_eq!(snapshot.instructions.len(), 1);
    assert_eq!(snapshot.instructions[0].scope, "workspace");
}

#[test]
fn relative_home_cannot_redirect_global_discovery_into_the_workspace() {
    let temp = TempDir::new().unwrap();
    assert!(ProjectContext::new(temp.path(), Some(Path::new("relative"))).is_err());
}
