#!/usr/bin/env python3
"""Check opt-in script effects and generated configuration without live requests."""
from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import live_modes_smoke
import live_protocol_smoke
import tui_journey


def generated_profile(command):
    config = Path(command[command.index('--config') + 1])
    profile = {}
    for line in config.read_text().splitlines():
        key, _, value = line.partition('=')
        if key.strip() in ('protocol', 'legacy_max_tokens'):
            profile[key.strip()] = json.loads(value)
    return profile


class ScriptHelpersTest(unittest.TestCase):
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
