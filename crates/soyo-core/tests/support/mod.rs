use std::io;

/// Creates an isolated workspace that is deleted when its guard is dropped.
pub fn temp_workspace() -> io::Result<tempfile::TempDir> {
    tempfile::Builder::new().prefix("saya-test-").tempdir()
}
