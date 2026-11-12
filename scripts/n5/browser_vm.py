#!/usr/bin/env python3
"""Explicit dedicated Colima VM lifecycle for the Saya browser worker.

This tool is the only place where VM creation, installation and copying
happen. A model Run never downloads or installs anything. The runtime provider
only accepts an already provisioned VM whose identity manifest and pinned
versions it verifies again.

Subcommands:

    create    start an isolated Colima profile with no host mounts and no SSH
              agent forwarding
    provision copy the explicit Node, worker tree and headless Chromium inputs
              into the VM, derive the identity manifest from the remote files,
              upload it, and print its digest
    check     verify the remote manifest, named files, permissions, versions
              and headless-shell path without changing anything
    destroy   delete the dedicated profile

All commands print one JSON object to stdout. Failures exit nonzero and print
a diagnostic to stderr. This tool never updates versions automatically.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile

PLAYWRIGHT_MCP_VERSION = "0.0.85"
PLAYWRIGHT_VERSION = "1.66.0-alpha-2026-11-09"
CHROMIUM_REVISION = 1254
CHROMIUM_VERSION = "158.0.8127.18"

# The Rust runtime applies the same bounds. The manifest is itself a named
# remote file and must satisfy the same regular-file and permission rules.
MAX_MANIFEST_BYTES = 64 * 1024
MAX_IDENTITY_FILE_BYTES = 512 * 1024 * 1024

CLI_JS = "node_modules/@playwright/mcp/cli.js"
MCP_PACKAGE = "node_modules/@playwright/mcp/package.json"
PLAYWRIGHT_PACKAGE = "node_modules/playwright/package.json"
PLAYWRIGHT_CORE_PACKAGE = "node_modules/playwright-core/package.json"
BROWSERS_JSON = "node_modules/playwright-core/browsers.json"

PROBE_SCRIPT = r"""set -eu
LC_ALL=C
export LC_ALL
for f in "$@"; do
  stat -c '%s|%a|%F' -- "$f"
  sha256sum -- "$f"
done
"""

CAT_SCRIPT = 'set -eu\ncat -- "$1"\n'

PREFLIGHT_SCRIPT = r"""set -eu
for tool in setsid sha256sum stat tr awk head cat kill mkdir chmod rm; do
  command -v "$tool" >/dev/null 2>&1 || { echo "missing=$tool"; exit 4; }
done
setsid -w true
[ -d /proc ] || { echo "missing=procfs"; exit 4; }
echo "preflight=ok"
"""


class ToolError(Exception):
    """A controlled provisioning failure."""


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def valid_digest(value: str) -> bool:
    return len(value) == 64 and all(ch in "0123456789abcdef" for ch in value)


def safe_token(value: str, name: str, maximum: int = 64) -> str:
    if not value or len(value) > maximum:
        raise ToolError(f"{name} is invalid")
    if value in (".", ".."):
        raise ToolError(f"{name} is invalid")
    if value.startswith("-"):
        raise ToolError(f"{name} is invalid")
    if not all(ch.isalnum() or ch in "_.-" for ch in value):
        raise ToolError(f"{name} must use simple [A-Za-z0-9_.-] characters")
    return value


def safe_remote_path(value: str, name: str) -> str:
    if not value.startswith("/") or len(value) > 4096:
        raise ToolError(f"{name} must be an absolute POSIX path")
    if any(ch.isspace() or ord(ch) < 0x20 or ord(ch) == 0x7F for ch in value):
        raise ToolError(f"{name} contains whitespace or control characters")
    if ".." in value.split("/"):
        raise ToolError(f"{name} contains parent traversal")
    forbidden = set("\\'\";|&$`<>(){}[]*?!#~")
    if any(ch in forbidden for ch in value):
        raise ToolError(f"{name} contains shell metacharacters")
    return value


def loopback_host(value: str) -> str:
    """Return the bare IP literal accepted by ssh.

    Bracketed IPv6 (``[::1]``) is URL display syntax and must never reach
    HostName, argv or the resolver with brackets.
    """
    import ipaddress

    if not value or value.strip() != value:
        raise ToolError("host must be a literal loopback address")
    if value.startswith("["):
        if not value.endswith("]"):
            raise ToolError("host must be a literal loopback address")
        bare = value[1:-1]
    else:
        bare = value
    if not bare or "[" in bare or "]" in bare:
        raise ToolError("host must be a literal loopback address")
    try:
        address = ipaddress.ip_address(bare)
    except ValueError as error:
        raise ToolError("host must be a literal loopback address") from error
    if not address.is_loopback:
        raise ToolError("host must be a literal loopback address")
    return str(address)


def safe_local_path(path: Path, name: str) -> Path:
    value = str(path)
    if not path.is_absolute() or not value or len(value) > 4096:
        raise ToolError(f"{name} must be an absolute path")
    if any(ch.isspace() or ord(ch) < 0x20 or ord(ch) == 0x7F for ch in value):
        raise ToolError(f"{name} contains whitespace or control characters")
    if any(ch in value for ch in "\"'\\"):
        raise ToolError(f"{name} contains quoting characters")
    return path


def canonical_local_path(path: Path, name: str) -> Path:
    """Resolve symlinks and validate the *target* path again.

    A link whose name is harmless must not hide a target containing
    whitespace, control characters or quoting from the ssh configuration.
    """
    safe_local_path(path, name)
    try:
        resolved = path.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise ToolError(f"{name} is unavailable: {path}") from error
    safe_local_path(resolved, name)
    return resolved


def validate_local_executable(path: Path, name: str) -> Path:
    resolved = canonical_local_path(path, name)
    if not resolved.is_file():
        raise ToolError(f"{name} is not a file: {path}")
    if not os.access(resolved, os.X_OK):
        raise ToolError(f"{name} is not executable: {path}")
    return resolved


def validate_private_file(path: Path, name: str) -> Path:
    resolved = canonical_local_path(path, name)
    if not resolved.is_file():
        raise ToolError(f"{name} is not a file: {path}")
    mode = resolved.stat().st_mode & 0o777
    if mode & 0o077:
        raise ToolError(f"{name} must not be readable or writable by other accounts: {oct(mode)}")
    return resolved


def validate_read_file(path: Path, name: str) -> Path:
    resolved = canonical_local_path(path, name)
    if not resolved.is_file():
        raise ToolError(f"{name} is not a file: {path}")
    if resolved.stat().st_mode & 0o022:
        raise ToolError(f"{name} must not be writable by other accounts")
    return resolved


def colima_start_argv(args: argparse.Namespace) -> list[str]:
    safe_token(args.profile, "profile")
    argv = [
        str(args.colima),
        "start",
        "--profile",
        args.profile,
        "--cpu",
        str(args.cpu),
        "--memory",
        str(args.memory),
        "--disk",
        str(args.disk),
        # No host directory is shared with the VM.
        "--mount",
        "none",
        # The VM must never receive the host SSH agent.
        "--ssh-agent=false",
    ]
    return argv


def colima_delete_argv(args: argparse.Namespace) -> list[str]:
    safe_token(args.profile, "profile")
    return [str(args.colima), "delete", "--profile", args.profile, "--force"]


def strict_ssh_config(args: argparse.Namespace) -> str:
    safe_token(args.user, "user", 32)
    host = loopback_host(args.host)
    if not (0 < args.port < 65536):
        raise ToolError("port must be between 1 and 65535")
    if args.known_hosts.stat().st_mode & 0o022:
        raise ToolError("known_hosts must not be writable by other accounts")
    identity_file = validate_private_file(args.identity_file, "identity file")
    known_hosts = validate_read_file(args.known_hosts, "known_hosts")
    return "\n".join(
        [
            "# Saya dedicated browser VM. Contains no secret values.",
            f"Host saya-browser-vm {host}",
            f"  HostName {host}",
            f"  User {args.user}",
            f"  Port {args.port}",
            f"  IdentityFile {identity_file}",
            "  IdentitiesOnly yes",
            "  BatchMode yes",
            "  PreferredAuthentications publickey",
            "  PasswordAuthentication no",
            "  KbdInteractiveAuthentication no",
            "  StrictHostKeyChecking yes",
            f"  HostKeyAlias lima-colima-{args.profile}",
            f"  UserKnownHostsFile {known_hosts}",
            "  GlobalKnownHostsFile /dev/null",
            "  UpdateHostKeys no",
            "  CheckHostIP yes",
            "  ForwardAgent no",
            "  ForwardX11 no",
            "  ForwardX11Trusted no",
            "  PermitLocalCommand no",
            "  ClearAllForwardings yes",
            "  ControlMaster no",
            "  ControlPath none",
            "  ProxyCommand none",
            "  ProxyJump none",
            "  ConnectTimeout 20",
            "  ServerAliveInterval 15",
            "  ServerAliveCountMax 3",
            "  RequestTTY no",
            "  LogLevel ERROR",
            "",
        ]
    )


def ssh_argv(
    args: argparse.Namespace, config_path: Path, remote_command: str
) -> list[str]:
    safe_token(args.user, "user", 32)
    host = loopback_host(args.host)
    identity_file = validate_private_file(args.identity_file, "identity file")
    if not (0 < args.port < 65536):
        raise ToolError("port must be between 1 and 65535")
    return [
        str(args.ssh),
        "-F",
        str(config_path),
        "-o",
        "BatchMode=yes",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ProxyCommand=none",
        "-o",
        "ProxyJump=none",
        "-o",
        "PasswordAuthentication=no",
        "-o",
        "KbdInteractiveAuthentication=no",
        "-p",
        str(args.port),
        "-l",
        args.user,
        "-i",
        str(identity_file),
        host,
        remote_command,
    ]


def scp_argv(
    args: argparse.Namespace,
    config_path: Path,
    source: str,
    destination: str,
    recursive: bool = False,
) -> list[str]:
    safe_token(args.user, "user", 32)
    host = loopback_host(args.host)
    identity_file = validate_private_file(args.identity_file, "identity file")
    argv = [
        str(args.scp),
        "-F",
        str(config_path),
        "-o",
        "BatchMode=yes",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ProxyCommand=none",
        "-o",
        "ProxyJump=none",
        "-P",
        str(args.port),
        "-i",
        str(identity_file),
    ]
    if recursive:
        argv.append("-r")
    # scp parses `host:path`; IPv6 needs brackets for its own grammar even
    # though ssh and the resolver receive the bare literal.
    authority = f"[{host}]" if ":" in host else host
    argv.extend([source, f"{args.user}@{authority}:{destination}"])
    return argv


def remote_command(script: str, arguments: list[str]) -> str:
    return " ".join(["sh", "-c", shlex.quote(script), "saya-browser-vm"] + [shlex.quote(value) for value in arguments])


def run_command(argv: list[str], input_bytes: bytes | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        argv,
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def checked(argv: list[str], input_bytes: bytes | None = None) -> bytes:
    result = run_command(argv, input_bytes)
    if result.returncode != 0:
        message = result.stderr.decode("utf-8", "replace").strip()
        raise ToolError(f"command failed ({result.returncode}): {message or argv[0]}")
    return result.stdout


def ssh_run(args: argparse.Namespace, config_path: Path, script: str, arguments: list[str]) -> bytes:
    return checked(ssh_argv(args, config_path, remote_command(script, arguments)))


def parse_probe(output: str, count: int) -> list[dict]:
    lines = output.splitlines()
    if len(lines) != count * 2:
        raise ToolError("remote identity probe returned an unexpected number of lines")
    records = []
    for index in range(count):
        stat_line = lines[index * 2]
        hash_line = lines[index * 2 + 1]
        fields = stat_line.split("|")
        if len(fields) != 3:
            raise ToolError("remote identity probe line is malformed")
        size_text, mode_text, kind = fields
        try:
            size = int(size_text, 10)
            mode = int(mode_text, 8)
        except ValueError as error:
            raise ToolError("remote identity size or mode is invalid") from error
        digest = hash_line.split(None, 1)[0]
        if not valid_digest(digest):
            raise ToolError("remote identity digest is invalid")
        records.append({"size": size, "mode": mode, "kind": kind, "sha256": digest})
    return records


def require_regular(
    record: dict,
    label: str,
    executable: bool,
    maximum: int = MAX_IDENTITY_FILE_BYTES,
) -> None:
    if record["kind"] not in ("regular file", "regular empty file"):
        raise ToolError(f"{label} is not a regular file")
    size = record["size"]
    if not isinstance(size, int) or isinstance(size, bool) or size < 0:
        raise ToolError(f"{label} size is invalid")
    if size > maximum:
        raise ToolError(f"{label} is larger than {maximum} bytes")
    mode = record["mode"]
    if mode & 0o022 or mode & 0o6000 or not mode & 0o400:
        raise ToolError(f"{label} permissions are unsafe")
    if executable and not mode & 0o111:
        raise ToolError(f"{label} is not executable")


def parse_package_bytes(data: bytes, name: str) -> dict:
    try:
        package = json.loads(data)
    except json.JSONDecodeError as error:
        raise ToolError(f"{name} package metadata is invalid") from error
    if not isinstance(package, dict) or package.get("name") != name:
        raise ToolError(f"{name} package metadata is invalid")
    return package


def parse_browsers_bytes(data: bytes) -> dict:
    try:
        browsers = json.loads(data)
    except json.JSONDecodeError as error:
        raise ToolError("browsers.json is invalid") from error
    records = browsers.get("browsers")
    if not isinstance(records, list):
        raise ToolError("browsers.json is invalid")
    for record in records:
        if isinstance(record, dict) and record.get("name") == "chromium":
            return record
    raise ToolError("browsers.json does not declare Chromium")


def build_manifest(
    records: list[dict],
    package_bytes: dict[str, bytes],
    browsers_bytes: bytes,
    node_version: str,
) -> dict:
    if len(records) != 7:
        raise ToolError("identity probe must cover seven named files")
    mcp = parse_package_bytes(package_bytes["mcp"], "@playwright/mcp")
    playwright = parse_package_bytes(package_bytes["playwright"], "playwright")
    core = parse_package_bytes(package_bytes["playwright_core"], "playwright-core")
    if mcp.get("version") != PLAYWRIGHT_MCP_VERSION:
        raise ToolError("@playwright/mcp version is not the pinned version")
    if playwright.get("version") != PLAYWRIGHT_VERSION or core.get("version") != PLAYWRIGHT_VERSION:
        raise ToolError("Playwright version is not the pinned version")
    chromium = parse_browsers_bytes(browsers_bytes)
    if str(chromium.get("revision")) != str(CHROMIUM_REVISION):
        raise ToolError("Chromium revision is not the pinned revision")
    if chromium.get("browserVersion") != CHROMIUM_VERSION:
        raise ToolError("Chromium version is not the pinned version")
    if node_version != node_version.strip() or not node_version.startswith("v"):
        raise ToolError("Node version must be the exact version reported by node --version")
    return {
        "schema_version": 1,
        "node": {
            "sha256": records[0]["sha256"],
            "size": records[0]["size"],
            "version": node_version,
        },
        "worker_cli_js": {"sha256": records[1]["sha256"], "size": records[1]["size"]},
        "mcp_package": {
            "sha256": records[2]["sha256"],
            "size": records[2]["size"],
            "version": PLAYWRIGHT_MCP_VERSION,
        },
        "playwright_package": {
            "sha256": records[3]["sha256"],
            "size": records[3]["size"],
            "version": PLAYWRIGHT_VERSION,
        },
        "playwright_core_package": {
            "sha256": records[4]["sha256"],
            "size": records[4]["size"],
            "version": PLAYWRIGHT_VERSION,
        },
        "playwright_core_browsers_json": {
            "sha256": records[5]["sha256"],
            "size": records[5]["size"],
        },
        "chromium": {
            "sha256": records[6]["sha256"],
            "size": records[6]["size"],
            "version": CHROMIUM_VERSION,
            "revision": CHROMIUM_REVISION,
        },
    }


def remote_layout(args: argparse.Namespace) -> dict[str, str]:
    root = safe_remote_path(args.remote_root, "remote root").rstrip("/")
    if not root:
        raise ToolError("remote root must be narrower than the filesystem root")
    chromium_dir_name = safe_token(args.chromium_dir_name, "chromium directory name", 128)
    layout = {
        "root": root,
        "node": f"{root}/node/bin/node",
        "worker": f"{root}/worker",
        "chromium": (
            f"{root}/browsers/chromium_headless_shell-{CHROMIUM_REVISION}/"
            f"{chromium_dir_name}/chrome-headless-shell"
        ),
        "manifest": f"{root}/identity.json",
        "runs": f"{root}/runs",
    }
    # Every generated path is used in remote scripts; validate the final
    # strings rather than trusting interpolation of already-checked pieces.
    for name, path in layout.items():
        safe_remote_path(path, f"generated {name} path")
    return layout


def named_paths(layout: dict[str, str]) -> list[str]:
    return [
        layout["node"],
        f"{layout['worker']}/{CLI_JS}",
        f"{layout['worker']}/{MCP_PACKAGE}",
        f"{layout['worker']}/{PLAYWRIGHT_PACKAGE}",
        f"{layout['worker']}/{PLAYWRIGHT_CORE_PACKAGE}",
        f"{layout['worker']}/{BROWSERS_JSON}",
        layout["chromium"],
    ]


def probe_paths(
    args: argparse.Namespace, config_path: Path, paths: list[str]
) -> list[dict]:
    output = ssh_run(args, config_path, PROBE_SCRIPT, paths).decode("utf-8")
    return parse_probe(output, len(paths))


def identity_probe(
    args: argparse.Namespace, config_path: Path, layout: dict[str, str]
) -> tuple[list[dict], dict[str, bytes], bytes]:
    paths = named_paths(layout)
    records = probe_paths(args, config_path, paths)
    for index, path in enumerate(paths):
        require_regular(records[index], path, executable=index in (0, 6))
    package_bytes = {}
    for index, key, path in (
        (2, "mcp", f"{layout['worker']}/{MCP_PACKAGE}"),
        (3, "playwright", f"{layout['worker']}/{PLAYWRIGHT_PACKAGE}"),
        (4, "playwright_core", f"{layout['worker']}/{PLAYWRIGHT_CORE_PACKAGE}"),
    ):
        data = ssh_run(args, config_path, CAT_SCRIPT, [path])
        if len(data) != records[index]["size"]:
            raise ToolError(f"{path} changed size while it was being read")
        package_bytes[key] = data
    browsers_bytes = ssh_run(
        args, config_path, CAT_SCRIPT, [f"{layout['worker']}/{BROWSERS_JSON}"]
    )
    if len(browsers_bytes) != records[5]["size"]:
        raise ToolError("browsers.json changed size while it was being read")
    return records, package_bytes, browsers_bytes


def verify_remote_identity(
    args: argparse.Namespace,
    config_path: Path,
    layout: dict[str, str],
    expected_manifest: bytes | None = None,
    expected_digest: str | None = None,
) -> tuple[dict, str, str]:
    """Verify the remote manifest and every file it names.

    The manifest is a named remote file too: it must be a regular file no
    larger than 64 KiB with safe permissions. Every named file must be a
    regular file no larger than 512 MiB, match its declared size and digest,
    and satisfy the runtime permission rules.
    """
    paths = [layout["manifest"]] + named_paths(layout)
    records = probe_paths(args, config_path, paths)
    require_regular(
        records[0],
        "remote manifest",
        executable=False,
        maximum=MAX_MANIFEST_BYTES,
    )

    remote_manifest = ssh_run(args, config_path, CAT_SCRIPT, [layout["manifest"]])
    if len(remote_manifest) != records[0]["size"]:
        raise ToolError("remote manifest changed size while it was being read")
    digest = sha256_bytes(remote_manifest)
    if expected_manifest is not None and remote_manifest != expected_manifest:
        raise ToolError("remote manifest differs from the expected manifest")
    if expected_digest is not None and digest != expected_digest:
        raise ToolError("remote manifest digest does not match the expected digest")
    try:
        manifest = json.loads(remote_manifest)
    except json.JSONDecodeError as error:
        raise ToolError("remote manifest is invalid") from error
    manifest = validate_manifest_structure(manifest)

    named = records[1:]
    for index, path in enumerate(named_paths(layout)):
        require_regular(named[index], path, executable=index in (0, 6))
    declared = [
        manifest["node"],
        manifest["worker_cli_js"],
        manifest["mcp_package"],
        manifest["playwright_package"],
        manifest["playwright_core_package"],
        manifest["playwright_core_browsers_json"],
        manifest["chromium"],
    ]
    for index, entry in enumerate(declared):
        if named[index]["size"] != entry["size"]:
            raise ToolError("named file size does not match the manifest")
        if named[index]["sha256"] != entry["sha256"]:
            raise ToolError("named file digest does not match the manifest")

    package_bytes = {}
    for key, path, entry in (
        ("mcp", f"{layout['worker']}/{MCP_PACKAGE}", manifest["mcp_package"]),
        (
            "playwright",
            f"{layout['worker']}/{PLAYWRIGHT_PACKAGE}",
            manifest["playwright_package"],
        ),
        (
            "playwright_core",
            f"{layout['worker']}/{PLAYWRIGHT_CORE_PACKAGE}",
            manifest["playwright_core_package"],
        ),
    ):
        data = ssh_run(args, config_path, CAT_SCRIPT, [path])
        if len(data) != entry["size"]:
            raise ToolError(f"{path} changed size while it was being read")
        package_bytes[key] = data
    browsers_bytes = ssh_run(
        args, config_path, CAT_SCRIPT, [f"{layout['worker']}/{BROWSERS_JSON}"]
    )
    if len(browsers_bytes) != manifest["playwright_core_browsers_json"]["size"]:
        raise ToolError("browsers.json changed size while it was being read")

    rebuilt = build_manifest(
        named, package_bytes, browsers_bytes, manifest["node"]["version"]
    )
    if rebuilt != manifest:
        raise ToolError("remote files do not match the uploaded manifest")
    version = ssh_run(
        args,
        config_path,
        'set -eu\n"$1" --version\n',
        [layout["node"]],
    ).decode("utf-8").strip()
    if version != manifest["node"]["version"]:
        raise ToolError("remote Node version does not match the manifest")
    return manifest, digest, version


def command_create(args: argparse.Namespace) -> dict:
    validate_local_executable(args.colima, "colima")
    checked(colima_start_argv(args))
    listing = checked([str(args.colima), "list", "--json"]).decode("utf-8")
    profiles = json.loads(listing)
    if isinstance(profiles, dict):
        profiles = [profiles]
    names = {
        entry.get("name")
        for entry in profiles
        if isinstance(entry, dict) and entry.get("name")
    }
    if args.profile not in names:
        raise ToolError("the dedicated Colima profile was not created")
    return {
        "action": "create",
        "profile": args.profile,
        "mount": "none",
        "ssh_agent": False,
    }


def command_destroy(args: argparse.Namespace) -> dict:
    validate_local_executable(args.colima, "colima")
    checked(colima_delete_argv(args))
    return {"action": "destroy", "profile": args.profile}


def command_provision(args: argparse.Namespace) -> dict:
    validate_local_executable(args.ssh, "ssh")
    validate_local_executable(args.scp, "scp")
    node_source = validate_local_executable(args.node_source, "node source")
    validate_private_file(args.identity_file, "identity file")
    validate_read_file(args.known_hosts, "known_hosts")
    worker_source = canonical_local_path(args.worker_source, "worker source")
    if not worker_source.is_dir():
        raise ToolError(f"worker source is not a directory: {worker_source}")
    if worker_source.name != "worker":
        raise ToolError(
            "worker source directory name must be 'worker' to match the remote layout"
        )
    chromium_source = canonical_local_path(args.chromium_source, "chromium source")
    if not chromium_source.is_file():
        raise ToolError(f"chromium source is not a file: {chromium_source}")
    if chromium_source.name != "chrome-headless-shell":
        raise ToolError("chromium source must name chrome-headless-shell")
    if chromium_source.parent.name != safe_token(
        args.chromium_dir_name, "chromium directory name", 128
    ):
        raise ToolError(
            "chromium directory name must match the chromium source parent directory"
        )
    layout = remote_layout(args)
    config = strict_ssh_config(args)
    with tempfile.TemporaryDirectory(prefix="saya-browser-vm-") as temporary:
        config_path = Path(temporary) / "ssh_config"
        config_path.write_text(config, encoding="utf-8")
        os.chmod(config_path, 0o600)
        preflight = ssh_run(args, config_path, PREFLIGHT_SCRIPT, []).decode("utf-8").strip()
        if preflight != "preflight=ok":
            raise ToolError("VM does not provide the required worker runtime tools")
        ssh_run(
            args,
            config_path,
            'set -eu\nmkdir -p -- "$1" "$2" "$3" "$4" "$5"\n',
            [
                f"{layout['root']}/node/bin",
                f"{layout['worker']}",
                f"{layout['root']}/browsers/chromium_headless_shell-{CHROMIUM_REVISION}",
                layout["runs"],
                f"{layout['root']}",
            ],
        )
        checked(scp_argv(args, config_path, str(node_source), layout["node"]))
        # Preserve the worker tree exactly as provided; no package manager runs.
        archive = Path(temporary) / "worker.tar"
        checked(
            [
                "tar",
                "-C",
                str(worker_source.parent),
                "-cf",
                str(archive),
                worker_source.name,
            ]
        )
        checked(
            scp_argv(
                args,
                config_path,
                str(archive),
                f"{layout['root']}/worker.tar",
            )
        )
        ssh_run(
            args,
            config_path,
            'set -eu\ntar -C "$1" -xf "$2"\nrm -f -- "$2"\n',
            [layout["root"], f"{layout['root']}/worker.tar"],
        )
        checked(
            scp_argv(
                args,
                config_path,
                str(chromium_source.parent),
                f"{layout['root']}/browsers/chromium_headless_shell-{CHROMIUM_REVISION}/",
                recursive=True,
            )
        )
        records, package_bytes, browsers_bytes = identity_probe(args, config_path, layout)
        version = ssh_run(
            args,
            config_path,
            'set -eu\n"$1" --version\n',
            [layout["node"]],
        ).decode("utf-8").strip()
        manifest = build_manifest(records, package_bytes, browsers_bytes, version)
        manifest_bytes = json.dumps(manifest, indent=2, sort_keys=True).encode("utf-8") + b"\n"
        if len(manifest_bytes) > MAX_MANIFEST_BYTES:
            raise ToolError("generated manifest exceeds 64 KiB")
        manifest_path = Path(temporary) / "identity.json"
        manifest_path.write_bytes(manifest_bytes)
        checked(
            scp_argv(args, config_path, str(manifest_path), layout["manifest"])
        )
        digest = sha256_bytes(manifest_bytes)
        # Reuse the same checks as `check`: probe the uploaded manifest itself
        # (regular file, size, mode) and every named file before reporting the
        # digest as usable.
        _, remote_digest, version = verify_remote_identity(
            args,
            config_path,
            layout,
            expected_manifest=manifest_bytes,
            expected_digest=digest,
        )
        if remote_digest != digest:
            raise ToolError("uploaded manifest digest does not match")
    return {
        "action": "provision",
        "profile": args.profile,
        "node": layout["node"],
        "worker_root": layout["worker"],
        "chromium": layout["chromium"],
        "identity_manifest": layout["manifest"],
        "identity_sha256": digest,
        "remote_temp_root": layout["runs"],
        "node_version": version,
    }


def validate_manifest_structure(manifest) -> dict:
    """Reject unexpected manifest shapes with a controlled ToolError.

    The remote manifest is untrusted input. Missing groups or a nested value of
    the wrong type must not escape as a traceback (TypeError/AttributeError)
    or be silently treated as a valid manifest.
    """
    if not isinstance(manifest, dict):
        raise ToolError("remote manifest is invalid")
    if manifest.get("schema_version") != 1:
        raise ToolError("remote manifest schema version is unsupported")
    node = manifest.get("node")
    if not isinstance(node, dict):
        raise ToolError("remote manifest node entry is invalid")
    for name in ("node", "worker_cli_js", "mcp_package", "playwright_package",
                 "playwright_core_package", "playwright_core_browsers_json"):
        entry = manifest.get(name)
        if not isinstance(entry, dict):
            raise ToolError(f"remote manifest {name} entry is invalid")
        digest_value = entry.get("sha256")
        size = entry.get("size")
        if not isinstance(digest_value, str) or not valid_digest(digest_value):
            raise ToolError(f"remote manifest {name} digest is invalid")
        if not isinstance(size, int) or isinstance(size, bool) or size < 0:
            raise ToolError(f"remote manifest {name} size is invalid")
    if not isinstance(node.get("version"), str) or not node["version"].startswith("v"):
        raise ToolError("remote manifest Node version is invalid")
    chromium = manifest.get("chromium")
    if not isinstance(chromium, dict):
        raise ToolError("remote manifest chromium entry is invalid")
    if not isinstance(chromium.get("version"), str) or not isinstance(
        chromium.get("revision"), int
    ):
        raise ToolError("remote manifest chromium entry is invalid")
    return manifest


def command_check(args: argparse.Namespace, manifest_bytes: bytes | None = None) -> dict:
    validate_local_executable(args.ssh, "ssh")
    validate_private_file(args.identity_file, "identity file")
    validate_read_file(args.known_hosts, "known_hosts")
    layout = remote_layout(args)
    config = strict_ssh_config(args)
    with tempfile.TemporaryDirectory(prefix="saya-browser-vm-") as temporary:
        config_path = Path(temporary) / "ssh_config"
        config_path.write_text(config, encoding="utf-8")
        os.chmod(config_path, 0o600)
        if not valid_digest(args.manifest_sha256):
            raise ToolError("--manifest-sha256 must be lowercase SHA-256")
        _, digest, version = verify_remote_identity(
            args,
            config_path,
            layout,
            expected_manifest=manifest_bytes,
            expected_digest=args.manifest_sha256,
        )
    return {
        "action": "check",
        "profile": args.profile,
        "identity_sha256": digest,
        "node_version": version,
        "chromium_version": CHROMIUM_VERSION,
        "chromium_revision": CHROMIUM_REVISION,
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    create = subparsers.add_parser("create", help="start the dedicated Colima profile")
    create.add_argument("--colima", type=Path, required=True)
    create.add_argument("--profile", required=True)
    create.add_argument("--cpu", type=int, default=2)
    create.add_argument("--memory", type=int, default=4)
    create.add_argument("--disk", type=int, default=60)
    create.set_defaults(handler=command_create)

    destroy = subparsers.add_parser("destroy", help="delete the dedicated Colima profile")
    destroy.add_argument("--colima", type=Path, required=True)
    destroy.add_argument("--profile", required=True)
    destroy.set_defaults(handler=command_destroy)

    for name, handler in (("provision", command_provision), ("check", command_check)):
        command = subparsers.add_parser(name)
        command.add_argument("--colima", type=Path)
        command.add_argument("--profile", required=True)
        command.add_argument("--ssh", type=Path, required=True)
        command.add_argument("--scp", type=Path, default=Path("/usr/bin/scp"))
        command.add_argument("--host", required=True)
        command.add_argument("--user", required=True)
        command.add_argument("--port", type=int, required=True)
        command.add_argument("--identity-file", type=Path, required=True)
        command.add_argument("--known-hosts", type=Path, required=True)
        command.add_argument("--remote-root", default="/opt/saya")
        command.add_argument("--chromium-dir-name", default="chrome-headless-shell-linux64")
        command.add_argument(
            "--manifest-sha256", required=(name == "check")
        )
        if name == "provision":
            command.add_argument("--node-source", type=Path, required=True)
            command.add_argument("--worker-source", type=Path, required=True)
            command.add_argument("--chromium-source", type=Path, required=True)
        command.set_defaults(handler=handler)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if getattr(args, "colima", None):
            validate_local_executable(args.colima, "colima")
        result = args.handler(args)
    except ToolError as error:
        print(f"browser_vm: {error}", file=sys.stderr)
        return 2
    except (OSError, ValueError, KeyError, TypeError, AttributeError, IndexError) as error:
        print(f"browser_vm: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
