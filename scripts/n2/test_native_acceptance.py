#!/usr/bin/env python3
"""Regression checks for acceptance oracles, cleanup and failure evidence."""

from contextlib import contextmanager
import copy
import hashlib
import http.client
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
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
        return {'identity': {'backend': 'seatbelt', 'id': 'fixture-environment'},
                'capabilities': {'filesystem': 'hard', 'network': 'hard'}}

    def facts(self):
        return {'identity': {'backend': self.backend, 'id': 'fixture-environment'},
                'capabilities': {'filesystem': 'unsupported', 'network': 'unsupported'}}

    def configure(self, source):
        self.backend = 'auto' if '"auto"' in source else 'host'


def warm_case(workspace):
    results = {f'n2-{index}': {'ok': True, 'data': {'exit_code': 0}} for index in range(1, 4)}
    case = FakeCase(workspace, results)
    requests = []
    observations = []
    for index in range(4):
        messages = [{'role': 'system', 'content': json.dumps({
            'saya_execution_environment_v1': case.facts()})}]
        if index:
            call_id = f'n2-{index}'
            messages.append({'role': 'tool', 'tool_call_id': call_id,
                             'content': json.dumps(results[call_id])})
        requests.append({'messages': messages})
        observations.append({'request_index': index, 'request_received_ns': (index + 1) * 1_000_000,
            'response_sent_ns': (index + 1) * 1_000_000 + 100_000,
            'tool_call_id': f'n2-{index + 1}' if index < 3 else None})
    case.model = SimpleNamespace(requests=requests, observations=observations)
    case.invocations = [object()]
    return case


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

    def test_file_write_benchmark_requires_real_bytes_and_matching_result(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            path = workspace / 'written.txt'
            data = {'path': str(path), 'created': True,
                    'version': hashlib.sha256(b'N2_BENCH_WRITE\n').hexdigest()}
            case = FakeCase(workspace, {'n2-1': {'ok': True, 'data': data}})
            with self.assertRaisesRegex(AssertionError, 'expected file content'):
                acceptance.validate_benchmark_sample(case, 'host', 'file_write')
            path.write_text('wrong')
            with self.assertRaisesRegex(AssertionError, 'expected file content'):
                acceptance.validate_benchmark_sample(case, 'host', 'file_write')
            path.write_bytes(b'N2_BENCH_WRITE\n')
            proof = acceptance.validate_benchmark_sample(case, 'host', 'file_write')
            self.assertEqual(proof['content_sha256'], data['version'])
            self.assertEqual(proof['bytes'], len(b'N2_BENCH_WRITE\n'))
            for key, value in [('path', str(workspace / 'wrong.txt')), ('created', False), ('version', 'wrong')]:
                with self.subTest(key=key):
                    case.tool_results['n2-1']['data'] = {**data, key: value}
                    with self.assertRaisesRegex(AssertionError, 'describe the created file'):
                        acceptance.validate_benchmark_sample(case, 'host', 'file_write')

    def test_search_benchmark_requires_complete_actual_match(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            hit = {'path': str(workspace / 'bench.txt'), 'line': 1, 'text': 'N2_BENCH'}
            data = {'items': [hit], 'truncated': False}
            case = FakeCase(workspace, {'n2-1': {'ok': True, 'data': data}})
            proof = acceptance.validate_benchmark_sample(case, 'host', 'search_text')
            self.assertEqual(proof['matches'], 1)
            for invalid in [{'items': [], 'truncated': False}, {'items': [hit], 'truncated': True},
                    {'items': [{**hit, 'path': str(workspace / 'other.txt')}], 'truncated': False},
                    {'items': [{**hit, 'line': 2}], 'truncated': False},
                    {'items': [{**hit, 'text': 'wrong'}], 'truncated': False}]:
                with self.subTest(invalid=invalid), self.assertRaises(AssertionError):
                    case.tool_results['n2-1']['data'] = invalid
                    acceptance.validate_benchmark_sample(case, 'host', 'search_text')

    def test_warm_benchmark_binds_complete_calls_to_one_environment_and_time(self):
        with tempfile.TemporaryDirectory() as directory:
            case = warm_case(Path(directory))
            proof = acceptance.validate_benchmark_sample(case, 'host', 'warm_command', 3)
            self.assertEqual(proof['tool_calls'], 3)
            self.assertEqual(proof['model_requests'], 4)
            self.assertEqual(proof['invocation_count'], 1)
            self.assertEqual([entry['elapsed_ms'] for entry in proof['call_intervals']], [.9, .9, .9])
            mutations = [
                lambda value: value.invocations.append(object()),
                lambda value: value.tool_results.pop('n2-2'),
                lambda value: value.tool_results['n2-2'].update(ok=False),
                lambda value: value.tool_results['n2-2']['data'].update(exit_code=1),
                lambda value: value.model.requests.pop(),
                lambda value: value.model.requests[1]['messages'].pop(),
                lambda value: value.model.requests[1]['messages'][0].update(content=json.dumps({
                    'saya_execution_environment_v1': {'identity': {'id': 'another-environment'}}})),
                lambda value: value.model.observations.pop(),
                lambda value: value.model.observations[0].update(response_sent_ns=None),
                lambda value: value.model.observations[1].update(request_received_ns=1),
                lambda value: value.model.observations[1].update(request_received_ns=10 ** 100),
                lambda value: value.model.observations[0].update(tool_call_id='wrong'),
            ]
            for index, mutate in enumerate(mutations):
                changed = copy.deepcopy(case)
                mutate(changed)
                with self.subTest(mutation=index), self.assertRaises(AssertionError):
                    acceptance.validate_benchmark_sample(changed, 'host', 'warm_command', 3)

    def test_model_records_actual_response_and_next_request_times(self):
        model = acceptance.ModelFixture()
        try:
            model.steps = [('exec', {'argv': ['/usr/bin/true']})]
            client = http.client.HTTPConnection('127.0.0.1', model.server.server_port, timeout=3)
            client.request('POST', '/v1', json.dumps({'tools': [{}], 'messages': []}))
            first = json.loads(client.getresponse().read())
            self.assertEqual(first['choices'][0]['message']['tool_calls'][0]['id'], 'n2-1')
            client.close()
            client = http.client.HTTPConnection('127.0.0.1', model.server.server_port, timeout=3)
            client.request('POST', '/v1', json.dumps({'tools': [{}], 'messages': [{
                'role': 'tool', 'tool_call_id': 'n2-1', 'content': json.dumps({'ok': True})}]}))
            client.getresponse().read()
            client.close()
            deadline = time.monotonic() + 2
            while model.observations[-1]['response_sent_ns'] is None:
                self.assertLess(time.monotonic(), deadline)
                time.sleep(.001)
            self.assertEqual([item['request_index'] for item in model.observations], [0, 1])
            self.assertLess(model.observations[0]['response_sent_ns'],
                            model.observations[1]['request_received_ns'])
        finally:
            model.close()

    def test_benchmark_keeps_warm_resources_whole_and_concurrent_resources_per_run(self):
        class ResourceCase(FakeCase):
            def run(self, *_arguments, **_options):
                return {'exit_code': 0, 'result': {'status': 'completed', 'run_id': 'fixture-run'},
                    'stderr': '', 'elapsed_ms': 1, 'user_cpu_ms': 2,
                    'system_cpu_ms': 3, 'peak_rss_bytes': 7}
        with tempfile.TemporaryDirectory() as directory:
            @contextmanager
            def fixture(*_arguments):
                yield ResourceCase(Path(directory), {})
            def validate(case, _backend, kind, expected_tools=1):
                proof = {'kind': kind, 'backend': case.backend, 'tool_calls': expected_tools}
                if kind == 'warm_command':
                    proof['call_intervals'] = [{'call_id': f'n2-{index + 1}',
                        'elapsed_ms': index + 1, 'proof': {'exit_code': 0}}
                        for index in range(expected_tools)]
                return proof
            with patch.object(acceptance, 'fixture', fixture), \
                    patch.object(acceptance, 'validate_benchmark_sample', validate):
                result = acceptance.benchmark(None, None, Path(directory), 2)
            for backend in ('auto', 'host'):
                variants = result['variants'][backend]
                self.assertEqual(set(variants), {'cold_start', 'file_roundtrip', 'file_write',
                    'search_text', 'short_command', 'warm_command', 'concurrent_runs'})
                warm = variants['warm_command']
                self.assertEqual([sample['elapsed_ms'] for sample in warm['samples']], [2, 3])
                self.assertEqual((warm['p50_ms'], warm['p95_ms']), (2, 3))
                self.assertEqual(warm['whole_cli_rusage']['user_cpu_ms'], 2)
                for sample in warm['samples']:
                    self.assertNotIn('user_cpu_ms', sample)
                    self.assertNotIn('peak_rss_bytes', sample)
                for batch in variants['concurrent_runs']['batches']:
                    self.assertEqual(len(batch['run_resources']), 4)
                    self.assertEqual(batch['run_elapsed_ms'], [1, 1, 1, 1])
                    self.assertEqual(batch['user_cpu_ms'], 8)
                    self.assertEqual(batch['system_cpu_ms'], 12)
                    self.assertEqual(batch['max_child_peak_rss_bytes'], 7)

    def test_benchmark_rejects_sample_counts_outside_the_supported_bounds(self):
        for samples in [0, -1, 21, True, 1.5]:
            with self.subTest(samples=samples), self.assertRaisesRegex(AssertionError, '1..20'):
                acceptance.benchmark(None, None, None, samples)

    def test_proxy_latency_requires_a_successful_bounded_precise_payload_sample(self):
        def result(text, *, ok=True, code=0):
            return {'ok': ok, 'data': {'exit_code': code, 'stdout': text}}
        self.assertEqual(acceptance.proxy_elapsed_ms(result('N2_NETWORK_MS=1.250000\n')), 1.25)
        for value in [result('N2_NETWORK_MS=1.250000\n', ok=False),
                result('N2_NETWORK_MS=1.250000\n', code=1), result('N2_NETWORK_MS=NaN\n'),
                result('N2_NETWORK_MS=inf\n'), result('N2_NETWORK_MS=-1.000000\n'),
                result('N2_NETWORK_MS=0.000000\n'), result('N2_NETWORK_MS=10000.000001\n'),
                result('N2_NETWORK_MS=1.250000'), result('N2_NETWORK_MS=1.25\n'),
                result('N2_NETWORK_MS=1.250000\nextra\n')]:
            with self.subTest(value=value), self.assertRaises(AssertionError):
                acceptance.proxy_elapsed_ms(value)

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
