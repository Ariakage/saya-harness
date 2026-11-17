#!/usr/bin/env python3
"""Offline tests; never launch a VM, candidate, or paid model."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from contextlib import nullcontext, redirect_stdout
import io
from types import SimpleNamespace

PATH = Path(__file__).resolve().with_name('acceptance.py')


class AcceptanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location('n5_acceptance', PATH)
        cls.a = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.a)

    def test_fixture_failure_is_scoped_but_preserved_in_reports(self):
        provider = SimpleNamespace(errors=[], counts={'execution': 0})
        report = {'cases': []}
        def broken():
            provider.errors.append('first fixture assertion')
            self.a.require(not self.a.current_errors(provider), 'fixture failed')
        first = self.a.run_case(report, provider, 'broken', broken)
        second = self.a.run_case(report, provider, 'independent',
            lambda: self.a.require(not self.a.current_errors(provider), 'old error contaminated case'))
        self.assertEqual(first['status'], 'failed')
        self.assertEqual(first['provider_errors'], ['first fixture assertion'])
        self.assertEqual(second['status'], 'passed')
        self.assertEqual(second['provider_errors'], [])
        self.assertEqual(provider.errors, ['first fixture assertion'])
        self.assertEqual(self.a.totals(report['cases']), {'failed': 1, 'passed': 1})

    def test_fixture_error_cannot_pass_when_operation_ignores_it(self):
        provider = SimpleNamespace(errors=[], counts={})
        result = self.a.run_case({'cases': []}, provider, 'ignored',
                                lambda: provider.errors.append('real assertion'))
        self.assertEqual(result['status'], 'failed')
        self.assertIn('real assertion', result['error'])

    def test_long_page_fits_element_budget_and_retains_tail(self):
        from urllib.request import urlopen
        with self.a.website() as (origin, _requests):
            with urlopen(origin) as response:
                body = response.read().decode()
        self.assertLess(body.count('<p>'), 250)
        self.assertIn('N5_LONG_0199', body)
        self.assertGreater(len(body.encode()), 16384)
        self.assertLess(max(len(text) for text in body.split('<p>')), 1024)

    def test_failure_cleanup_releases_gate_and_waits_before_close(self):
        import signal
        gate = self.a.threading.Event()
        state = {'code': None, 'closed': False}
        def pump(*_args):
            state['code'] = 0
        terminal = SimpleNamespace(process=SimpleNamespace(pid=123, poll=lambda: state['code']),
            pump=pump, close=lambda: state.update(closed=True))
        record = {}
        def terminate(pid, sig):
            self.assertTrue(gate.is_set())
            self.assertEqual((pid, sig), (123, signal.SIGTERM))
            self.assertFalse(state['closed'])
        with patch.object(self.a.os, 'kill', side_effect=terminate):
            self.a.close_terminal(terminal, record, gate)
        self.assertTrue(state['closed'])
        self.assertEqual(record['cleanup']['exit_code'], 0)
        self.assertFalse(record['cleanup']['forced_sigkill'])

    def test_failure_cleanup_forced_kill_is_evidence_and_failure(self):
        state = {'code': None}
        terminal = SimpleNamespace(process=SimpleNamespace(pid=123, poll=lambda: state['code']),
            pump=lambda *_args: None, close=lambda: state.update(code=-9))
        record = {}
        with patch.object(self.a.os, 'kill'), self.assertRaisesRegex(AssertionError, 'SIGKILL'):
            self.a.close_terminal(terminal, record, seconds=0)
        self.assertTrue(record['cleanup']['forced_sigkill'])
        self.assertEqual(record['cleanup']['exit_code'], -9)

    def test_background_panel_handles_wrapped_ids_and_closes_before_commands(self):
        job = 'job_' + 'a' * 60
        screen = SimpleNamespace(text='', details_text=lambda: job[:32] + '\nrunning browser_navigate')
        def send(key):
            self.assertEqual(key, b'\x1b')
            screen.text = ''
        def line(command):
            self.assertNotIn('Background tasks · current run', screen.text)
            self.assertEqual(command, '/ps')
            screen.text = 'Background tasks · current run'
        def wait(predicate, _description, _timeout=20):
            self.assertTrue(predicate())
        terminal = SimpleNamespace(screen=screen, send=send, line=line, wait=wait)
        self.a.background_panel(terminal, job, 'running')
        self.a.background_panel(terminal, job, 'running')
        self.a.close_background_panel(terminal)
        self.assertEqual(screen.text, '')

    def test_cancel_waits_for_rendered_tool_after_http_dispatch(self):
        screen = SimpleNamespace(text='earlier frame')
        def wait(predicate, description, timeout):
            self.assertFalse(predicate())
            screen.text = 'browser_navigate running'
            self.assertTrue(predicate())
        terminal = SimpleNamespace(screen=screen, wait=wait)
        self.a.wait_browser_tool(terminal, 'browser_navigate')

    def test_approval_matches_complete_wrapped_artifact_not_prefix(self):
        self.a.check_approval('│Action upload Artifact media_1234│\n│5678 Document doc_a │',
                              'browser_upload', {'artifact_ref': 'media_12345678'})
        with self.assertRaises(AssertionError):
            self.a.check_approval('upload Artifact media_1234', 'browser_upload',
                                  {'artifact_ref': 'media_12345678'})

    def test_background_panel_scrolls_to_wrapped_state(self):
        job = 'job_' + 'b' * 60
        state = {'scrolled': False}
        screen = SimpleNamespace(text='Background tasks · current run',
            details_text=lambda: 'failed · outcome_unknown' if state['scrolled'] else job[:32] + ' browser_navigate')
        terminal = SimpleNamespace(screen=screen, send=lambda key: state.update(scrolled=True), settle=lambda: None)
        self.assertIn('outcome_unknown', self.a.background_state_text(terminal))

    def test_complete_snapshot_rejects_limit_and_partial_coverage(self):
        view = self.host_view()
        view['data']['output_refs'] = [{'retained_bytes': 45000, 'artifact_complete': True,
                                       'completion': 'limit_reached', 'coverage': 'retained_ranges_only'}]
        checker = self.a.browser_steps('http://127.0.0.1:4')[2][3]
        with self.assertRaises(AssertionError):
            checker(view, None)
        view['data']['output_refs'][0].update(completion='eof', coverage='complete')
        checker(view, None)
        view['data']['truncated'] = True
        with self.assertRaises(AssertionError):
            checker(view, None)

    def retained_metadata(self, raw):
        import hashlib
        scope = {'session_id': 'session_fixture', 'run_id': 'run_fixture',
                 'call_id': 'call_fixture', 'stream': 'browser'}
        record = ('{"version":1,"id":"out_fixture","scope":'
                  '{"session_id":"session_fixture","run_id":"run_fixture",'
                  '"call_id":"call_fixture","stream":"browser"},'
                  '"capture_limit_bytes":262144,"total_bytes_seen":' + str(len(raw)) +
                  ',"completion":"eof","segments":[{"source_window":{"start":0,"end":' +
                  str(len(raw)) + '},"text":' + json.dumps(raw.decode(), ensure_ascii=False) +
                  '}],"withheld_reason":null}')
        digest = hashlib.sha256(record.encode()).hexdigest()
        return {'output_ref': 'out_fixture_' + digest, 'sha256': digest,
                'scope': scope, 'capture_limit_bytes': 262144, 'total_bytes_seen': len(raw),
                'retained_bytes': len(raw), 'completion': 'eof', 'withheld_reason': None,
                'retained_ranges': [{'start': 0, 'end': len(raw)}]}

    def test_output_read_follows_cursor_and_verifies_complete_bytes(self):
        import hashlib
        view = self.host_view()
        view['data']['text'] = ' '.join('N5_LONG_%04d' % n for n in range(200))
        raw = json.dumps(view['data']).encode()
        metadata = self.retained_metadata(raw)
        self.assertNotEqual(metadata['sha256'], hashlib.sha256(raw).hexdigest())
        view['data']['output_refs'] = [metadata]
        steps = self.a.browser_steps('http://127.0.0.1:4')
        dialogue = SimpleNamespace(observed={'snapshot': view}, steps=steps, index=4)
        first = {'ok': True, 'data': {'metadata': metadata, 'chunks': [
            {'segment': 0, 'byte_offset': 0, 'text': raw[:17].decode()}],
            'next_cursor': {'segment': 0, 'byte_offset': 17, 'view_sha256': metadata['sha256']}}}
        steps[3][3](first, dialogue)
        continuation = dialogue.steps[dialogue.index]
        self.assertEqual(continuation[1], 'output_read')
        self.assertEqual(continuation[2]['cursor'], first['data']['next_cursor'])
        dialogue.index += 1
        continuation[3]({'ok': True, 'data': {'metadata': metadata, 'chunks': [
            {'segment': 0, 'byte_offset': 17, 'text': raw[17:].decode()}], 'next_cursor': None}}, dialogue)
        self.assertEqual(dialogue.output_read_verified,
                         {'bytes': len(raw), 'view_sha256': metadata['sha256'],
                          'content_sha256': hashlib.sha256(raw).hexdigest(), 'pages': 2})

    def test_output_read_rejects_corruption_and_early_eof_with_valid_metadata(self):
        raw = b'complete retained browser payload'
        metadata = self.retained_metadata(raw)
        for payload in (raw[:-1] + b'X', raw[:-1]):
            with self.subTest(payload=payload):
                view = self.host_view()
                view['data']['output_refs'] = [metadata]
                steps = self.a.browser_steps('http://127.0.0.1:4')
                dialogue = SimpleNamespace(observed={'snapshot': view}, steps=steps, index=4)
                result = {'ok': True, 'data': {'metadata': metadata, 'chunks': [
                    {'segment': 0, 'byte_offset': 0, 'text': payload.decode()}], 'next_cursor': None}}
                with self.assertRaisesRegex(AssertionError, 'OutputRef'):
                    steps[3][3](result, dialogue)

    def test_cli_pty_keeps_exact_canonical_baseline_after_real_input(self):
        import termios
        Terminal = self.a.terminal_class(canonical=True)
        terminal = Terminal([self.a.sys.executable, '-I', '-c', 'input(); print("done", flush=True)'],
                            self.a.ROOT, self.a.os.environ)
        try:
            baseline = self.a.prepare_cli_terminal(terminal)
            terminal.line('hello')
            terminal.wait(lambda: terminal.process.poll() is not None, 'canonical reader exit', 10)
            self.assertEqual(termios.tcgetattr(terminal.slave), terminal.before)
            self.assertIn('canonical_initial_fd_flags', baseline)
        finally:
            terminal.close()

    def test_cli_baseline_does_not_hide_startup_terminal_mutation(self):
        import termios
        Terminal = self.a.terminal_class(canonical=True)
        program = ('import termios; attributes = termios.tcgetattr(0); '
                   'attributes[3] &= ~termios.ECHO; '
                   'termios.tcsetattr(0, termios.TCSANOW, attributes); '
                   'print("startup-ready", flush=True); input()')
        terminal = Terminal([self.a.sys.executable, '-I', '-c', program],
                            self.a.ROOT, self.a.os.environ)
        try:
            initial = repr(terminal.before)
            terminal.until('startup-ready')
            baseline = self.a.prepare_cli_terminal(terminal)
            terminal.line('finish')
            terminal.wait(lambda: terminal.process.poll() is not None, 'mutating reader exit', 10)
            self.assertEqual(baseline['canonical_initial_attributes'], initial)
            self.assertNotEqual(termios.tcgetattr(terminal.slave), terminal.before)
        finally:
            terminal.close()

    def test_cleanup_records_forced_kill_when_output_pump_fails(self):
        state = {'code': None}
        def pump(*_args):
            raise RuntimeError('broken PTY output')
        terminal = SimpleNamespace(process=SimpleNamespace(pid=123, poll=lambda: state['code']),
            pump=pump, close=lambda: state.update(code=-9))
        record = {}
        with patch.object(self.a.os, 'kill'), self.assertRaisesRegex(RuntimeError, 'broken PTY'):
            self.a.close_terminal(terminal, record)
        self.assertTrue(record['cleanup']['forced_sigkill'])
        self.assertEqual(record['cleanup']['exit_code'], -9)

    def test_missing_vm_is_a_failed_prerequisite_not_skip(self):
        with self.assertRaisesRegex(AssertionError, 'SOYO_TEST_VM_COLIMA'):
            self.a.vm_config({})

    def test_vm_config_requires_every_input_and_never_host_fallback(self):
        env = {'SOYO_TEST_VM_' + key: value for key, value in {
            'COLIMA': '/bin/colima', 'SSH': '/usr/bin/ssh', 'PROFILE': 'dedicated',
            'HOST': '127.0.0.1', 'USER': 'tester', 'PORT': '2222',
            'IDENTITY_FILE': '/private/key', 'KNOWN_HOSTS': '/private/known',
            'NODE': '/opt/node', 'WORKER_ROOT': '/opt/worker', 'CHROMIUM': '/opt/chrome',
            'MANIFEST': '/opt/manifest', 'MANIFEST_SHA256': 'a' * 64,
            'TEMP_ROOT': '/tmp/soyo-runs'}.items()}
        config = self.a.vm_config(env)
        self.assertEqual(config['backend'], 'colima')
        self.assertEqual(config['identity_manifest'], '/opt/manifest')
        self.assertEqual(config['port'], 2222)
        del env['SOYO_TEST_VM_NODE']
        with self.assertRaisesRegex(AssertionError, 'SOYO_TEST_VM_NODE'):
            self.a.vm_config(env)

    def test_host_snapshot_uses_nested_result_and_checks_source(self):
        result = {'ok': True, 'data': {'source': 'untrusted_browser', 'page': 'page_x',
                  'document': 'doc_x', 'document_revision': 1, 'url': 'http://127.0.0.1:4/',
                  'elements': []}}
        self.assertEqual(self.a.snapshot(result)['document'], 'doc_x')
        result['data']['source'] = 'host_instruction'
        with self.assertRaises(AssertionError):
            self.a.snapshot(result)
        with self.assertRaises(AssertionError):
            self.a.snapshot({'page': 'page_x'})

    def test_disabled_screenshot_is_not_passed_and_strict_fails(self):
        self.assertEqual(self.a.screenshot_boundary([], False)['status'], 'disabled')
        with self.assertRaisesRegex(AssertionError, 'required'):
            self.a.screenshot_boundary([], True)
        self.assertEqual(self.a.screenshot_boundary(['browser_screenshot'], True)['status'], 'registered')

    def test_manifest_detects_dirty_file_without_asserting_build_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'Cargo.toml').write_text('before')
            first = self.a.source_manifest(root, ['Cargo.toml'])
            (root / 'Cargo.toml').write_text('after')
            second = self.a.source_manifest(root, ['Cargo.toml'])
            self.assertNotEqual(first, second)
            self.assertEqual(second[0]['path'], 'Cargo.toml')

    def test_summary_counts_actual_results_and_disabled_separately(self):
        self.assertEqual(self.a.totals([{'status': 'passed'}, {'status': 'failed'},
                                       {'status': 'disabled'}]),
                         {'passed': 1, 'failed': 1, 'disabled': 1})

    def test_candidate_rejects_replaced_binary_before_dispatch(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / 'soyo'
            binary.write_bytes(b'initial')
            candidate = object.__new__(self.a.Candidate)
            candidate.binary = binary
            candidate.binary_sha256 = self.a.fixture.sha256(binary)
            candidate.config = Path('/tmp/config')
            candidate.root = Path('/tmp/work')
            candidate.data_dir = Path('/tmp/data')
            binary.write_bytes(b'replaced')
            with self.assertRaisesRegex(AssertionError, 'binary changed'):
                candidate.command(['run', 'task'])

    def test_main_persists_failed_prerequisite_with_binary_binding_and_zero_requests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'candidate'
            binary.write_bytes(b'not executed')
            binary.chmod(0o700)
            with patch.dict(self.a.os.environ, {}, clear=True), patch.object(
                    self.a.fixture, 'bounded_process', side_effect=[
                        (0, 'abcdef\\n', '', 0.01), (0, 'Cargo.toml\\0', '', 0.01)]), redirect_stdout(io.StringIO()):
                code = self.a.main([str(binary), '--evidence-dir', str(root / 'evidence')])
            self.assertEqual(code, 1)
            evidence = json.loads(next((root / 'evidence').glob('run-*/evidence.json')).read_text())
            self.assertEqual(evidence['status'], 'failed')
            self.assertEqual(evidence['provider_counts'], {'execution': 0, 'review': 0, 'compaction': 0})
            self.assertEqual(len(evidence['binary_sha256']), 64)
            self.assertIn('SOYO_TEST_VM_COLIMA', evidence['cases'][0]['error'])

    def test_background_poll_is_bounded_and_requires_unknown_terminal_result(self):
        steps = self.a.background_steps('http://127.0.0.1:4', [])
        checker = next(step[3] for step in steps if step[0] == 'status')
        class Dialogue:
            index = 0
            steps = []
        dialogue = Dialogue()
        checker({'ok': True, 'data': {'status': 'running'}}, dialogue)
        self.assertEqual(len(dialogue.steps), 1)
        with self.assertRaises(AssertionError):
            checker({'ok': True, 'data': {'status': 'finished', 'result': {'ok': True}}}, dialogue)
        checker({'ok': True, 'data': {'status': 'failed', 'error': {'code': 'outcome_unknown'}}}, dialogue)
        for _ in range(40):
            try:
                checker({'ok': True, 'data': {'status': 'running'}}, dialogue)
            except AssertionError:
                break
        else:
            self.fail('poll budget failed to terminate')

    def test_approval_checks_actions_and_does_not_accept_fill_leak(self):
        self.a.check_approval('Action: press key Enter', 'browser_press', {'key': 'Enter'})
        with self.assertRaises(AssertionError):
            self.a.check_approval('Action: click', 'browser_click', {'button': 'right'})
        self.a.check_approval('fill text 6 bytes', 'browser_fill', {'text': '秘密'})
        with self.assertRaises(AssertionError):
            self.a.check_approval('fill text 6 bytes 秘密', 'browser_fill', {'text': '秘密'})

    def test_case_does_not_promote_explicit_failure(self):
        class Counts:
            counts = {'execution': 0, 'review': 0, 'compaction': 0}
        report = {'cases': []}
        case = self.a.run_case(report, Counts(), 'failed proof', lambda: {'status': 'failed'})
        self.assertEqual(case['status'], 'failed')
        case = self.a.run_case(report, Counts(), 'success', lambda: None)
        self.assertEqual(case['status'], 'passed')
        def invalid_approval():
            self.a.check_approval('browser_click', 'browser_click', {'button': 'right'})
        case = self.a.run_case(report, Counts(), 'approval failure', invalid_approval)
        self.assertEqual(case['status'], 'failed')
        self.assertIn('approval hides button', case['error'])
        case = self.a.run_case(report, Counts(), 'screenshot disabled',
                               lambda: self.a.screenshot_boundary([], False))
        self.assertEqual(case['status'], 'disabled')
        self.assertEqual(self.a.totals(report['cases']), {'failed': 2, 'passed': 1, 'disabled': 1})

    def test_command_global_options_precede_run(self):
        command = self.a.cli_command(Path('/bin/soyo'), Path('/tmp/cfg'), Path('/tmp/root'),
                                    Path('/tmp/data'), 'manual', 'plan', ['run', 'task'])
        self.assertEqual(command, ['/bin/soyo', '--config', '/tmp/cfg', '--cwd', '/tmp/root',
                                  '--data-dir', '/tmp/data', '--output', 'json',
                                  '--permissions', 'manual', '--mode', 'plan', 'run', 'task'])

    def test_candidate_keeps_storage_outside_workspace(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'candidate'
            binary.write_bytes(b'never executed')
            provider = SimpleNamespace(server=SimpleNamespace(server_port=1234), errors=[])
            candidate = self.a.Candidate(binary, root, provider, {}, 'http://127.0.0.1:4')
            workspace = root / 'workspace'
            command = candidate.command(['run', 'task'])
            self.assertEqual(command[command.index('--cwd') + 1], str(workspace))
            self.assertTrue(workspace.is_dir())
            for path in (candidate.config, candidate.data_dir, Path(candidate.env['HOME'])):
                self.assertFalse(path.is_relative_to(workspace))
            with patch.object(self.a.fixture, 'bounded_process', return_value=(
                    0, '{"status":"completed"}', '', 0)) as process:
                candidate.invoke(['run', 'task'])
            self.assertEqual(process.call_args.args[1], workspace)

    def host_view(self):
        return {'ok': True, 'data': {'source': 'untrusted_browser', 'page': 'page_host',
            'document': 'doc_host', 'document_revision': 1, 'url': 'http://127.0.0.1:4/',
            'title': 'Local', 'text': 'Local', 'truncated': False, 'elements': [
                {'element_ref': 'element_' + str(index), 'role': role, 'name': name}
                for index, (role, name) in enumerate([
                    ('link', 'Download local'), ('textbox', 'Name'),
                    ('button', 'Commit local'), ('button', 'Upload local')])]}}

    def test_actions_consume_host_element_ref_not_worker_key(self):
        view = self.host_view()
        dialogue = SimpleNamespace(observed={label: view for label in (
            'snapshot', 'fresh', 'press_snapshot', 'fill_snapshot', 'click_snapshot')})
        for steps, label, expected in [(self.a.browser_steps(''), 'download', 'element_0'),
                                        (self.a.approval_steps(''), 'fill', 'element_1')]:
            arguments = next(step[2] for step in steps if step[0] == label)
            self.assertEqual(arguments(dialogue)['element_ref'], expected)
            element = view['data']['elements'][0 if label == 'download' else 1]
            element['key'] = element.pop('element_ref')
            with self.assertRaises((KeyError, AssertionError)):
                arguments(dialogue)
            element['element_ref'] = element.pop('key')

    def test_tui_fill_approval_checks_real_byte_label_and_rejects_plaintext(self):
        self.a.check_approval('browser_fill\nText: 6 text bytes', 'browser_fill', {'text': '秘密'})
        for text in ('browser_fill Text: 7 text bytes', 'browser_fill Text: 6 text bytes 秘密'):
            with self.assertRaises(AssertionError):
                self.a.check_approval(text, 'browser_fill', {'text': '秘密'})
        for field, tool, value in [('key', 'browser_press', 'Enter'),
                                   ('button', 'browser_click', 'right'),
                                   ('artifact_ref', 'browser_upload', 'artifact_host')]:
            self.a.check_approval(tool + ' ' + value, tool, {field: value})
            with self.assertRaises(AssertionError):
                self.a.check_approval(tool, tool, {field: value})

    def test_work_rejects_second_open_but_recovers_with_new_page_after_close(self):
        provider = SimpleNamespace(counts={'review': 0, 'execution': 0, 'compaction': 0}, requests=[])
        observed_steps = []
        limit_code, reopened_page = 'browser_limit', 'page_new_host'
        def run(dialogue, permission):
            if permission == 'auto':
                provider.counts['review'] += 1
            observed_steps.extend(step[1] for step in dialogue.steps)
            for step in dialogue.steps:
                label, tool, arguments = step[:3]
                arguments = arguments(dialogue) if callable(arguments) else arguments
                if tool == 'browser_open':
                    if not dialogue.observed:
                        result = {'ok': True, 'data': {'page': 'page_host', 'url': None}}
                    elif not any(key.startswith('close') for key in dialogue.observed):
                        result = {'ok': False, 'error': {'code': limit_code, 'message': 'one page'}}
                    else:
                        result = {'ok': True, 'data': {'page': reopened_page, 'url': None}}
                elif tool == 'browser_snapshot':
                    self.assertEqual(arguments['page'], 'page_host')
                    result = self.host_view()
                elif tool == 'browser_navigate':
                    self.assertEqual(arguments, {'page': 'page_host', 'url': 'http://127.0.0.1:4/'})
                    result = {'ok': True, 'data': {'page': 'page_host',
                        'url': 'http://127.0.0.1:4/', 'document_stale': True}}
                else:
                    self.assertEqual(tool, 'browser_close')
                    result = {'ok': True, 'data': {}}
                if len(step) > 3:
                    step[3](result, dialogue)
                else:
                    self.assertTrue(result['ok'])
                dialogue.observed[label] = result
        def only_work(report, provider, name, operation):
            if name.startswith('cli-work-') and name != 'cli-work-manual-noninteractive':
                operation()
        with patch.object(self.a, 'run_case', side_effect=only_work), patch.object(self.a, 'exercise_pty'):
            self.a.exercise(SimpleNamespace(run=run), provider, {}, 'http://127.0.0.1:4', [], False)
            self.assertEqual(observed_steps, ['browser_open', 'browser_open', 'browser_snapshot',
                'browser_navigate', 'browser_close', 'browser_open', 'browser_close'] * 2)
            reopened_page = 'page_host'
            with self.assertRaisesRegex(AssertionError, 'stale host page identity'):
                self.a.exercise(SimpleNamespace(run=run), provider, {}, 'http://127.0.0.1:4', [], False)
            limit_code = 'worker_closed'
            with self.assertRaisesRegex(AssertionError, 'browser_limit'):
                self.a.exercise(SimpleNamespace(run=run), provider, {}, 'http://127.0.0.1:4', [], False)

    def test_main_rejects_nonempty_dedicated_vm_before_candidate_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'candidate'
            binary.write_bytes(b'never executed')
            binary.chmod(0o700)
            provider = SimpleNamespace(counts={'execution': 0, 'review': 0, 'compaction': 0}, errors=[])
            provider.serving = lambda: nullcontext(provider)
            occupied = ['/tmp/soyo-runs/soyo-browser-vm-existing']
            with patch.object(self.a, 'vm_config', return_value={'identity_sha256': 'a' * 64}), \
                    patch.object(self.a, 'remote_inventory', return_value=occupied), \
                    patch.object(self.a, 'website', return_value=nullcontext(('http://127.0.0.1:4', []))), \
                    patch.object(self.a, 'Candidate', return_value=SimpleNamespace(processes=[])), \
                    patch.object(self.a.fixture, 'Provider', return_value=provider), \
                    patch.object(self.a, 'exercise', side_effect=RuntimeError('candidate was launched')), \
                    patch.object(self.a.fixture, 'bounded_process', side_effect=[
                        (0, 'abcdef\n', '', 0), (0, 'Cargo.toml\0', '', 0)]), redirect_stdout(io.StringIO()):
                code = self.a.main([str(binary), '--evidence-dir', str(root / 'evidence')])
            self.assertEqual(code, 1)
            evidence = json.loads(next((root / 'evidence').glob('run-*/evidence.json')).read_text())
            self.assertEqual(evidence['provider_counts']['execution'], 0)
            self.assertIn('baseline is not empty', evidence['cases'][-1]['error'])

    def test_main_does_not_pass_when_no_execution_scenarios_ran(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'candidate'
            binary.write_bytes(b'never executed')
            binary.chmod(0o700)
            provider = SimpleNamespace(counts={'execution': 0, 'review': 0, 'compaction': 0}, errors=[])
            provider.serving = lambda: nullcontext(provider)
            with patch.object(self.a, 'vm_config', return_value={'identity_sha256': 'a' * 64}), \
                    patch.object(self.a, 'remote_inventory', return_value=[]), \
                    patch.object(self.a, 'website', return_value=nullcontext(('http://127.0.0.1:4', []))), \
                    patch.object(self.a, 'Candidate', return_value=SimpleNamespace(processes=[])), \
                    patch.object(self.a.fixture, 'Provider', return_value=provider), \
                    patch.object(self.a, 'exercise'), \
                    patch.object(self.a.fixture, 'bounded_process', side_effect=[
                        (0, 'abcdef\n', '', 0), (0, 'Cargo.toml\0', '', 0)]), redirect_stdout(io.StringIO()):
                code = self.a.main([str(binary), '--evidence-dir', str(root / 'evidence')])
            self.assertEqual(code, 1)
            evidence = json.loads(next((root / 'evidence').glob('run-*/evidence.json')).read_text())
            self.assertTrue(any('no execution acceptance scenarios' in case.get('error', '')
                                for case in evidence['cases']))

    def test_pty_uses_workspace_and_records_failed_approval(self):
        provider = SimpleNamespace(errors=[])
        candidate = SimpleNamespace(root=Path('/tmp/managed'), workspace=Path('/tmp/managed/workspace'),
            env={}, command=lambda *args: ['soyo', '--output', 'json'])
        screen = SimpleNamespace(details_text=lambda: 'browser_fill Text: 6 text bytes 秘密',
                                 status=lambda: 'Awaiting approval', text='failed approval')
        def pump():
            provider.dialogue.index = 1
            provider.dialogue.arguments['fill'] = {'text': '秘密'}
        terminal = SimpleNamespace(resize=lambda *args: None, title=lambda: None,
            line=lambda *args: None, pump=pump, screen=screen,
            process=SimpleNamespace(poll=lambda: None), close=lambda: None)
        report = {}
        with patch.object(self.a, 'close_terminal'), patch.object(self.a, 'terminal_class', return_value=lambda command, cwd, env:
                          (self.assertEqual(cwd, candidate.workspace) or terminal)):
            with self.assertRaisesRegex(AssertionError, 'leaked'):
                self.a.pty_journey(candidate, provider, report, 'approval',
                    [('fill', 'browser_fill', {'text': '秘密'})], 'manual')
        self.assertEqual(report['pty'][0]['approval_screens'][0]['validation'], 'failed')

    def test_manual_pty_cannot_pass_without_approval(self):
        provider = SimpleNamespace(errors=[])
        candidate = SimpleNamespace(workspace=Path('/tmp/workspace'), root=Path('/tmp'), env={},
                                    command=lambda *args: ['soyo', '--output', 'json'])
        def pump():
            provider.dialogue.completed = True
            provider.dialogue.index = 1
        terminal = SimpleNamespace(resize=lambda *args: None, title=lambda: None,
            line=lambda *args: None, pump=pump,
            screen=SimpleNamespace(details_text=lambda: '', status=lambda: 'Completed', text=''),
            process=SimpleNamespace(poll=lambda: None), until=lambda *args: None,
            finish=lambda: None, close=lambda: None)
        with patch.object(self.a, 'close_terminal'), patch.object(self.a, 'terminal_class', return_value=lambda *args: terminal):
            with self.assertRaisesRegex(AssertionError, 'approval'):
                self.a.pty_journey(candidate, provider, {}, 'missing-approval',
                    [('open', 'browser_open', {})], 'manual')

    def test_native_image_detector_does_not_mistake_file_reference_for_image(self):
        self.assertFalse(self.a.has_image({'messages': [{'role': 'tool', 'content': json.dumps(
            {'artifact_ref': 'image-not-actually-image', 'kind': 'file'})}]}))
        self.assertTrue(self.a.has_image({'messages': [{'role': 'user', 'content': [
            {'type': 'image_url', 'image_url': {'url': 'data:image/png;base64,AA=='}}]}]}))


if __name__ == '__main__':
    unittest.main()
