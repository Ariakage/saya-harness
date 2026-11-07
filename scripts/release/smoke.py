#!/usr/bin/env python3
"""Offline release smoke: run the supplied binary against a loopback fake model."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def main():
    if len(sys.argv) != 2:
        print('usage: smoke.py PATH_TO_SAYA', file=sys.stderr)
        return 2
    binary = Path(sys.argv[1]).resolve(strict=True)
    marker = 'release_smoke_known_content_731'
    seen = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            seen.append(request)
            if len(seen) == 1:
                arguments = json.dumps({'path': 'fact.txt', 'start_line': 1, 'line_count': 4})
                message = {'role': 'assistant', 'content': None, 'tool_calls': [{
                    'id': 'release-read', 'type': 'function', 'function': {
                        'name': 'read_file', 'arguments': arguments}}]}
                reason = 'tool_calls'
            else:
                message = {'role': 'assistant', 'content': 'Read the fixture successfully.'}
                reason = 'stop'
            body = json.dumps({'choices': [{'index': 0, 'finish_reason': reason, 'message': message}]}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='saya-release-smoke-') as directory:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            (workspace / 'fact.txt').write_text(marker + '\n')
            config = root / 'config.toml'
            config.write_text('\n'.join([
                'schema_version = 1', '[defaults]', 'profile = "smoke"',
                '[profiles.smoke]', 'protocol = "openai-chat-completions"',
                f'base_url = "http://127.0.0.1:{server.server_port}/v1"',
                'model = "fixture"', 'auth = "none"', 'context_window_tokens = 128000',
                'stream = false', '']))
            env = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': str(root),
                   'XDG_DATA_HOME': str(root / 'data'), 'XDG_STATE_HOME': str(root / 'state'),
                   'XDG_CACHE_HOME': str(root / 'cache'), 'NO_PROXY': '127.0.0.1,localhost'}
            result = subprocess.run([str(binary), '--config', str(config), '--cwd', str(workspace),
                '--mode', 'plan', '--output', 'json', 'run', 'Read fact.txt'],
                env=env, cwd=workspace, stdin=subprocess.DEVNULL, capture_output=True, timeout=60)
            if result.returncode != 0:
                print('CLI failed with exit', result.returncode, file=sys.stderr)
                return 1
            value = json.loads(result.stdout)
            tool_messages = [m for m in (seen[1].get('messages', []) if len(seen) == 2 else [])
                             if m.get('role') == 'tool' and m.get('tool_call_id') == 'release-read']
            tool_result = json.loads(tool_messages[0]['content']) if len(tool_messages) == 1 else {}
            unchanged = (workspace / 'fact.txt').read_text() == marker + '\n' and len(list(workspace.iterdir())) == 1
            if value.get('status') != 'completed' or not tool_result.get('ok') or marker not in json.dumps(tool_result) or not unchanged:
                print('CLI result, tool roundtrip, or workspace check failed', file=sys.stderr)
                return 1
            print('offline PLAN tool roundtrip: passed; workspace unchanged')
            return 0
    finally:
        server.shutdown()
        thread.join(timeout=5)
        server.server_close()


if __name__ == '__main__':
    sys.exit(main())
