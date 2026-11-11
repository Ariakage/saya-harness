use super::*;
use crate::{
    plugin_manifest::{PackageSource, SourceKind, inspect_package},
    plugin_store::{CleanupOutcome, PluginStore, StoreLimits},
    project_context::ProjectContext,
};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    home: PathBuf,
    secrets: Arc<SecretRegistry>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(root.path()).unwrap();
        let workspace = canonical.join("workspace");
        let home = canonical.join("home");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&home).unwrap();
        Self {
            root,
            workspace,
            home,
            secrets: Arc::new(SecretRegistry::default()),
        }
    }
    fn skill(&self, root: &Path, namespace: &str, name: &str, body: &str) -> PathBuf {
        let directory = root.join(namespace).join("skills").join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: A bounded test Skill.\n---\n{body}"),
        )
        .unwrap();
        directory
    }
    fn snapshot(&self) -> project_context::Snapshot {
        ProjectContext::new(&self.workspace, Some(&self.home))
            .unwrap()
            .snapshot(None)
            .unwrap()
    }
    fn manager(&self) -> SkillManager {
        SkillManager::new(
            &self.workspace,
            Some(&self.home),
            self.secrets.clone(),
            SkillLimits::default(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fn writable(path: &Path) {
            let Ok(metadata) = fs::symlink_metadata(path) else {
                return;
            };
            if metadata.is_dir() {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        writable(&entry.path());
                    }
                }
            }
        }
        writable(self.root.path());
    }
}
fn stop() -> AtomicBool {
    AtomicBool::new(false)
}
fn activate(manager: &mut SkillManager, name: &str) -> ActiveSkill {
    let metadata = manager
        .catalog()
        .unwrap()
        .into_iter()
        .find(|metadata| metadata.name == name)
        .unwrap();
    let selected = manager
        .resolve(name, &metadata.source_sha256, None)
        .unwrap();
    manager.activate(&selected, &stop()).unwrap()
}

#[test]
fn discovery_winners_are_reused_and_source_override_invalidates_even_identical_documents() {
    let fixture = Fixture::new();
    fixture.skill(&fixture.home, ".agents", "review", "home agents");
    fixture.skill(&fixture.home, ".saya", "review", "home saya");
    let fallback = fixture.skill(
        &fixture.workspace,
        ".agents",
        "review",
        "same workspace text",
    );
    let winner = fixture.skill(&fixture.workspace, ".saya", "review", "same workspace text");
    let mut manager = fixture.manager();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let metadata = manager.catalog().unwrap().remove(0);
    let selected = manager
        .resolve("review", &metadata.source_sha256, None)
        .unwrap();
    match selected.target() {
        SkillAccessTarget::ProjectFile {
            path,
            catalog_root,
            skill_directory,
        } => {
            assert_eq!(path, &winner.join("SKILL.md"));
            assert_eq!(catalog_root.scope, "workspace");
            assert_eq!(skill_directory, &winner);
        }
        _ => panic!("project source"),
    }
    let active = manager.activate(&selected, &stop()).unwrap();
    assert!(active.document.starts_with("---\n"));
    assert!(active.document.contains("same workspace text"));
    fs::remove_file(winner.join("SKILL.md")).unwrap();
    let changed = manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    assert_eq!(changed.invalidated.len(), 1);
    assert!(manager.revalidate(&stop()).unwrap().skills.is_empty());
    assert_eq!(
        manager.activate(&selected, &stop()).unwrap_err().0.code,
        "skill_source_changed"
    );
    let current = activate(&mut manager, "review");
    assert_eq!(
        current.metadata.source_sha256,
        active.metadata.source_sha256
    );
    match current.metadata.origin {
        SkillOrigin::Project { path, .. } => assert_eq!(path, fallback.join("SKILL.md")),
        _ => panic!("project source"),
    }
}

#[test]
fn user_skill_target_preserves_exact_catalog_root_and_does_not_execute_script_resources() {
    let fixture = Fixture::new();
    let directory = fixture.skill(
        &fixture.home,
        ".agents",
        "guide",
        "Read the selected resource.",
    );
    fs::write(directory.join("run.sh"), "touch SHOULD_NOT_EXIST\n").unwrap();
    let mut manager = fixture.manager();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let active = activate(&mut manager, "guide");
    let selection = manager
        .resolve("guide", &active.metadata.source_sha256, Some("run.sh"))
        .unwrap();
    match selection.target() {
        SkillAccessTarget::ProjectFile {
            catalog_root, path, ..
        } => {
            assert_eq!(catalog_root.scope, "user");
            assert_eq!(catalog_root.path, fixture.home);
            assert_eq!(path, &directory.join("run.sh"));
        }
        _ => panic!("project source"),
    }
    let resource = manager
        .read_resource(&selection, None, None, 1024, &stop())
        .unwrap();
    assert_eq!(resource.content, "touch SHOULD_NOT_EXIST\n");
    assert!(!fixture.workspace.join("SHOULD_NOT_EXIST").exists());
    assert!(!directory.join("SHOULD_NOT_EXIST").exists());
    assert!(
        manager
            .deactivate("guide", &active.metadata.source_sha256)
            .unwrap()
    );
    let error = manager
        .read_resource(&selection, None, None, 1024, &stop())
        .unwrap_err();
    assert_eq!(error.0.code, "skill_inactive");
    assert!(error.0.message.contains("skill_activate"));
}

#[test]
fn changed_document_is_removed_before_context_and_reactivation_requires_fresh_catalog() {
    let fixture = Fixture::new();
    let directory = fixture.skill(&fixture.workspace, ".saya", "guide", "first document");
    let mut manager = fixture.manager();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let first = activate(&mut manager, "guide");
    fs::write(
        directory.join("SKILL.md"),
        "---\nname: guide\ndescription: updated\n---\nchanged document",
    )
    .unwrap();
    let current = manager.revalidate(&stop()).unwrap();
    assert!(current.skills.is_empty());
    assert_eq!(current.invalidated.len(), 1);
    assert_eq!(
        current.catalog.entries[0].source_sha256,
        first.metadata.source_sha256
    );
    let stale = manager
        .resolve("guide", &first.metadata.source_sha256, None)
        .unwrap();
    assert_eq!(
        manager.activate(&stale, &stop()).unwrap_err().0.code,
        "skill_source_changed"
    );
    assert!(manager.resolve("guide", &"a".repeat(64), None).is_err());
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let new = activate(&mut manager, "guide");
    assert_ne!(first.metadata.source_sha256, new.metadata.source_sha256);
}

#[test]
fn resource_pages_are_unicode_safe_and_bound_to_the_current_redacted_view() {
    let fixture = Fixture::new();
    let directory = fixture.skill(&fixture.workspace, ".agents", "guide", "Read notes.");
    fs::write(directory.join("notes.md"), "中文🌸\ne\u{301} end\n").unwrap();
    let mut manager = fixture.manager();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let active = activate(&mut manager, "guide");
    let selected = manager
        .resolve("guide", &active.metadata.source_sha256, Some("notes.md"))
        .unwrap();
    assert_eq!(
        manager
            .read_resource(&selected, None, None, 1, &stop())
            .unwrap_err()
            .0
            .code,
        "skill_page_limit"
    );
    let first = manager
        .read_resource(&selected, None, None, 6, &stop())
        .unwrap();
    assert_eq!(first.content, "中文");
    let second = manager
        .read_resource(
            &selected,
            Some(&first.resource.source_sha256),
            first.next_cursor.as_ref(),
            64,
            &stop(),
        )
        .unwrap();
    assert!(second.content.starts_with("🌸"));
    assert!(second.next_cursor.is_none());
    let bad = ResourceCursor {
        byte_offset: 1,
        view_sha256: first.resource.view_sha256.clone(),
    };
    assert!(
        manager
            .read_resource(&selected, None, Some(&bad), 64, &stop())
            .is_err()
    );
    fixture.secrets.register(&["中文".into()]).unwrap();
    assert_eq!(
        manager
            .read_resource(&selected, None, first.next_cursor.as_ref(), 64, &stop())
            .unwrap_err()
            .0
            .code,
        "cursor_invalid"
    );
    let safe = manager
        .read_resource(&selected, None, None, 64, &stop())
        .unwrap();
    assert!(!safe.content.contains("中文"));
    assert_eq!(safe.resource.source_sha256, first.resource.source_sha256);
    assert_ne!(safe.resource.view_sha256, first.resource.view_sha256);
}

#[test]
fn changed_previously_read_resource_invalidates_activation_and_idempotent_activate_preserves_tracking()
 {
    let fixture = Fixture::new();
    let directory = fixture.skill(&fixture.workspace, ".saya", "guide", "Read notes.");
    fs::write(directory.join("notes"), "first notes").unwrap();
    let mut manager = fixture.manager();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let active = activate(&mut manager, "guide");
    let selection = manager
        .resolve("guide", &active.metadata.source_sha256, Some("notes"))
        .unwrap();
    manager
        .read_resource(&selection, None, None, 1024, &stop())
        .unwrap();
    let same = activate(&mut manager, "guide");
    assert_eq!(same.resources.len(), 1);
    fs::write(directory.join("notes"), "external updated notes").unwrap();
    let selection = manager
        .resolve("guide", &active.metadata.source_sha256, None)
        .unwrap();
    assert_eq!(
        manager.activate(&selection, &stop()).unwrap_err().0.code,
        "skill_source_changed"
    );
    assert!(manager.revalidate(&stop()).unwrap().skills.is_empty());
    let fresh = activate(&mut manager, "guide");
    let resource = manager
        .resolve("guide", &fresh.metadata.source_sha256, Some("notes"))
        .unwrap();
    assert!(
        manager
            .read_resource(&resource, None, None, 1024, &stop())
            .unwrap()
            .content
            .contains("external updated")
    );
}

#[test]
fn secrets_are_reprojected_after_registration_without_changing_actual_source_identity() {
    let fixture = Fixture::new();
    let secret = "fixture-protected-key-123456";
    fixture.skill(
        &fixture.workspace,
        ".saya",
        "guide",
        &format!("Document contains {secret} and \u{1b}[31m control."),
    );
    let mut manager = fixture.manager();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let before = activate(&mut manager, "guide");
    assert!(!before.document.contains('\u{1b}'));
    let version = manager.revalidate(&stop()).unwrap().version_sha256;
    fixture.secrets.register(&[secret.into()]).unwrap();
    let after = manager.revalidate(&stop()).unwrap();
    assert_eq!(
        after.skills[0].metadata.source_sha256,
        before.metadata.source_sha256
    );
    assert_ne!(after.skills[0].view_sha256, before.view_sha256);
    assert!(
        !serde_json::to_string(&after.context_data())
            .unwrap()
            .contains(secret)
    );
    assert_ne!(version, after.version_sha256);
}

#[test]
fn traversal_links_hardlinks_binary_and_canonical_aliases_are_rejected() {
    let fixture = Fixture::new();
    let directory = fixture.skill(&fixture.workspace, ".saya", "guide", "Read a resource.");
    fs::write(fixture.workspace.join("outside"), "outside root").unwrap();
    symlink(fixture.workspace.join("outside"), directory.join("linked")).unwrap();
    fs::hard_link(
        fixture.workspace.join("outside"),
        directory.join("hardlinked"),
    )
    .unwrap();
    fs::write(directory.join("binary"), [0u8, 255]).unwrap();
    fs::write(directory.join("caf\u{e9}.md"), "Unicode resource").unwrap();
    let mut manager = fixture.manager();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let active = activate(&mut manager, "guide");
    for path in ["../outside", "/outside", "a\\b", "a/./b", "a//b"] {
        assert!(
            manager
                .resolve("guide", &active.metadata.source_sha256, Some(path))
                .is_err(),
            "{path}"
        );
    }
    for path in ["linked", "hardlinked", "binary"] {
        let selected = manager
            .resolve("guide", &active.metadata.source_sha256, Some(path))
            .unwrap();
        assert!(
            manager
                .read_resource(&selected, None, None, 1024, &stop())
                .is_err(),
            "{path}"
        );
    }
    let selected = manager
        .resolve(
            "guide",
            &active.metadata.source_sha256,
            Some("caf\u{e9}.md"),
        )
        .unwrap();
    manager
        .read_resource(&selected, None, None, 1024, &stop())
        .unwrap();
    assert!(
        manager
            .resolve(
                "guide",
                &active.metadata.source_sha256,
                Some("cafe\u{301}.md")
            )
            .is_err()
    );
    assert!(
        manager
            .resolve("guide", &active.metadata.source_sha256, Some("CAFÉ.md"))
            .is_err()
    );
}

#[test]
fn active_budget_resource_budget_and_cancellation_preserve_consistent_state() {
    let fixture = Fixture::new();
    let first = fixture.skill(&fixture.workspace, ".saya", "first", "First body.");
    fixture.skill(&fixture.workspace, ".saya", "second", "Second body.");
    fs::write(first.join("one"), "one").unwrap();
    fs::write(first.join("two"), "two").unwrap();
    let mut manager = SkillManager::new(
        &fixture.workspace,
        Some(&fixture.home),
        fixture.secrets.clone(),
        SkillLimits {
            max_active: 1,
            max_resources_per_skill: 1,
            ..Default::default()
        },
    )
    .unwrap();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let active = activate(&mut manager, "first");
    let second = manager
        .catalog()
        .unwrap()
        .into_iter()
        .find(|entry| entry.name == "second")
        .unwrap();
    let selected = manager
        .resolve("second", &second.source_sha256, None)
        .unwrap();
    assert_eq!(
        manager.activate(&selected, &stop()).unwrap_err().0.code,
        "skill_budget"
    );
    let one = manager
        .resolve("first", &active.metadata.source_sha256, Some("one"))
        .unwrap();
    manager
        .read_resource(&one, None, None, 1024, &stop())
        .unwrap();
    let two = manager
        .resolve("first", &active.metadata.source_sha256, Some("two"))
        .unwrap();
    assert_eq!(
        manager
            .read_resource(&two, None, None, 1024, &stop())
            .unwrap_err()
            .0
            .code,
        "skill_budget"
    );
    let cancel = AtomicBool::new(true);
    assert_eq!(
        manager
            .read_resource(&one, None, None, 1024, &cancel)
            .unwrap_err()
            .0
            .code,
        "cancelled"
    );
    let selected = manager
        .resolve("first", &active.metadata.source_sha256, None)
        .unwrap();
    assert_eq!(
        manager.activate(&selected, &cancel).unwrap_err().0.code,
        "cancelled"
    );
    let current = manager.revalidate(&stop()).unwrap();
    assert_eq!(current.skills.len(), 1);
    assert_eq!(current.skills[0].resources.len(), 1);
}

#[test]
fn forged_catalog_paths_and_replaced_roots_fail_closed() {
    let fixture = Fixture::new();
    fixture.skill(&fixture.workspace, ".saya", "guide", "Local body.");
    let mut manager = fixture.manager();
    let snapshot = fixture.snapshot();
    manager.refresh(&snapshot, &[], &stop()).unwrap();
    activate(&mut manager, "guide");
    let mut forged = snapshot;
    forged.skills[0].path = fixture.workspace.join("unrelated-secret-file");
    assert!(manager.refresh(&forged, &[], &stop()).is_err());
    let moved = fixture.workspace.with_file_name("moved-workspace");
    fs::rename(&fixture.workspace, moved).unwrap();
    fs::create_dir(&fixture.workspace).unwrap();
    let current = manager.revalidate(&stop()).unwrap();
    assert!(current.skills.is_empty());
    assert_eq!(current.invalidated.len(), 1);
    assert!(current.catalog.entries.is_empty());
    assert!(manager.catalog().is_err());
}

#[test]
fn sealed_plugin_skills_are_qualified_leased_and_revoked_without_implicit_execution() {
    let fixture = Fixture::new();
    let package_root = fixture.root.path().join("package");
    fs::create_dir_all(package_root.join("skills/guide")).unwrap();
    fs::write(
        package_root.join("saya-plugin.toml"),
        r#"schema_version=1
[plugin]
id="io.saya.pkg"
version="1.0.0"
description="Sealed Skill package."
license="MIT"
[compatibility]
requires_saya=">=0.2.0, <1.0.0"
requires_features=["plugin-manifest-v1","skill-directory-v1"]
[capabilities]
requested=["workspace.read"]
[[skills]]
id="guide"
path="skills/guide"
"#,
    )
    .unwrap();
    fs::write(
        package_root.join("skills/guide/SKILL.md"),
        "A sealed plugin Skill.",
    )
    .unwrap();
    fs::write(package_root.join("skills/guide/notes"), "sealed notes").unwrap();
    let package = inspect_package(&package_root).unwrap();
    let store = PluginStore::open(
        fixture.root.path().join("plugins"),
        fixture.secrets.clone(),
        StoreLimits::default(),
    )
    .unwrap();
    store
        .install(
            &package,
            PackageSource {
                kind: SourceKind::LocalDirectory,
                reference: "./package".into(),
            },
        )
        .unwrap();
    let features = BTreeSet::from(["plugin-manifest-v1".into(), "skill-directory-v1".into()]);
    store
        .enable(
            package.digest(),
            PluginScope::User,
            crate::plugin_store::EnableGrant::requested(package.manifest()),
            "0.2.0",
            &features,
        )
        .unwrap();
    let lease = Arc::new(
        store
            .acquire(
                "io.saya.pkg",
                &fixture.workspace,
                SessionId::new(),
                RunId::new(),
                "0.2.0",
                &features,
            )
            .unwrap(),
    );
    let mut manager = fixture.manager();
    manager
        .refresh(&fixture.snapshot(), std::slice::from_ref(&lease), &stop())
        .unwrap();
    let active = activate(&mut manager, "io.saya.pkg/guide");
    assert!(
        matches!(active.metadata.origin,SkillOrigin::Plugin {ref package_digest,..} if package_digest==package.digest())
    );
    fs::write(
        package_root.join("skills/guide/notes"),
        "mutable source changed",
    )
    .unwrap();
    let selected = manager
        .resolve(
            "io.saya.pkg/guide",
            &active.metadata.source_sha256,
            Some("notes"),
        )
        .unwrap();
    assert_eq!(
        manager
            .read_resource(&selected, None, None, 1024, &stop())
            .unwrap()
            .content,
        "sealed notes"
    );
    store.disable("io.saya.pkg", None).unwrap();
    let current = manager.revalidate(&stop()).unwrap();
    assert!(current.skills.is_empty());
    assert_eq!(current.invalidated[0].reason, "plugin_revoked");
    drop(manager);
    Arc::try_unwrap(lease)
        .unwrap()
        .finish(CleanupOutcome::Cleaned)
        .unwrap();
}

#[test]
fn approval_binding_covers_every_revalidation_target_and_rejects_a_growing_target_set() {
    let fixture = Fixture::new();
    let directory = fixture.skill(&fixture.workspace, ".saya", "guide", "Read named notes.");
    fs::write(directory.join("one"), "one").unwrap();
    fs::write(directory.join("two"), "two").unwrap();
    let mut manager = fixture.manager();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let active = activate(&mut manager, "guide");
    let one = manager
        .resolve("guide", &active.metadata.source_sha256, Some("one"))
        .unwrap();
    manager
        .read_resource(&one, None, None, 1024, &stop())
        .unwrap();
    let pending = manager
        .resolve("guide", &active.metadata.source_sha256, None)
        .unwrap();
    assert_eq!(pending.targets().len(), 2);
    let two = manager
        .resolve("guide", &active.metadata.source_sha256, Some("two"))
        .unwrap();
    assert_eq!(two.targets().len(), 3);
    manager
        .read_resource(&two, None, None, 1024, &stop())
        .unwrap();
    assert_eq!(
        manager.activate(&pending, &stop()).unwrap_err().0.code,
        "skill_stale_selection"
    );
    let refreshed = manager.revalidate(&stop()).unwrap();
    assert_eq!(refreshed.skills.len(), 1);
    assert_eq!(refreshed.skills[0].resources.len(), 2);
    let current = manager
        .resolve("guide", &active.metadata.source_sha256, None)
        .unwrap();
    assert_eq!(current.targets().len(), 3);
    assert_ne!(pending.binding_sha256(), current.binding_sha256());
    manager.activate(&current, &stop()).unwrap();
}

#[test]
fn catalog_pages_are_bounded_cursor_bound_metadata_without_inactive_documents() {
    let fixture = Fixture::new();
    for index in 0..90 {
        fixture.skill(
            &fixture.workspace,
            ".saya",
            &format!("guide-{index:03}"),
            "Inactive private body.",
        );
    }
    let mut manager = fixture.manager();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let frame = manager.revalidate(&stop()).unwrap();
    assert!(frame.skills.is_empty());
    assert!(frame.catalog.entries.len() <= 64);
    assert!(frame.catalog.omitted > 0);
    assert!(serde_json::to_vec(&frame.catalog).unwrap().len() <= 16 * 1024);
    assert!(
        !frame
            .context_data()
            .to_string()
            .contains("Inactive private body.")
    );
    let first = manager.catalog_page(None, 5, 4096).unwrap();
    assert_eq!(first.entries.len(), 5);
    let cursor = first.next_cursor.unwrap();
    let next = manager.catalog_page(Some(&cursor), 5, 4096).unwrap();
    assert_eq!(next.offset, 5);
    assert_ne!(first.entries[0].name, next.entries[0].name);
    fixture.skill(&fixture.workspace, ".saya", "extra", "new metadata");
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    assert_eq!(
        manager
            .catalog_page(Some(&cursor), 5, 4096)
            .unwrap_err()
            .0
            .code,
        "cursor_invalid"
    );
    assert!(manager.catalog_page(None, 65, 4096).is_err());
    assert!(manager.catalog_page(None, 1, 16 * 1024 + 1).is_err());
}

#[test]
fn prepared_deactivation_binds_targets_and_cancellation_preserves_activation() {
    let fixture = Fixture::new();
    let directory = fixture.skill(&fixture.workspace, ".saya", "guide", "source");
    fs::write(directory.join("notes"), "notes").unwrap();
    let mut manager = fixture.manager();
    manager.refresh(&fixture.snapshot(), &[], &stop()).unwrap();
    let active = activate(&mut manager, "guide");
    let old = manager
        .resolve("guide", &active.metadata.source_sha256, None)
        .unwrap();
    let resource = manager
        .resolve("guide", &active.metadata.source_sha256, Some("notes"))
        .unwrap();
    manager
        .read_resource(&resource, None, None, 1024, &stop())
        .unwrap();
    assert_eq!(
        manager
            .deactivate_prepared(&old, &stop())
            .unwrap_err()
            .0
            .code,
        "skill_stale_selection"
    );
    assert_eq!(manager.authorization_targets().unwrap().len(), 2);
    let current = manager
        .resolve("guide", &active.metadata.source_sha256, None)
        .unwrap();
    assert_eq!(
        manager
            .deactivate_prepared(&current, &AtomicBool::new(true))
            .unwrap_err()
            .0
            .code,
        "cancelled"
    );
    assert_eq!(manager.revalidate(&stop()).unwrap().skills.len(), 1);
    assert!(manager.deactivate_prepared(&current, &stop()).unwrap());
    assert!(manager.revalidate(&stop()).unwrap().skills.is_empty());
}
