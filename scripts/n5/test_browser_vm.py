#!/usr/bin/env python3
"""Offline regression checks for the explicit Colima browser VM tool."""

import hashlib
import importlib.util
import json
import os
import shutil
import stat
from pathlib import Path
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("browser_vm.py")
SPEC = importlib.util.spec_from_file_location("browser_vm", MODULE_PATH)
browser_vm = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(browser_vm)


def canonical_tempdir():
    # macOS default TMPDIR (/var/folders/...) traverses the /var -> private/var symlink;
    # resolve only the test parent so open_root still rejects symlinks created inside tests.
    return tempfile.TemporaryDirectory(dir=str(Path(tempfile.gettempdir()).resolve()))


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
            {name: {"sha256": "a" * 64, "entries": 1, "bytes": 0}
             for name in ("node", "worker", "chromium")},
        )


def namespace(**overrides):
    values = {
        "colima": Path("/opt/homebrew/bin/colima"),
        "profile": "soyo-browser",
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
        "profile": "soyo-browser",
        "host": "127.0.0.1",
        "user": "soyo",
        "port": 2222,
        "identity_file": identity,
        "known_hosts": known_hosts,
        "remote_root": "/opt/soyo",
        "chromium_dir_name": "chrome-headless-shell-linux64",
    }
    values.update(overrides)
    return type("Args", (), values)()


class InstallationTreeTests(unittest.TestCase):
    def tree(self, root):
        root.chmod(0o755)
        (root / "empty").write_bytes(b"")
        (root / "empty").chmod(0o644)
        (root / "lib").mkdir(mode=0o755)
        (root / "lib/core.js").write_bytes(b"export{}")
        (root / "lib/core.js").chmod(0o644)
        (root / "alias").symlink_to("lib/core.js")
        if (root / "alias").lstat().st_mode & 0o777 != 0o777:
            os.chmod(root / "alias", 0o777, follow_symlinks=False)

    def test_golden_tree_includes_modes_files_and_internal_link(self):
        self.assertTrue(hasattr(browser_vm, "installation_tree"), "closed tree scanner missing")
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            self.tree(root)
            self.assertEqual(browser_vm.installation_tree(root), {
                "sha256": "7806fc4375d6d69b9139c1fe18869de403a12a33db8873cc53d74e3cea570b28",
                "entries": 5, "bytes": 8,
            })

    def test_directory_depth_allows_leaf_below_128_directories(self):
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            current = root
            for _ in range(128):
                current = current / "d"
                current.mkdir()
            (current / "leaf").write_bytes(b"x")
            try:
                identity = browser_vm.installation_tree(root)
            except browser_vm.ToolError as error:
                self.fail(f"leaf below 128 directories must be allowed: {error}")
            self.assertEqual(identity["entries"], 130)
            (current / "too-deep").mkdir()
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.installation_tree(root)

    def test_directory_collection_stops_before_consuming_unbounded_names(self):
        self.assertTrue(hasattr(browser_vm, "MAX_TREE_ENTRIES"), "shared tree bound missing")
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            for name in ("a", "b", "c", "d", "e"):
                (root / name).write_bytes(b"")
            scandir = os.scandir
            observed = []
            class ObservedEntries:
                def __init__(self, descriptor):
                    self.entries = scandir(descriptor)
                def __enter__(self):
                    self.entries.__enter__()
                    return self
                def __exit__(self, *args):
                    return self.entries.__exit__(*args)
                def __iter__(self):
                    for entry in self.entries:
                        observed.append(entry.name)
                        yield entry
            with mock.patch.object(browser_vm, "MAX_TREE_ENTRIES", 3), \
                 mock.patch.object(browser_vm.os, "scandir", ObservedEntries):
                with self.assertRaises(browser_vm.ToolError):
                    browser_vm.installation_tree(root)
            # Root consumes one entry; the third observed child proves overflow.
            self.assertEqual(len(observed), 3)

    def test_rejects_hardlinked_symlink_even_when_alias_is_outside_tree(self):
        with canonical_tempdir() as temporary:
            parent = Path(temporary)
            root = parent / "tree"
            root.mkdir()
            (root / "file").write_bytes(b"x")
            link = root / "link"
            link.symlink_to("file")
            os.link(link, parent / "outside-alias", follow_symlinks=False)
            self.assertEqual(link.lstat().st_nlink, 2)
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.installation_tree(root)

    def test_link_mode_is_normalized_across_platforms(self):
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            self.tree(root)
            try:
                os.chmod(root / "alias", 0o755, follow_symlinks=False)
            except (NotImplementedError, OSError):
                self.skipTest("platform cannot assign a non-0777 symlink mode")
            self.assertEqual((root / "alias").lstat().st_mode & 0o7777, 0o755)
            self.assertEqual(browser_vm.installation_tree(root), {
                "sha256": "7806fc4375d6d69b9139c1fe18869de403a12a33db8873cc53d74e3cea570b28",
                "entries": 5, "bytes": 8,
            })

    def test_bundle_same_size_add_remove_and_mode_change_identity(self):
        self.assertTrue(hasattr(browser_vm, "installation_tree"), "closed tree scanner missing")
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            self.tree(root)
            original = browser_vm.installation_tree(root)
            (root / "lib/core.js").write_bytes(b"EXPORT{}")
            self.assertNotEqual(browser_vm.installation_tree(root), original)
            (root / "lib/core.js").write_bytes(b"export{}")
            (root / "new.js").write_bytes(b"")
            self.assertNotEqual(browser_vm.installation_tree(root), original)
            (root / "new.js").unlink()
            (root / "empty").chmod(0o600)
            self.assertNotEqual(browser_vm.installation_tree(root), original)
            (root / "empty").unlink()
            self.assertNotEqual(browser_vm.installation_tree(root), original)

    def test_rejects_symlinked_root_and_intermediate_directory(self):
        self.assertTrue(hasattr(browser_vm, "installation_tree"), "closed tree scanner missing")
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            (root / "real").mkdir()
            (root / "real/sub").mkdir()
            (root / "link").symlink_to("real")
            for path in (root / "link", root / "link/sub"):
                with self.subTest(path=path), self.assertRaises(browser_vm.ToolError):
                    browser_vm.installation_tree(path)

    def test_rejects_escaping_dangling_cyclic_and_file_traversal_links(self):
        self.assertTrue(hasattr(browser_vm, "installation_tree"), "closed tree scanner missing")
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            (root / "file").write_bytes(b"x")
            for target in ("../outside", "/etc/passwd", "missing", "link", "file/../file"):
                (root / "link").symlink_to(target)
                with self.subTest(target=target), self.assertRaises(browser_vm.ToolError):
                    browser_vm.installation_tree(root)
                (root / "link").unlink()
            (root / "sub").mkdir()
            (root / "sub/ok").symlink_to("../file")
            self.assertEqual(browser_vm.installation_tree(root)["entries"], 4)

    def test_rejects_hardlinks_fifo_unsafe_mode_and_controls(self):
        self.assertTrue(hasattr(browser_vm, "installation_tree"), "closed tree scanner missing")
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            file = root / "file"
            file.write_bytes(b"x")
            os.link(file, root / "hard")
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.installation_tree(root)
            (root / "hard").unlink()
            os.mkfifo(root / "fifo")
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.installation_tree(root)
            (root / "fifo").unlink()
            file.chmod(0o666)
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.installation_tree(root)
            file.chmod(0o644)
            (root / "bad\nname").write_bytes(b"")
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.installation_tree(root)

    def test_schema_rejects_unknown_named_fields_and_boolean_numbers(self):
        for group, field, value in ((None, "extra", 0), ("node", "extra", 0),
                                    ("chromium", "revision", True),
                                    ("worker_cli_js", "size", False),
                                    (None, "schema_version", True)):
            manifest = Fixture().manifest()
            entry = manifest if group is None else manifest[group]
            entry[field] = value
            with self.subTest(group=group, field=field), self.assertRaises(browser_vm.ToolError):
                browser_vm.validate_manifest_structure(manifest)

    def test_deadline_and_mutation_during_read_fail_closed(self):
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            file = root / "bundle.js"
            file.write_bytes(b"a" * 70000)
            with mock.patch.object(browser_vm.time, "monotonic", side_effect=[0, 21]):
                with self.assertRaises(browser_vm.ToolError):
                    browser_vm.installation_tree(root)
            read = os.read
            mutated = False
            def mutating_read(fd, count):
                nonlocal mutated
                data = read(fd, count)
                if data and not mutated:
                    mutated = True
                    file.write_bytes(b"b" * 70000)
                return data
            with mock.patch.object(browser_vm.os, "read", mutating_read):
                with self.assertRaises(browser_vm.ToolError):
                    browser_vm.installation_tree(root)
            self.assertTrue(mutated)

    def _assert_prefix_mutation_rejected(self, snapshot, same_timestamp_tick):
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            file = root / ("node" if snapshot else "bundle.js")
            file.write_bytes(b"a" * 70000)
            file.chmod(0o755 if snapshot else 0o644)
            original_read = os.read
            original_identity = browser_vm._metadata_identity
            observed = {}
            wanted = file.stat()

            def mutating_read(fd, count):
                data = original_read(fd, count)
                info = os.fstat(fd)
                if data and not observed and (info.st_dev, info.st_ino) == (wanted.st_dev, wanted.st_ino):
                    observed["read_bytes"] = len(data)
                    observed["before"] = original_identity(info)
                    with file.open("r+b") as output:
                        output.write(b"b" * 512)
                    observed["after"] = original_identity(os.fstat(fd))
                return data

            def same_tick_identity(info):
                # Test only the observation boundary: timestamp ticks do not advance.
                # Stat, actual file reads/writes and both content hashes remain real.
                return original_identity(info)[:-2] + ("same tick", "same tick")

            def operation():
                if snapshot:
                    browser_vm.snapshot_node(file, root / "snapshot")
                else:
                    browser_vm.installation_tree(root)

            with mock.patch.object(browser_vm.os, "read", mutating_read):
                if same_timestamp_tick:
                    with mock.patch.object(browser_vm, "_metadata_identity", same_tick_identity):
                        with self.assertRaises(browser_vm.ToolError):
                            operation()
                else:
                    try:
                        with self.assertRaises(browser_vm.ToolError):
                            operation()
                    finally:
                        print(json.dumps({"mutation_case": "snapshot" if snapshot else "tree",
                                          "observed_real_stat": observed}), flush=True)
            self.assertEqual(observed["read_bytes"], 65536)
            self.assertEqual(file.read_bytes(), b"b" * 512 + b"a" * (70000 - 512))

    def test_prefix_mutation_during_tree_read_is_rejected_with_real_stat(self):
        self._assert_prefix_mutation_rejected(snapshot=False, same_timestamp_tick=False)

    def test_prefix_mutation_during_node_copy_is_rejected_with_real_stat(self):
        self._assert_prefix_mutation_rejected(snapshot=True, same_timestamp_tick=False)

    def test_prefix_mutation_during_tree_read_is_rejected_in_same_timestamp_tick(self):
        self._assert_prefix_mutation_rejected(snapshot=False, same_timestamp_tick=True)

    def test_prefix_mutation_during_node_copy_is_rejected_in_same_timestamp_tick(self):
        self._assert_prefix_mutation_rejected(snapshot=True, same_timestamp_tick=True)

    def test_node_snapshot_copies_only_binary_preserving_safe_mode(self):
        self.assertTrue(hasattr(browser_vm, "snapshot_node"), "single node snapshot missing")
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            source = root / "bin"
            source.mkdir()
            (source / "node").write_bytes(b"node bytes")
            (source / "node").chmod(0o750)
            (source / "unrelated").write_bytes(b"must not copy")
            destination = root / "snapshot"
            browser_vm.snapshot_node(source / "node", destination)
            self.assertEqual(sorted(p.name for p in destination.iterdir()), ["node"])
            self.assertEqual((destination / "node").read_bytes(), b"node bytes")
            self.assertEqual((destination / "node").stat().st_mode & 0o7777, 0o750)
            self.assertEqual(destination.stat().st_mode & 0o7777, 0o755)
            (source / "node").chmod(0o777)
            with self.assertRaises(browser_vm.ToolError):
                browser_vm.snapshot_node(source / "node", root / "unsafe")

    def test_remote_reply_mismatch_and_oversized_shape_fail(self):
        self.assertTrue(hasattr(browser_vm, "remote_installation_trees"), "remote tree verifier missing")
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            args = ssh_namespace(root)
            layout = browser_vm.remote_layout(args)
            for payload in (b"{}", b" " * 4097,
                            json.dumps({name: {"sha256": "a" * 64, "entries": True, "bytes": 0}
                                        for name in ("node", "worker", "chromium")}).encode()):
                with mock.patch.object(browser_vm, "ssh_run", return_value=payload):
                    with self.assertRaises(browser_vm.ToolError):
                        browser_vm.remote_installation_trees(args, root / "config", layout)

    def test_fresh_remote_root_refuses_existing_tree_without_changing_it(self):
        self.assertTrue(hasattr(browser_vm, "require_fresh_remote_root"), "fresh root guard missing")
        with canonical_tempdir() as temporary:
            root = Path(temporary) / "install"
            def local_ssh(args, config, script, arguments, input_bytes=None):
                return browser_vm.checked(["/bin/sh", "-c", script, "test"] + arguments, input_bytes)
            with mock.patch.object(browser_vm, "ssh_run", local_ssh):
                browser_vm.require_fresh_remote_root(None, None, str(root))
                self.assertEqual(root.stat().st_mode & 0o777, 0o700)
                (root / "keep").write_bytes(b"old installation")
                with self.assertRaises(browser_vm.ToolError):
                    browser_vm.require_fresh_remote_root(None, None, str(root))
                self.assertEqual((root / "keep").read_bytes(), b"old installation")

    def test_v1_and_unknown_or_boolean_tree_fields_are_rejected(self):
        manifest = Fixture().manifest()
        manifest["schema_version"] = 1
        with self.assertRaises(browser_vm.ToolError):
            browser_vm.validate_manifest_structure(manifest)
        for field, value in (("entries", True), ("bytes", True), ("entries", 0),
                             ("bytes", 4294967297), ("sha256", "A" * 64), ("extra", 1)):
            manifest = Fixture().manifest()
            manifest["installation_trees"]["worker"][field] = value
            with self.subTest(field=field), self.assertRaises(browser_vm.ToolError):
                browser_vm.validate_manifest_structure(manifest)


class CreationTests(unittest.TestCase):
    def test_create_uses_no_mounts_and_no_ssh_agent(self):
        argv = browser_vm.colima_start_argv(namespace())
        self.assertEqual(argv[1:3], ["start", "--profile"])
        self.assertIn("--mount", argv)
        self.assertEqual(argv[argv.index("--mount") + 1], "none")
        self.assertIn("--ssh-agent=false", argv)
        self.assertNotIn("--mount-type", argv)

    def test_create_preserves_global_client_context(self):
        argv = browser_vm.colima_start_argv(namespace())
        self.assertIn("--activate=false", argv)

    def test_destroy_is_explicit_and_forced(self):
        argv = browser_vm.colima_delete_argv(namespace())
        self.assertEqual(argv[1:4], ["delete", "--profile", "soyo-browser"])
        self.assertEqual(argv[-1], "--force")

    def test_profile_rejects_injection(self):
        with self.assertRaises(browser_vm.ToolError):
            browser_vm.colima_start_argv(namespace(profile="soyo;rm"))
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
        with canonical_tempdir() as temporary:
            args = ssh_namespace(Path(temporary), host="[::1]")
            config = browser_vm.strict_ssh_config(args)
            self.assertIn("HostName ::1", config)
            self.assertNotIn("[::1]", config)
            argv = browser_vm.ssh_argv(args, Path("/tmp/config"), "true")
            self.assertIn("::1", argv)
            self.assertNotIn("[::1]", " ".join(argv))
            scp = browser_vm.scp_argv(
                args, Path("/tmp/config"), "/tmp/source", "/opt/soyo/node"
            )
            self.assertEqual(scp[-1], "soyo@[::1]:/opt/soyo/node")

    def test_symlink_to_unsafe_target_is_rejected(self):
        with canonical_tempdir() as temporary:
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
        with canonical_tempdir() as temporary:
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
        with canonical_tempdir() as temporary:
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
        with canonical_tempdir() as temporary:
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
            browser_vm.safe_remote_path("/opt/soyo/worker", "path"),
            "/opt/soyo/worker",
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
            {name: {"sha256": "a" * 64, "entries": 1, "bytes": 0}
             for name in ("node", "worker", "chromium")},
        )
        self.assertEqual(manifest["schema_version"], 2)
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
            {name: {"sha256": "a" * 64, "entries": 1, "bytes": 0}
             for name in ("node", "worker", "chromium")},
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
            {name: {"sha256": "a" * 64, "entries": 1, "bytes": 0}
             for name in ("node", "worker", "chromium")},
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

        def fake_ssh(args, config_path, script, arguments, input_bytes=None):
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

        with canonical_tempdir() as temporary:
            for payload in malformed:
                fake_ssh.payload = payload
                args = ssh_namespace(
                    Path(temporary), manifest_sha256=digest(payload)
                )
                with mock.patch.object(browser_vm, "ssh_run", fake_ssh):
                    with self.assertRaises(browser_vm.ToolError):
                        browser_vm.command_check(args)

    def test_remote_layout_revalidates_generated_paths(self):
        with canonical_tempdir() as temporary:
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
        with canonical_tempdir() as temporary:
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
        trees: dict | None = None,
    ) -> dict:
        fixture = Fixture()
        files = {
            "/opt/soyo/node/bin/node": fixture.node,
            "/opt/soyo/worker/node_modules/@playwright/mcp/package.json": fixture.mcp,
            "/opt/soyo/worker/node_modules/playwright/package.json": fixture.playwright,
            "/opt/soyo/worker/node_modules/playwright-core/package.json": fixture.core,
            "/opt/soyo/worker/node_modules/playwright-core/browsers.json": fixture.browsers,
        }

        def fake_ssh(args, config_path, script, arguments, input_bytes=None):
            if script == browser_vm.NETWORK_PREFLIGHT_SCRIPT:
                return b"network=preflight-ok\n"
            if script == browser_vm.SSHD_FORWARDING_SCRIPT:
                return b"sshd=forwarding-ok\n"
            if script == browser_vm.PROBE_SCRIPT:
                return (manifest_record + named_records).encode()
            if script == browser_vm.INSTALLATION_PROBE_SCRIPT:
                return json.dumps(trees or fixture.manifest()["installation_trees"]).encode()
            if script == browser_vm.CAT_SCRIPT:
                path = arguments[0]
                if path.endswith("identity.json"):
                    return manifest_bytes
                return files[path]
            if '--version' in script:
                return b"v22.23.0\n"
            raise AssertionError(f"unexpected script: {script}")

        with canonical_tempdir() as temporary:
            args = ssh_namespace(
                Path(temporary),
                manifest_sha256=expected_digest or digest(manifest_bytes),
            )
            with mock.patch.object(browser_vm, "ssh_run", fake_ssh):
                return browser_vm.command_check(args)

    def test_check_rejects_changed_installation_reply(self):
        fixture = Fixture()
        manifest = fixture.manifest()
        data = json.dumps(manifest).encode()
        changed = json.loads(json.dumps(manifest["installation_trees"]))
        changed["worker"]["sha256"] = "b" * 64
        with self.assertRaises(browser_vm.ToolError):
            self.check_with(data, fixture.records(), probe_record(data), trees=changed)

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

    def test_provision_existing_remote_root_never_copies_or_alters_members(self):
        fixture = Fixture()
        with canonical_tempdir() as temporary:
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
            existing = root / "existing"
            existing.mkdir()
            (existing / "keep").write_bytes(b"old")
            args = ssh_namespace(root, node_source=node, worker_source=worker,
                                 chromium_source=chromium, remote_root=str(existing))
            def local_ssh(a, config, script, arguments, input_bytes=None):
                if script == browser_vm.PREFLIGHT_SCRIPT:
                    return b"preflight=ok\n"
                if script == browser_vm.NETWORK_PREFLIGHT_SCRIPT:
                    return b"network=preflight-ok\n"
                if script == browser_vm.SSHD_FORWARDING_SCRIPT:
                    return b"sshd=forwarding-ok\n"
                result = browser_vm.run_command(["/bin/sh", "-c", script, "test"] + arguments)
                if result.returncode:
                    raise browser_vm.ToolError("remote root exists")
                return result.stdout
            with mock.patch.object(browser_vm, "ssh_run", local_ssh), \
                 mock.patch.object(browser_vm, "checked", side_effect=AssertionError("must not copy into existing installation")):
                with self.assertRaises(browser_vm.ToolError):
                    browser_vm.command_provision(args)
            self.assertEqual(sorted(p.name for p in existing.iterdir()), ["keep"])
            self.assertEqual((existing / "keep").read_bytes(), b"old")

    def test_provision_reuses_the_remote_identity_checks(self):
        fixture = Fixture()
        with canonical_tempdir() as temporary:
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

            expected_node = root / "expected-node"
            expected_node.mkdir(mode=0o755)
            (expected_node / "node").write_bytes(fixture.node)
            (expected_node / "node").chmod(0o755)
            local_trees = {"node": browser_vm.installation_tree(expected_node),
                           "worker": browser_vm.installation_tree(worker),
                           "chromium": browser_vm.installation_tree(chromium_dir)}

            def fake_ssh(ssh_args, config_path, script, arguments, input_bytes=None):
                if script == browser_vm.PREFLIGHT_SCRIPT:
                    return b"preflight=ok\n"
                if script == browser_vm.NETWORK_PREFLIGHT_SCRIPT:
                    return b"network=preflight-ok\n"
                if script == browser_vm.SSHD_FORWARDING_SCRIPT:
                    return b"sshd=forwarding-ok\n"
                if script == browser_vm.INSTALLATION_PROBE_SCRIPT:
                    return json.dumps(local_trees).encode()
                if script == browser_vm.PROBE_SCRIPT:
                    return fixture.records().encode()
                if script == browser_vm.CAT_SCRIPT:
                    path = arguments[0]
                    return {
                        "/opt/soyo/worker/node_modules/@playwright/mcp/package.json": fixture.mcp,
                        "/opt/soyo/worker/node_modules/playwright/package.json": fixture.playwright,
                        "/opt/soyo/worker/node_modules/playwright-core/package.json": fixture.core,
                        "/opt/soyo/worker/node_modules/playwright-core/browsers.json": fixture.browsers,
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


class LocalProvisionTests(unittest.TestCase):
    """Run real copies, tar extraction and tree checks behind a local SSH transport."""

    def provision(self, root, corrupt_tree=False):
        fixture = Fixture()
        worker = root / "worker"
        for relative, data in ((browser_vm.CLI_JS, fixture.cli),
                               (browser_vm.MCP_PACKAGE, fixture.mcp),
                               (browser_vm.PLAYWRIGHT_PACKAGE, fixture.playwright),
                               (browser_vm.PLAYWRIGHT_CORE_PACKAGE, fixture.core),
                               (browser_vm.BROWSERS_JSON, fixture.browsers),
                               ("node_modules/playwright-core/lib/coreBundle.js", b"bundle")):
            path = worker / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod(0o644)
        (worker / "bundle-link").symlink_to("node_modules/playwright-core/lib/coreBundle.js")
        node_source_dir = root / "host-bin"
        node_source_dir.mkdir()
        node = node_source_dir / "node"
        node.write_bytes(b"#!/bin/sh\nprintf 'v22.23.0\\n'\n")
        node.chmod(0o750)
        (node_source_dir / "unrelated").write_bytes(b"not node")
        chromium_dir = root / "chrome-headless-shell-linux64"
        chromium_dir.mkdir()
        chromium = chromium_dir / "chrome-headless-shell"
        chromium.write_bytes(fixture.chromium)
        chromium.chmod(0o755)
        (chromium_dir / "resource").write_bytes(b"resource")
        (chromium_dir / "resource-link").symlink_to("resource")
        remote = root / "remote"
        args = ssh_namespace(root, node_source=node, worker_source=worker,
                             chromium_source=chromium, remote_root=str(remote))
        copied = []
        def checked(argv, input_bytes=None):
            if argv[0] == str(args.scp):
                source, destination = argv[-2:]
                destination = destination.split(":", 1)[1]
                copied.append(destination)
                shutil.copyfile(source, destination)
                return b""
            result = browser_vm.run_command(argv, input_bytes)
            if result.returncode:
                raise browser_vm.ToolError(result.stderr.decode())
            return result.stdout
        def local_ssh(a, config, script, arguments, input_bytes=None):
            if script == browser_vm.PREFLIGHT_SCRIPT:
                return b"preflight=ok\n"
            if script == browser_vm.NETWORK_PREFLIGHT_SCRIPT:
                return b"network=preflight-ok\n"
            if script == browser_vm.SSHD_FORWARDING_SCRIPT:
                return b"sshd=forwarding-ok\n"
            if script == browser_vm.PROBE_SCRIPT:
                output = []
                for value in arguments:
                    path = Path(value)
                    info = path.lstat()
                    kind = "regular file" if stat.S_ISREG(info.st_mode) else "symbolic link"
                    output.append(probe_record(path.read_bytes(), info.st_mode & 0o7777, kind))
                return "".join(output).encode()
            if script == browser_vm.INSTALLATION_PROBE_SCRIPT:
                trees = {name: browser_vm.installation_tree(Path(path)) for name, path in
                         zip(("node", "worker", "chromium"), arguments[1:4])}
                if corrupt_tree:
                    trees["worker"]["sha256"] = "0" * 64
                return json.dumps(trees).encode()
            return checked(["/bin/sh", "-c", script, "test"] + arguments, input_bytes)
        with mock.patch.object(browser_vm, "checked", checked), \
             mock.patch.object(browser_vm, "ssh_run", local_ssh):
            result = browser_vm.command_provision(args)
            manifest_bytes = Path(result["identity_manifest"]).read_bytes()
            manifest = browser_vm.validate_manifest_structure(json.loads(manifest_bytes))
            self.assertEqual(manifest["installation_trees"]["worker"], browser_vm.installation_tree(worker))
            self.assertEqual(manifest["installation_trees"]["chromium"], browser_vm.installation_tree(chromium_dir))
            self.assertEqual(manifest["installation_trees"]["node"]["entries"], 2)
            self.assertEqual(sorted(p.name for p in Path(result["node"]).parent.iterdir()), ["node"])
            self.assertEqual(Path(result["node"]).stat().st_mode & 0o7777, 0o750)
            self.assertTrue((Path(result["worker_root"]) / "bundle-link").is_symlink())
            self.assertTrue((Path(result["chromium"]).parent / "resource-link").is_symlink())
            self.assertFalse((remote / "worker.tar").exists())
            self.assertFalse((remote / "chromium.tar").exists())
            self.assertEqual(result["identity_sha256"], digest(manifest_bytes))
            args.manifest_sha256 = result["identity_sha256"]
            self.assertEqual(browser_vm.command_check(args)["identity_sha256"], result["identity_sha256"])
            return copied

    def test_real_local_archives_preserve_links_and_check_matches(self):
        with canonical_tempdir() as temporary:
            copied = self.provision(Path(temporary))
            self.assertTrue(any(path.endswith("chromium.tar") for path in copied))

    def test_provision_rejects_remote_tree_mismatch_before_manifest_upload(self):
        with canonical_tempdir() as temporary:
            root = Path(temporary)
            with self.assertRaises(browser_vm.ToolError):
                self.provision(root, corrupt_tree=True)
            self.assertFalse((root / "remote/identity.json").exists())


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
        self.assertIn("/etc/sysctl.d/99-soyo-browser-userns.conf", apparmor)
        # The dedicated VM is reconfigured, never the host.
        self.assertNotIn("sysctl --system", apparmor)

    def test_network_prerequisites_are_verified_and_fail_controlled(self):
        with canonical_tempdir() as temporary:
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
        with canonical_tempdir() as temporary:
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
