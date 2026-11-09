use super::tests::{cancel, fixture, seed};

#[test]
fn export_review_holds_real_parent_and_rejects_changed_symlink_alias() {
    let (root, admin) = fixture();
    seed(&admin);
    let approved = root.path().join("approved");
    let other = root.path().join("other");
    std::fs::create_dir(&approved).unwrap();
    std::fs::create_dir(&other).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&approved, &alias).unwrap();
    let target = alias.join("backup.json");
    let prepared = admin
        .prepare(&format!("export {}", target.display()), &cancel())
        .unwrap();
    assert_eq!(
        prepared.review()["target"],
        std::fs::canonicalize(&approved)
            .unwrap()
            .join("backup.json")
            .to_str()
            .unwrap()
    );
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&other, &alias).unwrap();
    assert_eq!(
        admin.confirm(prepared, &cancel()).unwrap_err().0.code,
        "export_path_changed"
    );
    assert_eq!(std::fs::read_dir(&approved).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&other).unwrap().count(), 0);
}

#[test]
fn export_review_rejects_replaced_named_directory_and_new_target_credentials() {
    let (root, admin) = fixture();
    seed(&admin);
    let approved = root.path().join("approved");
    let moved = root.path().join("moved");
    std::fs::create_dir(&approved).unwrap();
    let target = approved.join("backup.json");
    let prepared = admin
        .prepare(&format!("export {}", target.display()), &cancel())
        .unwrap();
    std::fs::rename(&approved, &moved).unwrap();
    std::fs::create_dir(&approved).unwrap();
    assert_eq!(
        admin.confirm(prepared, &cancel()).unwrap_err().0.code,
        "export_path_changed"
    );
    assert_eq!(std::fs::read_dir(&approved).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 0);
    let target = approved.join("newly-protected-backup.json");
    let prepared = admin
        .prepare(&format!("export {}", target.display()), &cancel())
        .unwrap();
    admin
        .secrets
        .register(&["newly-protected-backup.json".into()])
        .unwrap();
    assert_eq!(
        admin.confirm(prepared, &cancel()).unwrap_err().0.code,
        "sensitive_data"
    );
    assert!(!target.exists());
}

#[test]
fn export_review_rechecks_alias_credentials_and_backups_after_rotation() {
    let (root, admin) = fixture();
    seed(&admin);
    let approved = root.path().join("approved");
    std::fs::create_dir(&approved).unwrap();
    let alias = root.path().join("newly-protected-export-alias");
    std::os::unix::fs::symlink(&approved, &alias).unwrap();
    let target = alias.join("backup.json");
    let prepared = admin
        .prepare(&format!("export {}", target.display()), &cancel())
        .unwrap();
    admin
        .secrets
        .register(&["newly-protected-export-alias".into()])
        .unwrap();
    assert_eq!(
        admin.confirm(prepared, &cancel()).unwrap_err().0.code,
        "sensitive_data"
    );
    assert!(!target.exists());
    let target = approved.join("backup.json");
    let prepared = admin
        .prepare(&format!("export {}", target.display()), &cancel())
        .unwrap();
    admin
        .secrets
        .register(&["Prefer focused changes".into()])
        .unwrap();
    assert_eq!(
        admin.confirm(prepared, &cancel()).unwrap_err().0.code,
        "sensitive_data"
    );
    assert!(!target.exists());
}
