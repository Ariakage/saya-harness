#!/usr/bin/env python3
"""Offline first-use, manual approval and resume journey for one candidate binary."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import subprocess
import sys
import tempfile
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def main():
    if len(sys.argv) != 2:
        print('usage: journey.py PATH_TO_SAYA', file=sys.stderr)
        return 2
    binary = Path(sys.argv[1]).resolve(strict=True)
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append(request)
            if len(requests) == 1:
                args = json.dumps({'path': 'fact.txt', 'start_line': 1, 'line_count': 4})
                message = {'role': 'assistant', 'content': None, 'tool_calls': [{'id': 'journey-read',
                    'type': 'function', 'function': {'name': 'read_file', 'arguments': args}}]}
                reason = 'tool_calls'
            else:
                message = {'role': 'assistant', 'content': 'FIRST_DONE' if len(requests) == 2 else 'RESUMED_DONE'}
                reason = 'stop'
            body = json.dumps({'choices': [{'index': 0, 'finish_reason': reason, 'message': message}]}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='saya-release-journey-') as directory:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            (workspace / 'fact.txt').write_text('JOURNEY_PRIVATE_FIXTURE\n')
            config = root / 'config.toml'
            config.write_text('\n'.join(['schema_version = 1', '[defaults]', 'profile = "smoke"',
                '[profiles.smoke]', 'protocol = "openai-chat-completions"',
                f'base_url = "http://127.0.0.1:{server.server_port}/v1"', 'model = "fixture"',
                'auth = "none"', 'context_window_tokens = 128000', 'stream = false', '']))
            env = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': str(root),
                   'XDG_DATA_HOME': str(root / 'data'), 'XDG_STATE_HOME': str(root / 'state'),
                   'XDG_CACHE_HOME': str(root / 'cache'), 'NO_PROXY': '127.0.0.1,localhost'}
            common = [str(binary), '--config', str(config), '--cwd', str(workspace)]
            subprocess.run(common + ['config', 'check'], cwd=workspace, env=env,
                           check=True, capture_output=True, timeout=15)

            def start(extra):
                master, slave = pty.openpty()
                def controlling_terminal():
                    os.setsid()
                    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
                proc = subprocess.Popen(common + extra, cwd=workspace, env=env, stdin=slave,
                    stdout=slave, stderr=slave, preexec_fn=controlling_terminal)
                os.close(slave)
                return proc, master

            def until(proc, master, needle):
                data = b''
                end = time.monotonic() + 30
                while needle.encode() not in data:
                    if time.monotonic() >= end or proc.poll() is not None:
                        raise RuntimeError('terminal did not reach expected prompt: ' + needle)
                    ready, _, _ = select.select([master], [], [], 0.2)
                    if ready:
                        data += os.read(master, 4096)
                return data

            def exit_chat(proc, master):
                os.write(master, b'/exit\n')
                until(proc, master, 'Goodbye.')
                status = proc.wait(timeout=10)
                os.close(master)
                if status != 0:
                    raise RuntimeError('chat did not exit successfully')

            proc, master = start(['--permissions', 'manual', 'chat'])
            try:
                until(proc, master, '> ')
                os.write(master, b'Read fact.txt\n')
                until(proc, master, 'Allow this call? [y/N] ')
                if len(requests) != 1:
                    raise RuntimeError('unexpected request before manual approval')
                os.write(master, b'y\n')
                if b'FIRST_DONE' not in until(proc, master, '> '):
                    raise RuntimeError('first task did not finish')
                exit_chat(proc, master)
            finally:
                if proc.poll() is None:
                    proc.kill()
                    proc.wait()
            listed = subprocess.run(common + ['--output', 'json', 'session', 'list'], cwd=workspace,
                                    env=env, capture_output=True, check=True, timeout=15)
            session_id = json.loads(listed.stdout)['data'][0]['id']
            proc, master = start(['--permissions', 'auto', 'resume', session_id])
            try:
                until(proc, master, '> ')
                if len(requests) != 2:
                    raise RuntimeError('resume sent a model request before new input')
                os.write(master, b'Continue the previous task\n')
                if b'RESUMED_DONE' not in until(proc, master, '> '):
                    raise RuntimeError('continued task did not finish')
                exit_chat(proc, master)
            finally:
                if proc.poll() is None:
                    proc.kill()
                    proc.wait()
            if len(requests) != 3 or 'JOURNEY_PRIVATE_FIXTURE' not in json.dumps(requests[2]):
                raise RuntimeError('resumed request did not preserve the tool result')
            print('clean config, manual approval, exit, session list, resume and continuation: passed')
            return 0
    finally:
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    sys.exit(main())
