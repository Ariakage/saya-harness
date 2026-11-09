#!/usr/bin/env python3
"""Regression checks for acceptance oracles, cleanup and failure evidence."""

from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location('native_acceptance', Path(__file__).with_name('native_acceptance.py'))
acceptance = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(acceptance)


class FakeCase:
    def __init__(self, workspace, results, backend='host'):
        self.workspace = workspace
        self.tool_results = results
        self.backend = backend

    def run(self, *_arguments, **_options):
        return {'exit_code': 0, 'result': {'status': 'completed'}, 'stderr': '',
                'elapsed_ms': 1, 'user_cpu_ms': 0, 'system_cpu_ms': 0, 'peak_rss_bytes': 1}

    def results(self):
        return self.tool_results

    def native(self):
        acceptance.require(self.backend == 'auto', 'Wrong native backend')
        return {}

    def facts(self):
        return {'identity': {'backend': self.backend},
                'capabilities': {'filesystem': 'unsupported', 'network': 'unsupported'}}

    def configure(self, source):
        self.backend = 'auto' if '"auto"' in source else 'host'


class AcceptanceOracles(unittest.TestCase):
    def test_linux_private_udp_send_requires_no_host_delivery(self):
        with patch.object(acceptance.platform, 'system', return_value='Linux'):
            acceptance.require_udp_isolation('SENT\n', b'')
            acceptance.require_udp_isolation('DENIED errno=1\n', b'')
            with self.assertRaisesRegex(AssertionError, 'Host received'):
                acceptance.require_udp_isolation('SENT\n', b'N2_RAW_DNS_TRANSPORT_PROBE')
            with self.assertRaisesRegex(AssertionError, 'Host received'):
                acceptance.require_udp_isolation('DENIED errno=1\n', ['ipv6'])

    def test_seatbelt_udp_requires_syscall_denial_and_no_host_delivery(self):
        with patch.object(acceptance.platform, 'system', return_value='Darwin'):
            acceptance.require_udp_isolation('DENIED errno=1\n', b'')
            with self.assertRaisesRegex(AssertionError, 'Unexpected native UDP'):
                acceptance.require_udp_isolation('SENT\n', b'')
            with self.assertRaisesRegex(AssertionError, 'Host received'):
                acceptance.require_udp_isolation('DENIED errno=1\n', b'packet')

    def test_udp_oracle_rejects_unknown_or_partial_probe_output(self):
        for system in ['Linux', 'Darwin']:
            for output in ['', 'NO_PROXY', 'DENIED', 'DENIED errno=', 'DENIED errno=0',
                           'DENIED errno=١', 'DENIED errno=01', 'SENT\nextra']:
                with self.subTest(system=system, output=output), \
                        patch.object(acceptance.platform, 'system', return_value=system), \
                        self.assertRaisesRegex(AssertionError, 'Unexpected native UDP'):
                    acceptance.require_udp_isolation(output, b'')

    def test_plan_requires_both_denied_results(self):
        denied = {'ok': False, 'error': {'code': 'mode_denied'}}
        with tempfile.TemporaryDirectory() as directory:
            for results in ({}, {'n2-1': denied}, {'n2-1': denied, 'wrong': denied}):
                with self.subTest(results=results), self.assertRaises(AssertionError):
                    acceptance.plan_case(FakeCase(Path(directory), results, 'auto'))
            result = acceptance.plan_case(FakeCase(Path(directory), {'n2-1': denied, 'n2-2': denied}, 'auto'))
            self.assertEqual(result['denied'], 2)

    def test_benchmark_rejects_completed_run_with_failed_tool(self):
        with tempfile.TemporaryDirectory() as directory:
            class FailedToolCase(FakeCase):
                def run(self, steps=(), **options):
                    self.tool_results = {'n2-1': {'ok': False}} if steps else {}
                    return super().run(steps, **options)
            @contextmanager
            def fixture(*_arguments):
                yield FailedToolCase(Path(directory), {})
            with patch.object(acceptance, 'fixture', fixture), self.assertRaises(AssertionError):
                acceptance.benchmark(None, None, Path(directory), 1)

    def test_benchmark_checks_actual_backend_and_tool_values(self):
        with tempfile.TemporaryDirectory() as directory:
            case = FakeCase(Path(directory), {}, 'seatbelt')
            with self.assertRaises(AssertionError):
                acceptance.validate_benchmark_sample(case, 'host', 'cold_start')
            for kind, data in [('file_roundtrip', {'content': 'wrong'}), ('short_command', {'exit_code': 1})]:
                case = FakeCase(Path(directory), {'n2-1': {'ok': True, 'data': data}})
                with self.subTest(kind=kind), self.assertRaises(AssertionError):
                    acceptance.validate_benchmark_sample(case, 'host', kind)
            for kind, data in [('file_roundtrip', {'content': 'N2_BENCH\n'}), ('short_command', {'exit_code': 0})]:
                case = FakeCase(Path(directory), {'n2-1': {'ok': True, 'data': data}})
                acceptance.validate_benchmark_sample(case, 'host', kind)

    def test_invalid_utf8_capture_is_retained_and_stays_invalid_on_retry(self):
        invocation = acceptance.Invocation([sys.executable, '-c', "import os;os.write(1,b'\\xff')"],
                                            os.environ.copy(), Path.cwd())
        with self.assertRaisesRegex(AssertionError, 'UTF-8'):
            invocation.finish()
        self.assertFalse(invocation.outcome['stdout_valid_utf8'])
        self.assertTrue(invocation.stdout.closed and invocation.stderr.closed)
        with self.assertRaisesRegex(AssertionError, 'UTF-8'):
            invocation.finish()

    def test_capture_limit_failure_keeps_diagnostics(self):
        invocation = acceptance.Invocation([sys.executable, '-c', "print('N2_SYNTHETIC_CAPTURE')"],
                                            os.environ.copy(), Path.cwd())
        with patch.object(acceptance, 'CAPTURE_BYTES', 8), self.assertRaisesRegex(AssertionError, 'capture limit'):
            invocation.finish()
        self.assertTrue(invocation.outcome['capture_truncated'])
        self.assertEqual(invocation.outcome['stdout'], 'N2_SYNTH')
        self.assertTrue(invocation.stdout.closed and invocation.stderr.closed)

    def test_all_fixture_cleanup_runs_after_one_cleanup_failure(self):
        calls = []
        def failed():
            calls.append('failed')
            raise RuntimeError('synthetic cleanup failure')
        case = acceptance.CaseFixture.__new__(acceptance.CaseFixture)
        case.model = SimpleNamespace(release=threading.Event(), close=lambda: calls.append('model'))
        case.directory = SimpleNamespace(cleanup=lambda: calls.append('directory'))
        case.invocations = [SimpleNamespace(close=failed), SimpleNamespace(close=lambda: calls.append('second'))]
        with self.assertRaisesRegex(RuntimeError, 'synthetic cleanup failure'):
            case.close()
        self.assertEqual(calls, ['failed', 'second', 'model', 'directory'])
        self.assertTrue(case.model.release.is_set())

    def test_failure_diagnostics_omit_large_parsed_results(self):
        case = SimpleNamespace(model=SimpleNamespace(requests=[]), invocations=[
            SimpleNamespace(outcome={'exit_code': 1, 'stdout': 'X' * 10000,
                                     'result': {'data': 'X' * 300000}})])
        diagnostics = acceptance.failure_diagnostics(case)
        self.assertNotIn('result', diagnostics['invocations'][0])
        self.assertEqual(len(diagnostics['invocations'][0]['stdout']), 4096)

    def test_compiler_failure_leaves_machine_readable_evidence(self):
        real_run = subprocess.run
        def run(arguments, **options):
            if arguments[0] == 'cc':
                raise subprocess.CalledProcessError(17, arguments, stderr=b'N2_SYNTHETIC_COMPILER_FAILURE')
            return real_run(arguments, **options)
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'evidence.json'
            arguments = ['native_acceptance.py', sys.executable, '--skip-benchmarks', '--output', str(output)]
            with patch.object(sys, 'argv', arguments), patch.object(acceptance, 'tool_evidence', return_value={}), \
                    patch.object(acceptance.subprocess, 'run', side_effect=run), \
                    self.assertRaises(subprocess.CalledProcessError):
                acceptance.main()
            evidence = json.loads(output.read_text())
            self.assertFalse(evidence['passed'])
            self.assertEqual(evidence['phase'], 'failed')
            self.assertEqual(evidence['fatal']['stderr'], 'N2_SYNTHETIC_COMPILER_FAILURE')
            self.assertIn('finished_utc', evidence)


if __name__ == '__main__':
    unittest.main()
