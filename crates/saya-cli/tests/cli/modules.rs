use crate::common::*;
use crate::fixture;
#[allow(unused_imports)]
use serde_json::json;
#[allow(unused_imports)]
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use tempfile::TempDir;

#[test]
fn module_command_manages_browser_and_vm_switches() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (url, _requests) = fixture::server(vec![(
        "application/json",
        response(
            json!({"role":"assistant","content":"ok without browser"}),
            "stop",
        ),
    )])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let raw_cfg = fs::read(&cfg).unwrap();
    let db_path = home
        .path()
        .join(".local/share/saya/modules/modules.sqlite3");

    // Read-only `module list` creates zero state files and reports config defaults.
    let list = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "module", "list"])
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(!db_path.exists());
    let list_json: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(list_json["schema_version"], 1);
    assert_eq!(list_json["kind"], "module");
    assert_eq!(list_json["action"], "list");
    assert_eq!(list_json["data"]["modules"][0]["module"], "browser");
    assert_eq!(list_json["data"]["modules"][0]["enabled"], false);
    assert_eq!(list_json["data"]["modules"][0]["source"], "config");
    assert_eq!(list_json["data"]["modules"][1]["module"], "vm");
    assert_eq!(list_json["data"]["modules"][1]["enabled"], false);

    // Bare `saya module` lists statuses in text mode.
    let bare = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["module"])
        .output()
        .unwrap();
    assert!(bare.status.success());
    let bare_out = String::from_utf8(bare.stdout).unwrap();
    assert!(bare_out.contains("browser: disabled"));
    assert!(bare_out.contains("vm: disabled"));

    // Enabling `vm` without VM prerequisites fails with exit code 2 without persisting.
    let vm_missing = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "module", "vm", "enable"])
        .output()
        .unwrap();
    assert_eq!(vm_missing.status.code(), Some(2));
    let vm_err: serde_json::Value = serde_json::from_slice(&vm_missing.stdout).unwrap();
    assert_eq!(vm_err["error"]["code"], "invalid_query");
    assert!(
        vm_err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Browser VM")
    );

    // Unknown module returns exit code 2 cleanly.
    let bogus = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "module", "bogus", "enable"])
        .output()
        .unwrap();
    assert_eq!(bogus.status.code(), Some(2));

    // Enabling `browser` stores override without touching config.toml.
    let enable_browser = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["module", "browser", "enable"])
        .output()
        .unwrap();
    assert!(
        enable_browser.status.success(),
        "{}",
        String::from_utf8_lossy(&enable_browser.stderr)
    );
    assert!(db_path.exists());
    assert_eq!(fs::read(&cfg).unwrap(), raw_cfg);

    let status_browser = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "module", "browser", "status"])
        .output()
        .unwrap();
    assert!(status_browser.status.success());
    let status_json: serde_json::Value = serde_json::from_slice(&status_browser.stdout).unwrap();
    assert_eq!(status_json["action"], "status");
    assert_eq!(status_json["data"]["module"]["module"], "browser");
    assert_eq!(status_json["data"]["module"]["enabled"], true);
    assert_eq!(status_json["data"]["module"]["configured_enabled"], false);
    assert_eq!(status_json["data"]["module"]["source"], "module_store");
    assert_eq!(status_json["data"]["module"]["active"], false);
    assert!(status_json["data"]["module"]["issue"].as_str().is_some());

    // `saya run` fails fail-closed while browser module is enabled without worker/vm,
    // then succeeds cleanly after `saya module browser disable`.
    let failed_run = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "run", "test browser gate"])
        .output()
        .unwrap();
    assert!(!failed_run.status.success());

    let disable_browser = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["module", "browser", "disable"])
        .output()
        .unwrap();
    assert!(disable_browser.status.success());

    let ok_run = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "run", "test browser disabled"])
        .output()
        .unwrap();
    assert!(
        ok_run.status.success(),
        "stderr={}\nstdout={}",
        String::from_utf8_lossy(&ok_run.stderr),
        String::from_utf8_lossy(&ok_run.stdout)
    );

    // VM config with worker disabled allows `vm enable` (JSONL) and `vm disable`,
    // and enforces D-066 mutual exclusion when `worker.enabled = true`.
    let vm_home = TempDir::new().unwrap();
    let vm_cfg = temp.path().join("vm-config.toml");
    let sha = "a".repeat(64);
    fs::write(
        &vm_cfg,
        format!(
            r#"{base}
[browser]
enabled = false

[browser.vm]
enabled = false
colima = "/usr/local/bin/colima"
ssh = "/usr/bin/ssh"
profile = "saya-browser"
host = "127.0.0.1"
user = "saya"
port = 2222
identity_file = "/var/lib/saya/vm_key"
known_hosts = "/var/lib/saya/known_hosts"
node = "/opt/saya/node"
worker_root = "/opt/saya/worker"
chromium = "/opt/saya/chromium"
identity_manifest = "/opt/saya/identity.json"
identity_sha256 = "{sha}"
remote_temp_root = "/tmp/saya-runs"

[browser.network]
enabled = true
public_web = true
"#,
            base = String::from_utf8_lossy(&raw_cfg),
        ),
    )
    .unwrap();

    let vm_enable = command(temp.path(), vm_home.path())
        .arg("--config")
        .arg(&vm_cfg)
        .args(["--output", "jsonl", "module", "vm", "enable"])
        .output()
        .unwrap();
    assert!(
        vm_enable.status.success(),
        "stderr={}\nstdout={}",
        String::from_utf8_lossy(&vm_enable.stderr),
        String::from_utf8_lossy(&vm_enable.stdout)
    );
    let vm_jsonl: serde_json::Value = serde_json::from_slice(&vm_enable.stdout).unwrap();
    assert_eq!(vm_jsonl["type"], "module.result");
    assert_eq!(vm_jsonl["kind"], "module");
    assert_eq!(vm_jsonl["action"], "enable");
    assert_eq!(vm_jsonl["data"]["module"]["module"], "vm");
    assert_eq!(vm_jsonl["data"]["module"]["enabled"], true);
    assert_eq!(vm_jsonl["data"]["module"]["source"], "module_store");

    let check = command(temp.path(), vm_home.path())
        .arg("--config")
        .arg(&vm_cfg)
        .args(["config", "check"])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );

    let vm_disable = command(temp.path(), vm_home.path())
        .arg("--config")
        .arg(&vm_cfg)
        .args(["module", "vm", "disable"])
        .output()
        .unwrap();
    assert!(vm_disable.status.success());

    // D-066: worker.enabled = true blocks `saya module vm enable`.
    let conflict_home = TempDir::new().unwrap();
    let conflict_cfg = temp.path().join("conflict-config.toml");
    fs::write(
        &conflict_cfg,
        format!(
            r#"{vm_toml}
[browser.worker]
enabled = true
node = "/opt/saya/node"
worker_root = "/opt/saya/worker"
chromium = "/opt/saya/chromium"
identity_manifest = "/opt/saya/identity.json"
identity_sha256 = "{sha}"
"#,
            vm_toml = fs::read_to_string(&vm_cfg).unwrap(),
        ),
    )
    .unwrap();
    let conflict = command(temp.path(), conflict_home.path())
        .arg("--config")
        .arg(&conflict_cfg)
        .args(["--output", "json", "module", "vm", "enable"])
        .output()
        .unwrap();
    assert_eq!(conflict.status.code(), Some(2));
    let conflict_json: serde_json::Value = serde_json::from_slice(&conflict.stdout).unwrap();
    assert!(
        conflict_json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not both")
    );
}
