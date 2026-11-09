use rmcp::transport::auth::{CredentialStore, StoredCredentials};
use saya_core::credentials::{
    CredentialScope, CredentialValue, FileCredentialStore, SecretRegistry, SecretValue,
};
use serde_json::json;
use std::collections::BTreeSet;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::Arc;
use std::time::Duration;

fn scope(server: &str) -> CredentialScope {
    CredentialScope::for_mcp(
        server,
        "https://example.invalid/mcp",
        BTreeSet::from(["read".into()]),
    )
    .unwrap()
}
fn store() -> (tempfile::TempDir, FileCredentialStore, Arc<SecretRegistry>) {
    let directory = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretRegistry::default());
    let store =
        FileCredentialStore::open(directory.path().join("credentials"), secrets.clone()).unwrap();
    (directory, store, secrets)
}
fn oauth(access: &str, refresh: &str) -> StoredCredentials {
    StoredCredentials::new("fixture-client".into(),Some(serde_json::from_value(json!({"access_token":access,"token_type":"Bearer","refresh_token":refresh,"expires_in":3600})).unwrap()),vec!["read".into()],Some(1)).with_issuer(Some("https://issuer.invalid".into()))
}

#[tokio::test]
async fn private_store_resolves_only_exact_scope_and_metadata_never_contains_values() {
    let (_directory, store, secrets) = store();
    let scope = scope("one");
    let reference = store
        .create(
            scope.clone(),
            CredentialValue::Secret(SecretValue::new("fixture-private-value").unwrap()),
        )
        .await
        .unwrap();
    let resolved = store.resolve(reference, &scope).await.unwrap();
    assert!(!format!("{resolved:?}").contains("fixture-private-value"));
    assert!(
        !serde_json::to_string(&store.list().await.unwrap())
            .unwrap()
            .contains("fixture-private-value")
    );
    let CredentialValue::Secret(secret) = resolved.value else {
        panic!("wrong kind");
    };
    assert_eq!(secret.expose(), "fixture-private-value");
    assert_eq!(secrets.snapshot().unwrap(), vec!["fixture-private-value"]);
    let mut wrong = scope.clone();
    wrong.server_id = "two".into();
    assert_eq!(
        store.resolve(reference, &wrong).await.unwrap_err().0.code,
        "credential_scope"
    );
    wrong = scope.clone();
    wrong.audience = "https://example.invalid/other".into();
    assert_eq!(
        store.resolve(reference, &wrong).await.unwrap_err().0.code,
        "credential_scope"
    );
    wrong = scope.clone();
    wrong.scopes.insert("write".into());
    assert_eq!(
        store.resolve(reference, &wrong).await.unwrap_err().0.code,
        "credential_scope"
    );
    let root = std::fs::metadata(store.protected_path()).unwrap();
    assert_eq!(root.permissions().mode() & 0o777, 0o700);
    let record =
        std::fs::metadata(store.protected_path().join(format!("{reference}.json"))).unwrap();
    assert_eq!(record.permissions().mode() & 0o777, 0o600);
    store.revoke(reference, &scope).await.unwrap();
    store.revoke(reference, &scope).await.unwrap();
    assert_eq!(
        store.resolve(reference, &scope).await.unwrap_err().0.code,
        "credential_revoked"
    );
    assert!(store.list().await.unwrap()[0].revoked);
    assert_eq!(secrets.snapshot().unwrap(), vec!["fixture-private-value"]);
    assert!(
        !std::fs::read_to_string(store.protected_path().join(format!("{reference}.json")))
            .unwrap()
            .contains("fixture-private-value")
    );
}

#[test]
fn root_symlinks_and_permissive_directories_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(FileCredentialStore::open(&root, Arc::new(SecretRegistry::default())).is_err());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = directory.path().join("link");
    symlink(&root, &link).unwrap();
    assert!(FileCredentialStore::open(link, Arc::new(SecretRegistry::default())).is_err());
}

#[tokio::test]
async fn record_symlinks_hardlinks_and_permissive_files_fail_closed() {
    for attack in ["symlink", "hardlink", "mode", "corrupt"] {
        let (directory, store, _) = store();
        let scope = scope("one");
        let reference = store
            .create(
                scope.clone(),
                CredentialValue::Secret(SecretValue::new("fake-value").unwrap()),
            )
            .await
            .unwrap();
        let path = store.protected_path().join(format!("{reference}.json"));
        let outside = directory.path().join("outside");
        match attack {
            "symlink" => {
                std::fs::rename(&path, &outside).unwrap();
                symlink(&outside, &path).unwrap();
            }
            "hardlink" => std::fs::hard_link(&path, outside).unwrap(),
            "mode" => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap()
            }
            _ => std::fs::write(&path, "{partial credential").unwrap(),
        }
        assert!(store.resolve(reference, &scope).await.is_err());
    }
}

#[tokio::test]
async fn oauth_save_observes_old_and_new_values_and_revoke_cannot_be_undone() {
    let (_directory, store, secrets) = store();
    let scope = scope("one");
    let reference = store
        .create(
            scope.clone(),
            CredentialValue::oauth(oauth("old-access", "old-refresh")),
        )
        .await
        .unwrap();
    let bridge = store.oauth(reference, scope.clone()).unwrap();
    bridge
        .save(oauth("new-access", "new-refresh"))
        .await
        .unwrap();
    let known = secrets.snapshot().unwrap();
    for value in ["old-access", "old-refresh", "new-access", "new-refresh"] {
        assert!(known.iter().any(|known| known == value));
    }
    assert!(!format!("{bridge:?}").contains("new-access"));
    assert_eq!(
        bridge.load().await.unwrap().unwrap().client_id,
        "fixture-client"
    );
    store.revoke(reference, &scope).await.unwrap();
    assert!(bridge.load().await.unwrap().is_none());
    assert!(
        bridge
            .save(oauth("resurrect-access", "resurrect-refresh"))
            .await
            .is_err()
    );
    assert!(bridge.load().await.unwrap().is_none());
}

#[tokio::test]
async fn refresh_lock_serializes_with_revoke_without_reentrant_record_deadlock() {
    let (_directory, store, _) = store();
    let scope = scope("one");
    let reference = store
        .create(
            scope.clone(),
            CredentialValue::oauth(oauth("old-access", "old-refresh")),
        )
        .await
        .unwrap();
    let bridge = store.oauth(reference, scope.clone()).unwrap();
    let guard = bridge.acquire_refresh_guard().await.unwrap().unwrap();
    let revoker = store.clone();
    let revoke_scope = scope.clone();
    let revoke = tokio::spawn(async move { revoker.revoke(reference, &revoke_scope).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!revoke.is_finished());
    tokio::time::timeout(Duration::from_secs(1), bridge.load())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        bridge.save(oauth("rotated-access", "rotated-refresh")),
    )
    .await
    .unwrap()
    .unwrap();
    drop(guard);
    tokio::time::timeout(Duration::from_secs(1), revoke)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(bridge.load().await.unwrap().is_none());
    assert!(
        bridge
            .save(oauth("too-late-access", "too-late-refresh"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rejected_refresh_only_clears_matching_credentials_and_never_reenters_guard() {
    let (_directory, store, _) = store();
    let scope = scope("one");
    let old = oauth("old-access", "old-refresh");
    let reference = store
        .create(scope.clone(), CredentialValue::oauth(old.clone()))
        .await
        .unwrap();
    let bridge = store.oauth(reference, scope).unwrap();
    let guard = bridge.acquire_refresh_guard().await.unwrap().unwrap();
    let new = oauth("current-access", "current-refresh");
    bridge.save(new.clone()).await.unwrap();
    bridge.on_refresh_token_rejected(&old).await.unwrap();
    assert!(bridge.load().await.unwrap().is_some());
    tokio::time::timeout(
        Duration::from_secs(1),
        bridge.on_refresh_token_rejected(&new),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(bridge.load().await.unwrap().is_none());
    drop(guard);
}

#[tokio::test]
async fn distinct_store_handles_coordinate_and_client_issuer_changes_fail_closed() {
    let (_directory, store, _) = store();
    let scope = scope("one");
    let reference = store
        .create(
            scope.clone(),
            CredentialValue::oauth(oauth("old-access", "old-refresh")),
        )
        .await
        .unwrap();
    let second =
        FileCredentialStore::open(store.protected_path(), Arc::new(SecretRegistry::default()))
            .unwrap();
    let bridge = store.oauth(reference, scope.clone()).unwrap();
    let other = second.oauth(reference, scope).unwrap();
    let guard = bridge.acquire_refresh_guard().await.unwrap().unwrap();
    let waiter = tokio::spawn(async move { other.acquire_refresh_guard().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!waiter.is_finished());
    bridge
        .save(oauth("current-access", "current-refresh"))
        .await
        .unwrap();
    drop(guard);
    let acquired = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(acquired);
    let mut wrong = oauth("wrong-access", "wrong-refresh");
    wrong.client_id = "different-client".into();
    assert!(bridge.save(wrong).await.is_err());
    assert!(bridge.load().await.unwrap().is_some());
    let bound = oauth("issuer-access", "issuer-refresh")
        .with_issuer(Some("https://issuer-one.invalid".into()));
    let reference = store
        .create(crate::scope("issuer"), CredentialValue::oauth(bound))
        .await
        .unwrap();
    let bound_store = store.oauth(reference, crate::scope("issuer")).unwrap();
    let wrong = oauth("wrong-issuer-access", "wrong-issuer-refresh")
        .with_issuer(Some("https://issuer-two.invalid".into()));
    assert!(bound_store.save(wrong).await.is_err());
}

#[test]
fn secret_registry_is_bounded_and_registration_is_atomic() {
    let registry = SecretRegistry::default();
    registry.register(&["existing".into()]).unwrap();
    let oversized = "x".repeat(65537);
    assert!(registry.register(&["new".into(), oversized]).is_err());
    assert_eq!(registry.snapshot().unwrap(), vec!["existing"]);
    assert!(!format!("{registry:?}").contains("existing"));
    let too_many = (0..4097)
        .map(|index| format!("fixture-value-{index}"))
        .collect::<Vec<_>>();
    assert!(registry.register(&too_many).is_err());
    assert_eq!(registry.snapshot().unwrap(), vec!["existing"]);
}

#[tokio::test]
async fn metadata_rejects_values_embedded_in_the_resource_path() {
    let (_directory, store, _) = store();
    let scope = CredentialScope::for_mcp(
        "one",
        "https://example.invalid/mcp/fake-path-token",
        BTreeSet::new(),
    )
    .unwrap();
    let error = store
        .create(
            scope,
            CredentialValue::Secret(SecretValue::new("fake-path-token").unwrap()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "credential_scope");
    assert!(!format!("{error:?}").contains("fake-path-token"));
    assert!(store.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn credential_refresh_lock_child() {
    let Some(root) = std::env::var_os("SAYA_CREDENTIAL_LOCK_TEST_ROOT") else {
        return;
    };
    let reference = std::env::var("SAYA_CREDENTIAL_LOCK_TEST_REF")
        .unwrap()
        .parse()
        .unwrap();
    let store = FileCredentialStore::open(&root, Arc::new(SecretRegistry::default())).unwrap();
    let bridge = store.oauth(reference, scope("one")).unwrap();
    let _guard = bridge.acquire_refresh_guard().await.unwrap().unwrap();
    std::fs::write(
        std::path::Path::new(&root).join("child-acquired"),
        b"acquired",
    )
    .unwrap();
}

#[tokio::test]
async fn refresh_lock_coordinates_with_an_independent_process() {
    let (_directory, store, _) = store();
    let reference = store
        .create(
            scope("one"),
            CredentialValue::oauth(oauth("cross-access", "cross-refresh")),
        )
        .await
        .unwrap();
    let bridge = store.oauth(reference, scope("one")).unwrap();
    let guard = bridge.acquire_refresh_guard().await.unwrap().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "credential_refresh_lock_child", "--nocapture"])
        .env("SAYA_CREDENTIAL_LOCK_TEST_ROOT", store.protected_path())
        .env("SAYA_CREDENTIAL_LOCK_TEST_REF", reference.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(child.try_wait().unwrap().is_none());
    assert!(!store.protected_path().join("child-acquired").exists());
    drop(guard);
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || child.wait()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(status.success());
    assert!(store.protected_path().join("child-acquired").exists());
}

#[tokio::test]
async fn invalid_refresh_material_leaves_the_existing_record_intact() {
    let (_directory, store, _) = store();
    let reference = store
        .create(
            scope("one"),
            CredentialValue::oauth(oauth("stable-access", "stable-refresh")),
        )
        .await
        .unwrap();
    let bridge = store.oauth(reference, scope("one")).unwrap();
    let path = store.protected_path().join(format!("{reference}.json"));
    let before = std::fs::read(&path).unwrap();
    assert!(
        bridge
            .save(oauth(&"x".repeat(65537), "oversized-refresh"))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(bridge.load().await.unwrap().is_some());
}

#[tokio::test]
async fn active_oauth_requires_a_valid_issuer_and_registration_placeholder_can_bind_once() {
    let (_directory, store, _) = store();
    for issuer in [
        None,
        Some("http://public.invalid"),
        Some("https://user:password@issuer.invalid"),
        Some("https://issuer.invalid?credential=private"),
    ] {
        let credentials = oauth("fake-active-access", "fake-active-refresh")
            .with_issuer(issuer.map(String::from));
        assert_eq!(
            store
                .create(scope("one"), CredentialValue::oauth(credentials))
                .await
                .unwrap_err()
                .0
                .code,
            "credential_scope"
        );
    }
    let placeholder = StoredCredentials::new("fixture-client".into(), None, Vec::new(), None);
    let reference = store
        .create(scope("one"), CredentialValue::oauth(placeholder))
        .await
        .unwrap();
    let bridge = store.oauth(reference, scope("one")).unwrap();
    bridge
        .save(oauth("bound-access", "bound-refresh"))
        .await
        .unwrap();
    assert_eq!(
        bridge.load().await.unwrap().unwrap().issuer.as_deref(),
        Some("https://issuer.invalid")
    );
    assert!(
        bridge
            .save(
                oauth("changed-access", "changed-refresh")
                    .with_issuer(Some("https://different-issuer.invalid".into()))
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn oauth_refresh_cannot_expand_the_approved_scopes_or_change_an_active_client() {
    let (_directory, store, known) = store();
    let reference = store
        .create(
            scope("one"),
            CredentialValue::oauth(oauth("read-access", "read-refresh")),
        )
        .await
        .unwrap();
    let bridge = store.oauth(reference, scope("one")).unwrap();
    let mut expanded = oauth("admin-access", "admin-refresh");
    expanded.granted_scopes.push("admin".into());
    assert!(bridge.save(expanded).await.is_err());
    let mut token_expanded = oauth("scope-access", "scope-refresh");
    token_expanded.token_response=Some(serde_json::from_value(json!({"access_token":"scope-access","refresh_token":"scope-refresh","token_type":"Bearer","scope":"read admin"})).unwrap());
    assert!(bridge.save(token_expanded).await.is_err());
    for value in [
        "admin-access",
        "admin-refresh",
        "scope-access",
        "scope-refresh",
    ] {
        assert!(known.snapshot().unwrap().contains(&value.into()));
    }
    let mut empty = oauth("empty-access", "empty-refresh");
    empty.client_id.clear();
    assert!(
        store
            .create(scope("empty"), CredentialValue::oauth(empty))
            .await
            .is_err()
    );
    let no_scope =
        CredentialScope::for_mcp("default", "https://example.invalid/mcp", BTreeSet::new())
            .unwrap();
    assert!(
        store
            .create(
                no_scope.clone(),
                CredentialValue::oauth(oauth("bad-default-access", "bad-default-refresh"))
            )
            .await
            .is_err()
    );
    let mut unspecified = oauth("unspecified-access", "unspecified-refresh");
    unspecified.granted_scopes.clear();
    store
        .create(no_scope, CredentialValue::oauth(unspecified))
        .await
        .unwrap();
    assert!(
        bridge
            .load()
            .await
            .unwrap()
            .unwrap()
            .granted_scopes
            .iter()
            .all(|scope| scope == "read")
    );
}

#[tokio::test]
async fn metadata_listing_rechecks_values_learned_from_later_records() {
    let (_directory, store, _) = store();
    let first = CredentialScope::for_mcp(
        "first",
        "https://example.invalid/mcp/later-private-value",
        BTreeSet::new(),
    )
    .unwrap();
    store
        .create(
            first,
            CredentialValue::Secret(SecretValue::new("first-private-value").unwrap()),
        )
        .await
        .unwrap();
    store
        .create(
            scope("second"),
            CredentialValue::Secret(SecretValue::new("later-private-value").unwrap()),
        )
        .await
        .unwrap();
    let reopened =
        FileCredentialStore::open(store.protected_path(), Arc::new(SecretRegistry::default()))
            .unwrap();
    assert_eq!(
        reopened.list().await.unwrap_err().0.code,
        "credential_scope"
    );
}
