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
            r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow process-info* (target same-sandbox))
(deny process-info-pidinfo (require-not (target self)))
(allow signal (target same-sandbox))
(allow sysctl-read
  (sysctl-name-prefix "hw.")
  (sysctl-name-prefix "machdep.cpu.")
  (sysctl-name "kern.argmax")
  (sysctl-name "kern.hostname")
  (sysctl-name "kern.maxfilesperproc")
  (sysctl-name "kern.maxproc")
  (sysctl-name "kern.osproductversion")
  (sysctl-name "kern.osrelease")
  (sysctl-name "kern.ostype")
  (sysctl-name "kern.osversion")
  (sysctl-name "kern.secure_kernel")
  (sysctl-name "kern.osvariant_status")
  (sysctl-name "kern.usrstack64")
  (sysctl-name "kern.version")
  (sysctl-name "kern.sysv.semmns")
  (sysctl-name "sysctl.proc_cputype")
  (sysctl-name "vm.loadavg"))
(allow file-read-metadata (vnode-type DIRECTORY))
(allow file-read*
  (require-all (vnode-type SYMLINK)
    (require-any (literal "/tmp") (literal "/var") (literal "/etc"))))
(allow file-read* file-map-executable
  (literal "/")
  (subpath "/System")
  (subpath "/usr/lib")
  (subpath "/bin")
  (subpath "/usr/bin")
  (subpath "/usr/share")
  (subpath "/private/var/db/dyld")
  (literal "/dev/null")
  (literal "/dev/urandom")
  (literal "/dev/random"))
(allow file-write-data (literal "/dev/null"))
"#,
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
        // Exact read-only files (for example an SSH identity and known-hosts
        // file) avoid exposing their whole parent directory.
        for (index, path) in policy.read_files.iter().enumerate() {
            let key = format!("F{index}");
            parameters.push("-D".into());
            parameters.push(path_parameter(&key, path));
            profile.push_str(&format!(
                "(allow file-read* file-map-executable (literal (param \"{key}\")))\n"
            ));
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
        // Repository control data and similar targets stay readable for
        // ordinary inspection but cannot be written, including paths that do
        // not exist yet inside a writable ancestor.
        for (index, path) in policy.write_denied_paths.iter().enumerate() {
            let key = format!("D{index}");
            parameters.push("-D".into());
            parameters.push(path_parameter(&key, path));
            profile.push_str(&format!("(deny file-write* (subpath (param \"{key}\")))\n"));
        }
        if let NetworkPolicy::Proxy(proxy) = &policy.network {
            let address = format!("localhost:{}", proxy.endpoint.port());
            // SBPL only accepts localhost or * here. The host broker must own
            // this port on both IPv4 and IPv6 loopback.
            profile.push_str(&format!(
                "(allow network-outbound (remote tcp \"{address}\"))\n"
            ));
        }
        if let NetworkPolicy::LoopbackPort {
            port, proxy_port, ..
        } = &policy.network
        {
            // The VM host channel can reach only the forwarded SSH port and,
            // when configured, the per-Run broker port used as the local side
            // of one reverse streamlocal forward. No DNS, proxy environment or
            // general outbound network is allowed.
            let mut rules = format!("(allow network-outbound (remote tcp \"localhost:{port}\"))\n");
            if let Some(proxy) = proxy_port {
                rules.push_str(&format!(
                    "(allow network-outbound (remote tcp \"localhost:{proxy}\"))\n"
                ));
            }
            rules.push_str(
                "(allow mach-lookup\n\
                   (global-name \"com.apple.system.opendirectoryd.libinfo\")\n\
                   (global-name \"com.apple.system.opendirectoryd.membership\")\n\
                   (global-name \"com.apple.bsd.dirhelper\"))\n",
            );
            profile.push_str(&rules);
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
        command.args.push("__soyo-sandbox-launch".into());
        command.bootstrap_index = Some(command.args.len());
        command.args.push("--".into());
        let mut payload = argv.to_vec();
        if Path::new(&payload[0]) == std::env::current_exe().map_err(super::io_error)?
            && payload
                .get(1)
                .is_some_and(|value| value == "__soyo-file-worker")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_denied_paths_emit_write_only_denies() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let scratch = tempfile::tempdir().expect("tempdir");
        let root = workspace.path().canonicalize().expect("canonical");
        let mut policy = SandboxPolicy::workspace(root.clone());
        policy.write_denied_paths = vec![
            root.join(".git/config"),
            root.join(".git/hooks"),
            root.join(".git/missing-entry"),
        ];
        let backend = SeatbeltBackend::prepare(&policy, scratch.path()).expect("prepare");
        assert!(
            backend
                .profile
                .contains("(deny file-write* (subpath (param \"D0\")))")
        );
        assert!(
            backend
                .profile
                .contains("(deny file-write* (subpath (param \"D2\")))")
        );
        assert!(
            !backend
                .profile
                .contains("file-map-executable (subpath (param \"D0\"))")
        );
        assert!(
            backend
                .parameters
                .iter()
                .any(|parameter| { parameter.to_string_lossy().starts_with("D0=") })
        );
    }
}
