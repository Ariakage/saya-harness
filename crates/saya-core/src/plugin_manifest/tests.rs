use super::*;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

const SIMPLE: &str = r#"schema_version = 1
[plugin]
id = "io.saya.test"
version = "1.0.0"
description = "A bounded test package."
license = "MIT"
[compatibility]
requires_saya = ">=0.2.0, <1.0.0"
requires_features = ["plugin-manifest-v1"]
[capabilities]
requested = ["workspace.read"]
[[resources]]
id = "guide"
path = "resources/guide.md"
media_type = "text/markdown"
"#;

fn root(manifest: &str, body: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("resources")).unwrap();
    fs::write(directory.path().join(MANIFEST_NAME), manifest).unwrap();
    fs::write(directory.path().join("resources/guide.md"), body).unwrap();
    directory
}
fn package(
    id: &str,
    dependency: Option<(&str, &str)>,
    body: &str,
) -> (tempfile::TempDir, VerifiedPackage) {
    let mut manifest = SIMPLE.replace("io.saya.test", id);
    if let Some((id, version)) = dependency {
        manifest.push_str(&format!(
            "\n[[dependencies]]\nid = {id:?}\nversion = {version:?}\n"
        ));
    }
    let directory = root(&manifest, body);
    let package = inspect_package(directory.path()).unwrap();
    (directory, package)
}
fn source(reference: &str) -> PackageSource {
    PackageSource {
        kind: SourceKind::LocalDirectory,
        reference: reference.into(),
    }
}

#[test]
fn manifest_structure_and_semantic_fields_are_checked_without_echoing_input() {
    assert!(parse_manifest(SIMPLE).is_ok());
    for text in [
        SIMPLE.replace("schema_version = 1", "schema_version = 2"),
        format!("unexpected = true\n{SIMPLE}"),
        SIMPLE.replace("description =", "unknown = true\ndescription ="),
        SIMPLE.replace("1.0.0", "01.0.0"),
        SIMPLE.replace(">=0.2.0, <1.0.0", "not a version range"),
        SIMPLE.replace("plugin-manifest-v1", "unavailable-v2"),
        SIMPLE.replace("io.saya.test", "INVALID.SECRET.VALUE"),
        SIMPLE.replace("resources/guide.md", "../SECRET_VALUE"),
        format!(
            "{SIMPLE}\n[[resources]]\nid = \"guide\"\npath = \"resources/other.md\"\nmedia_type = \"text/plain\"\n"
        ),
    ] {
        let error = parse_manifest(&text).unwrap_err();
        assert!(!error.to_string().contains("SECRET"));
    }
    let empty = SIMPLE.split("[[resources]]").next().unwrap();
    assert!(parse_manifest(empty).is_err());
    let huge = " ".repeat(MAX_MANIFEST_BYTES + 1);
    assert!(parse_manifest(&huge).is_err());
}

#[test]
fn host_contract_reports_missing_features_and_respects_semver_prereleases() {
    let manifest = parse_manifest(SIMPLE).unwrap();
    let empty = BTreeSet::new();
    let report = manifest.compatibility_report("0.2.0", &empty).unwrap();
    assert!(report.version_matches);
    assert_eq!(report.missing_features, ["plugin-manifest-v1"]);
    assert!(manifest.check_host("0.2.0", &empty).is_err());
    let features = BTreeSet::from(["plugin-manifest-v1".into()]);
    assert!(manifest.check_host("0.2.0", &features).is_ok());
    assert!(manifest.check_host("1.0.0", &features).is_err());
    assert!(manifest.check_host("0.3.0-alpha.1", &features).is_err());
}

#[test]
fn executable_features_capabilities_origins_slots_and_argument_paths_are_bound() {
    let mut manifest = parse_manifest(SIMPLE).unwrap();
    manifest.mcp.push(McpEntry::Stdio {
        id: "check".into(),
        program: "python3".into(),
        args: vec!["@plugin/mcp/server.py".into()],
        credential_slots: vec![],
    });
    assert!(manifest.validate().is_err());
    manifest
        .compatibility
        .requires_features
        .push("mcp-stdio-v1".into());
    manifest
        .capabilities
        .requested
        .extend(["mcp.stdio".into(), "process.exec".into()]);
    assert!(manifest.validate().is_ok());
    if let McpEntry::Stdio { args, .. } = &mut manifest.mcp[0] {
        args[0] = "@plugin/../secret".into();
    }
    assert!(manifest.validate().is_err());
    manifest.mcp.clear();
    manifest.mcp.push(McpEntry::Http {
        id: "remote".into(),
        url: "https://mcp.example.test/v1".into(),
        credential_slots: vec!["account".into()],
    });
    manifest
        .compatibility
        .requires_features
        .push("mcp-http-v1".into());
    manifest.capabilities.requested.extend([
        "mcp.http".into(),
        "network.outbound".into(),
        "credential.use".into(),
    ]);
    assert!(manifest.validate().is_err());
    manifest
        .capabilities
        .network_origins
        .push("https://MCP.EXAMPLE.test:443/".into());
    assert!(manifest.validate().is_err());
    manifest
        .capabilities
        .credential_slots
        .push("account".into());
    assert!(manifest.validate().is_ok());
    manifest
        .capabilities
        .network_origins
        .push("https://mcp.example.test".into());
    assert!(manifest.validate().is_err());
    manifest.capabilities.network_origins.pop();
    if let McpEntry::Http { url, .. } = &mut manifest.mcp[0] {
        *url = "https://user:PRIVATE_VALUE@mcp.example.test/v1".into();
    }
    assert!(manifest.validate().is_err());
}

#[test]
fn hooks_skill_features_credential_literals_and_known_secrets_are_rejected() {
    let example = include_str!("../../../../examples/saya-plugin.example.toml");
    assert!(parse_manifest(example).is_ok());
    assert!(parse_manifest(&example.replace("hook-events-v1", "unknown-events-v1")).is_err());
    assert!(parse_manifest(&example.replace("skill-directory-v1", "unknown-skills-v1")).is_err());
    assert!(parse_manifest(&example.replace("kind = \"observer\"", "kind = \"check\"")).is_err());
    assert!(
        parse_manifest(&example.replace("@plugin/mcp/server.py", "--token=PRIVATE_VALUE")).is_err()
    );
    let text = SIMPLE.replace("A bounded test package.", "fixture-credential-text");
    let error =
        parse_manifest_with_secrets(&text, &["fixture-credential-text".into()]).unwrap_err();
    assert!(!error.to_string().contains("fixture-credential-text"));
}

#[test]
fn relative_paths_and_incremental_expansion_limits_reject_ambiguous_candidates() {
    for path in [
        "",
        "/absolute",
        "C:/drive",
        "a\\b",
        "a//b",
        "a/./b",
        "a/../b",
        "a/",
        "x\0y",
        "x\u{202e}y",
    ] {
        assert!(validate_relative_path(path).is_err(), "{path:?}");
    }
    assert!(validate_relative_path(&"a/".repeat(MAX_PATH_DEPTH)).is_err());
    assert!(validate_relative_path(&"界".repeat(MAX_PATH_BYTES)).is_err());
    let mut budget = FileBudget::default();
    budget.reserve_file("Straße/guide.md", 2).unwrap();
    assert!(budget.reserve_file("STRASSE/other.md", 2).is_err());
    assert_eq!((budget.file_count(), budget.expanded_bytes()), (1, 2));
    assert!(budget.reserve_file("Straße", 2).is_err());
    assert!(budget.reserve_file("Straße/guide.md/child", 2).is_err());
    budget.reserve_directory("Straße").unwrap();
    assert!(budget.reserve_directory("Straße").is_err());
    assert!(
        budget
            .reserve_file("large", package::MAX_PACKAGE_FILE_BYTES + 1)
            .is_err()
    );
    let mut total = FileBudget::default();
    for index in 0..4 {
        total
            .reserve_file(&format!("file{index}"), package::MAX_PACKAGE_FILE_BYTES)
            .unwrap();
    }
    assert!(total.reserve_file("overflow", 1).is_err());
    assert_eq!(total.expanded_bytes(), package::MAX_PACKAGE_BYTES);
    let mut count = FileBudget::default();
    for index in 0..package::MAX_PACKAGE_FILES {
        count.reserve_file(&format!("f{index}"), 0).unwrap();
    }
    assert!(count.reserve_file("extra", 0).is_err());
    let mut directories = FileBudget::default();
    for index in 0..package::MAX_PACKAGE_ENTRIES {
        directories.reserve_directory(&format!("d{index}")).unwrap();
    }
    assert!(directories.reserve_directory("extra").is_err());
}

#[test]
fn canonical_unicode_path_and_directory_aliases_are_rejected_before_expansion() {
    for (first, equivalent) in [
        ("resources/caf\u{e9}.md", "resources/cafe\u{301}.md"),
        ("resources/\u{ac00}.md", "resources/\u{1100}\u{1161}.md"),
        (
            "resources/a\u{323}\u{301}.md",
            "resources/a\u{301}\u{323}.md",
        ),
        ("resources/\u{1f0}.md", "resources/J\u{30c}.md"),
    ] {
        let mut budget = FileBudget::default();
        budget.reserve_file(first, 3).unwrap();
        assert!(budget.reserve_file(equivalent, 7).is_err(), "{equivalent}");
        assert_eq!((budget.file_count(), budget.expanded_bytes()), (1, 3));
    }
    let mut budget = FileBudget::default();
    budget.reserve_file("caf\u{e9}/first", 3).unwrap();
    assert!(budget.reserve_file("cafe\u{301}/second", 7).is_err());
    assert_eq!((budget.file_count(), budget.expanded_bytes()), (1, 3));
}

#[test]
fn unicode_collision_checks_preserve_sealed_path_bytes_and_exact_references() {
    let directory = root(SIMPLE, "guide");
    let requested_name = "cafe\u{301}.md";
    fs::rename(
        directory.path().join("resources/guide.md"),
        directory.path().join("resources").join(requested_name),
    )
    .unwrap();
    let actual_name = fs::read_dir(directory.path().join("resources"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .into_string()
        .unwrap();
    let actual_path = format!("resources/{actual_name}");
    fs::write(
        directory.path().join(MANIFEST_NAME),
        SIMPLE.replace("resources/guide.md", &actual_path),
    )
    .unwrap();
    let package = inspect_package(directory.path()).unwrap();
    assert_eq!(
        package.file(&actual_path).unwrap().path().as_bytes(),
        actual_path.as_bytes()
    );
    assert_eq!(package.file(&actual_path).unwrap().bytes(), b"guide");
    let alternate = if actual_name == requested_name {
        "resources/caf\u{e9}.md"
    } else {
        "resources/cafe\u{301}.md"
    };
    assert!(package.file(alternate).is_none());
    fs::write(
        directory.path().join(MANIFEST_NAME),
        SIMPLE.replace("resources/guide.md", alternate),
    )
    .unwrap();
    assert!(inspect_package(directory.path()).is_err());
}

#[test]
fn alias_names_match_the_actual_model_tool_registry_identifier_boundaries() {
    let (_directory, package) = package("io.test.alias", None, "guide");
    let alias = ToolAlias::stable("io.test.alias", "guide", package.digest(), "1").unwrap();
    for (name, expected) in [
        ("1guide", false),
        ("_guide", false),
        ("-guide", false),
        ("Guide_1", true),
        ("guide-name", true),
    ] {
        let mut candidate = alias.clone();
        candidate.name = name.into();
        let lock = PluginLock::build(&[(&package, source("alias"))], vec![candidate]);
        let tool = crate::boundary::ToolDefinition {
            name: name.into(),
            description: "Alias boundary fixture".into(),
            input_schema: serde_json::json!({"type":"object"}),
        };
        let prompt = crate::prompt::execution(
            crate::contract::WorkMode::Work,
            crate::contract::PermissionMode::Auto,
            &[tool],
        );
        assert_eq!(lock.is_ok(), prompt.is_ok(), "{name}");
        assert_eq!(lock.is_ok(), expected, "{name}");
    }
}

#[test]
fn sealed_tree_digest_uses_paths_and_bytes_and_survives_later_source_changes() {
    let directory = root(SIMPLE, "before");
    let first = inspect_package(directory.path()).unwrap();
    let file = first.file("resources/guide.md").unwrap();
    assert_eq!(file.bytes(), b"before");
    assert_eq!(file.sha256(), digest_hex(b"before"));
    assert_eq!(first.files().len(), 2);
    let mut hash = Sha256::new();
    hash.update(b"saya-plugin-tree-v1\0");
    for (path, bytes) in [
        ("resources/guide.md", b"before".as_slice()),
        (MANIFEST_NAME, SIMPLE.as_bytes()),
    ] {
        hash.update((path.len() as u32).to_be_bytes());
        hash.update(path.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(Sha256::digest(bytes));
    }
    let expected: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(first.digest(), expected);
    fs::set_permissions(
        directory.path().join("resources/guide.md"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert_eq!(
        inspect_package(directory.path()).unwrap().digest(),
        first.digest()
    );
    fs::write(directory.path().join("resources/guide.md"), "after").unwrap();
    let second = inspect_package(directory.path()).unwrap();
    assert_ne!(first.digest(), second.digest());
    assert_eq!(first.file("resources/guide.md").unwrap().bytes(), b"before");
}

#[test]
fn open_directory_inspection_stays_with_the_original_root_after_a_path_replacement() {
    let original = root(SIMPLE, "before");
    let opened = cap_std::fs::Dir::from_std_file(fs::File::open(original.path()).unwrap());
    let other = tempfile::tempdir().unwrap();
    fs::rename(original.path(), other.path().join("moved")).unwrap();
    fs::create_dir(original.path()).unwrap();
    fs::create_dir(original.path().join("resources")).unwrap();
    fs::write(original.path().join(MANIFEST_NAME), SIMPLE).unwrap();
    fs::write(original.path().join("resources/guide.md"), "after").unwrap();
    let anchored = inspect_package_directory(&opened).unwrap();
    let replacement = inspect_package(original.path()).unwrap();
    assert_eq!(
        anchored.file("resources/guide.md").unwrap().bytes(),
        b"before"
    );
    assert_eq!(
        replacement.file("resources/guide.md").unwrap().bytes(),
        b"after"
    );
    assert_ne!(anchored.digest(), replacement.digest());
}

#[test]
fn package_paths_links_special_files_missing_entries_and_oversized_files_fail_closed() {
    let directory = root(SIMPLE, "guide");
    symlink("resources/guide.md", directory.path().join("link")).unwrap();
    assert!(inspect_package(directory.path()).is_err());
    fs::remove_file(directory.path().join("link")).unwrap();
    fs::hard_link(
        directory.path().join("resources/guide.md"),
        directory.path().join("hard-link"),
    )
    .unwrap();
    assert!(inspect_package(directory.path()).is_err());
    fs::remove_file(directory.path().join("hard-link")).unwrap();
    fs::remove_file(directory.path().join("resources/guide.md")).unwrap();
    assert!(inspect_package(directory.path()).is_err());
    let file = fs::File::create(directory.path().join("resources/guide.md")).unwrap();
    file.set_len(package::MAX_PACKAGE_FILE_BYTES + 1).unwrap();
    assert!(inspect_package(directory.path()).is_err());
    let outside = tempfile::tempdir().unwrap();
    symlink(directory.path(), outside.path().join("root")).unwrap();
    assert!(inspect_package(&outside.path().join("root")).is_err());
    let fifo = directory.path().join("pipe");
    fs::write(directory.path().join("resources/guide.md"), "guide").unwrap();
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(inspect_package(directory.path()).is_err());
}

#[test]
fn lock_resolves_supplied_packages_and_rejects_missing_ranges_cycles_and_corruption() {
    let (_a_dir, a) = package("io.test.a", Some(("io.test.b", "^1.0")), "a");
    let (_b_dir, b) = package("io.test.b", None, "b");
    assert!(PluginLock::build(&[(&a, source("a"))], vec![]).is_err());
    let lock = PluginLock::build(&[(&a, source("a")), (&b, source("b"))], vec![]).unwrap();
    let parsed = PluginLock::parse(&lock.to_toml().unwrap()).unwrap();
    parsed.verify_package(&a).unwrap();
    parsed.verify_package(&b).unwrap();
    assert_eq!(parsed, lock);
    let mut damaged = lock.clone();
    damaged.packages[0].dependencies[0].digest = "0".repeat(64);
    assert!(damaged.validate().is_err());
    assert!(PluginLock::parse(&(lock.to_toml().unwrap() + "\nunknown = 1\n")).is_err());
    let (_bad_dir, bad) = package("io.test.a", Some(("io.test.b", ">=2.0")), "a");
    assert!(PluginLock::build(&[(&bad, source("a")), (&b, source("b"))], vec![]).is_err());
    let (_cycle_dir, cycle) = package("io.test.b", Some(("io.test.a", "^1.0")), "b");
    assert!(PluginLock::build(&[(&a, source("a")), (&cycle, source("b"))], vec![]).is_err());
    let (_changed_dir, changed) = package("io.test.b", None, "modified");
    assert!(lock.verify_package(&changed).is_err());
}

#[test]
fn source_conflicts_alias_collisions_and_capability_changes_invalidate_identity() {
    let (_first_dir, first) = package("io.test.a", None, "first");
    let (_second_dir, second) = package("io.test.a", None, "second");
    let error = PluginLock::build(
        &[(&first, source("first")), (&second, source("second"))],
        vec![],
    )
    .unwrap_err();
    assert_eq!(error.code, "plugin_source_conflict");
    let alias = ToolAlias::stable("io.test.a", "guide", first.digest(), "1").unwrap();
    let next = ToolAlias::stable("io.test.a", "guide", first.digest(), "2").unwrap();
    assert_ne!(alias.name, next.name);
    assert!(alias.name.len() <= 64);
    PluginLock::build(&[(&first, source("first"))], vec![alias.clone()]).unwrap();
    assert!(
        PluginLock::build(
            &[(&first, source("first"))],
            vec![alias.clone(), alias.clone()]
        )
        .is_err()
    );
    let mut reserved = alias.clone();
    reserved.name = "read_file".into();
    assert!(PluginLock::build(&[(&first, source("first"))], vec![reserved]).is_err());
    let mut absent = alias.clone();
    absent.entry_id = "absent".into();
    assert!(PluginLock::build(&[(&first, source("first"))], vec![absent]).is_err());
    assert!(
        PackageSource {
            kind: SourceKind::Https,
            reference: "https://user:PRIVATE_VALUE@source.example.test/plugin".into()
        }
        .validate()
        .is_err()
    );
    let mut manifest = parse_manifest(SIMPLE).unwrap();
    let before = manifest.capability_fingerprint().unwrap();
    manifest
        .capabilities
        .requested
        .push("workspace.write".into());
    let after = manifest.capability_fingerprint().unwrap();
    assert_ne!(before, after);
    manifest.capabilities.requested.reverse();
    assert_eq!(after, manifest.capability_fingerprint().unwrap());
}

#[test]
fn declared_package_executable_paths_are_resolved_only_inside_the_sealed_snapshot() {
    let manifest = SIMPLE
        .replace(
            "[\"plugin-manifest-v1\"]",
            "[\"plugin-manifest-v1\", \"mcp-stdio-v1\"]",
        )
        .replace(
            "[\"workspace.read\"]",
            "[\"workspace.read\", \"mcp.stdio\", \"process.exec\"]",
        )
        + "\n[[mcp]]\nid = \"server\"\ntransport = \"stdio\"\nprogram = \"python3\"\nargs = [\"@plugin/mcp/server.py\"]\n";
    let directory = root(&manifest, "guide");
    assert!(inspect_package(directory.path()).is_err());
    fs::create_dir(directory.path().join("mcp")).unwrap();
    fs::write(directory.path().join("mcp/server.py"), "print('test')").unwrap();
    assert!(inspect_package(directory.path()).is_ok());
    let text = fs::read_to_string(directory.path().join(MANIFEST_NAME)).unwrap();
    fs::write(
        directory.path().join(MANIFEST_NAME),
        text.replace("@plugin/mcp/server.py", "@plugin/mcp/missing.py"),
    )
    .unwrap();
    assert!(inspect_package(directory.path()).is_err());
}
