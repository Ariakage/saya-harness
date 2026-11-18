fn main() {
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=TARGET");

    let dist_dir = std::path::Path::new("../dist");
    let index = dist_dir.join("index.html");
    if !index.exists() {
        let _ = std::fs::create_dir_all(dist_dir);
        let _ = std::fs::write(
            &index,
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>Soyo</title></head><body><div id=\"root\"></div></body></html>\n",
        );
    }

    // `tauri_build::build()` validates `bundle.externalBin` paths at compile time.
    // Non-release dev/test builds stage an empty sidecar placeholder when the
    // real `soyo` binary has not been copied yet; release builds never mask a
    // missing sidecar.
    let profile = std::env::var("PROFILE").unwrap_or_default();
    if profile != "release"
        && let Ok(target) = std::env::var("TARGET")
    {
        let suffix = if target.contains("windows") {
            ".exe"
        } else {
            ""
        };
        let sidecar = std::path::Path::new("binaries").join(format!("soyo-{target}{suffix}"));
        if !sidecar.exists() {
            if let Some(parent) = sidecar.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&sidecar, b"");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755));
            }
        }
    }

    tauri_build::build()
}
