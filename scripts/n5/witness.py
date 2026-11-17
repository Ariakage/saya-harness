#!/usr/bin/env python3
"""Fixed-source, native-platform, worker-patch, and fail-closed witnesses for N5."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
N3_DIR = ROOT / "scripts/n3"
N5_DIR = Path(__file__).resolve().parent


def _load_module(name: str, path: Path, extra_sys_path: Path | None = None):
    if extra_sys_path is not None:
        sys.path.insert(0, str(extra_sys_path))
    try:
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        if extra_sys_path is not None:
            sys.path.remove(str(extra_sys_path))


n3_fixture = _load_module("n5_n3_fixture", N3_DIR / "fixture.py")
n3_witness = _load_module("n5_n3_witness", N3_DIR / "witness.py", N3_DIR)
browser_vm = _load_module("n5_browser_vm", N5_DIR / "browser_vm.py")
require = n3_fixture.require
sha256 = n3_fixture.sha256

RUNNERS = n3_witness.RUNNERS
STOCK_CORE_BUNDLE_SHA256 = "9d3260b17408501ba46ca708fa870a7c95e1e3f6e9cc34d799465f17b7d78c50"
PATCH_CHAIN = (
    (
        "scripts/n5/patches/playwright-isolated-stdio.patch",
        "385866007a5df69db4c3e22bea193b2afe7ae20b54769a144855b52e1d608a0d",
    ),
    (
        "scripts/n5/patches/playwright-process-info.patch",
        "28512f80e0a84cb225bd1431217f0ca98265d5010cf920c2f171e28fc056093a",
    ),
    (
        "scripts/n5/patches/playwright-masked-capture.patch",
        "de88c9fc365498503fb9a3cf8da5c7a0fc3724f9b409b79928aa169d1859e276",
    ),
)
COMMON_STEPS = (
    "source",
    "toolchain",
    "rust",
    "tooling",
    "candidate",
    "baseline",
    "performance",
    "mcp",
    "legacy",
    "plugins",
    "n3_cli",
    "n3_tui",
    "worker_patches",
    "worker_protocol",
    "worker_process_info",
    "worker_capture",
    "browser_negative_controls",
    "final_witness",
)
LINUX_STEPS = (
    "linux_tools",
    "native_linux",
    "browser_vm_host_linux",
    "native_chromium_sandbox",
)
MACOS_STEPS = ("native_macos",)


def _apply_patch(patch_path: Path, cwd: Path) -> None:
    process = subprocess.run(
        ["patch", "-p1", "--no-backup-if-mismatch", "-i", str(patch_path)],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=60,
    )
    require(
        process.returncode == 0,
        f"patch {patch_path.name} failed ({process.returncode}): {process.stderr or process.stdout}",
    )


def apply_worker_patch_chain(worker_root: Path, patch_runner=_apply_patch) -> dict:
    """Verify stock coreBundle.js, apply all three pinned worker patches, and verify every digest."""
    bundle = worker_root / "node_modules/playwright-core/lib/coreBundle.js"
    require(bundle.is_file(), f"missing pinned worker bundle: {bundle}")
    stock_sha = sha256(bundle)
    require(
        stock_sha == STOCK_CORE_BUNDLE_SHA256,
        f"stock coreBundle.js SHA-256 mismatch: {stock_sha} != {STOCK_CORE_BUNDLE_SHA256}",
    )
    steps = []
    for rel_patch, expected_sha in PATCH_CHAIN:
        patch_path = ROOT / rel_patch
        require(patch_path.is_file(), f"missing worker patch: {patch_path}")
        patch_runner(patch_path, worker_root)
        actual_sha = sha256(bundle)
        require(
            actual_sha == expected_sha,
            f"patched coreBundle.js mismatch after {patch_path.name}: {actual_sha} != {expected_sha}",
        )
        steps.append(
            {
                "patch": rel_patch,
                "patch_sha256": sha256(patch_path),
                "core_bundle_sha256": actual_sha,
            }
        )
    return {
        "worker_root": str(worker_root),
        "stock_core_bundle_sha256": stock_sha,
        "final_core_bundle_sha256": steps[-1]["core_bundle_sha256"],
        "final_core_bundle_bytes": bundle.stat().st_size,
        "patches": steps,
    }


def strip_group_world_write(root: Path) -> None:
    """Clear group/world write bits on files and directories without following symlinks."""
    for current_root, dir_names, file_names in os.walk(root, topdown=False, followlinks=False):
        current = Path(current_root)
        for name in file_names:
            member = current / name
            info = member.lstat()
            if stat.S_ISREG(info.st_mode):
                trimmed = stat.S_IMODE(info.st_mode) & ~0o022
                if trimmed != stat.S_IMODE(info.st_mode):
                    member.chmod(trimmed)
        for name in dir_names:
            member = current / name
            info = member.lstat()
            if stat.S_ISDIR(info.st_mode) and not stat.S_ISLNK(info.st_mode):
                trimmed = stat.S_IMODE(info.st_mode) & ~0o022
                if trimmed != stat.S_IMODE(info.st_mode):
                    member.chmod(trimmed)
    root_info = root.lstat()
    if stat.S_ISDIR(root_info.st_mode) and not stat.S_ISLNK(root_info.st_mode):
        trimmed = stat.S_IMODE(root_info.st_mode) & ~0o022
        if trimmed != stat.S_IMODE(root_info.st_mode):
            root.chmod(trimmed)


def build_native_manifest(
    node: Path,
    worker_root: Path,
    chromium: Path,
    manifest_path: Path,
) -> dict:
    """Generate and validate a schema v2 identity manifest for native Linux execution."""
    node = node.resolve(strict=True)
    worker_root = worker_root.resolve(strict=True)
    chromium = chromium.resolve(strict=True)
    require(node.is_file() and os.access(node, os.X_OK), "node must be executable")
    require(worker_root.is_dir(), "worker_root must be a directory")
    require(chromium.is_file() and os.access(chromium, os.X_OK), "chromium must be executable")

    version_proc = subprocess.run([str(node), "--version"], capture_output=True, text=True, timeout=15)
    require(version_proc.returncode == 0, f"node --version failed: {version_proc.stderr}")
    node_version = version_proc.stdout.strip()
    require(bool(node_version), "empty node --version output")

    cli_js = worker_root / browser_vm.CLI_JS
    mcp_pkg = worker_root / browser_vm.MCP_PACKAGE
    pw_pkg = worker_root / browser_vm.PLAYWRIGHT_PACKAGE
    pw_core_pkg = worker_root / browser_vm.PLAYWRIGHT_CORE_PACKAGE
    browsers_json = worker_root / browser_vm.BROWSERS_JSON

    browsers = json.loads(browsers_json.read_text(encoding="utf-8")).get("browsers", [])
    chromium_entry = next((item for item in browsers if item.get("name") == "chromium"), None)
    require(chromium_entry is not None, "chromium entry missing from browsers.json")

    def file_record(path: Path) -> dict:
        return {"sha256": sha256(path), "size": path.stat().st_size}

    manifest = {
        "schema_version": 2,
        "installation_trees": {
            "node": browser_vm.installation_tree(node.parent),
            "worker": browser_vm.installation_tree(worker_root),
            "chromium": browser_vm.installation_tree(chromium.parent),
        },
        "node": {**file_record(node), "version": node_version},
        "worker_cli_js": file_record(cli_js),
        "mcp_package": {**file_record(mcp_pkg), "version": browser_vm.PLAYWRIGHT_MCP_VERSION},
        "playwright_package": {**file_record(pw_pkg), "version": browser_vm.PLAYWRIGHT_VERSION},
        "playwright_core_package": {
            **file_record(pw_core_pkg),
            "version": browser_vm.PLAYWRIGHT_VERSION,
        },
        "playwright_core_browsers_json": file_record(browsers_json),
        "chromium": {
            **file_record(chromium),
            "version": chromium_entry["browserVersion"],
            "revision": int(chromium_entry["revision"]),
        },
    }
    browser_vm.validate_manifest_structure(manifest)
    encoded = (json.dumps(manifest, indent=2) + "\n").encode("utf-8")
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_bytes(encoded)
    manifest_path.chmod(0o600)
    return {
        "node": str(node),
        "worker_root": str(worker_root),
        "chromium": str(chromium),
        "manifest": str(manifest_path.resolve()),
        "manifest_sha256": hashlib.sha256(encoded).hexdigest(),
        "trees": manifest["installation_trees"],
    }


def outcomes(label: str, steps: dict) -> dict:
    """Verify every required step for the runner completed with outcome == success."""
    require(label in RUNNERS, "unknown native runner label")
    required = list(COMMON_STEPS) + list(LINUX_STEPS if label.startswith("ubuntu-") else MACOS_STEPS)
    observed = {
        name: {"outcome": item.get("outcome"), "conclusion": item.get("conclusion")}
        for name, item in steps.items()
    }
    missing = [name for name in required if observed.get(name, {}).get("outcome") != "success"]
    return {
        "status": "passed" if not missing else "failed",
        "required_steps": required,
        "unsatisfied_steps": missing,
        "steps": observed,
    }


def _delegated_n3_phase(options: argparse.Namespace) -> dict:
    evidence = {
        "source": n3_witness.source(options.source_sha),
        "platform": n3_witness.platform_witness(options.runner),
    }
    if options.phase in ("candidate", "baseline", "final"):
        require(options.binary is not None, "candidate path is required")
        if options.phase == "candidate":
            require(
                options.copy_from is not None and not options.binary.exists(),
                "fresh candidate destination and build input are required",
            )
            options.binary.parent.mkdir(parents=True, exist_ok=True)
            with options.copy_from.open("rb") as source_file, options.binary.open("xb") as dest:
                shutil.copyfileobj(source_file, dest, 65536)
                dest.flush()
                os.fsync(dest.fileno())
            options.binary.chmod(0o500)
            require(
                sha256(options.copy_from) == sha256(options.binary),
                "candidate copy differs from the built release program",
            )
        evidence["binary"] = n3_witness.binary_witness(options.binary, options.runner)
        if options.phase != "candidate":
            require(
                len(options.expected_sha256 or "") == 64
                and evidence["binary"]["sha256"] == options.expected_sha256,
                "candidate changed from its original witness",
            )
        if options.phase == "candidate":
            evidence["toolchain"] = n3_witness.command("rustc", "--version", "--verbose")
            evidence["cargo_lock_sha256"] = sha256(Path("Cargo.lock"))
            if os.environ.get("GITHUB_OUTPUT"):
                with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
                    output.write(
                        f"binary={options.binary.resolve()}\n"
                        f"sha256={evidence['binary']['sha256']}\n"
                    )
    return evidence


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "phase",
        choices=("source", "candidate", "baseline", "worker-prepare", "native-manifest", "final", "outcomes"),
    )
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--runner", required=True, choices=RUNNERS)
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--copy-from", type=Path)
    parser.add_argument("--expected-sha256")
    parser.add_argument("--worker-root", type=Path)
    parser.add_argument("--node", type=Path)
    parser.add_argument("--snapshot-node-dir", type=Path)
    parser.add_argument("--chromium", type=Path)
    parser.add_argument("--manifest", type=Path)
    options = parser.parse_args(argv)
    evidence = {
        "schema_version": 1,
        "phase": options.phase,
        "status": "failed",
        "recorded_utc": datetime.now(timezone.utc).isoformat(),
        "workflow_sha": os.environ.get("GITHUB_WORKFLOW_SHA"),
        "run_id": os.environ.get("GITHUB_RUN_ID"),
        "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
    }
    try:
        if options.phase == "outcomes":
            evidence.update(outcomes(options.runner, json.loads(os.environ["STEP_OUTCOMES"])))
            evidence["job_status_before_upload"] = os.environ.get("JOB_STATUS")
        elif options.phase == "worker-prepare":
            require(options.worker_root is not None, "--worker-root is required")
            evidence["source"] = n3_witness.source(options.source_sha)
            worker_root = options.worker_root.resolve(strict=True)
            evidence["worker"] = apply_worker_patch_chain(worker_root)
            strip_group_world_write(worker_root)
            if options.snapshot_node_dir is not None:
                found_node = shutil.which("node") if options.node is None else str(options.node)
                require(bool(found_node), "node executable not found on PATH")
                source_node = Path(found_node).resolve(strict=True)
                require(source_node.is_file() and os.access(source_node, os.X_OK), "node must be executable")
                options.snapshot_node_dir.mkdir(parents=True, exist_ok=True, mode=0o755)
                strip_group_world_write(options.snapshot_node_dir)
                destination_node = options.snapshot_node_dir / "node"
                shutil.copyfile(source_node, destination_node)
                destination_node.chmod(0o755)
                expected_digest = sha256(source_node)
                require(sha256(destination_node) == expected_digest, "copied node digest mismatch")
                evidence["node"] = {
                    "source": str(source_node),
                    "snapshot": str(destination_node.resolve(strict=True)),
                    "sha256": expected_digest,
                }
            evidence["status"] = "passed"
        elif options.phase == "native-manifest":
            require(
                options.node is not None
                and options.worker_root is not None
                and options.chromium is not None
                and options.manifest is not None,
                "--node, --worker-root, --chromium, and --manifest are required",
            )
            evidence["source"] = n3_witness.source(options.source_sha)
            evidence["platform"] = n3_witness.platform_witness(options.runner)
            strip_group_world_write(options.node.resolve(strict=True).parent)
            strip_group_world_write(options.worker_root.resolve(strict=True))
            strip_group_world_write(options.chromium.resolve(strict=True).parent)
            evidence["manifest"] = build_native_manifest(
                options.node,
                options.worker_root,
                options.chromium,
                options.manifest,
            )
            evidence["status"] = "passed"
        else:
            evidence.update(_delegated_n3_phase(options))
            evidence["status"] = "passed"
    except Exception as cause:  # fail-closed diagnostic recording
        evidence["failure"] = {"type": type(cause).__name__, "message": str(cause)[:4096]}
    options.report.parent.mkdir(parents=True, exist_ok=True)
    encoded = json.dumps(evidence, indent=2) + "\n"
    require(len(encoded.encode("utf-8")) <= 2 * 1024 * 1024, "witness report exceeds byte bound")
    options.report.write_text(encoded, encoding="utf-8")
    print(encoded, end="")
    return 0 if evidence["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
