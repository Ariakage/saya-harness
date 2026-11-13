#!/usr/bin/env python3
"""Offline regression checks for the explicit Colima browser VM tool."""

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("browser_vm.py")
SPEC = importlib.util.spec_from_file_location("browser_vm", MODULE_PATH)
browser_vm = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(browser_vm)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def package(name: str, version: str) -> bytes:
    return json.dumps({"name": name, "version": version}).encode()


def probe_record(data: bytes, mode: int = 0o644, kind: str = "regular file", size=None) -> str:
    size = len(data) if size is None else size
    return f"{size}|{mode:o}|{kind}\n{digest(data)}  file\n"


class Fixture:
    def __init__(self):
        self.node = b"pinned node"
        self.cli = b"// worker\n"
        self.mcp = package("@playwright/mcp", browser_vm.PLAYWRIGHT_MCP_VERSION)
        self.playwright = package("playwright", browser_vm.PLAYWRIGHT_VERSION)
        self.core = package("playwright-core", browser_vm.PLAYWRIGHT_VERSION)
        self.browsers = json.dumps(
            {
                "browsers": [
                    {
                        "name": "chromium",
                        "revision": str(browser_vm.CHROMIUM_REVISION),
                        "browserVersion": browser_vm.CHROMIUM_VERSION,
                    }
                ]
            }
        ).encode()
        self.chromium = b"pinned chromium"

    def entries(self):
        return [
            (self.node, 0o755),
            (self.cli, 0o644),
            (self.mcp, 0o644),
            (self.playwright, 0o644),
            (self.core, 0o644),
            (self.browsers, 0o644),
            (self.chromium, 0o755),
        ]

    def records(self, overrides=None):
        lines = []
        for index, (data, mode) in enumerate(self.entries()):
            options = (overrides or {}).get(index, {})
            lines.append(
                probe_record(
                    data,
                    mode=options.get("mode", mode),
                    kind=options.get("kind", "regular file"),
                    size=options.get("size"),
                )
            )
        return "".join(lines)

    def manifest(self):
        records = browser_vm.parse_probe(self.records(), 7)
        return browser_vm.build_manifest(
            records,
            {"mcp": self.mcp, "playwright": self.playwright, "playwright_core": self.core},
            self.browsers,
            "v22.23.0",
        )


def namespace(**overrides):
    values = {
        "colima": Path("/opt/homebrew/bin/colima"),
        "profile": "saya-browser",
        "cpu": 2,
        "memory": 4,
        "disk": 60,
    }
    values.update(overrides)
    return type("Args", (), values)()


def ssh_namespace(root: Path, **overrides):
    identity = root / "id_ed25519"
    known_hosts = root / "known_hosts"
    identity.write_bytes(b"placeholder")
    identity.chmod(0o600)
    known_hosts.write_text("127.0.0.1 ssh-ed25519 AAAA\n")
    known_hosts.chmod(0o644)
    values = {
        "ssh": Path("/usr/bin/true"),
        "scp": Path("/usr/bin/true"),
        "profile": "saya-browser",
        "host": "127.0.0.1",
        "user": "saya",
        "port": 2222,
        "identity_file": identity,
        "known_hosts": known_hosts,
        "remote_root": "/opt/saya",
        "chromium_dir_name": "chrome-headless-shell-linux64",
    }
    values.update(overrides)
    return type("Args", (), values)()


class CreationTests(unittest.TestCase):
    def test_create_uses_no_mounts_and_no_ssh_agent(self):
        argv = browser_vm.colima_start_argv(namespace())
        self.assertEqual(argv[1:3], ["start", "--profile"])
        self.assertIn("--mount", argv)
        self.assertEqual(argv[argv.index("--mount") + 1], "none")
        self.assertIn("--ssh-agent=false", argv)
        self.assertNotIn("--mount-type", argv)

    def test_destroy_is_explicit_and_forced(self):
        argv = browser_vm.colima_delete_argv(namespace())
        self.assertEqual(argv[1:4], ["delete", "--profile", "saya-browser"])
        self.assertEqual(argv[-1], "--force")

    def test_profile_rejects_injection(self):
        with self.assertRaises(browser_vm.ToolError):
            browser_vm.colima_start_argv(namespace(profile="saya;rm"))
        with self.assertRaises(browser_vm.ToolError):
            browser_vm.colima_start_argv(namespace(profile="-oProxyCommand=x"))


class SshTests(unittest.TestCase):
    def test_token_rejects_current_and_parent_directory_names(self):
        for value in (".", ".."):
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.safe_token(value, "token")

    def test_bracketed_ipv6_is_normalized_before_ssh_and_config(self):
        self.assertEqual(browser_vm.loopback_host("[::1]"), "::1")
        self.assertEqual(browser_vm.loopback_host("127.0.0.1"), "127.0.0.1")
        with tempfile.TemporaryDirectory() as temporary:
            args = ssh_namespace(Path(temporary), host="[::1]")
            config = browser_vm.strict_ssh_config(args)
            self.assertIn("HostName ::1", config)
            self.assertNotIn("[::1]", config)
            argv = browser_vm.ssh_argv(args, Path("/tmp/config"), "true")
            self.assertIn("::1", argv)
            self.assertNotIn("[::1]", " ".join(argv))
            scp = browser_vm.scp_argv(
                args, Path("/tmp/config"), "/tmp/source", "/opt/saya/node"
            )
            self.assertEqual(scp[-1], "saya@[::1]:/opt/saya/node")

    def test_symlink_to_unsafe_target_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "key\nProxyCommand=evil"
            target.write_bytes(b"placeholder")
            target.chmod(0o600)
            link = root / "identity"
            link.symlink_to(target)
            args = ssh_namespace(root, identity_file=link)
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.strict_ssh_config(args)
            known_target = root / "known hosts"
            known_target.write_text("127.0.0.1 ssh-ed25519 AAAA\n")
            known_target.chmod(0o644)
            known_link = root / "known_hosts_link"
            known_link.symlink_to(known_target)
            args = ssh_namespace(root, known_hosts=known_link)
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.strict_ssh_config(args)

    def test_strict_config_has_no_secret_or_agent_forwarding(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = ssh_namespace(Path(temporary))
            config = browser_vm.strict_ssh_config(args)
            for required in [
                "BatchMode yes",
                "IdentitiesOnly yes",
                "StrictHostKeyChecking yes",
                "PasswordAuthentication no",
                "KbdInteractiveAuthentication no",
                "ForwardAgent no",
                "ClearAllForwardings yes",
                "ProxyCommand none",
                "ProxyJump none",
                "ControlMaster no",
                "UpdateHostKeys no",
            ]:
                self.assertIn(required, config)
            self.assertNotIn("PRIVATE KEY", config)

    def test_ssh_argv_is_strict_and_quotes_remote_arguments(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = ssh_namespace(Path(temporary))
            argv = browser_vm.ssh_argv(
                args,
                Path("/tmp/config"),
                browser_vm.remote_command("set -eu\ntrue\n", []),
            )
            joined = " ".join(argv)
            self.assertIn("ForwardAgent=no", joined)
            self.assertIn("StrictHostKeyChecking=yes", joined)
            self.assertIn("IdentitiesOnly=yes", joined)
            self.assertNotIn("ForwardAgent=yes", joined)
            self.assertTrue(argv[-1].startswith("sh -c '"))

    def test_user_and_host_reject_injection(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.strict_ssh_config(
                    ssh_namespace(Path(temporary), user="root@evil")
                )
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.strict_ssh_config(
                    ssh_namespace(Path(temporary), host="example.com")
                )
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.strict_ssh_config(
                    ssh_namespace(Path(temporary), port=0)
                )

    def test_remote_paths_reject_traversal_and_metacharacters(self):
        for bad in ["relative", "/opt/../root", "/opt/a;b", "/opt/$(id)", "/opt/a b"]:
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.safe_remote_path(bad, "path")
        self.assertEqual(
            browser_vm.safe_remote_path("/opt/saya/worker", "path"),
            "/opt/saya/worker",
        )


class IdentityTests(unittest.TestCase):
    def test_probe_and_manifest_pin_versions(self):
        fixture = Fixture()
        records = browser_vm.parse_probe(fixture.records(), 7)
        manifest = browser_vm.build_manifest(
            records,
            {
                "mcp": fixture.mcp,
                "playwright": fixture.playwright,
                "playwright_core": fixture.core,
            },
            fixture.browsers,
            "v22.23.0",
        )
        self.assertEqual(manifest["schema_version"], 1)
        self.assertEqual(manifest["chromium"]["revision"], browser_vm.CHROMIUM_REVISION)
        self.assertEqual(manifest["node"]["version"], "v22.23.0")
        self.assertEqual(manifest["mcp_package"]["sha256"], digest(fixture.mcp))

    def test_unpinned_versions_and_permissions_fail(self):
        fixture = Fixture()
        records = browser_vm.parse_probe(fixture.records(), 7)
        records[0]["mode"] = 0o666
        with self.assertRaises(browser_vm.ToolError):
            browser_vm.require_regular(records[0], "node", executable=True)

        wrong = package("@playwright/mcp", "0.0.84")
        with self.assertRaises(browser_vm.ToolError):
            browser_vm.build_manifest(
                browser_vm.parse_probe(fixture.records(), 7),
                {"mcp": wrong, "playwright": fixture.playwright, "playwright_core": fixture.core},
                fixture.browsers,
                "v22.23.0",
            )

        wrong_browsers = json.dumps(
            {
                "browsers": [
                    {
                        "name": "chromium",
                        "revision": "1255",
                        "browserVersion": browser_vm.CHROMIUM_VERSION,
                    }
                ]
            }
        ).encode()
        with self.assertRaises(browser_vm.ToolError):
            browser_vm.build_manifest(
                browser_vm.parse_probe(fixture.records(), 7),
                {
                    "mcp": fixture.mcp,
                    "playwright": fixture.playwright,
                    "playwright_core": fixture.core,
                },
                wrong_browsers,
                "v22.23.0",
            )

    def test_malformed_manifest_shapes_raise_tool_error(self):
        fixture = Fixture()
        malformed = [
            b"[]\n",
            b"null\n",
            b'{"schema_version": 1}\n',
            json.dumps({"schema_version": 1, "node": []}).encode() + b"\n",
            json.dumps({"schema_version": 1, "node": {"version": 1}}).encode() + b"\n",
        ]

        def fake_ssh(args, config_path, script, arguments):
            if script == browser_vm.NETWORK_PREFLIGHT_SCRIPT:
                return b"network=preflight-ok\n"
            if script == browser_vm.SSHD_FORWARDING_SCRIPT:
                return b"sshd=forwarding-ok\n"
            if script == browser_vm.PROBE_SCRIPT:
                payload = fake_ssh.payload
                return (
                    probe_record(payload) + fixture.records()
                ).encode()
            if script == browser_vm.CAT_SCRIPT:
                return fake_ssh.payload
            if '--version' in script:
                return b"v22.23.0\n"
            raise AssertionError(f"unexpected script: {script}")

        with tempfile.TemporaryDirectory() as temporary:
            for payload in malformed:
                fake_ssh.payload = payload
                args = ssh_namespace(
                    Path(temporary), manifest_sha256=digest(payload)
                )
                with mock.patch.object(browser_vm, "ssh_run", fake_ssh):
                    with self.assertRaises(browser_vm.ToolError):
                        browser_vm.command_check(args)

    def test_remote_layout_revalidates_generated_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = ssh_namespace(Path(temporary))
            layout = browser_vm.remote_layout(args)
            for path in layout.values():
                self.assertTrue(path.startswith("/"))
                browser_vm.safe_remote_path(path, "path")
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.remote_layout(
                    ssh_namespace(Path(temporary), chromium_dir_name=".")
                )
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.remote_layout(
                    ssh_namespace(Path(temporary), remote_root="/")
                )

    def test_provision_rejects_worker_source_directory_mismatch(self):
        fixture = Fixture()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            worker = root / "not-worker"
            worker.mkdir()
            node = root / "node"
            node.write_bytes(fixture.node)
            node.chmod(0o755)
            chromium_dir = root / "chrome-headless-shell-linux64"
            chromium_dir.mkdir()
            chromium = chromium_dir / "chrome-headless-shell"
            chromium.write_bytes(fixture.chromium)
            chromium.chmod(0o755)
            args = ssh_namespace(
                root,
                node_source=node,
                worker_source=worker,
                chromium_source=chromium,
            )
            with mock.patch.object(
                browser_vm, "ssh_run", lambda *_: self.fail("must not contact ssh")
            ):
                with self.assertRaises(browser_vm.ToolError):
                    browser_vm.command_provision(args)

    def test_check_rejects_digest_mismatch(self):
        fixture = Fixture()
        manifest_bytes = (
            json.dumps(fixture.manifest(), indent=2, sort_keys=True).encode() + b"\n"
        )
        with self.assertRaises(browser_vm.ToolError):
            self.check_with(
                manifest_bytes,
                fixture.records(),
                probe_record(manifest_bytes),
                expected_digest="0" * 64,
            )

    def check_with(
        self,
        manifest_bytes: bytes,
        named_records: str,
        manifest_record: str,
        expected_digest: str | None = None,
    ) -> dict:
        fixture = Fixture()
        files = {
            "/opt/saya/node/bin/node": fixture.node,
            "/opt/saya/worker/node_modules/@playwright/mcp/package.json": fixture.mcp,
            "/opt/saya/worker/node_modules/playwright/package.json": fixture.playwright,
            "/opt/saya/worker/node_modules/playwright-core/package.json": fixture.core,
            "/opt/saya/worker/node_modules/playwright-core/browsers.json": fixture.browsers,
        }

        def fake_ssh(args, config_path, script, arguments):
            if script == browser_vm.NETWORK_PREFLIGHT_SCRIPT:
                return b"network=preflight-ok\n"
            if script == browser_vm.SSHD_FORWARDING_SCRIPT:
                return b"sshd=forwarding-ok\n"
            if script == browser_vm.PROBE_SCRIPT:
                return (manifest_record + named_records).encode()
            if script == browser_vm.CAT_SCRIPT:
                path = arguments[0]
                if path.endswith("identity.json"):
                    return manifest_bytes
                return files[path]
            if '--version' in script:
                return b"v22.23.0\n"
            raise AssertionError(f"unexpected script: {script}")

        with tempfile.TemporaryDirectory() as temporary:
            args = ssh_namespace(
                Path(temporary),
                manifest_sha256=expected_digest or digest(manifest_bytes),
            )
            with mock.patch.object(browser_vm, "ssh_run", fake_ssh):
                return browser_vm.command_check(args)

    def test_check_succeeds_with_matching_remote_identity(self):
        fixture = Fixture()
        manifest = fixture.manifest()
        manifest_bytes = (
            json.dumps(manifest, indent=2, sort_keys=True).encode() + b"\n"
        )
        result = self.check_with(
            manifest_bytes,
            fixture.records(),
            probe_record(manifest_bytes),
        )
        self.assertEqual(result["identity_sha256"], digest(manifest_bytes))
        self.assertEqual(result["node_version"], "v22.23.0")

    def test_check_rejects_unsafe_or_oversized_manifest_file(self):
        fixture = Fixture()
        manifest_bytes = (
            json.dumps(fixture.manifest(), indent=2, sort_keys=True).encode() + b"\n"
        )
        for manifest_record in [
            probe_record(manifest_bytes, kind="symbolic link"),
            probe_record(manifest_bytes, mode=0o666),
            probe_record(manifest_bytes, mode=0o664),
            probe_record(
                manifest_bytes,
                size=browser_vm.MAX_MANIFEST_BYTES + 1,
            ),
        ]:
            with self.assertRaises(browser_vm.ToolError):
                self.check_with(manifest_bytes, fixture.records(), manifest_record)

    def test_check_rejects_oversized_or_unsafe_named_files(self):
        fixture = Fixture()
        manifest_bytes = (
            json.dumps(fixture.manifest(), indent=2, sort_keys=True).encode() + b"\n"
        )
        overrides = [
            {1: {"size": browser_vm.MAX_IDENTITY_FILE_BYTES + 1}},
            {2: {"mode": 0o664}},
            {6: {"kind": "directory"}},
            {0: {"mode": 0o777}},
        ]
        for override in overrides:
            with self.assertRaises(browser_vm.ToolError):
                self.check_with(
                    manifest_bytes,
                    fixture.records(override),
                    probe_record(manifest_bytes),
                )

    def test_check_rejects_named_file_size_mismatch(self):
        fixture = Fixture()
        manifest_bytes = (
            json.dumps(fixture.manifest(), indent=2, sort_keys=True).encode() + b"\n"
        )
        with self.assertRaises(browser_vm.ToolError):
            self.check_with(
                manifest_bytes,
                fixture.records({1: {"size": len(fixture.cli) + 1}}),
                probe_record(manifest_bytes),
            )

    def test_provision_reuses_the_remote_identity_checks(self):
        fixture = Fixture()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            worker = root / "worker"
            worker.mkdir()
            node = root / "node"
            node.write_bytes(fixture.node)
            node.chmod(0o755)
            chromium_dir = root / "chrome-headless-shell-linux64"
            chromium_dir.mkdir()
            chromium = chromium_dir / "chrome-headless-shell"
            chromium.write_bytes(fixture.chromium)
            chromium.chmod(0o755)
            args = ssh_namespace(
                root,
                node_source=node,
                worker_source=worker,
                chromium_source=chromium,
            )

            def fake_ssh(ssh_args, config_path, script, arguments):
                if script == browser_vm.PREFLIGHT_SCRIPT:
                    return b"preflight=ok\n"
                if script == browser_vm.NETWORK_PREFLIGHT_SCRIPT:
                    return b"network=preflight-ok\n"
                if script == browser_vm.SSHD_FORWARDING_SCRIPT:
                    return b"sshd=forwarding-ok\n"
                if script == browser_vm.PROBE_SCRIPT:
                    return fixture.records().encode()
                if script == browser_vm.CAT_SCRIPT:
                    path = arguments[0]
                    return {
                        "/opt/saya/worker/node_modules/@playwright/mcp/package.json": fixture.mcp,
                        "/opt/saya/worker/node_modules/playwright/package.json": fixture.playwright,
                        "/opt/saya/worker/node_modules/playwright-core/package.json": fixture.core,
                        "/opt/saya/worker/node_modules/playwright-core/browsers.json": fixture.browsers,
                    }[path]
                if '--version' in script:
                    return b"v22.23.0\n"
                return b""

            verifier = mock.Mock(side_effect=browser_vm.ToolError("unsafe manifest"))
            with mock.patch.object(browser_vm, "ssh_run", fake_ssh):
                with mock.patch.object(browser_vm, "checked", lambda *_: b""):
                    with mock.patch.object(
                        browser_vm, "verify_remote_identity", verifier
                    ):
                        with self.assertRaises(browser_vm.ToolError):
                            browser_vm.command_provision(args)
            verifier.assert_called_once()

    def test_provisioning_never_downloads_or_updates_versions(self):
        source = MODULE_PATH.read_text()
        for forbidden in ("npm install", "npx ", "curl ", "wget ", "apt-get ", "pip install"):
            self.assertNotIn(forbidden, source)
        self.assertNotIn("latest", source.lower())


class NetworkPrerequisiteTests(unittest.TestCase):
    def test_network_preflight_covers_tools_and_user_namespace_values(self):
        script = browser_vm.NETWORK_PREFLIGHT_SCRIPT
        for required in [
            "sudo",
            "unshare",
            "socat",
            "setpriv",
            "nsenter",
            "ip -o route show",
            "sudo -n true",
            "sudo -n unshare -n",
            "sudo -n nsenter -t",
            "unshare -Ur",
            "unprivileged_userns_clone",
            "apparmor_restrict_unprivileged_userns",
            "network=preflight-ok",
        ]:
            self.assertIn(required, script)
        forwarding = browser_vm.SSHD_FORWARDING_SCRIPT
        for required in [
            "sshd_bin",
            "-T 2>/dev/null",
            "sudo -n",
            "allowstreamlocalforwarding",
            "allowtcpforwarding",
            "sshd=forwarding-ok",
        ]:
            self.assertIn(required, forwarding)
        apparmor = browser_vm.APPARMOR_USERNS_SCRIPT
        self.assertIn(
            "kernel.apparmor_restrict_unprivileged_userns=0", apparmor
        )
        self.assertIn("/etc/sysctl.d/99-saya-browser-userns.conf", apparmor)
        # The dedicated VM is reconfigured, never the host.
        self.assertNotIn("sysctl --system", apparmor)

    def test_network_prerequisites_are_verified_and_fail_controlled(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = ssh_namespace(Path(temporary))
            calls = []

            def ok_ssh(a, config_path, script, arguments):
                calls.append(script)
                if script == browser_vm.SSHD_FORWARDING_SCRIPT:
                    return b"sshd=forwarding-ok\n"
                return b"network=preflight-ok\n"

            with mock.patch.object(browser_vm, "ssh_run", ok_ssh):
                browser_vm.verify_network_prerequisites(args, Path("/tmp/config"))
            self.assertEqual(
                calls,
                [
                    browser_vm.NETWORK_PREFLIGHT_SCRIPT,
                    browser_vm.SSHD_FORWARDING_SCRIPT,
                ],
            )

            def failing_ssh(a, config_path, script, arguments):
                raise browser_vm.ToolError("command failed (4): missing=unshare-net")

            with mock.patch.object(browser_vm, "ssh_run", failing_ssh):
                with self.assertRaises(browser_vm.ToolError):
                    browser_vm.verify_network_prerequisites(args, Path("/tmp/config"))

            def refusing_forwarding(a, config_path, script, arguments):
                if script == browser_vm.SSHD_FORWARDING_SCRIPT:
                    return b"sshd=streamlocal-forwarding-no\n"
                return b"network=preflight-ok\n"

            with mock.patch.object(browser_vm, "ssh_run", refusing_forwarding):
                with self.assertRaises(browser_vm.ToolError):
                    browser_vm.verify_network_prerequisites(args, Path("/tmp/config"))

    def test_apparmor_user_namespace_opt_in_runs_only_when_requested(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = ssh_namespace(Path(temporary))
            calls = []

            def fake_ssh(a, config_path, script, arguments):
                calls.append(script)
                if script == browser_vm.APPARMOR_USERNS_SCRIPT:
                    return b"apparmor-userns=disabled\n"
                if script == browser_vm.SSHD_FORWARDING_SCRIPT:
                    return b"sshd=forwarding-ok\n"
                return b"network=preflight-ok\n"

            with mock.patch.object(browser_vm, "ssh_run", fake_ssh):
                browser_vm.verify_network_prerequisites(args, Path("/tmp/config"))
            self.assertNotIn(browser_vm.APPARMOR_USERNS_SCRIPT, calls)
            self.assertIn(browser_vm.SSHD_FORWARDING_SCRIPT, calls)

            calls.clear()
            with mock.patch.object(browser_vm, "ssh_run", fake_ssh):
                browser_vm.verify_network_prerequisites(
                    args, Path("/tmp/config"), configure_userns=True
                )
            self.assertEqual(calls[0], browser_vm.APPARMOR_USERNS_SCRIPT)
            self.assertIn(browser_vm.NETWORK_PREFLIGHT_SCRIPT, calls)
            self.assertIn(browser_vm.SSHD_FORWARDING_SCRIPT, calls)


if __name__ == "__main__":
    unittest.main()
