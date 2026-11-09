#!/usr/bin/env python3
"""Verify complete native performance evidence without running the candidate."""

import argparse
import hashlib
import json
import math
from pathlib import Path
import re
from urllib.parse import urlsplit
import uuid


METRICS = {
    'cold_start', 'file_roundtrip', 'file_write', 'search_text',
    'short_command', 'warm_command', 'concurrent_runs',
}
RESOURCE_KEYS = ('elapsed_ms', 'user_cpu_ms', 'system_cpu_ms', 'peak_rss_bytes')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def number(value, label, positive=False):
    require(type(value) in (int, float) and math.isfinite(value), label + ' is not finite numeric data')
    require(value > 0 if positive else value >= 0, label + ' is outside its nonnegative bound')
    return value


def same_number(actual, expected, label):
    require(type(actual) in (int, float) and math.isfinite(actual), label + ' is not finite numeric data')
    require(math.isclose(actual, expected, rel_tol=1e-12, abs_tol=1e-9), label + ' does not match raw samples')


def resources(sample, label):
    number(sample['elapsed_ms'], label + '.elapsed_ms', positive=True)
    for key in ('user_cpu_ms', 'system_cpu_ms'):
        number(sample[key], label + '.' + key)
    require(type(sample['peak_rss_bytes']) is int and sample['peak_rss_bytes'] > 0,
            label + '.peak_rss_bytes is not positive integer bytes')


def percentiles(metric, samples, count, label):
    require(len(samples) == count, label + ' has an unexpected raw sample count')
    values = sorted(number(sample['elapsed_ms'], label + '.sample.elapsed_ms', positive=True)
                    for sample in samples)
    same_number(metric['p50_ms'], values[math.ceil(0.5 * count) - 1], label + '.p50_ms')
    same_number(metric['p95_ms'], values[math.ceil(0.95 * count) - 1], label + '.p95_ms')


def operation(proof, kind, backend, label):
    require(proof['kind'] == kind and proof['backend'] == backend, label + ' operation/backend differs')
    expected_calls = 0 if kind == 'cold_start' else 1
    require(type(proof['tool_calls']) is int and proof['tool_calls'] == expected_calls,
            label + ' has unexpected tool count')
    if kind == 'file_roundtrip':
        require(proof['content_sha256'] == hashlib.sha256(b'N2_BENCH\n').hexdigest()
                and type(proof['bytes']) is int and proof['bytes'] == 9,
                label + ' read proof differs from expected content')
    elif kind == 'file_write':
        path = Path(proof['path'])
        require(path.is_absolute() and path.name == 'written.txt', label + ' write path differs')
        require(proof['content_sha256'] == hashlib.sha256(b'N2_BENCH_WRITE\n').hexdigest()
                and type(proof['bytes']) is int and proof['bytes'] == 15,
                label + ' write proof differs from expected content')
    elif kind == 'search_text':
        path = Path(proof['path'])
        require(path.is_absolute() and path.name == 'bench.txt', label + ' search path differs')
        require(type(proof['line']) is int and proof['line'] == 1
                and proof['text'] == 'N2_BENCH'
                and type(proof['matches']) is int and proof['matches'] == 1,
                label + ' search proof differs from expected match')
    elif kind == 'short_command':
        require(type(proof['exit_code']) is int and proof['exit_code'] == 0,
                label + ' short command did not succeed')


def comparison(measured, baseline, value, label):
    for percentile in ('p50', 'p95'):
        actual = measured[percentile + '_ms']
        base = baseline[percentile + '_ms']
        same_number(value[percentile + '_extra_ms'], actual - base, label + '.' + percentile + '_extra_ms')
        same_number(value[percentile + '_ratio'], actual / base, label + '.' + percentile + '_ratio')


def warm(metric, backend, system, machine, count, label):
    proof = metric['operation_proof']
    require(proof['kind'] == 'warm_command' and proof['backend'] == backend,
            label + ' warm operation/backend differs')
    identity = proof['environment_identity']
    require(isinstance(identity['id'], str) and isinstance(proof['run_id'], str),
            label + ' environment/Run identity is missing')
    uuid.UUID(identity['id'])
    uuid.UUID(proof['run_id'])
    require(identity['backend'] == backend and identity['host_os'] == system
            and identity['execution_os'] == system and identity['architecture'] == machine
            and re.fullmatch('[0-9a-f]{64}', identity['policy_digest']) is not None,
            label + ' prepared environment identity differs')
    expected_calls = [f'n2-{index}' for index in range(1, count + 2)]
    require(type(metric['warmup_calls']) is int and metric['warmup_calls'] == 1
            and type(proof['tool_calls']) is int and proof['tool_calls'] == count + 1
            and type(proof['model_requests']) is int and proof['model_requests'] == count + 2
            and type(proof['invocation_count']) is int and proof['invocation_count'] == 1
            and proof['call_ids'] == expected_calls
            and proof['request_environment_ids'] == [identity['id']] * (count + 2),
            label + ' did not retain one Run/environment across warmup and measured calls')
    require(isinstance(metric['scope'], str) and 'same CLI Run and PreparedEnvironment' in metric['scope'],
            label + ' omitted same-Run scope')
    require(len(metric['samples']) == count, label + ' warm sample count differs')
    intervals = [metric['warmup_sample']] + metric['samples']
    previous_end = 0
    for index, interval in enumerate(intervals):
        start = interval['response_sent_ns']
        end = interval['tool_result_request_received_ns']
        require(type(start) is int and type(end) is int and previous_end <= start < end,
                label + ' interval is incomplete or out of chronological order')
        require(interval['call_id'] == expected_calls[index], label + ' interval belongs to another call')
        same_number(interval['elapsed_ms'], (end - start) / 1_000_000, label + ' interval duration')
        require(interval['elapsed_ms'] <= 40000, label + ' warm interval exceeds the actual invocation bound')
        item = interval['proof']
        require(item['kind'] == 'warm_command' and item['backend'] == backend
                and item['environment_id'] == identity['id']
                and type(item['exit_code']) is int and item['exit_code'] == 0,
                label + ' interval did not succeed in its prepared environment')
        require(not any(key in interval for key in RESOURCE_KEYS[1:]),
                label + ' reports whole-CLI CPU/RSS as per-call resources')
        previous_end = end
    percentiles(metric, metric['samples'], count, label)
    whole = metric['whole_cli_rusage']
    resources(whole, label + '.whole_cli_rusage')
    require(isinstance(whole['scope'], str) and 'not measured separately for each warm tool call' in whole['scope'],
            label + ' omitted whole-invocation resource scope')
    require(whole['elapsed_ms'] + 1e-6 >= sum(item['elapsed_ms'] for item in intervals),
            label + ' warm intervals exceed their whole CLI invocation')


def concurrent(metric, backend, count, label):
    require(isinstance(metric['resource_scope'], str) and 'not simultaneous aggregate Run memory' in metric['resource_scope'],
            label + ' claims aggregate RSS without a simultaneous measurement')
    batches = metric['batches']
    percentiles(metric, batches, count, label)
    for index, batch in enumerate(batches):
        name = label + '.batch' + str(index)
        runs = batch['run_resources']
        require(len(runs) == len(batch['run_elapsed_ms']) == 4, name + ' omitted a concurrent Run')
        for item, elapsed in zip(runs, batch['run_elapsed_ms']):
            resources(item, name)
            operation(item['proof'], 'short_command', backend, name)
            same_number(elapsed, item['elapsed_ms'], name + '.run_elapsed_ms')
        for key in ('user_cpu_ms', 'system_cpu_ms'):
            same_number(batch[key], sum(item[key] for item in runs), name + '.' + key)
        require(type(batch['max_child_peak_rss_bytes']) is int
                and batch['max_child_peak_rss_bytes'] == max(item['peak_rss_bytes'] for item in runs),
                name + ' RSS maximum differs from individual child peaks')
        require(batch['elapsed_ms'] + 1e-6 >= max(item['elapsed_ms'] for item in runs),
                name + ' batch elapsed time excludes part of a child invocation')


def validate(performance, platform, expected_samples=None):
    """Return explicit validation results for one complete performance section."""
    checks = {}
    result = {'checks': checks}
    try:
        require(type(performance['measurement_version']) is int and performance['measurement_version'] == 2,
                'complete performance measurement_version 2 is required')
        proxy = performance['proxy_latency']
        checks['complete_measurement_version'] = True
        system = {'Darwin': 'macos', 'Linux': 'linux'}[platform['system']]
        machine = {'arm64': 'aarch64', 'aarch64': 'aarch64', 'x86_64': 'x86_64'}[platform['machine']]
        native = 'seatbelt' if system == 'macos' else 'bubblewrap'
        count = performance['samples_per_variant']
        require(type(count) is int and 1 <= count <= 20
                and performance['percentile'] == 'nearest-rank', 'main sample count/percentile differs')
        if expected_samples is not None:
            require(type(expected_samples) is int and 1 <= expected_samples <= 20
                    and count == expected_samples, 'main sample count differs from the required count')
        require(all(isinstance(performance[key], str)
                    for key in ['resource_measurement', 'peak_rss_scope', 'cold_start_scope'])
                and 'wait4' in performance['resource_measurement']
                and 'not an aggregate simultaneous Run memory' in performance['peak_rss_scope']
                and 'OS page cache retained' in performance['cold_start_scope'],
                'main measurement scope is missing')
        require(set(performance['variants']) == {'auto', 'host'}, 'main backend variants differ')
        checks['resource_measurement_scope'] = True
        for configured, backend in [('auto', native), ('host', 'host')]:
            variants = performance['variants'][configured]
            require(set(variants) == METRICS, configured + ' does not contain all seven operation metrics')
            for kind in sorted(METRICS):
                name = configured + '.' + kind
                value = variants[kind]
                if kind == 'warm_command':
                    warm(value, backend, system, machine, count, name)
                elif kind == 'concurrent_runs':
                    concurrent(value, backend, count, name)
                else:
                    percentiles(value, value['samples'], count, name)
                    require(isinstance(value['scope'], str) and 'new CLI process and Run' in value['scope'],
                            name + ' omitted cold invocation scope')
                    for sample in value['samples']:
                        resources(sample, name)
                        operation(sample['proof'], kind, backend, name)
                checks[name] = True
        require(set(performance['native_over_host']) == METRICS, 'main comparison metrics differ')
        for kind in sorted(METRICS):
            comparison(performance['variants']['auto'][kind], performance['variants']['host'][kind],
                       performance['native_over_host'][kind], kind)
        checks['all_main_percentile_deltas_and_ratios'] = True
        require(type(proxy['samples_per_variant']) is int and proxy['samples_per_variant'] == count
                and proxy['percentile'] == 'nearest-rank'
                and proxy['clock'] == 'payload CLOCK_MONOTONIC', 'proxy sample/clock contract differs')
        require(isinstance(proxy['scope'], str) and isinstance(proxy['whole_cli_rusage_scope'], str)
                and 'no TLS' in proxy['scope'] and 'excludes CLI, model, backend preparation and process startup' in proxy['scope']
                and 'not aggregate simultaneous Run memory' in proxy['whole_cli_rusage_scope'],
                'proxy RTT/resource measurement scope differs')
        require(set(proxy['variants']) == {'host_direct', 'native_http', 'native_connect'}, 'proxy variants differ')
        port = None
        for variant in ['host_direct', 'native_http', 'native_connect']:
            value = proxy['variants'][variant]
            expected_backend = 'host' if variant == 'host_direct' else native
            scheme = 'https' if variant == 'native_connect' else 'http'
            require(isinstance(value['origin'], str), variant + ' target origin is missing')
            origin = urlsplit(value['origin'])
            require(origin.scheme == scheme and origin.hostname == '127.0.0.1'
                    and origin.port is not None and 0 < origin.port <= 65535
                    and origin.path == '' and origin.query == '' and origin.fragment == ''
                    and origin.username is None and origin.password is None,
                    variant + ' target origin differs')
            if port is None:
                port = origin.port
            require(origin.port == port and value['backend'] == expected_backend
                    and value['target_path'] == '/probe'
                    and type(value['target_requests']) is int and value['target_requests'] == count,
                    variant + ' did not complete its exact target requests/backend')
            percentiles(value, value['samples'], count, variant)
            require(len(value['whole_cli_rusage']) == count, variant + ' omitted whole-CLI resources')
            for sample, whole in zip(value['samples'], value['whole_cli_rusage']):
                require(sample['elapsed_ms'] <= 10000, variant + ' RTT exceeds probe bound')
                resources(whole, variant)
                require(whole['elapsed_ms'] + 1e-6 >= sample['elapsed_ms'], variant + ' RTT exceeds CLI duration')
            checks['proxy.' + variant] = True
        require(set(proxy['native_over_host']) == {'native_http', 'native_connect'}, 'proxy comparison variants differ')
        for variant in ['native_http', 'native_connect']:
            comparison(proxy['variants'][variant], proxy['variants']['host_direct'],
                       proxy['native_over_host'][variant], variant)
        checks['all_proxy_percentile_deltas_and_ratios'] = True
    except (KeyError, TypeError, ValueError, IndexError, ZeroDivisionError, OverflowError) as error:
        result['error'] = {'type': type(error).__name__, 'message': str(error)}
    result['verified'] = 'error' not in result and len(checks) == 21 and all(checks.values())
    result['scope'] = 'Checks every raw sample and recomputes nearest-rank percentiles, actual-operation proof metadata, same-Run warm timings, per-CLI wait4 resources, four-child CPU sums/RSS maximum, and plaintext proxy RTT. Simultaneous aggregate Run memory and performance SLOs are unmeasured.'
    return result


def verify_report(evidence, expected_samples=None):
    """Verify performance and completion/hash claims from one native invocation."""
    try:
        report = validate(evidence['performance'], evidence['platform'], expected_samples)
        report['native_invocation_complete'] = evidence['passed'] is True and evidence['phase'] == 'complete'
        digest = evidence['binary']['sha256']
        report['binary_before_after_match'] = (
            isinstance(digest, str) and re.fullmatch('[0-9a-f]{64}', digest) is not None
            and digest == evidence['binary']['executed_copy_sha256_after']
        )
        report['verified'] = (report['verified'] and report['native_invocation_complete']
                              and report['binary_before_after_match'])
        return report
    except (KeyError, TypeError, ValueError) as error:
        return {'verified': False, 'error': {'type': type(error).__name__, 'message': str(error)}}


def main(arguments=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('report', type=Path, help='one complete native-evidence.json')
    parser.add_argument('--samples', type=int, choices=range(1, 21), help='require this sample count per metric')
    options = parser.parse_args(arguments)
    try:
        evidence = json.loads(options.report.read_text())
        report = verify_report(evidence, options.samples)
    except (OSError, ValueError) as error:
        report = {'verified': False, 'error': {'type': type(error).__name__, 'message': str(error)}}
    print(json.dumps(report, indent=2))
    return 0 if report['verified'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
