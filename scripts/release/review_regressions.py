#!/usr/bin/env python3
"""Exercise the five 2026-11-07 review findings against a supplied candidate."""
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading


def chat(text):
    return {'choices': [{'index': 0, 'finish_reason': 'stop',
                         'message': {'role': 'assistant', 'content': text}}]}


def tool(name, arguments):
    return {'choices': [{'index': 0, 'finish_reason': 'tool_calls', 'message': {
        'role': 'assistant', 'content': None, 'tool_calls': [{
            'id': 'review-call', 'type': 'function', 'function': {
                'name': name, 'arguments': json.dumps(arguments)}}]}}]}


def completed(output):
    return {'status': 'completed', 'output': output}


def message(text):
    return {'type': 'message', 'id': 'review-message', 'status': 'completed',
            'role': 'assistant', 'content': [{'type': 'output_text', 'text': text,
                                             'annotations': []}]}


@contextmanager
def fixture(binary, responses, protocol='openai-chat-completions', limits='', secret=None):
    seen = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            index = len(seen)
            seen.append(request)
            reply = responses[min(index, len(responses) - 1)]
            body = json.dumps(reply).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='saya-review-candidate-') as directory:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            config = root / 'config.toml'
            auth = 'auth = "none"' if secret is None else 'auth = "bearer"\napi_key_env = "REVIEW_FIXTURE_KEY"'
            config.write_text('\n'.join([
                'schema_version = 1', '[defaults]', 'profile = "review"',
                '[profiles.review]', f'protocol = "{protocol}"',
                f'base_url = "http://127.0.0.1:{server.server_port}/v1"',
                'model = "fixture"', auth, 'context_window_tokens = 128000',
                'stream = false', limits, '']))
            env = {'PATH': '/usr/bin:/bin', 'HOME': str(root),
                   'XDG_DATA_HOME': str(root / 'data'),
                   'XDG_STATE_HOME': str(root / 'state'),
                   'XDG_CACHE_HOME': str(root / 'cache'),
                   'NO_PROXY': '127.0.0.1,localhost'}
            if secret is not None:
                env['REVIEW_FIXTURE_KEY'] = secret

            def invoke(*args):
                result = subprocess.run([
                    str(binary), '--config', str(config), '--cwd', str(workspace),
                    '--output', 'json', *args], env=env, cwd=workspace,
                    stdin=subprocess.DEVNULL, capture_output=True, timeout=30)
                return result.returncode, json.loads(result.stdout)

            yield workspace, invoke, seen
    finally:
        server.shutdown()
        thread.join(timeout=5)
        server.server_close()


def check(binary):
    write = {'path': 'forbidden.txt', 'content': 'changed', 'expected_version': None}
    with fixture(binary, [chat('start'), tool('write_file', write), chat('plan')]) as (workspace, run, seen):
        code, first = run('run', 'start')
        assert code == 0
        code, result = run('--mode', 'plan', 'run', '--session', first['session_id'], 'inspect')
        assert code == 0 and result['status'] == 'completed'
        assert not (workspace / 'forbidden.txt').exists()
        context = json.loads(seen[1]['messages'][1]['content'])
        assert context['saya_runtime_context_v1']['work_mode'] == 'plan'
    print('R-01 explicit PLAN on resume: passed')

    secret = 'fixture"secret\\tail'
    nested = json.dumps({'token': secret})
    for _ in range(8):
        nested = json.dumps(nested)
    unicode = r'{"token":"fixture\u0022secret\u005ctail"}'
    assert json.loads(unicode)['token'] == secret
    review = chat('{"decision":"allow","reason":"fixture"}')
    duplicate = r'{"token":"fixture\u0022secret\u005ctail","token":"safe"}'
    large_number = r'{"token":"fixture\u0022secret\u005ctail","n":1e400}'
    imprecise_integer = r'{"token":"fixture\u0022secret\u005ctail","n":18446744073709551617}'
    small_number = r'{"token":"fixture\u0022secret\u005ctail","n":1e-9999}'
    for index, payload in enumerate([nested, unicode, duplicate, large_number, imprecise_integer, small_number]):
        arguments = {'argv': ['/bin/echo', 'safe'], 'env': {
            'PAYLOAD': {'source': 'literal', 'value': payload}}}
        with fixture(binary, [tool('exec', arguments), review, chat('done')], secret=secret) as (_, run, seen):
            code, result = run('run', 'execute')
            if index < 2:
                assert code == 3 and result['status'] == 'needs_confirmation'
            else:
                # Ambiguous or lossy history is rejected before authorization.
                assert code == 1 and result['status'] == 'failed'
                assert result['error']['code'] == 'storage_error'
            assert len(seen) == 1
    print('R-02 encoded credential stays out of model review: passed')

    oversized = {'path': 'large.txt', 'content': '123456789', 'expected_version': None}
    with fixture(binary, [tool('write_file', oversized), chat('done')],
                 limits='[limits]\nwrite_max_bytes = 8') as (workspace, run, seen):
        code, result = run('run', 'write')
        assert code == 0 and result['status'] == 'completed'
        assert not (workspace / 'large.txt').exists()
        paired = [json.loads(item['content']) for item in seen[1]['messages'] if item['role'] == 'tool']
        assert len(paired) == 1 and paired[0]['error']['code'] == 'write_too_large'
    print('R-03 configured write byte limit: passed')

    call = {'type': 'function_call', 'id': 'review-function', 'status': 'completed',
            'call_id': 'review-call', 'name': 'read_file',
            'arguments': json.dumps({'path': 'input.txt', 'start_line': 1, 'line_count': 1})}
    commentary = message('Inspecting the file.')
    with fixture(binary, [completed([call, commentary]), completed([message('done')])],
                 protocol='openai-responses') as (workspace, run, seen):
        (workspace / 'input.txt').write_text('fixture\n')
        code, result = run('run', 'inspect')
        assert code == 0 and result['status'] == 'completed'
        replay = seen[1]['input']
        call_index = next(i for i, item in enumerate(replay) if item.get('type') == 'function_call')
        assert replay[call_index:call_index + 2] == [call, commentary]
        assert replay[call_index + 2]['type'] == 'function_call_output'
    print('R-04 Responses mixed output replay: passed')

    cases = [('background_start', {'tool': 'unsupported', 'arguments': {}}, 'unknown_tool')]
    for cwd, expected in [('missing-directory', 'invalid_path'), ('regular-file', 'invalid_path'),
                          ('regular-file/child', 'invalid_path'), ('\x00', 'invalid_args'),
                          ('x' * 1024, 'invalid_path'), ('loop', 'invalid_path')]:
        arguments = {'argv': ['/usr/bin/true'], 'cwd': cwd}
        cases.append(('exec', arguments, expected))
        cases.append(('background_start', {'tool': 'exec', 'arguments': arguments}, expected))
    for name, arguments, expected in cases:
        with fixture(binary, [tool(name, arguments), chat('corrected')]) as (workspace, run, seen):
            (workspace / 'regular-file').write_text('data')
            (workspace / 'loop').symlink_to('loop')
            code, result = run('run', 'start tool')
            assert code == 0 and result['status'] == 'completed'
            paired = [json.loads(item['content']) for item in seen[1]['messages'] if item['role'] == 'tool']
            assert len(paired) == 1 and paired[0]['error']['code'] == expected
    print('R-05 invalid tool and cwd return paired errors: passed')


def main():
    if len(sys.argv) != 2:
        print('usage: review_regressions.py PATH_TO_SAYA', file=sys.stderr)
        return 2
    try:
        check(Path(sys.argv[1]).resolve(strict=True))
        return 0
    except (OSError, ValueError, KeyError, IndexError, AssertionError, subprocess.TimeoutExpired) as error:
        print('Candidate review regression failed:', type(error).__name__, file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
