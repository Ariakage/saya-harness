#!/usr/bin/env python3
"""Offline security, packaging, workspace, and UI icon boundary tests for soyo-desktop."""

import json
from pathlib import Path
import re
import struct
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
DESKTOP = ROOT / "desktop"
TAURI_DIR = DESKTOP / "src-tauri"


def parse_workspace_array(cargo_text: str, key: str) -> list[str]:
    pattern = re.compile(rf"^\s*{re.escape(key)}\s*=\s*\[(.*?)\]", re.MULTILINE | re.DOTALL)
    match = pattern.search(cargo_text)
    if not match:
        raise AssertionError(f"Missing `{key}` array in Cargo.toml")
    return re.findall(r'"([^"]+)"', match.group(1))


class DesktopPackagingTest(unittest.TestCase):
    def test_workspace_isolates_desktop_from_default_members_and_core_crates(self):
        cargo_text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        members = parse_workspace_array(cargo_text, "members")
        default_members = parse_workspace_array(cargo_text, "default-members")
        self.assertIn("desktop/src-tauri", members)
        self.assertNotIn("desktop/src-tauri", default_members)
        for core_crate in (
            "crates/soyo-contract",
            "crates/soyo-fs",
            "crates/soyo-core",
            "crates/soyo-cli",
            "crates/soyo-app",
            "crates/soyo-tui",
        ):
            self.assertIn(core_crate, default_members)

        for path in (ROOT / "crates").rglob("*"):
            if path.suffix in {".rs", ".toml"} and path.is_file():
                content = path.read_text(encoding="utf-8")
                self.assertNotRegex(
                    content,
                    r"\b(tauri|tauri_specta|tauri_plugin_updater|soyo[-_]desktop)\b",
                    f"GUI/Tauri leakage in {path}",
                )

    def test_tauri_conf_security_bundle_updater_and_rgba_icons(self):
        conf = json.loads((TAURI_DIR / "tauri.conf.json").read_text(encoding="utf-8"))
        self.assertEqual(conf.get("productName"), "Soyo")
        self.assertEqual(conf.get("identifier"), "com.ariakage.soyo.desktop")

        security = conf.get("app", {}).get("security", {})
        self.assertNotIn("assetProtocol", security)
        csp = security.get("csp", "")
        self.assertIn("default-src 'self'", csp)
        self.assertIn("script-src 'self'", csp)
        self.assertIn("object-src 'none'", csp)
        self.assertIn("frame-ancestors 'none'", csp)
        self.assertIn("ipc:", csp)
        self.assertIn("http://ipc.localhost", csp)
        self.assertNotIn("*", csp)
        self.assertNotRegex(csp, r"https?://(?!localhost\b|ipc\.localhost\b|soyo-attachment\.localhost\b|saya-attachment\.localhost\b)")

        cap = json.loads((TAURI_DIR / "capabilities" / "default.json").read_text(encoding="utf-8"))
        permissions = cap.get("permissions", [])
        self.assertIn("updater:default", permissions)
        for perm in permissions:
            self.assertNotIn("fs:", str(perm))
            self.assertNotIn("keyring", str(perm).lower())

        bundle = conf.get("bundle", {})
        self.assertTrue(bundle.get("active"))
        self.assertTrue(bundle.get("createUpdaterArtifacts"))
        targets = bundle.get("targets", [])
        self.assertEqual(targets, ["dmg", "app", "nsis", "appimage"])
        self.assertNotIn("msi", targets)
        self.assertEqual(bundle.get("macOS", {}).get("minimumSystemVersion"), "15.0")

        updater = conf.get("plugins", {}).get("updater", {})
        self.assertEqual(updater.get("pubkey"), "")
        self.assertIn(
            "https://github.com/Ariakage/soyo-harness/releases/latest/download/latest.json",
            updater.get("endpoints", []),
        )

        icon_paths = [TAURI_DIR / rel for rel in bundle.get("icon", [])]
        icon_paths.append(DESKTOP / "public" / "app-icon.png")
        for icon_path in icon_paths:
            self.assertTrue(icon_path.is_file(), f"Missing icon asset {icon_path}")
            raw = icon_path.read_bytes()
            if icon_path.suffix == ".png":
                self.assertEqual(raw[:8], b"\x89PNG\r\n\x1a\n", f"Invalid PNG header: {icon_path}")
                _w, _h, depth, color_type = struct.unpack(">IIBB", raw[16:26])
                self.assertGreaterEqual(depth, 8)
                self.assertEqual(color_type, 6, f"Expected RGBA color_type=6 in {icon_path}")
            elif icon_path.suffix == ".icns":
                self.assertEqual(raw[:4], b"icns")
            elif icon_path.suffix == ".ico":
                self.assertEqual(raw[:4], b"\x00\x00\x01\x00")

    def test_dependencies_are_exact_pinned_without_keyring_and_use_lucide(self):
        pkg = json.loads((DESKTOP / "package.json").read_text(encoding="utf-8"))
        for section in ("dependencies", "devDependencies"):
            deps = pkg.get(section, {})
            for name, ver in deps.items():
                self.assertNotIn("keyring", name.lower())
                self.assertNotRegex(
                    ver,
                    r"[\^~*><x]|workspace:|file:|git\+|latest",
                    f"Unpinned npm dependency {name}={ver}",
                )
        self.assertIn("lucide-react", pkg.get("dependencies", {}))
        for competing in (
            "@heroicons/react",
            "react-icons",
            "@radix-ui/react-icons",
            "@tabler/icons-react",
            "feather-icons",
        ):
            self.assertNotIn(competing, pkg.get("dependencies", {}))

        cargo_desktop = (TAURI_DIR / "Cargo.toml").read_text(encoding="utf-8")
        self.assertNotIn("keyring", cargo_desktop.lower())
        in_deps_section = False
        for line in cargo_desktop.splitlines():
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if stripped.startswith("["):
                in_deps_section = stripped in (
                    "[dependencies]",
                    "[build-dependencies]",
                    "[dev-dependencies]",
                )
                continue
            if not in_deps_section or "path =" in stripped or "workspace = true" in stripped:
                continue
            ver_match = re.search(r'version\s*=\s*"([^"]+)"|^[A-Za-z0-9_-]+\s*=\s*"([^"]+)"', stripped)
            if ver_match:
                ver = ver_match.group(1) or ver_match.group(2)
                self.assertTrue(
                    ver.startswith("="),
                    f"Rust desktop dependency not exact-pinned with `=`: {stripped}",
                )

        lucide_imports = 0
        for src in (DESKTOP / "src").rglob("*"):
            if not src.is_file() or src.suffix not in {".ts", ".tsx"}:
                continue
            if src.name.endswith(".test.ts") or src.name.endswith(".test.tsx"):
                continue
            text = src.read_text(encoding="utf-8")
            self.assertNotRegex(text, r"\b(localStorage|sessionStorage|indexedDB)\b")
            self.assertNotRegex(text, r"<svg\b", f"Inline <svg> forbidden outside lucide-react: {src}")
            self.assertNotRegex(
                text,
                r"from ['\"](@heroicons|react-icons|@radix-ui/react-icons|@tabler/icons|feather-icons)",
            )
            if 'from "lucide-react"' in text or "from 'lucide-react'" in text:
                lucide_imports += 1
        self.assertGreaterEqual(lucide_imports, 10)

    def test_attachment_protocol_registers_scoped_schemes_and_safe_headers(self):
        proto_rs = (TAURI_DIR / "src" / "attachment_protocol.rs").read_text(encoding="utf-8")
        lib_rs = (TAURI_DIR / "src" / "lib.rs").read_text(encoding="utf-8")
        self.assertIn("soyo-attachment", proto_rs)
        self.assertIn("saya-attachment", proto_rs)
        self.assertIn("register_uri_scheme_protocol", proto_rs)
        self.assertIn("register_attachment_protocols", lib_rs)
        self.assertIn('"X-Content-Type-Options", "nosniff"', proto_rs)
        self.assertIn('"Cache-Control", "private, no-store"', proto_rs)
        self.assertIn("symlink", proto_rs.lower())

    def test_desktop_release_workflow_matrix_and_secret_hygiene(self):
        workflow_path = ROOT / ".github" / "workflows" / "desktop-release.yml"
        self.assertTrue(workflow_path.is_file(), "Missing .github/workflows/desktop-release.yml")
        wf = workflow_path.read_text(encoding="utf-8")

        for runner in ("macos-15", "macos-15-intel", "ubuntu-22.04", "windows-latest"):
            self.assertIn(runner, wf)
        self.assertRegex(wf, r"uses:\s*tauri-apps/tauri-action@[0-9a-f]{40}\b")
        self.assertIn("projectPath: desktop", wf)
        self.assertIn("uploadUpdaterJson: true", wf)
        self.assertIn("updaterJsonPreferNsis: true", wf)
        self.assertIn("${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}", wf)
        self.assertIn("${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}", wf)
        for marker in ("BEGIN PRIVATE KEY", "BEGIN EC PRIVATE KEY", "untrusted comment: minisign"):
            self.assertNotIn(marker, wf)


if __name__ == "__main__":
    unittest.main()
