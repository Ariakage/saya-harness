mod support;

use std::fs;

#[test]
fn workspaces_are_isolated_and_cleaned_up() -> Result<(), Box<dyn std::error::Error>> {
    let first = support::temp_workspace()?;
    let second = support::temp_workspace()?;
    let first_path = first.path().to_owned();

    fs::write(first.path().join("example.txt"), "first")?;
    assert!(!second.path().join("example.txt").exists());
    drop(first);
    assert!(!first_path.exists());
    assert!(second.path().exists());
    Ok(())
}
