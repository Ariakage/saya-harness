#!/usr/bin/env python3
"""Check opt-in script effects and generated configuration without live requests."""
from contextlib import redirect_stdout
import errno
import fcntl
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import live_modes_smoke
import live_protocol_smoke
import tui_journey
from process_witness import ProcessLease
from n2 import mcp_acceptance


def generated_profile(command):
    config = Path(command[command.index('--config') + 1])
    profile = {}
    for line in config.read_text().splitlines():
        key, _, value = line.partition('=')
        if key.strip() in ('protocol', 'legacy_max_tokens'):
            profile[key.strip()] = json.loads(value)
    return profile


class ScriptHelpersTest(unittest.TestCase):
    def test_process_lease_requires_live_readiness_before_release(self):
        with tempfile.TemporaryDirectory() as temporary:
            with ProcessLease(Path(temporary) / 'writer.fifo') as lease:
                flags = fcntl.fcntl(lease.fileno(), fcntl.F_GETFL)
                self.assertTrue(flags & os.O_NONBLOCK)
                self.assertTrue(fcntl.fcntl(lease.fileno(), fcntl.F_GETFD) & fcntl.FD_CLOEXEC)
                writer = os.open(lease.path, os.O_WRONLY | os.O_NONBLOCK)
                try:
                    os.write(writer, b'READY\n')
                    lease.wait_ready(timeout=1)
                    self.assertTrue(lease.started())
                    self.assertTrue(lease.ready())
                    self.assertTrue(lease.active())
                    self.assertFalse(lease.released())
                finally:
                    os.close(writer)
                lease.wait_released(timeout=1)
                self.assertTrue(lease.released())
                self.assertFalse(lease.active())

    def test_process_lease_never_started_is_not_successful_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            with ProcessLease(Path(temporary) / 'writer.fifo') as lease:
                self.assertFalse(lease.ready())
                self.assertFalse(lease.started())
                self.assertFalse(lease.released())
                with self.assertRaisesRegex(RuntimeError, 'positive control'):
                    lease.wait_released(timeout=0)
                with self.assertRaisesRegex(RuntimeError, 'no live'):
                    lease.wait_ready(timeout=0)

    def test_process_lease_rejects_readiness_observed_only_after_exit(self):
        with tempfile.TemporaryDirectory() as temporary:
            with ProcessLease(Path(temporary) / 'writer.fifo') as lease:
                writer = os.open(lease.path, os.O_WRONLY | os.O_NONBLOCK)
                os.write(writer, b'READY\n')
                os.close(writer)
                self.assertFalse(lease.ready())
                self.assertTrue(lease.started())
                self.assertFalse(lease.released())
                with self.assertRaisesRegex(RuntimeError, 'positive control'):
                    lease.wait_released(timeout=0)

    def test_process_lease_detects_writer_before_its_readiness_marker(self):
        with tempfile.TemporaryDirectory() as temporary:
            with ProcessLease(Path(temporary) / 'writer.fifo') as lease:
                writer = os.open(lease.path, os.O_WRONLY | os.O_NONBLOCK)
                try:
                    self.assertTrue(lease.started())
                    self.assertFalse(lease.ready())
                    self.assertFalse(lease.released())
                finally:
                    os.close(writer)

    def test_process_lease_inherited_writer_survives_parent_handle_close(self):
        with tempfile.TemporaryDirectory() as temporary:
            with ProcessLease(Path(temporary) / 'writer.fifo') as lease:
                writer = os.open(lease.path, os.O_WRONLY | os.O_NONBLOCK)
                child = None
                try:
                    os.write(writer, b'READY\n')
                    lease.wait_ready(timeout=1)
                    child = subprocess.Popen([sys.executable, '-c',
                        'import sys; sys.stdin.buffer.read(1)'],
                        stdin=subprocess.PIPE, pass_fds=(writer,))
                finally:
                    os.close(writer)
                try:
                    self.assertIsNone(child.poll())
                    self.assertFalse(lease.released())
                    with self.assertRaisesRegex(RuntimeError, 'survived cleanup'):
                        lease.wait_released(timeout=0)
                    child.communicate(b'x', timeout=5)
                    self.assertEqual(child.returncode, 0)
                    lease.wait_released(timeout=1)
                finally:
                    if child.poll() is None:
                        child.kill()
                    child.communicate(timeout=5)

    def test_process_lease_close_is_idempotent_and_preserves_replaced_path(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'writer.fifo'
            with ProcessLease(path) as lease:
                descriptor = lease.fileno()
                path.unlink()
                path.write_text('replacement fixture')
            lease.close()
            with self.assertRaises(OSError) as error:
                os.fstat(descriptor)
            self.assertEqual(error.exception.errno, errno.EBADF)
            self.assertEqual(path.read_text(), 'replacement fixture')
            with self.assertRaisesRegex(RuntimeError, 'closed'):
                lease.ready()
            with self.assertRaises(FileExistsError):
                ProcessLease(path)
            self.assertEqual(path.read_text(), 'replacement fixture')

    def test_process_lease_context_cleans_reader_and_fifo_after_exception(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'writer.fifo'
            with self.assertRaisesRegex(ValueError, 'controlled failure'):
                with ProcessLease(path) as lease:
                    descriptor = lease.fileno()
                    raise ValueError('controlled failure')
            self.assertFalse(path.exists())
            with self.assertRaises(OSError) as error:
                os.fstat(descriptor)
            self.assertEqual(error.exception.errno, errno.EBADF)
            lease.close()

    def test_stdio_fixture_waits_for_live_host_acknowledgement_before_publishing_pid(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pid = root / 'namespace-pid'
            log = root / 'methods.log'
            with ProcessLease(root / 'server.lease') as lease:
                child = subprocess.Popen([sys.executable, '-u', '-c', mcp_acceptance.STDIO_SERVER,
                    str(pid), str(log), 'normal', str(lease.path)],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                try:
                    lease.wait_ready(timeout=5)
                    self.assertFalse(pid.exists())
                    self.assertFalse(Path(str(lease.path) + '.ack').exists())
                    self.assertTrue(mcp_acceptance.acknowledge_ready_lease(lease))
                    deadline = time.monotonic() + 5
                    while not pid.exists():
                        self.assertLess(time.monotonic(), deadline, 'fixture did not publish its namespace PID')
                        time.sleep(0.01)
                    self.assertGreater(int(pid.read_text()), 0)
                    stdout, stderr = child.communicate(timeout=5)
                    self.assertEqual(child.returncode, 0, stderr)
                    self.assertEqual(stdout, b'')
                    lease.wait_released(timeout=1)
                finally:
                    if child.poll() is None:
                        child.kill()
                    child.communicate(timeout=5)

    def test_stdio_acknowledgement_rejects_a_previously_live_released_writer(self):
        with tempfile.TemporaryDirectory() as temporary:
            with ProcessLease(Path(temporary) / 'server.lease') as lease:
                writer = os.open(lease.path, os.O_WRONLY | os.O_NONBLOCK)
                try:
                    os.write(writer, b'READY\n')
                    lease.wait_ready(timeout=1)
                finally:
                    os.close(writer)
                self.assertTrue(lease.ready())
                self.assertTrue(lease.released())
                self.assertFalse(mcp_acceptance.acknowledge_ready_lease(lease))
                self.assertFalse(Path(str(lease.path) + '.ack').exists())

    def test_tui_failure_diagnostics_preserve_readable_exit_tail_and_pending_metadata(self):
        reader, writer = os.pipe()
        try:
            screen = tui_journey.Screen(160, 40)
            raw = bytearray(b'x' * (3 * 1024 * 1024))
            terminal = SimpleNamespace(master=reader, screen=screen, raw=raw,
                process=SimpleNamespace(poll=lambda: 1, supervisor=SimpleNamespace(poll=lambda: None)))

            def pump(_timeout):
                data = os.read(reader, 65536)
                raw.extend(data)
                screen.feed(data)

            terminal.pump = pump
            tail = b'CONTROLLED_FINAL_ERROR_TAIL\x1b[\xe7\x95'
            os.write(writer, tail)
            with tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                value = mcp_acceptance.save_tui_diagnostics(
                    terminal, directory, 'initial-title', RuntimeError('controlled failure'))
                captured = (directory / 'mcp-tui-raw.ansi').read_bytes()
                saved = json.loads((directory / 'mcp-tui-diagnostics.json').read_text())
                self.assertEqual(saved, value)
                self.assertEqual(len(captured), 2 * 1024 * 1024)
                self.assertTrue(captured.endswith(tail))
                self.assertEqual(value['raw_bytes_retained'], len(captured))
                self.assertEqual(value['raw_tail'], captured[-8192:].decode(errors='replace'))
                self.assertEqual(value['cli_exit_code'], 1)
                self.assertIsNone(value['supervisor_exit_code'])
                self.assertEqual(value['phase'], 'initial-title')
                self.assertEqual(value['failure_type'], 'RuntimeError')
                self.assertEqual(value['failure'], 'controlled failure')
                self.assertEqual(value['drained_chunks'], 1)
                self.assertIsNone(value['drain_error'])
                self.assertEqual(value['pending_ansi_characters'], 2)
                self.assertEqual(value['pending_utf8_bytes'], 2)
                text = (directory / 'mcp-tui-last-screen.txt').read_text()
                self.assertIn('CONTROLLED_FINAL_ERROR_TAIL', text)
                self.assertLessEqual(len(text), 32768)
                cells = json.loads((directory / 'mcp-tui-last-cells.json').read_text())
                self.assertFalse(cells['truncated'])
                self.assertEqual((cells['columns'], cells['rows']), (160, 40))
        finally:
            os.close(reader)
            os.close(writer)

    def test_tui_diagnostics_bound_screen_characters_cells_and_colors(self):
        reader, writer = os.pipe()
        try:
            screen = tui_journey.Screen(300, 120)
            screen.cells = [['界'] * screen.columns for _ in range(screen.rows)]
            terminal = SimpleNamespace(master=reader, screen=screen, raw=bytearray(),
                process=SimpleNamespace(poll=lambda: 0, supervisor=SimpleNamespace(poll=lambda: 0)))
            with tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                value = mcp_acceptance.save_tui_diagnostics(terminal, directory, 'complete')
                captured = json.loads((directory / 'mcp-tui-last-cells.json').read_text())
                self.assertTrue(captured['truncated'])
                self.assertEqual((captured['columns'], captured['rows']), (300, 120))
                for name in ('cells', 'colors'):
                    self.assertEqual(len(captured[name]), 64)
                    self.assertTrue(all(len(row) == 240 for row in captured[name]))
                text = (directory / 'mcp-tui-last-screen.txt').read_text()
                self.assertEqual(len(text), 32768)
                self.assertEqual(text, screen.text[-32768:])
                self.assertEqual((directory / 'mcp-tui-raw.ansi').read_bytes(), b'')
                self.assertEqual(value['raw_bytes_retained'], 0)
                self.assertEqual(value['drained_chunks'], 0)
                self.assertEqual(value['phase'], 'complete')
                self.assertEqual(value['cli_exit_code'], 0)
                self.assertEqual(value['supervisor_exit_code'], 0)
                self.assertEqual(value['pending_ansi_characters'], 0)
                self.assertEqual(value['pending_utf8_bytes'], 0)
                self.assertIsNone(value['failure_type'])
                self.assertIsNone(value['failure'])
        finally:
            os.close(reader)
            os.close(writer)

    def test_screen_capture_requires_an_explicit_directory(self):
        with mock.patch.dict(os.environ, {}, clear=True), \
             mock.patch.object(Path, 'mkdir') as mkdir, \
             mock.patch.object(Path, 'write_text') as write:
            tui_journey.save_interactive_screen(None, 'fixture')
            mkdir.assert_not_called()
            write.assert_not_called()
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / 'captures'
            screen = SimpleNamespace(columns=2, rows=1, cells=[['X', ' ']], colors=[[]])
            with mock.patch.dict(os.environ, {'SAYA_TUI_SNAPSHOT_DIR': str(directory)}):
                tui_journey.save_interactive_screen(SimpleNamespace(screen=screen), 'fixture')
            saved = json.loads((directory / 'tui-command-palette-fixture-cells.json').read_text())
            self.assertEqual(saved['cells'], screen.cells)
            self.assertEqual(saved['columns'], 2)

    def test_docker_platform_defaults_to_the_engine_and_accepts_an_override(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / 'main.rs'
            source.write_text('fn main() {}')
            result = subprocess.CompletedProcess([], 0, b'lines=2 words=4\n', b'')
            with mock.patch.dict(os.environ, {'SAYA_LIVE_API_KEY': 'synthetic-fixture'}), \
                 mock.patch.object(live_modes_smoke.subprocess, 'run', return_value=result) as run:
                self.assertTrue(live_modes_smoke.sandbox_check(source, 'rust-fixture'))
                command = run.call_args.args[0]
                self.assertNotIn('--platform', command)
                self.assertNotIn('SAYA_LIVE_API_KEY', run.call_args.kwargs['env'])
                self.assertTrue(live_modes_smoke.sandbox_check(source, 'rust-fixture', 'linux/amd64'))
                command = run.call_args.args[0]
                self.assertEqual(command[command.index('--platform') + 1], 'linux/amd64')

    def test_live_mode_binary_and_legacy_token_parameter_are_explicit(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / 'candidate'
            binary.write_bytes(b'fixture')
            environment = {
                'SAYA_LIVE_API_KEY': 'synthetic-fixture',
                'SAYA_LIVE_BASE_URL': 'http://127.0.0.1:9/v1',
                'SAYA_LIVE_MODEL': 'fixture', 'SAYA_LIVE_SANDBOX_IMAGE': 'rust-fixture',
            }
            for legacy in [False, True]:
                profiles = []

                def invoke(command, **kwargs):
                    self.assertEqual(Path(command[0]), binary.resolve())
                    profiles.append(generated_profile(command))
                    return subprocess.CompletedProcess(command, 1, b'', b'')

                args = ['live_modes_smoke.py', '--binary', str(binary)]
                if legacy:
                    args.append('--legacy-max-tokens')
                with mock.patch.dict(os.environ, environment, clear=True), \
                     mock.patch.object(sys, 'argv', args), \
                     mock.patch.object(live_modes_smoke.subprocess, 'run', side_effect=invoke), \
                     redirect_stdout(io.StringIO()):
                    self.assertEqual(live_modes_smoke.main(), 1)
                self.assertEqual(len(profiles), 4)
                self.assertTrue(all(profile['legacy_max_tokens'] is legacy for profile in profiles))

    def test_live_protocol_legacy_flag_only_configures_chat_completions(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / 'candidate'
            binary.write_bytes(b'fixture')
            for legacy in [False, True]:
                profiles = []

                def invoke(command, **kwargs):
                    profiles.append(generated_profile(command))
                    return subprocess.CompletedProcess(command, 1, b'', b'')

                args = ['live_protocol_smoke.py', '--binary', str(binary), '--model', 'fixture',
                        '--context-window', '128000', '--chat-root', 'http://127.0.0.1:9/v1',
                        '--responses-root', 'http://127.0.0.1:9/v1',
                        '--anthropic-root', 'http://127.0.0.1:9/v1']
                if legacy:
                    args.append('--legacy-max-tokens')
                with mock.patch.dict(os.environ, {'SAYA_LIVE_API_KEY': 'synthetic-fixture'}, clear=True), \
                     mock.patch.object(sys, 'argv', args), \
                     mock.patch.object(live_protocol_smoke.subprocess, 'run', side_effect=invoke), \
                     redirect_stdout(io.StringIO()):
                    self.assertEqual(live_protocol_smoke.main(), 1)
                self.assertEqual(len(profiles), 6)
                for profile in profiles:
                    if profile['protocol'] == 'openai-chat-completions':
                        self.assertIs(profile['legacy_max_tokens'], legacy)
                    else:
                        self.assertNotIn('legacy_max_tokens', profile)


if __name__ == '__main__':
    unittest.main()
