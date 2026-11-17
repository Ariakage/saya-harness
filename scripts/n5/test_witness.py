#!/usr/bin/env python3
"""Offline tests for scripts/n5/witness.py."""
from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

PATH = Path(__file__).resolve().with_name("witness.py")
SPEC = importlib.util.spec_from_file_location("n5_witness", PATH)
witness = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(witness)


def canonical_tempdir():
    return tempfile.TemporaryDirectory(dir=str(Path(tempfile.gettempdir()).resolve()))


class N5WitnessTests(unittest.TestCase):
    def test_outcomes_require_every_platform_step_without_skip_or_failure(self):
        for label in witness.RUNNERS:
            names = list(witness.COMMON_STEPS) + list(
                witness.LINUX_STEPS if label.startswith("ubuntu-") else witness.MACOS_STEPS
            )
            complete = {name: {"outcome": "success", "conclusion": "success"} for name in names}
            self.assertEqual(witness.outcomes(label, complete)["status"], "passed")
            for required in names:
                for state in ("skipped", "failure", "cancelled"):
                    bad = {**complete, required: {"outcome": state, "conclusion": "success"}}
                    report = witness.outcomes(label, bad)
                    self.assertEqual(report["status"], "failed")
                    self.assertEqual(report["unsatisfied_steps"], [required])
                missing = {k: v for k, v in complete.items() if k != required}
                self.assertEqual(witness.outcomes(label, missing)["status"], "failed")

    def test_worker_patch_chain_enforces_exact_digests_and_rejects_drift(self):
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            bundle = root / "node_modules/playwright-core/lib/coreBundle.js"
            bundle.parent.mkdir(parents=True)
            bundle.write_bytes(b"wrong stock")
            with self.assertRaisesRegex(AssertionError, "stock coreBundle.js SHA-256 mismatch"):
                witness.apply_worker_patch_chain(root)

            # Simulate valid deterministic patch outputs via a controlled fake applier.
            bundle.write_bytes(b"stock")
            digests = {
                bundle: witness.STOCK_CORE_BUNDLE_SHA256,
            }
            step_hashes = [expected for _patch, expected in witness.PATCH_CHAIN]
            index = 0

            def fake_sha(path: Path) -> str:
                return digests.get(path, "0" * 64)

            def fake_apply(_patch_path: Path, cwd: Path) -> None:
                nonlocal index
                self.assertEqual(cwd, root)
                digests[bundle] = step_hashes[index]
                index += 1

            with patch.object(witness, "sha256", side_effect=fake_sha):
                report = witness.apply_worker_patch_chain(root, patch_runner=fake_apply)
            self.assertEqual(report["final_core_bundle_sha256"], step_hashes[-1])
            self.assertEqual(len(report["patches"]), 3)

            index = 0
            digests[bundle] = witness.STOCK_CORE_BUNDLE_SHA256

            def bad_apply(_patch_path: Path, _cwd: Path) -> None:
                digests[bundle] = "f" * 64

            with patch.object(witness, "sha256", side_effect=fake_sha):
                with self.assertRaisesRegex(AssertionError, "mismatch after"):
                    witness.apply_worker_patch_chain(root, patch_runner=bad_apply)

    def test_build_native_manifest_emits_schema_v2_and_trims_group_world_write(self):
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            node_dir = root / "node/bin"
            node_dir.mkdir(parents=True)
            node = node_dir / "node"
            node.write_text("#!/bin/sh\necho v22.23.5\n", encoding="utf-8")
            node.chmod(0o777)

            worker = root / "worker"
            for rel, payload in (
                (witness.browser_vm.CLI_JS, b"// cli\n"),
                (
                    witness.browser_vm.MCP_PACKAGE,
                    json.dumps({"name": "@playwright/mcp", "version": witness.browser_vm.PLAYWRIGHT_MCP_VERSION}).encode(),
                ),
                (
                    witness.browser_vm.PLAYWRIGHT_PACKAGE,
                    json.dumps({"name": "playwright", "version": witness.browser_vm.PLAYWRIGHT_VERSION}).encode(),
                ),
                (
                    witness.browser_vm.PLAYWRIGHT_CORE_PACKAGE,
                    json.dumps({"name": "playwright-core", "version": witness.browser_vm.PLAYWRIGHT_VERSION}).encode(),
                ),
                (
                    witness.browser_vm.BROWSERS_JSON,
                    json.dumps(
                        {
                            "browsers": [
                                {
                                    "name": "chromium",
                                    "revision": str(witness.browser_vm.CHROMIUM_REVISION),
                                    "browserVersion": witness.browser_vm.CHROMIUM_VERSION,
                                }
                            ]
                        }
                    ).encode(),
                ),
            ):
                target = worker / rel
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(payload)
                target.chmod(0o664)

            chromium_dir = root / "browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64"
            chromium_dir.mkdir(parents=True)
            chromium = chromium_dir / "chrome-headless-shell"
            chromium.write_bytes(b"chromium-elf")
            chromium.chmod(0o755)

            for directory in (node_dir, worker, chromium_dir):
                witness.strip_group_world_write(directory)

            manifest_path = root / "manifest.json"
            result = witness.build_native_manifest(node, worker, chromium, manifest_path)
            self.assertEqual(manifest_path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(node.stat().st_mode & 0o022, 0)
            self.assertEqual(len(result["manifest_sha256"]), 64)
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            self.assertEqual(manifest["schema_version"], 2)
            self.assertEqual(manifest["node"]["version"], "v22.23.5")
            self.assertEqual(manifest["chromium"]["revision"], witness.browser_vm.CHROMIUM_REVISION)

            # Verify --snapshot-node-dir copies node cleanly without copying sibling entries.
            snapshot_dir = root / "isolated-node/bin"
            (node_dir / "sibling").write_text("do-not-copy", encoding="utf-8")
            report_path = root / "worker-prepare.json"
            with patch.object(witness.n3_witness, "source", return_value={"actual_sha": "a" * 40, "worktree_clean": True}), \
                 patch.object(witness, "apply_worker_patch_chain", return_value={"final_core_bundle_sha256": "0" * 64}):
                exit_code = witness.main(
                    [
                        "worker-prepare",
                        "--source-sha",
                        "a" * 40,
                        "--runner",
                        "ubuntu-22.04",
                        "--worker-root",
                        str(worker),
                        "--node",
                        str(node),
                        "--snapshot-node-dir",
                        str(snapshot_dir),
                        "--report",
                        str(report_path),
                    ]
                )
            self.assertEqual(exit_code, 0)
            self.assertEqual([p.name for p in snapshot_dir.iterdir()], ["node"])
            self.assertEqual(witness.sha256(snapshot_dir / "node"), witness.sha256(node))


if __name__ == "__main__":
    unittest.main()
