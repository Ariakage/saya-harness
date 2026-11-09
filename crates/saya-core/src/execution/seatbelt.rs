//! Standalone Seatbelt compiler. All filesystem paths are passed as SBPL parameters.
use super::{NetworkPolicy, PreparedCommand, SandboxPolicy, failure, unavailable};
use crate::contract::BoundaryError;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub(super) struct SeatbeltBackend {
    profile: String,
    parameters: Vec<OsString>,
    helper_path: PathBuf,
}
impl SeatbeltBackend {
    pub(super) fn helper_path(&self) -> &Path {
        &self.helper_path
    }
    pub(super) fn prepare(policy: &SandboxPolicy, scratch: &Path) -> Result<Self, BoundaryError> {
        if !Path::new("/usr/bin/sandbox-exec").is_file() {
            return Err(unavailable("system sandbox-exec is unavailable"));
        }
        let mut profile = String::from(
            "(version 1)\n(deny default)\n(allow process-exec)\n(allow process-fork)\n(allow process-info* (target same-sandbox))\n(allow signal (target same-sandbox))\n(allow sysctl-read)\n(allow file-read-metadata (vnode-type DIRECTORY))\n(allow file-read* file-map-executable (literal \"/\") (subpath \"/System\") (subpath \"/usr/lib\") (subpath \"/bin\") (subpath \"/usr/bin\") (subpath \"/usr/share\") (subpath \"/private/var/db/dyld\") (literal \"/dev/null\") (literal \"/dev/urandom\") (literal \"/dev/random\"))\n(allow file-write-data (literal \"/dev/null\"))\n",
        );
        let mut parameters = Vec::new();
        let helper_path = scratch.join("saya-worker");
        super::transport::copy_launcher(&helper_path).map_err(super::io_error)?;
        let mut reads = policy.read_roots.clone();
        reads.extend(policy.write_roots.clone());
        reads.push(helper_path.clone());
        for (index, path) in reads.iter().enumerate() {
            parameter(
                &mut profile,
                &mut parameters,
                "file-read* file-map-executable",
                "R",
                index,
                path,
            );
        }
        for (index, path) in policy.write_roots.iter().enumerate() {
            parameter(
                &mut profile,
                &mut parameters,
                "file-write*",
                "W",
                index,
                path,
            );
        }
        parameter(
            &mut profile,
            &mut parameters,
            "file-read* file-write* file-map-executable",
            "T",
            0,
            &scratch.join("tmp"),
        );
        // Denials are emitted last, including missing paths, so later creation
        // in a writable ancestor cannot remove their protection.
        for (index, path) in policy.protected_paths.iter().enumerate() {
            let key = format!("P{index}");
            parameters.push("-D".into());
            parameters.push(path_parameter(&key, path));
            profile.push_str(&format!(
                "(deny file-read* file-write* file-map-executable (subpath (param \"{key}\")))\n"
            ));
        }
        if let NetworkPolicy::Proxy(proxy) = &policy.network {
            let address = format!("localhost:{}", proxy.endpoint.port());
            // SBPL only accepts localhost or * here. The host broker must own
            // this port on both IPv4 and IPv6 loopback.
            profile.push_str(&format!(
                "(allow network-outbound (remote ip \"{address}\"))\n"
            ));
        }
        let backend = Self {
            profile,
            parameters,
            helper_path,
        };
        // A real harmless process validates availability and the compiled policy.
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .arg("-p")
            .arg(&backend.profile)
            .args(&backend.parameters)
            .arg("/usr/bin/true")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let status = command
            .status()
            .map_err(|_| unavailable("Seatbelt probe could not start"))?;
        if !status.success() {
            return Err(unavailable(
                "Seatbelt could not enforce the compiled policy",
            ));
        }
        Ok(backend)
    }
    pub(super) fn wrap(
        &self,
        command: &mut PreparedCommand,
        argv: &[OsString],
    ) -> Result<(), BoundaryError> {
        if !Path::new(&argv[0]).is_absolute() {
            return Err(failure(
                "invalid_args",
                "sandbox command executable must be absolute",
            ));
        }
        command.program = PathBuf::from("/usr/bin/sandbox-exec");
        command.args = vec!["-p".into(), self.profile.clone().into()];
        command.args.extend(self.parameters.clone());
        command.args.push(self.helper_path.clone().into_os_string());
        command.args.push("__saya-sandbox-launch".into());
        command.bootstrap_index = Some(command.args.len());
        command.args.push("--".into());
        let mut payload = argv.to_vec();
        if Path::new(&payload[0]) == std::env::current_exe().map_err(super::io_error)?
            && payload
                .get(1)
                .is_some_and(|value| value == "__saya-file-worker")
        {
            payload[0] = self.helper_path.clone().into_os_string();
        }
        command.args.extend(payload);
        Ok(())
    }
}
fn parameter(
    profile: &mut String,
    parameters: &mut Vec<OsString>,
    operation: &str,
    prefix: &str,
    index: usize,
    path: &Path,
) {
    let key = format!("{prefix}{index}");
    parameters.push("-D".into());
    parameters.push(path_parameter(&key, path));
    profile.push_str(&format!(
        "(allow {operation} (subpath (param \"{key}\")))\n"
    ));
}
fn path_parameter(key: &str, path: &Path) -> OsString {
    let mut value = OsString::from(format!("{key}="));
    value.push(path);
    value
}
