#!/usr/bin/env python3
"""Offline regressions for complete native performance evidence verification."""

from contextlib import redirect_stderr, redirect_stdout
import hashlib
import importlib.util
import io
import json
import math
from pathlib import Path
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location('verify_performance', Path(__file__).with_name('verify_performance.py'))
verifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verifier)


def resource_sample(elapsed=10.0):
    return {'elapsed_ms': elapsed, 'user_cpu_ms': 0.5,
            'system_cpu_ms': 0.25, 'peak_rss_bytes': 8192}


def summary(samples, key='samples'):
    ordered = sorted(sample['elapsed_ms'] for sample in samples)
    count = len(ordered)
    return {'p50_ms': ordered[math.ceil(0.5 * count) - 1],
            'p95_ms': ordered[math.ceil(0.95 * count) - 1], key: samples}


def operation_proof(kind, backend):
    proof = {'kind': kind, 'backend': backend, 'tool_calls': 0 if kind == 'cold_start' else 1}
    if kind == 'file_roundtrip':
        proof.update(content_sha256=hashlib.sha256(b'N2_BENCH\n').hexdigest(), bytes=9)
    elif kind == 'file_write':
        proof.update(path='/synthetic/workspace/written.txt',
                     content_sha256=hashlib.sha256(b'N2_BENCH_WRITE\n').hexdigest(), bytes=15)
    elif kind == 'search_text':
        proof.update(path='/synthetic/workspace/bench.txt', line=1, text='N2_BENCH', matches=1)
    elif kind == 'short_command':
        proof['exit_code'] = 0
    return proof


def differences(measured, baseline):
    result = {}
    for percentile in ('p50', 'p95'):
        value = measured[percentile + '_ms']
        base = baseline[percentile + '_ms']
        result[percentile + '_extra_ms'] = value - base
        result[percentile + '_ratio'] = value / base
    return result


def synthetic_report(count=3, system='Darwin'):
    """Create internally consistent evidence without executing a runtime or tool."""
    native = 'seatbelt' if system == 'Darwin' else 'bubblewrap'
    execution_os = 'macos' if system == 'Darwin' else 'linux'
    performance = {
        'measurement_version': 2, 'samples_per_variant': count, 'percentile': 'nearest-rank',
        'resource_measurement': 'wait4 rusage for each CLI child',
        'peak_rss_scope': 'kernel ru_maxrss, not an aggregate simultaneous Run memory peak',
        'cold_start_scope': 'new CLI process and new Run/environment, OS page cache retained',
        'variants': {},
    }
    for configured, backend in [('auto', native), ('host', 'host')]:
        offset = 1.0 if configured == 'auto' else 0.0
        variants = {}
        for kind in ['cold_start', 'file_roundtrip', 'file_write', 'search_text', 'short_command']:
            samples = []
            for index in range(count):
                sample = resource_sample(2 + offset + index / 4)
                sample['proof'] = operation_proof(kind, backend)
                samples.append(sample)
            variants[kind] = {**summary(samples), 'scope': 'new CLI process and Run; successful operation'}
        identity = {'id': '00000000-0000-0000-0000-000000000001', 'backend': backend,
                    'host_os': execution_os, 'execution_os': execution_os,
                    'architecture': 'aarch64', 'policy_digest': 'a' * 64}
        intervals = []
        for index in range(count + 1):
            start = 1_000_000_000 + index * 4_000_000
            end = start + 1_000_000 + int(offset * 1000) + index * 100
            intervals.append({'call_id': f'n2-{index + 1}', 'response_sent_ns': start,
                'tool_result_request_received_ns': end, 'elapsed_ms': (end - start) / 1_000_000,
                'proof': {'kind': 'warm_command', 'backend': backend,
                          'environment_id': identity['id'], 'exit_code': 0}})
        variants['warm_command'] = {
            **summary(intervals[1:]), 'scope': 'same CLI Run and PreparedEnvironment after one exec warmup',
            'warmup_calls': 1, 'warmup_sample': intervals[0],
            'operation_proof': {'kind': 'warm_command', 'backend': backend, 'tool_calls': count + 1,
                'environment_identity': identity, 'model_requests': count + 2,
                'request_environment_ids': [identity['id']] * (count + 2), 'invocation_count': 1,
                'call_ids': [f'n2-{index}' for index in range(1, count + 2)],
                'run_id': '00000000-0000-0000-0000-000000000002'},
            'whole_cli_rusage': {**resource_sample(1000),
                'scope': 'whole CLI invocation; CPU/RSS are not measured separately for each warm tool call'},
        }
        batches = []
        for index in range(count):
            runs = []
            for worker in range(4):
                run = resource_sample(2 + offset + worker)
                run['peak_rss_bytes'] += worker * 1024
                run['proof'] = operation_proof('short_command', backend)
                runs.append(run)
            batches.append({'elapsed_ms': 12 + offset + index,
                'run_elapsed_ms': [run['elapsed_ms'] for run in runs], 'run_resources': runs,
                'user_cpu_ms': sum(run['user_cpu_ms'] for run in runs),
                'system_cpu_ms': sum(run['system_cpu_ms'] for run in runs),
                'max_child_peak_rss_bytes': max(run['peak_rss_bytes'] for run in runs)})
        variants['concurrent_runs'] = {
            **summary(batches, 'batches'), 'scope': 'four-worker batch of independent CLI Runs',
            'resource_scope': 'CPU sums four CLI samples; RSS maximum is not simultaneous aggregate Run memory',
        }
        performance['variants'][configured] = variants
    performance['native_over_host'] = {
        kind: differences(performance['variants']['auto'][kind], performance['variants']['host'][kind])
        for kind in verifier.METRICS
    }
    proxy = {'samples_per_variant': count, 'percentile': 'nearest-rank',
        'clock': 'payload CLOCK_MONOTONIC',
        'scope': 'complete response body and close; no TLS; excludes CLI, model, backend preparation and process startup',
        'whole_cli_rusage_scope': 'kernel child peak, not aggregate simultaneous Run memory', 'variants': {}}
    for index, variant in enumerate(['host_direct', 'native_http', 'native_connect']):
        scheme = 'https' if variant == 'native_connect' else 'http'
        samples = [{'elapsed_ms': 0.5 + index + item / 10} for item in range(count)]
        proxy['variants'][variant] = {
            **summary(samples), 'backend': 'host' if variant == 'host_direct' else native,
            'origin': f'{scheme}://127.0.0.1:18080', 'target_path': '/probe', 'target_requests': count,
            'whole_cli_rusage': [resource_sample(100) for _ in range(count)],
        }
    proxy['native_over_host'] = {
        variant: differences(proxy['variants'][variant], proxy['variants']['host_direct'])
        for variant in ['native_http', 'native_connect']
    }
    performance['proxy_latency'] = proxy
    return {'schema_version': 1, 'phase': 'complete', 'passed': True,
            'platform': {'system': system, 'machine': 'arm64' if system == 'Darwin' else 'aarch64'},
            'binary': {'sha256': 'b' * 64, 'executed_copy_sha256_after': 'b' * 64},
            'performance': performance}


def replace_path(value, path, replacement):
    for key in path[:-1]:
        value = value[key]
    value[path[-1]] = replacement


class PerformanceVerification(unittest.TestCase):
    def test_accepts_one_three_and_twenty_samples_on_both_platforms(self):
        for count in [1, 3, 20]:
            for system in ['Darwin', 'Linux']:
                with self.subTest(count=count, system=system):
                    result = verifier.verify_report(synthetic_report(count, system), count)
                    self.assertTrue(result['verified'], result)
                    self.assertEqual(len(result['checks']), 21)

    def test_expected_samples_pins_the_required_count(self):
        for count in [1, 20]:
            with self.subTest(count=count):
                self.assertFalse(verifier.verify_report(synthetic_report(count), 3)['verified'])
        for expected in [0, 21, True, 3.0]:
            with self.subTest(expected=expected):
                self.assertFalse(verifier.verify_report(synthetic_report(), expected)['verified'])

    def test_rejects_twenty_five_corrupted_performance_claims(self):
        mutations = [
            ('old_version', ['measurement_version'], 1),
            ('boolean_version', ['measurement_version'], True),
            ('missing_proxy_layer', ['proxy_latency'], {}),
            ('boolean_sample_count', ['samples_per_variant'], True),
            ('missing_write_metric', ['variants', 'auto', 'file_write'], {}),
            ('forged_percentile', ['variants', 'auto', 'cold_start', 'p95_ms'], 9999),
            ('nonfinite_cpu', ['variants', 'auto', 'cold_start', 'samples', 0, 'user_cpu_ms'], math.nan),
            ('cold_tool_count', ['variants', 'auto', 'cold_start', 'samples', 0, 'proof', 'tool_calls'], 1),
            ('write_content', ['variants', 'auto', 'file_write', 'samples', 0, 'proof', 'content_sha256'], '0' * 64),
            ('search_hit', ['variants', 'auto', 'search_text', 'samples', 0, 'proof', 'text'], 'WRONG'),
            ('warm_environment', ['variants', 'auto', 'warm_command', 'samples', 0, 'proof', 'environment_id'], 'wrong'),
            ('warm_interval', ['variants', 'auto', 'warm_command', 'samples', 0, 'elapsed_ms'], -1),
            ('warm_call', ['variants', 'auto', 'warm_command', 'samples', 0, 'call_id'], 'n2-1'),
            ('warm_per_call_rss', ['variants', 'auto', 'warm_command', 'samples', 0, 'peak_rss_bytes'], 1000),
            ('warm_request_count', ['variants', 'auto', 'warm_command', 'operation_proof', 'model_requests'], 100),
            ('batch_cpu_sum', ['variants', 'auto', 'concurrent_runs', 'batches', 0, 'user_cpu_ms'], 123),
            ('batch_rss_max', ['variants', 'auto', 'concurrent_runs', 'batches', 0, 'max_child_peak_rss_bytes'], 1),
            ('proxy_target', ['proxy_latency', 'variants', 'native_http', 'target_path'], '/different'),
            ('proxy_zero_interval', ['proxy_latency', 'variants', 'native_connect', 'samples', 0, 'elapsed_ms'], 0),
            ('proxy_percentile', ['proxy_latency', 'variants', 'host_direct', 'p50_ms'], 9999),
            ('proxy_ratio', ['proxy_latency', 'native_over_host', 'native_http', 'p95_ratio'], 0.1),
            ('proxy_backend', ['proxy_latency', 'variants', 'native_http', 'backend'], 'host'),
            ('proxy_resources', ['proxy_latency', 'variants', 'native_connect', 'whole_cli_rusage'], []),
            ('warm_request_ids', ['variants', 'auto', 'warm_command', 'operation_proof', 'request_environment_ids'], []),
            ('boolean_rss', ['variants', 'auto', 'cold_start', 'samples', 0, 'peak_rss_bytes'], True),
        ]
        self.assertEqual(len(mutations), 25)
        for name, path, replacement in mutations:
            with self.subTest(name=name):
                report = synthetic_report()
                replace_path(report['performance'], path, replacement)
                self.assertFalse(verifier.verify_report(report)['verified'])

    def test_old_four_metric_layer_cannot_pass_as_complete(self):
        report = synthetic_report()
        performance = report['performance']
        performance.pop('measurement_version')
        performance.pop('proxy_latency')
        for variants in performance['variants'].values():
            for kind in ['file_write', 'search_text', 'warm_command']:
                variants.pop(kind)
        self.assertFalse(verifier.verify_report(report)['verified'])
        performance['measurement_version'] = 2
        performance['proxy_latency'] = synthetic_report()['performance']['proxy_latency']
        self.assertFalse(verifier.verify_report(report)['verified'])

    def test_requires_complete_invocation_and_matching_nonempty_hash(self):
        for path, value in [(['passed'], False), (['phase'], 'benchmarking'),
                            (['binary', 'executed_copy_sha256_after'], 'c' * 64),
                            (['binary', 'sha256'], ''), (['performance'], {})]:
            with self.subTest(path=path):
                report = synthetic_report()
                replace_path(report, path, value)
                self.assertFalse(verifier.verify_report(report)['verified'])
        self.assertFalse(verifier.verify_report({'passed': True})['verified'])

    def test_cli_requires_requested_samples_and_one_complete_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'native-evidence.json'
            path.write_text(json.dumps(synthetic_report()))
            output = io.StringIO()
            with redirect_stdout(output):
                self.assertEqual(verifier.main([str(path), '--samples', '3']), 0)
            self.assertTrue(json.loads(output.getvalue())['verified'])
            with redirect_stdout(io.StringIO()):
                self.assertEqual(verifier.main([str(path), '--samples', '1']), 1)
            with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                verifier.main([str(path), str(path)])
            self.assertEqual(error.exception.code, 2)

    def test_cli_rejects_missing_and_invalid_reports(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'native-evidence.json'
            with redirect_stdout(io.StringIO()):
                self.assertEqual(verifier.main([str(path)]), 1)
                path.write_text('{')
                self.assertEqual(verifier.main([str(path)]), 1)


if __name__ == '__main__':
    unittest.main()
