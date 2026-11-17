#!/usr/bin/env python3
"""Explicit dedicated Colima VM lifecycle for the Soyo browser worker.

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
    check     verify the remote manifest, named files, permissions, versions,
              headless-shell path, network prerequisites (sudo/unshare/socat/
              setpriv/ip route/nsenter and Chromium user-namespace/AppArmor
              values) and that sshd permits reverse streamlocal forwarding,
              without changing anything
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
import stat
import struct
import time

PLAYWRIGHT_MCP_VERSION = "0.0.85"
PLAYWRIGHT_VERSION = "1.66.0-alpha-2026-11-09"
CHROMIUM_REVISION = 1254
CHROMIUM_VERSION = "158.0.8127.18"

# The Rust runtime applies the same bounds. The manifest is itself a named
# remote file and must satisfy the same regular-file and permission rules.
MAX_MANIFEST_BYTES = 64 * 1024
MAX_IDENTITY_FILE_BYTES = 512 * 1024 * 1024
MAX_TREE_ENTRIES = 65536

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

# Every prerequisite the in-VM private-network launch depends on. The check is
# read-only apart from creating and discarding a throwaway network namespace.
# Chromium's own sandbox needs unprivileged user namespaces, which AppArmor can
# restrict on Ubuntu 24.04; the dedicated VM may opt into disabling that
# restriction during `provision`.
NETWORK_PREFLIGHT_SCRIPT = r"""set -eu
for tool in sudo unshare socat setpriv ip nsenter; do
  command -v "$tool" >/dev/null 2>&1 || { echo "missing=$tool" >&2; exit 4; }
done
ip -o route show >/dev/null 2>&1 || { echo "missing=ip-route" >&2; exit 4; }
sudo -n true >/dev/null 2>&1 || { echo "missing=sudo-n" >&2; exit 4; }
sudo -n unshare -n -- /bin/sh -c 'set -eu; ip link set lo up; ip -o route show >/dev/null' \
  || { echo "missing=unshare-net" >&2; exit 4; }
# The real network test enters the worker's private netns with sudo nsenter.
sudo -n nsenter -t "$$" -n true >/dev/null 2>&1 \
  || { echo "missing=sudo-nsenter" >&2; exit 4; }
# Chromium creates user namespaces for its own sandbox.
unshare -Ur true >/dev/null 2>&1 || { echo "missing=user-namespace" >&2; exit 4; }
if [ -e /proc/sys/kernel/unprivileged_userns_clone ]; then
  clone=$(cat /proc/sys/kernel/unprivileged_userns_clone)
  [ "$clone" = "1" ] || { echo "missing=unprivileged-userns" >&2; exit 4; }
fi
if [ -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]; then
  restrict=$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns)
  [ "$restrict" = "0" ] || { echo "missing=apparmor-userns" >&2; exit 4; }
fi
echo "network=preflight-ok"
"""

# The runtime's only browser egress is a reverse streamlocal forward. A client
# that offers `-R` is not enough: sshd must actually permit it. `sshd -T`
# reports the effective (defaults and includes applied) server policy.
SSHD_FORWARDING_SCRIPT = r"""set -eu
sshd_bin=$(command -v sshd 2>/dev/null || true)
[ -n "$sshd_bin" ] || sshd_bin=/usr/sbin/sshd
[ -x "$sshd_bin" ] || { echo "sshd=missing" >&2; exit 4; }
sudo -n true >/dev/null 2>&1 || { echo "sshd=sudo-unavailable" >&2; exit 4; }
config=$(sudo -n "$sshd_bin" -T 2>/dev/null) || { echo "sshd=query-failed" >&2; exit 4; }
streamlocal=$(printf '%s\n' "$config" | awk '$1=="allowstreamlocalforwarding"{print $2; exit}')
case "$streamlocal" in
  yes|all) ;;
  '') echo "sshd=streamlocal-forwarding-unset" >&2; exit 4 ;;
  *) echo "sshd=streamlocal-forwarding-$streamlocal" >&2; exit 4 ;;
esac
# Reverse streamlocal forwarding is the only forward used, but sshd
# deployments commonly gate forwarding as a family; reject an explicit "no".
tcp=$(printf '%s\n' "$config" | awk '$1=="allowtcpforwarding"{print $2; exit}')
case "$tcp" in
  yes|all|'') ;;
  *) echo "sshd=tcp-forwarding-$tcp" >&2; exit 4 ;;
esac
echo "sshd=forwarding-ok"
"""

# Explicit, dedicated-VM-only opt-in. This never runs on the host and never
# applies to any other profile: it writes a sysctl drop-in inside the VM so
# Chromium can create unprivileged user namespaces across reboots.
APPARMOR_USERNS_SCRIPT = r"""set -eu
if [ ! -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]; then
  echo "apparmor-userns=absent"
  exit 0
fi
sudo -n true >/dev/null 2>&1 || { echo "apparmor-userns=sudo-unavailable" >&2; exit 4; }
printf '%s\n' 'kernel.apparmor_restrict_unprivileged_userns=0' \
  | sudo -n tee /etc/sysctl.d/99-soyo-browser-userns.conf >/dev/null
sudo -n sysctl -w kernel.apparmor_restrict_unprivileged_userns=0 >/dev/null
restrict=$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns)
[ "$restrict" = "0" ] || { echo "apparmor-userns=restricted" >&2; exit 4; }
echo "apparmor-userns=disabled"
"""


class ToolError(Exception):
    """A controlled provisioning failure."""


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def valid_digest(value: str) -> bool:
    return len(value) == 64 and all(ch in "0123456789abcdef" for ch in value)


def _tree_text(value: str) -> bytes:
    try:
        data = value.encode("utf-8", "strict")
    except UnicodeError as error:
        raise ToolError("installation tree path is not UTF-8") from error
    if len(data) > 4096 or any(ord(ch) < 32 or 127 <= ord(ch) <= 159 for ch in value):
        raise ToolError("installation tree path is unsafe or too long")
    return data


def _metadata_identity(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mode, info.st_nlink,
            info.st_mtime_ns, info.st_ctime_ns)


def _open_tree_root(path: Path) -> int:
    if not path.is_absolute() or ".." in path.parts:
        raise ToolError("installation tree root must be an absolute safe path")
    _tree_text(str(path))
    descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in path.parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW |
                            os.O_NONBLOCK, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def installation_tree(path: Path) -> dict:
    """Hash a bounded installation without following directory or member links."""
    deadline = time.monotonic() + 20
    hasher = hashlib.sha256(b"saya-browser-installation-v2\0")
    members = {}
    total = 0
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC

    def tick():
        if time.monotonic() >= deadline:
            raise ToolError("installation tree deadline exceeded")

    def unchanged(before, after):
        if _metadata_identity(before) != _metadata_identity(after):
            raise ToolError("installation tree changed during verification")

    def scan(parent, name, relative, depth, device):
        nonlocal total
        tick()
        if len(members) >= MAX_TREE_ENTRIES:
            raise ToolError("installation tree entry limit exceeded")
        encoded = _tree_text(relative)
        before = os.fstat(parent) if name is None else os.stat(
            name, dir_fd=parent, follow_symlinks=False)
        if before.st_dev != device:
            raise ToolError("installation tree crosses a device boundary")
        mode = stat.S_IMODE(before.st_mode)
        if stat.S_ISDIR(before.st_mode):
            kind = b"D"
        elif stat.S_ISREG(before.st_mode):
            kind = b"F"
        elif stat.S_ISLNK(before.st_mode):
            kind = b"L"
        else:
            raise ToolError("installation tree contains a special file")
        if kind == b"D" and depth > 128:
            raise ToolError("installation tree directory depth limit exceeded")
        if kind == b"L" and before.st_nlink != 1:
            raise ToolError("installation tree contains a hardlinked symlink")
        if kind != b"L" and (mode & 0o6022 or not mode & 0o400 or
                              (kind == b"D" and not mode & 0o100)):
            raise ToolError("installation tree permissions are unsafe")
        # Link permission bits differ by platform and do not gate dereferencing.
        encoded_mode = 0o777 if kind == b"L" else mode
        hasher.update(kind + struct.pack(">I", len(encoded)) + encoded + struct.pack(">I", encoded_mode))
        members[relative] = (kind, None)
        if kind == b"L":
            target = os.readlink(name, dir_fd=parent)
            target_bytes = _tree_text(target)
            hasher.update(struct.pack(">I", len(target_bytes)) + target_bytes)
            members[relative] = (kind, target)
            unchanged(before, os.stat(name, dir_fd=parent, follow_symlinks=False))
            return
        descriptor = os.dup(parent) if name is None else os.open(
            name, flags | (os.O_DIRECTORY if kind == b"D" else 0), dir_fd=parent)
        try:
            unchanged(before, os.fstat(descriptor))
            if kind == b"D":
                names = []
                with os.scandir(descriptor) as entries:
                    for entry in entries:
                        tick()
                        if len(members) + len(names) >= MAX_TREE_ENTRIES:
                            raise ToolError("installation tree entry limit exceeded")
                        names.append(entry.name)
                for child in sorted(names, key=_tree_text):
                    scan(descriptor, child, f"{relative}/{child}" if relative else child,
                         depth + 1, device)
            else:
                if before.st_nlink != 1 or before.st_size > MAX_IDENTITY_FILE_BYTES:
                    raise ToolError("installation file is hardlinked or oversized")
                total += before.st_size
                if total > 4294967296:
                    raise ToolError("installation tree byte limit exceeded")
                content = hashlib.sha256()
                size = 0
                while True:
                    tick()
                    block = os.read(descriptor, 64 * 1024)
                    if not block:
                        break
                    size += len(block)
                    if size > before.st_size:
                        raise ToolError("installation file grew during verification")
                    content.update(block)
                if size != before.st_size:
                    raise ToolError("installation file changed size")
                os.lseek(descriptor, 0, os.SEEK_SET)
                repeated_content = hashlib.sha256()
                repeated_size = 0
                while True:
                    tick()
                    block = os.read(descriptor, 64 * 1024)
                    if not block:
                        break
                    repeated_size += len(block)
                    if repeated_size > before.st_size:
                        raise ToolError("installation file grew during verification")
                    repeated_content.update(block)
                if repeated_size != size or repeated_content.digest() != content.digest():
                    raise ToolError("installation file content changed during verification")
                hasher.update(struct.pack(">Q", size) + content.digest())
            unchanged(before, os.fstat(descriptor))
            if name is not None:
                unchanged(before, os.stat(name, dir_fd=parent, follow_symlinks=False))
        finally:
            os.close(descriptor)

    try:
        root = _open_tree_root(Path(path))
        try:
            scan(root, None, "", 0, os.fstat(root).st_dev)
        finally:
            os.close(root)
        for relative, (kind, target) in members.items():
            if kind != b"L":
                continue
            current = relative.split("/")[:-1]
            remaining = target.split("/")
            hops = 0
            while remaining:
                tick()
                component = remaining.pop(0)
                if component in ("", "."):
                    continue
                if component == "..":
                    if not current:
                        raise ToolError("installation link escapes root")
                    current.pop()
                    continue
                current.append(component)
                key = "/".join(current)
                if key not in members:
                    raise ToolError("installation link is dangling")
                child_kind, child_target = members[key]
                if child_kind == b"L":
                    hops += 1
                    if hops > 40 or child_target.startswith("/"):
                        raise ToolError("installation link is cyclic or absolute")
                    current.pop()
                    remaining = child_target.split("/") + remaining
                elif remaining and child_kind != b"D":
                    raise ToolError("installation link traverses a regular file")
            if target.startswith("/") or not target:
                raise ToolError("installation link is absolute or empty")
        return {"sha256": hasher.hexdigest(), "entries": len(members), "bytes": total}
    except (OSError, ValueError, UnicodeError) as error:
        raise ToolError(f"installation tree cannot be verified: {error}") from error


def validate_installation_trees(trees) -> dict:
    if not isinstance(trees, dict) or set(trees) != {"node", "worker", "chromium"}:
        raise ToolError("installation trees must name node, worker and chromium")
    for tree in trees.values():
        if not isinstance(tree, dict) or set(tree) != {"sha256", "entries", "bytes"}:
            raise ToolError("installation tree entry is invalid")
        if not isinstance(tree["sha256"], str) or not valid_digest(tree["sha256"]):
            raise ToolError("installation tree digest is invalid")
        for key, lower, upper in (("entries", 1, MAX_TREE_ENTRIES), ("bytes", 0, 4294967296)):
            if type(tree[key]) is not int or not lower <= tree[key] <= upper:
                raise ToolError("installation tree count is invalid")
    return trees


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
        "--activate=false",
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
            "# Soyo dedicated browser VM. Contains no secret values.",
            f"Host soyo-browser-vm {host}",
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
    return " ".join(["sh", "-c", shlex.quote(script), "soyo-browser-vm"] + [shlex.quote(value) for value in arguments])


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


def ssh_run(args: argparse.Namespace, config_path: Path, script: str,
            arguments: list[str], input_bytes: bytes | None = None) -> bytes:
    return checked(ssh_argv(args, config_path, remote_command(script, arguments)), input_bytes)


INSTALLATION_PROBE_SCRIPT = 'set -eu\ncd /\nexec env -i PATH=/usr/bin:/bin "$1" --no-addons - "$2" "$3" "$4" "$5"\n'


def require_fresh_remote_root(args, config_path, root: str) -> None:
    ssh_run(args, config_path, 'set -eu\numask 077\nmkdir -m 700 -- "$1"\n', [root])


def remote_installation_trees(args, config_path, layout: dict) -> dict:
    # Read at call time: the shared probe is a repository asset, not user input.
    probe_path = Path(__file__).resolve().parents[2] / "crates/soyo-core/src/browser/worker/installation_probe.js"
    try:
        source = probe_path.read_bytes()
    except OSError as error:
        raise ToolError("installation probe asset cannot be read") from error
    reply = ssh_run(args, config_path, INSTALLATION_PROBE_SCRIPT,
                    [layout["node"], str(Path(layout["node"]).parent), layout["worker"],
                     str(Path(layout["chromium"]).parent), "20000"], source)
    if len(reply) > 4096:
        raise ToolError("installation probe reply exceeds its bound")
    return validate_installation_trees(strict_json(reply))


def strict_json(payload: bytes):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ToolError("identity JSON has duplicate fields")
            result[key] = value
        return result
    try:
        return json.loads(payload, object_pairs_hook=unique)
    except (ValueError, UnicodeError) as error:
        raise ToolError("identity JSON is invalid") from error


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
    installation_trees: dict,
) -> dict:
    validate_installation_trees(installation_trees)
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
        "schema_version": 2,
        "installation_trees": installation_trees,
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
        manifest = strict_json(remote_manifest)
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
        named, package_bytes, browsers_bytes, manifest["node"]["version"],
        manifest["installation_trees"],
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
    trees = remote_installation_trees(args, config_path, layout)
    if trees != manifest["installation_trees"]:
        raise ToolError("remote installation trees do not match the manifest")
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


def verify_network_prerequisites(
    args: argparse.Namespace, config_path: Path, configure_userns: bool = False
) -> None:
    """Verify the dedicated VM can satisfy the browser network prerequisites.

    `configure_userns` is an explicit provision-only opt-in for this dedicated
    VM. It writes a sysctl drop-in inside the VM and never touches the host.
    `check` always verifies the current value and the required tools.
    """
    if configure_userns:
        output = ssh_run(args, config_path, APPARMOR_USERNS_SCRIPT, []).decode(
            "utf-8"
        ).strip()
        if output not in ("apparmor-userns=disabled", "apparmor-userns=absent"):
            raise ToolError(f"VM AppArmor user-namespace prerequisite failed: {output}")
    result = ssh_run(args, config_path, NETWORK_PREFLIGHT_SCRIPT, []).decode(
        "utf-8"
    ).strip()
    if result != "network=preflight-ok":
        raise ToolError("VM network prerequisites are unavailable")
    forwarding = ssh_run(args, config_path, SSHD_FORWARDING_SCRIPT, []).decode(
        "utf-8"
    ).strip()
    if forwarding != "sshd=forwarding-ok":
        raise ToolError(
            f"VM sshd does not permit the browser reverse streamlocal forward: {forwarding}"
        )


def snapshot_node(source: Path, destination: Path) -> dict:
    """Copy only the named Node binary, not its source installation directory."""
    deadline = time.monotonic() + 20
    try:
        parent = _open_tree_root(source.parent)
        try:
            descriptor = os.open(source.name, os.O_RDONLY | os.O_NOFOLLOW |
                                 os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=parent)
            try:
                before = os.fstat(descriptor)
                require_regular({"kind": "regular file" if stat.S_ISREG(before.st_mode) else "other",
                                 "size": before.st_size, "mode": stat.S_IMODE(before.st_mode)},
                                "Node source", executable=True)
                if before.st_nlink != 1:
                    raise ToolError("Node source is hardlinked")
                destination.mkdir(mode=0o755)
                destination.chmod(0o755)
                size = 0
                content = hashlib.sha256()
                with (destination / "node").open("xb") as output:
                    while True:
                        if time.monotonic() >= deadline:
                            raise ToolError("Node snapshot deadline exceeded")
                        block = os.read(descriptor, 64 * 1024)
                        if not block:
                            break
                        size += len(block)
                        if size > before.st_size:
                            raise ToolError("Node source changed while copying")
                        output.write(block)
                        content.update(block)
                    os.fchmod(output.fileno(), stat.S_IMODE(before.st_mode))
                os.lseek(descriptor, 0, os.SEEK_SET)
                repeated_content = hashlib.sha256()
                repeated_size = 0
                while True:
                    if time.monotonic() >= deadline:
                        raise ToolError("Node snapshot deadline exceeded")
                    block = os.read(descriptor, 64 * 1024)
                    if not block:
                        break
                    repeated_size += len(block)
                    if repeated_size > before.st_size:
                        raise ToolError("Node source changed while copying")
                    repeated_content.update(block)
                if repeated_size != size or repeated_content.digest() != content.digest():
                    raise ToolError("Node source content changed while copying")
                if size != before.st_size or _metadata_identity(before) != _metadata_identity(os.fstat(descriptor)) \
                        or _metadata_identity(before) != _metadata_identity(os.stat(source.name, dir_fd=parent, follow_symlinks=False)):
                    raise ToolError("Node source changed while copying")
            finally:
                os.close(descriptor)
        finally:
            os.close(parent)
    except OSError as error:
        raise ToolError(f"Node snapshot failed: {error}") from error
    return {"sha256": content.hexdigest(), "size": size, "mode": stat.S_IMODE(before.st_mode)}


def command_provision(args: argparse.Namespace) -> dict:
    validate_local_executable(args.ssh, "ssh")
    validate_local_executable(args.scp, "scp")
    node_source = safe_local_path(args.node_source, "node source")
    validate_private_file(args.identity_file, "identity file")
    validate_read_file(args.known_hosts, "known_hosts")
    worker_source = safe_local_path(args.worker_source, "worker source")
    if not worker_source.is_dir():
        raise ToolError(f"worker source is not a directory: {worker_source}")
    if worker_source.name != "worker":
        raise ToolError(
            "worker source directory name must be 'worker' to match the remote layout"
        )
    chromium_source = safe_local_path(args.chromium_source, "chromium source")
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
    local_trees = {"worker": installation_tree(worker_source),
                   "chromium": installation_tree(chromium_source.parent)}
    layout = remote_layout(args)
    config = strict_ssh_config(args)
    temporary_root = Path(__file__).resolve().parents[2] / "target/n5"
    temporary_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="soyo-browser-vm-", dir=temporary_root) as temporary:
        node_snapshot = Path(temporary) / "node-bin"
        node_identity = snapshot_node(node_source, node_snapshot)
        local_trees["node"] = installation_tree(node_snapshot)
        config_path = Path(temporary) / "ssh_config"
        config_path.write_text(config, encoding="utf-8")
        os.chmod(config_path, 0o600)
        preflight = ssh_run(args, config_path, PREFLIGHT_SCRIPT, []).decode("utf-8").strip()
        if preflight != "preflight=ok":
            raise ToolError("VM does not provide the required worker runtime tools")
        verify_network_prerequisites(
            args,
            config_path,
            configure_userns=getattr(args, "configure_apparmor_userns", False),
        )
        require_fresh_remote_root(args, config_path, layout["root"])
        ssh_run(
            args,
            config_path,
            'set -eu\nmkdir -m 755 -- "$1" "$2" "$3" "$4" "$5" "$6"\n',
            [f"{layout['root']}/node", f"{layout['root']}/node/bin", layout["worker"],
             f"{layout['root']}/browsers", f"{layout['root']}/browsers/chromium_headless_shell-{CHROMIUM_REVISION}",
             layout["runs"]],
        )
        checked(scp_argv(args, config_path, str(node_snapshot / "node"), layout["node"]))
        ssh_run(args, config_path, 'set -eu\nchmod -- "$1" "$2"\n',
                [f"{(node_snapshot / 'node').stat().st_mode & 0o7777:o}", layout["node"]])
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
        if installation_tree(worker_source) != local_trees["worker"]:
            raise ToolError("worker source changed during archiving")
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
            'set -eu\ntar -C "$1" -xpf "$2"\nrm -f -- "$2"\n',
            [layout["root"], f"{layout['root']}/worker.tar"],
        )
        chromium_archive = Path(temporary) / "chromium.tar"
        checked(["tar", "-C", str(chromium_source.parent.parent), "-cf",
                 str(chromium_archive), chromium_source.parent.name])
        if installation_tree(chromium_source.parent) != local_trees["chromium"]:
            raise ToolError("Chromium source changed during archiving")
        checked(scp_argv(args, config_path, str(chromium_archive),
                         f"{layout['root']}/chromium.tar"))
        ssh_run(args, config_path, 'set -eu\ntar -C "$1" -xpf "$2"\nrm -f -- "$2"\n',
                [f"{layout['root']}/browsers/chromium_headless_shell-{CHROMIUM_REVISION}",
                 f"{layout['root']}/chromium.tar"])
        records, package_bytes, browsers_bytes = identity_probe(args, config_path, layout)
        if any(records[0][key] != value for key, value in node_identity.items()):
            raise ToolError("uploaded Node identity differs from local snapshot")
        trees = remote_installation_trees(args, config_path, layout)
        if trees != local_trees:
            raise ToolError("uploaded installation trees differ from local sources")
        version = ssh_run(
            args,
            config_path,
            'set -eu\n"$1" --version\n',
            [layout["node"]],
        ).decode("utf-8").strip()
        manifest = build_manifest(records, package_bytes, browsers_bytes, version, trees)
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
    """Accept only the closed v2 identity contract, without coercing numbers."""
    groups = {"node", "worker_cli_js", "mcp_package", "playwright_package",
              "playwright_core_package", "playwright_core_browsers_json", "chromium"}
    if not isinstance(manifest, dict) or set(manifest) != groups | {
            "schema_version", "installation_trees"}:
        raise ToolError("remote manifest fields are invalid")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 2:
        raise ToolError("remote manifest schema version is unsupported")
    validate_installation_trees(manifest["installation_trees"])
    for name in groups:
        entry = manifest[name]
        keys = {"sha256", "size"}
        if name not in {"worker_cli_js", "playwright_core_browsers_json"}:
            keys.add("version")
        if name == "chromium":
            keys.add("revision")
        if not isinstance(entry, dict) or set(entry) != keys:
            raise ToolError(f"remote manifest {name} fields are invalid")
        if not isinstance(entry["sha256"], str) or not valid_digest(entry["sha256"]):
            raise ToolError(f"remote manifest {name} digest is invalid")
        if type(entry["size"]) is not int or not 0 <= entry["size"] <= MAX_IDENTITY_FILE_BYTES:
            raise ToolError(f"remote manifest {name} size is invalid")
        if "version" in keys and (not isinstance(entry["version"], str) or not entry["version"]):
            raise ToolError(f"remote manifest {name} version is invalid")
        if name == "chromium" and type(entry["revision"]) is not int:
            raise ToolError("remote manifest chromium revision is invalid")
    if not manifest["node"]["version"].startswith("v"):
        raise ToolError("remote manifest Node version is invalid")
    return manifest


def command_check(args: argparse.Namespace, manifest_bytes: bytes | None = None) -> dict:
    validate_local_executable(args.ssh, "ssh")
    validate_private_file(args.identity_file, "identity file")
    validate_read_file(args.known_hosts, "known_hosts")
    layout = remote_layout(args)
    config = strict_ssh_config(args)
    with tempfile.TemporaryDirectory(prefix="soyo-browser-vm-") as temporary:
        config_path = Path(temporary) / "ssh_config"
        config_path.write_text(config, encoding="utf-8")
        os.chmod(config_path, 0o600)
        if not valid_digest(args.manifest_sha256):
            raise ToolError("--manifest-sha256 must be lowercase SHA-256")
        verify_network_prerequisites(args, config_path)
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
        command.add_argument("--remote-root", default="/opt/soyo")
        command.add_argument("--chromium-dir-name", default="chrome-headless-shell-linux64")
        command.add_argument(
            "--manifest-sha256", required=(name == "check")
        )
        if name == "provision":
            command.add_argument("--node-source", type=Path, required=True)
            command.add_argument("--worker-source", type=Path, required=True)
            command.add_argument("--chromium-source", type=Path, required=True)
            # Dedicated-VM-only opt-in; never applied to the host.
            command.add_argument(
                "--configure-apparmor-userns",
                action="store_true",
                help=(
                    "set kernel.apparmor_restrict_unprivileged_userns=0 inside "
                    "this dedicated VM so Chromium can create unprivileged "
                    "user namespaces; never changes the host"
                ),
            )
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
