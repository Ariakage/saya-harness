#!/usr/bin/env python3
"""Offline native TUI journey using a PTY and loopback protocol fixtures."""
import argparse
import codecs
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import tty
import unicodedata
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Screen:
    """Small VT screen used to observe cursor-positioned Ratatui updates."""
    def __init__(self, columns=110, rows=32):
        self.resize(columns, rows)
        self.pending = ''
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.saved = (0, 0)

    def resize(self, columns, rows):
        self.columns, self.rows = columns, rows
        self.cells = [[' '] * columns for _ in range(rows)]
        self.x = self.y = 0

    def feed(self, raw):
        value = self.pending + self.decoder.decode(raw)
        self.pending = ''
        index = 0
        while index < len(value):
            char = value[index]
            if char == '\x1b':
                if index + 1 == len(value):
                    self.pending = value[index:]
                    break
                following = value[index + 1]
                if following == '[':
                    match = re.match(r'\x1b\[([0-9;?<>:]*)([@-~])', value[index:])
                    if not match:
                        self.pending = value[index:]
                        break
                    self.control(match.group(1), match.group(2))
                    index += len(match.group(0))
                    continue
                if following in ']P':
                    end = re.search(r'\x07|\x1b\\', value[index + 2:])
                    if not end:
                        self.pending = value[index:]
                        break
                    index += 2 + end.end()
                    continue
                if following == '7':
                    self.saved = (self.x, self.y)
                elif following == '8':
                    self.x, self.y = self.saved
                index += 2
                continue
            if char == '\r':
                self.x = 0
            elif char == '\n':
                self.y = min(self.y + 1, self.rows - 1)
            elif char == '\b':
                self.x = max(0, self.x - 1)
            elif char == '\t':
                self.x = min(self.columns - 1, (self.x // 8 + 1) * 8)
            elif ord(char) >= 32 and not unicodedata.category(char).startswith('C'):
                if unicodedata.combining(char):
                    if self.x:
                        self.cells[self.y][self.x - 1] += char
                else:
                    width = 2 if unicodedata.east_asian_width(char) in 'WF' else 1
                    if self.x >= self.columns:
                        self.x = 0
                        self.y = min(self.y + 1, self.rows - 1)
                    self.cells[self.y][self.x] = char
                    if width == 2 and self.x + 1 < self.columns:
                        self.cells[self.y][self.x + 1] = ''
                    self.x += width
            index += 1

    def control(self, parameters, final):
        private = parameters.startswith('?')
        numbers = [int(part) if part else 0 for part in parameters.lstrip('?<>').split(';') if ':' not in part]
        first = numbers[0] if numbers else 0
        amount = first or 1
        if final in 'Hf':
            self.y = min(self.rows - 1, max(0, amount - 1))
            self.x = min(self.columns - 1, max(0, (numbers[1] if len(numbers) > 1 and numbers[1] else 1) - 1))
        elif final == 'G':
            self.x = min(self.columns - 1, amount - 1)
        elif final == 'd':
            self.y = min(self.rows - 1, amount - 1)
        elif final == 'A':
            self.y = max(0, self.y - amount)
        elif final == 'B':
            self.y = min(self.rows - 1, self.y + amount)
        elif final == 'C':
            self.x = min(self.columns - 1, self.x + amount)
        elif final == 'D':
            self.x = max(0, self.x - amount)
        elif final == 'J':
            if first in (2, 3):
                self.cells = [[' '] * self.columns for _ in range(self.rows)]
            elif first == 0:
                self.cells[self.y][self.x:] = [' '] * (self.columns - self.x)
                for row in range(self.y + 1, self.rows):
                    self.cells[row] = [' '] * self.columns
        elif final == 'K':
            if first == 0:
                self.cells[self.y][self.x:] = [' '] * (self.columns - self.x)
            elif first == 1:
                self.cells[self.y][:self.x + 1] = [' '] * (self.x + 1)
            elif first == 2:
                self.cells[self.y] = [' '] * self.columns
        elif final == 's':
            self.saved = (self.x, self.y)
        elif final == 'u':
            self.x, self.y = self.saved
        elif private and final == 'h' and 1049 in numbers:
            self.cells = [[' '] * self.columns for _ in range(self.rows)]
            self.x = self.y = 0

    @property
    def text(self):
        return '\n'.join(''.join(row).rstrip() for row in self.cells)


SUPERVISOR = r"""
import os
import subprocess
import sys

release, ready, status = map(int, sys.argv[1:4])
child = subprocess.Popen(sys.argv[4:], close_fds=True)
os.write(ready, (str(child.pid) + '\n').encode())
os.close(ready)
_, wait_status = os.waitpid(child.pid, 0)
code = os.waitstatus_to_exitcode(wait_status)
child.returncode = code
os.write(status, (str(code) + '\n').encode())
os.close(status)
os.read(release, 1)
os.close(release)
"""


class SupervisedProcess:
    """Keep the controlling session alive while terminal restoration is checked."""
    def __init__(self, command, cwd, environment, slave):
        release_read, self.release_write = os.pipe()
        ready_read, ready_write = os.pipe()
        self.status_read, status_write = os.pipe()
        def prepare():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        self.supervisor = subprocess.Popen(
            [sys.executable, '-c', SUPERVISOR, str(release_read), str(ready_write),
             str(status_write), *command], cwd=cwd, env=environment,
            stdin=slave, stdout=slave, stderr=slave, preexec_fn=prepare,
            pass_fds=(release_read, ready_write, status_write))
        os.close(release_read)
        os.close(ready_write)
        os.close(status_write)
        try:
            ready, _, _ = select.select([ready_read], [], [], 10)
            if not ready:
                raise RuntimeError('terminal supervisor did not start the TUI')
            self.pid = int(os.read(ready_read, 64).strip())
        finally:
            os.close(ready_read)
        self.returncode = None

    def poll(self):
        if self.returncode is None:
            ready, _, _ = select.select([self.status_read], [], [], 0)
            if ready:
                value = os.read(self.status_read, 64).strip()
                if not value:
                    raise RuntimeError('terminal supervisor ended without a TUI exit status')
                self.returncode = int(value)
        return self.returncode

    def wait(self, timeout=None):
        deadline = time.monotonic() + timeout if timeout is not None else None
        while self.poll() is None:
            remaining = None if deadline is None else deadline - time.monotonic()
            if remaining is not None and remaining <= 0:
                raise subprocess.TimeoutExpired('supervised TUI', timeout)
            select.select([self.status_read], [], [], remaining)
        return self.returncode

    def kill(self):
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def close(self):
        if self.poll() is None:
            self.kill()
            self.wait(timeout=10)
        try:
            os.write(self.release_write, b'1')
        except BrokenPipeError:
            pass
        os.close(self.release_write)
        os.close(self.status_read)
        deadline = time.monotonic() + 10
        while self.supervisor.poll() is None and time.monotonic() < deadline:
            time.sleep(0.01)
        if self.supervisor.poll() is None:
            self.supervisor.kill()
            _, status = os.waitpid(self.supervisor.pid, 0)
            self.supervisor.returncode = os.waitstatus_to_exitcode(status)


class Terminal:
    def __init__(self, command, cwd, environment):
        self.master, self.slave = pty.openpty()
        # Darwin marks canonical re-entry with PENDIN. Prime that kernel state
        # before the snapshot, retaining exact comparison after restoration.
        canonical = termios.tcgetattr(self.slave)
        tty.setraw(self.slave)
        termios.tcsetattr(self.slave, termios.TCSANOW, canonical)
        # Darwin marks a PTY's first output in F_GETFL. Prime and drain one
        # byte before the snapshot so frontend flag restoration stays exact.
        os.write(self.slave, b'\x00')
        os.read(self.master, 1)
        self.before = termios.tcgetattr(self.slave)
        self.before_flags = fcntl.fcntl(self.slave, fcntl.F_GETFL)
        self.screen = Screen()
        self.raw = bytearray()
        self.resize(110, 32)
        try:
            self.process = SupervisedProcess(command, cwd, environment, self.slave)
        except BaseException:
            os.close(self.master)
            os.close(self.slave)
            raise

    def resize(self, columns, rows):
        # The kernel sends SIGWINCH to the foreground group for TIOCSWINSZ.
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', rows, columns, 0, 0))
        self.screen.resize(columns, rows)

    def pump(self, timeout=0.05):
        ready, _, _ = select.select([self.master], [], [], timeout)
        if ready:
            try:
                data = os.read(self.master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    return
                raise
            self.raw.extend(data)
            if len(self.raw) > 2 * 1024 * 1024:
                del self.raw[:-2 * 1024 * 1024]
            self.screen.feed(data)

    def wait(self, predicate, description, timeout=20):
        deadline = time.monotonic() + timeout
        while not predicate():
            self.pump()
            if self.process.poll() is not None:
                self.pump(0)
                if predicate():
                    return
                raise RuntimeError(f'TUI exited before {description}, code {self.process.returncode}\n{self.screen.text}')
            if time.monotonic() >= deadline:
                raise RuntimeError(f'TUI timed out waiting for {description}\n{self.screen.text}')

    def until(self, text, timeout=20):
        self.wait(lambda: text in self.screen.text, repr(text), timeout)

    def title(self):
        self.wait(lambda: 'saya' in self.screen.text.lower(), 'TUI title')

    def send(self, value):
        os.write(self.master, value.encode() if isinstance(value, str) else value)

    def line(self, value):
        self.send(value + '\r')

    def finish(self, code=0):
        self.send(b'\x04')
        self.wait(lambda: self.process.poll() is not None, 'normal exit')
        if self.process.returncode != code:
            raise RuntimeError(f'expected exit {code}, found {self.process.returncode}\n{self.screen.text}')
        self.verify_restoration()

    def verify_restoration(self):
        for _ in range(8):
            self.pump(0.01)
        if termios.tcgetattr(self.slave) != self.before:
            raise RuntimeError('terminal attributes were not restored')
        if fcntl.fcntl(self.slave, fcntl.F_GETFL) != self.before_flags:
            raise RuntimeError('terminal file status flags were not restored')
        if b'\x1b[?1049h' not in self.raw or b'\x1b[?1049l' not in self.raw:
            raise RuntimeError('alternate screen was not entered and restored')

    def close(self):
        # After the slow-reader assertion, release pending output before the
        # controlling session leader exits. Darwin may otherwise drain on exit.
        for _ in range(16):
            self.pump(0)
        self.process.close()
        os.close(self.master)
        os.close(self.slave)


def protocol_stream(protocol, text='', call=None, pieces=None):
    pieces = pieces if pieces is not None else [text]
    if protocol == 'openai-chat-completions':
        delta = {'content': text}
        if call:
            identifier, name, arguments = call
            delta = {'tool_calls': [{'index': 0, 'id': identifier, 'type': 'function',
                                    'function': {'name': name, 'arguments': json.dumps(arguments)}}]}
        deltas = [delta] if call else [{'content': piece} for piece in pieces]
        events = [{'choices': [{'index': 0, 'delta': piece, 'finish_reason': None}]} for piece in deltas]
        events.append({'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'tool_calls' if call else 'stop'}]})
        return ''.join('data: ' + json.dumps(event) + '\n\n' for event in events) + 'data: [DONE]\n\n'
    if protocol == 'openai-responses':
        if call:
            identifier, name, arguments = call
            item = {'type': 'function_call', 'id': 'item-' + identifier, 'status': 'completed',
                    'call_id': identifier, 'name': name, 'arguments': json.dumps(arguments)}
            events = []
        else:
            item = {'type': 'message', 'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text', 'text': text}]}
            events = [{'type': 'response.output_text.delta', 'output_index': 0,
                       'content_index': 0, 'delta': piece} for piece in pieces]
        events += [{'type': 'response.output_item.done', 'output_index': 0, 'item': item},
                   {'type': 'response.completed', 'response': {'status': 'completed', 'output': [item]}}]
        return ''.join('data: ' + json.dumps(event) + '\n\n' for event in events)
    if call:
        identifier, name, arguments = call
        block = {'type': 'tool_use', 'id': identifier, 'name': name, 'input': arguments}
        deltas = []
    else:
        block = {'type': 'text', 'text': ''}
        deltas = [{'type': 'content_block_delta', 'index': 0,
                   'delta': {'type': 'text_delta', 'text': piece}} for piece in pieces]
    events = [{'type': 'message_start', 'message': {'role': 'assistant', 'content': []}},
              {'type': 'content_block_start', 'index': 0, 'content_block': block}, *deltas,
              {'type': 'content_block_stop', 'index': 0},
              {'type': 'message_delta', 'delta': {'stop_reason': 'tool_use' if call else 'end_turn'}},
              {'type': 'message_stop'}]
    return ''.join(f'event: {event["type"]}\ndata: {json.dumps(event)}\n\n' for event in events)


class Fixture:
    def __init__(self, protocol, replies):
        self.protocol = protocol
        self.replies = list(replies)
        self.requests = []
        self.lock = threading.Lock()
        fixture = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass
            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                with fixture.lock:
                    fixture.requests.append(request)
                    reply = fixture.replies.pop(0) if fixture.replies else {'text': 'UNEXPECTED_REQUEST'}
                if callable(reply):
                    reply = reply(request)
                if 'delay' in reply:
                    time.sleep(reply['delay'])
                body = protocol_stream(protocol, reply.get('text', ''), reply.get('call'), reply.get('pieces')).encode()
                if reply.get('partial'):
                    body = body.split(b'\n\n', 1)[0] + b'\n\n'
                try:
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    if reply.get('stream_delay'):
                        for chunk in body.split(b'\n\n'):
                            self.wfile.write(chunk + b'\n\n')
                            self.wfile.flush()
                            time.sleep(reply['stream_delay'])
                    else:
                        self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def environment(root):
    return {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': str(root), 'TERM': 'xterm-256color',
            'XDG_DATA_HOME': str(root / 'data'), 'XDG_STATE_HOME': str(root / 'state'),
            'XDG_CACHE_HOME': str(root / 'cache'), 'NO_PROXY': '127.0.0.1,localhost'}


def configuration(root, fixture, permission='auto'):
    path = root / 'config.toml'
    path.write_text('\n'.join(['schema_version = 1', '[defaults]', 'profile = "fixture"',
        f'permissions = "{permission}"', '[profiles.fixture]', f'protocol = "{fixture.protocol}"',
        f'base_url = "http://127.0.0.1:{fixture.server.server_port}/v1"', 'model = "fixture"',
        'auth = "none"', 'context_window_tokens = 128000', 'stream = true', '']))
    return path


def invoke(binary, root, config, extra=()):
    return Terminal([str(binary), '--config', str(config), '--cwd', str(root / 'workspace'), *extra],
                    root / 'workspace', environment(root))


def stream_journey(binary, protocol):
    fixture = Fixture(protocol, [{'text': 'TUI_STREAM_你好_🌸', 'pieces': ['TUI_STREAM_', '你好_', '🌸']}])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-stream-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                if termios.tcgetattr(terminal.slave)[3] & termios.ICANON:
                    raise RuntimeError('TUI did not enable raw mode')
                expected_input = 'first 你好🌸\nsecond\nthird 粘贴'
                terminal.send('first 你好🌸')
                terminal.send(b'\x0a')  # Ctrl+J inserts a newline without sending.
                terminal.send('\x1b[200~second\nthird 粘贴\x1b[201~')
                terminal.send(b'\r')
                terminal.until('TUI_STREAM_你好_🌸')
                wait_idle(terminal, root)
                terminal.resize(48, 16)
                terminal.title()
                terminal.resize(110, 32)
                terminal.until('TUI_STREAM_你好_🌸')
                snapshot = terminal.screen.text
                terminal.finish()
                if len(fixture.requests) != 1 or fixture.requests[0].get('stream') is not True:
                    raise RuntimeError('TUI stream did not use one streamed model request')
                request = fixture.requests[0]
                messages = request['input'] if protocol == 'openai-responses' else request['messages']
                user_text = []
                for message in messages:
                    if message.get('role') != 'user':
                        continue
                    content = message['content']
                    if isinstance(content, str):
                        user_text.append(content)
                    else:
                        user_text.extend(block.get('text') for block in content if block.get('type') == 'text')
                if expected_input not in user_text:
                    raise RuntimeError('multiline Unicode editing and bracketed paste changed the model input')
                return snapshot
            finally:
                terminal.close()
    finally:
        fixture.close()


def session_data(binary, root, config):
    result = subprocess.run([str(binary), '--config', str(config), '--cwd', str(root / 'workspace'),
                             '--output', 'json', 'session', 'list'], cwd=root / 'workspace',
                            env=environment(root), capture_output=True, check=True, timeout=15)
    sessions = json.loads(result.stdout)['data']
    return sessions


def wait_idle(terminal, root, count=1, expected_status='completed'):
    path = root / 'data' / 'saya' / 'sessions.sqlite3'
    outcome = []
    def finished():
        if not path.exists():
            return False
        try:
            with sqlite3.connect('file:' + str(path) + '?mode=ro', uri=True) as connection:
                records = connection.execute('SELECT result FROM runs WHERE finished_at IS NOT NULL ORDER BY rowid').fetchall()
                if len(records) >= count:
                    outcome[:] = [json.loads(records[count - 1][0])]
                    return True
                return False
        except sqlite3.Error:
            return False
    terminal.wait(finished, 'persisted run completion')
    terminal.pump(0.1)
    if expected_status and outcome[0].get('status') != expected_status:
        raise RuntimeError('unexpected persisted run status: ' + json.dumps(outcome[0]))
    return outcome[0]


def mode_permission_matrix(binary):
    for mode in ['plan', 'build', 'code', 'work']:
        for permission in ['auto', 'bypass', 'manual']:
            marker = 'MATRIX_' + mode + '_' + permission
            fixture = Fixture('openai-chat-completions', [{'text': marker}])
            try:
                with tempfile.TemporaryDirectory(prefix='saya-tui-mode-') as directory:
                    root = Path(directory)
                    (root / 'workspace').mkdir()
                    config = configuration(root, fixture, permission)
                    terminal = invoke(binary, root, config, ['--mode', mode])
                    try:
                        terminal.title()
                        terminal.line('Report the configured mode')
                        terminal.until(marker)
                        wait_idle(terminal, root)
                        terminal.finish()
                        sessions = session_data(binary, root, config)
                        if len(sessions) != 1 or sessions[0]['work_mode'].lower() != mode:
                            raise RuntimeError('work mode was not persisted for ' + marker)
                        if len(fixture.requests) != 1:
                            raise RuntimeError('text answer unexpectedly requested approval or another model call')
                    finally:
                        terminal.close()
            finally:
                fixture.close()


def tool_result(request, call_id):
    for message in request.get('messages', []):
        if message.get('role') == 'tool' and message.get('tool_call_id') == call_id:
            return json.loads(message['content'])
    raise RuntimeError('missing tool result for ' + call_id)


def expect_tool(call_id, reply):
    def respond(request):
        result = tool_result(request, call_id)
        if not result.get('ok'):
            raise RuntimeError('tool failed before next model request: ' + call_id)
        return reply(request) if callable(reply) else reply
    return respond


def approval(terminal, fixture, call_id, expected_requests):
    def visible():
        text = terminal.screen.text
        lower = text.lower()
        marker = 'background-' if call_id == 'manual-start' else call_id
        return marker in text and ('approval required' in lower or 'approve' in lower or 'y/n' in lower or 'allow' in lower)
    terminal.wait(visible, 'approval for ' + call_id)
    if len(fixture.requests) != expected_requests:
        raise RuntimeError('model continued before approval for ' + call_id)
    terminal.send('y')


def manual_tools_and_clarification(binary):
    read = ('manual-read', 'read_file', {'path': 'fact.txt', 'start_line': 1, 'line_count': 2})
    search = ('manual-search', 'search_text', {'path': '.', 'pattern': 'TUI_PRIVATE_FIXTURE'})
    start = ('manual-start', 'background_start', {'tool': 'read_file',
              'arguments': {'path': 'fact.txt', 'start_line': 1, 'line_count': 2}})
    ask = ('manual-question', 'ask_user', {'question': 'TUI_QUESTION', 'options': ['first', 'second']})
    def status(request):
        job_id = tool_result(request, 'manual-start')['data']['job_id']
        return {'call': ('manual-status', 'background_status', {'job_id': job_id})}
    def final(request):
        result = tool_result(request, 'manual-question')
        if 'TUI_ANSWER' not in json.dumps(result):
            raise RuntimeError('clarification answer did not reach the model')
        return {'text': 'MANUAL_TOOLS_DONE'}
    replies = [{'call': read}, expect_tool('manual-read', {'call': search}),
               expect_tool('manual-search', {'call': start}), expect_tool('manual-start', status),
               expect_tool('manual-status', {'call': ask}), expect_tool('manual-question', final)]
    fixture = Fixture('openai-chat-completions', replies)
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-manual-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            (root / 'workspace' / 'fact.txt').write_text('TUI_PRIVATE_FIXTURE\n')
            config = configuration(root, fixture, 'manual')
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                terminal.line('Read, search, background status and ask the fixture')
                for index, call_id in enumerate(['manual-read', 'manual-search', 'manual-start', 'manual-status', 'manual-question'], 1):
                    approval(terminal, fixture, call_id, index)
                terminal.wait(lambda: 'Question' in terminal.screen.text and 'TUI_QUESTION' in terminal.screen.text
                              and 'Approval required' not in terminal.screen.text, 'clarification input')
                terminal.line('TUI_ANSWER')
                terminal.until('MANUAL_TOOLS_DONE')
                wait_idle(terminal, root)
                full_export = root / 'full tool session.json'
                terminal.line('/export-full json ' + str(full_export))
                terminal.wait(full_export.exists, 'full session export')
                terminal.until('Exported')
                exported = json.loads(full_export.read_text())
                if exported['snapshot']['scope'] != 'full' or not exported['snapshot']['include_tools']:
                    raise RuntimeError('full TUI export did not retain its explicit tool opt-in')
                if len(exported['tool_calls']) < 5 or 'TUI_PRIVATE_FIXTURE' not in full_export.read_text():
                    raise RuntimeError('full TUI export omitted completed tool records')
                terminal.finish()
                if len(fixture.requests) != 6:
                    raise RuntimeError('manual tool roundtrip request count is incorrect')
            finally:
                terminal.close()
    finally:
        fixture.close()


def mode_change_invalidates_approval(binary):
    call = ('stale-write', 'write_file', {'path': 'must-not-exist.txt', 'content': 'unapproved'})
    def after(request):
        result = tool_result(request, 'stale-write')
        if result.get('ok') or result.get('error', {}).get('code') not in ('stale_approval', 'permission_denied', 'mode_changed'):
            raise RuntimeError('mode switch did not invalidate the pending write approval')
        return {'text': 'STALE_MODE_DONE'}
    fixture = Fixture('openai-chat-completions', [{'call': call}, after])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-stale-') as directory:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            config = configuration(root, fixture, 'manual')
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                terminal.line('Request the manual write')
                terminal.wait(lambda: 'stale-write' in terminal.screen.text, 'pending write approval')
                if len(fixture.requests) != 1 or (workspace / 'must-not-exist.txt').exists():
                    raise RuntimeError('manual write executed before confirmation')
                terminal.line('/mode plan')
                terminal.until('STALE_MODE_DONE')
                wait_idle(terminal, root)
                if (workspace / 'must-not-exist.txt').exists():
                    raise RuntimeError('stale approval wrote a workspace file')
                sessions = session_data(binary, root, config)
                if sessions[0]['work_mode'].lower() != 'plan':
                    raise RuntimeError('requested PLAN mode was not applied and persisted')
                terminal.finish()
            finally:
                terminal.close()
    finally:
        fixture.close()


def session_restore_and_export(binary):
    fixture = Fixture('openai-chat-completions', [{'text': 'SESSION_FIRST'}, {'text': 'SESSION_CONTINUED'}])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-session-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                terminal.line('Create the first session')
                terminal.until('SESSION_FIRST')
                wait_idle(terminal, root)
                sessions = session_data(binary, root, config)
                identifier = sessions[0]['id']
                competing = invoke(binary, root, config)
                try:
                    competing.title()
                    competing.line('/resume ' + identifier)
                    competing.until('session_busy')
                    competing.finish()
                finally:
                    competing.close()
                if len(fixture.requests) != 1:
                    raise RuntimeError('locked session resume called the model')
                first_export = root / 'first session.json'
                terminal.line('/export json ' + str(first_export))
                terminal.wait(first_export.exists, 'JSON export file')
                terminal.until('Exported')
                first = json.loads(first_export.read_text())
                if first.get('export_type') != 'saya.session' or first['session']['id'] != identifier:
                    raise RuntimeError('TUI exported a different session')
                if first['snapshot']['scope'] != 'transcript' or first['snapshot']['include_tools']:
                    raise RuntimeError('default TUI export did not use transcript privacy defaults')
                terminal.line('/sessions')
                terminal.until('Sessions · UI')
                terminal.until(identifier[:8])
                terminal.send(b'\x1b')
                terminal.wait(lambda: 'Sessions · UI' not in terminal.screen.text, 'session list closed')
                terminal.line('/new')
                terminal.wait(lambda: 'New session ready.' in terminal.screen.text and
                              'SESSION_FIRST' not in terminal.screen.text, 'new empty session')
                if len(session_data(binary, root, config)) != 2:
                    raise RuntimeError('new session did not create a distinct saved session')
                terminal.line('/resume ' + identifier)
                terminal.until('Session restored.')
                terminal.until('SESSION_FIRST')
                if len(fixture.requests) != 1:
                    raise RuntimeError('session browsing or resume automatically called the model')
                second_export = root / 'restored session.md'
                terminal.line('/export markdown ' + str(second_export))
                terminal.wait(second_export.exists, 'Markdown export file')
                terminal.until('Exported')
                if 'SESSION_FIRST' not in second_export.read_text():
                    raise RuntimeError('restored transcript was missing from export')
                terminal.line('Continue the restored session')
                terminal.until('SESSION_CONTINUED')
                wait_idle(terminal, root, count=2)
                terminal.finish()
                if len(fixture.requests) != 2 or 'SESSION_FIRST' not in json.dumps(fixture.requests[1]):
                    raise RuntimeError('resume did not preserve original history')
            finally:
                terminal.close()
    finally:
        fixture.close()


def cancellation_and_incomplete_stream(binary):
    for first, expected in [({'text': 'DELAYED', 'delay': 5}, 'cancelled'),
                            ({'text': 'PARTIAL_ONLY', 'partial': True}, 'failed')]:
        fixture = Fixture('openai-chat-completions', [first, {'text': 'AFTER_FAILURE'}])
        try:
            with tempfile.TemporaryDirectory(prefix='saya-tui-control-') as directory:
                root = Path(directory)
                (root / 'workspace').mkdir()
                config = configuration(root, fixture)
                terminal = invoke(binary, root, config)
                try:
                    terminal.title()
                    terminal.line('Run the cancellable fixture')
                    terminal.wait(lambda: len(fixture.requests) == 1, 'first model request')
                    if expected == 'cancelled':
                        terminal.send(b'\x03')
                    wait_idle(terminal, root, expected_status=expected)
                    terminal.line('Continue after the interruption')
                    terminal.until('AFTER_FAILURE')
                    wait_idle(terminal, root, count=2)
                    terminal.finish()
                    if len(fixture.requests) != 2:
                        raise RuntimeError('interruption replayed or duplicated a model request')
                finally:
                    terminal.close()
        finally:
            fixture.close()


def reveal_details(terminal, predicate, description):
    deadline = time.monotonic() + 15
    next_page = time.monotonic() + 0.3
    pages = 0
    while not predicate():
        terminal.pump()
        if terminal.process.poll() is not None or time.monotonic() >= deadline:
            raise RuntimeError('details did not show ' + description + '\n' + terminal.screen.text)
        if time.monotonic() >= next_page and pages < 10:
            terminal.send(b'\x1b[6~')
            next_page = time.monotonic() + 0.15
            pages += 1


def background_control_and_unknown_records(binary):
    job = {}
    start = ('background-parent', 'background_start', {'tool': 'exec', 'arguments': {
        'argv': ['/bin/sh', '-c', 'echo $$ > child.pid; sleep 20; echo unsafe > late-write.txt'],
        'timeout_seconds': 30}})
    def after_start(request):
        result = tool_result(request, 'background-parent')
        if not result.get('ok'):
            raise RuntimeError('background command was not started')
        job['id'] = result['data']['job_id']
        return {'call': ('after-background', 'read_file', {'path': 'fact.txt', 'start_line': 1, 'line_count': 2}), 'delay': 5}
    fixture = Fixture('openai-chat-completions', [{'call': start}, after_start,
                      expect_tool('after-background', {'text': 'BG_CONTROL_DONE'})])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-background-') as directory:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            (workspace / 'fact.txt').write_text('background fixture\n')
            config = configuration(root, fixture, 'bypass')
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                terminal.line('Start the background fixture')
                terminal.wait(lambda: 'id' in job and (workspace / 'child.pid').exists(), 'running background child')
                child_pid = int((workspace / 'child.pid').read_text())
                terminal.line('/jobs')
                terminal.until('Background jobs · UI snapshot')
                terminal.send(b'\x1b')
                terminal.wait(lambda: 'Background jobs · UI snapshot' not in terminal.screen.text, 'background jobs closed')
                terminal.line('/cancel-job ' + job['id'])
                terminal.until('Background cancellation requested')
                def stopped():
                    try:
                        os.kill(child_pid, 0)
                        return False
                    except ProcessLookupError:
                        return True
                terminal.wait(stopped, 'background child cleanup')
                terminal.line('/mode plan')
                terminal.until('BG_CONTROL_DONE')
                wait_idle(terminal, root)
                if (workspace / 'late-write.txt').exists():
                    raise RuntimeError('cancelled background command performed its late write')
                sessions = session_data(binary, root, config)
                identifier = sessions[0]['id']
                if sessions[0]['work_mode'].lower() != 'plan':
                    raise RuntimeError('PLAN transition was not persisted after cleanup')
                terminal.finish()
            finally:
                terminal.close()
            database = root / 'data' / 'saya' / 'sessions.sqlite3'
            with sqlite3.connect(database) as connection:
                changed = connection.execute("UPDATE tool_calls SET state='unknown',result=NULL WHERE call_id=?", (job['id'],))
                if changed.rowcount != 1:
                    raise RuntimeError('fixture did not identify exactly one persisted background call')
                connection.commit()
            resumed = invoke(binary, root, config)
            try:
                resumed.title()
                resumed.line('/resume ' + identifier)
                resumed.until('Session restored.')
                resumed.line('/tools')
                reveal_details(resumed, lambda: 'unknown' in resumed.screen.text.lower()
                               and 'unknown command' not in resumed.screen.text.lower(), 'persisted unknown tool outcome')
                resumed.finish()
                if len(fixture.requests) != 3:
                    raise RuntimeError('viewing unknown tool records replayed a model request')
            finally:
                resumed.close()
    finally:
        fixture.close()


def high_frequency_stream(binary):
    pieces = ['chunk_%04d ' % index for index in range(512)] + ['STREAM_FINAL']
    text = ''.join(pieces)
    fixture = Fixture('openai-chat-completions', [{'text': text, 'pieces': pieces}])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-frequency-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                terminal.line('Emit the complete stream')
                terminal.until('STREAM_FINAL')
                result = wait_idle(terminal, root)
                if result.get('text') != text:
                    raise RuntimeError('high-frequency stream lost final text')
                terminal.finish()
            finally:
                terminal.close()
    finally:
        fixture.close()


def slow_terminal_consumer(binary):
    pieces = [('frame_%04d_' % index) + ('x' * 500) for index in range(240)]
    fixture = Fixture('openai-chat-completions', [{'text': ''.join(pieces), 'pieces': pieces,
                                               'stream_delay': 0.015}])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-slow-consumer-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                terminal.line('Stream until the terminal stops consuming')
                terminal.wait(lambda: bool(fixture.requests), 'stream request')
                # Deliberately leave the PTY output queue undrained.
                time.sleep(1.5)
                if terminal.process.poll() is None:
                    os.kill(terminal.process.pid, signal.SIGTERM)
                try:
                    terminal.process.wait(timeout=8)
                except subprocess.TimeoutExpired as error:
                    raise RuntimeError('slow terminal consumer blocked cancellation or output-error exit') from error
                if termios.tcgetattr(terminal.slave) != terminal.before:
                    raise RuntimeError('slow consumer exit left the terminal in raw mode')
                if fcntl.fcntl(terminal.slave, fcntl.F_GETFL) != terminal.before_flags:
                    raise RuntimeError('slow consumer exit left modified terminal FD flags')
            finally:
                terminal.close()
    finally:
        fixture.close()


def signal_restore(binary):
    for sig in [signal.SIGTERM, signal.SIGHUP]:
        fixture = Fixture('openai-chat-completions', [])
        try:
            with tempfile.TemporaryDirectory(prefix='saya-tui-signal-') as directory:
                root = Path(directory)
                (root / 'workspace').mkdir()
                config = configuration(root, fixture)
                terminal = invoke(binary, root, config)
                try:
                    terminal.title()
                    os.kill(terminal.process.pid, sig)
                    terminal.wait(lambda: terminal.process.poll() is not None, 'signal shutdown')
                    terminal.verify_restoration()
                    if fixture.requests:
                        raise RuntimeError('idle signal shutdown started a model request')
                finally:
                    terminal.close()
        finally:
            fixture.close()


def invalid_routes(binary):
    with tempfile.TemporaryDirectory(prefix='saya-tui-routing-') as directory:
        root = Path(directory)
        (root / 'workspace').mkdir()
        config = root / 'config.toml'
        config.write_text('schema_version = 1\n')
        common = [str(binary), '--config', str(config), '--cwd', str(root / 'workspace')]
        for extra in [[], ['--output', 'json'], ['--output', 'jsonl']]:
            result = subprocess.run(common + extra, cwd=root / 'workspace', env=environment(root),
                                    input=b'', capture_output=True, timeout=10)
            if result.returncode != 2 or result.stdout:
                raise RuntimeError('non-TTY default route must return usage error 2 with empty stdout')
        if (root / 'data').exists() or (root / 'state').exists():
            raise RuntimeError('invalid default route created application storage')
        for extra in [['--output', 'json'], ['--output', 'jsonl']]:
            terminal = Terminal(common + extra, root / 'workspace', environment(root))
            try:
                terminal.wait(lambda: terminal.process.poll() is not None, 'machine-output rejection')
                if terminal.process.returncode != 2 or b'\x1b[?1049h' in terminal.raw:
                    raise RuntimeError('machine-output route entered alternate screen')
            finally:
                terminal.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=Path)
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    results = []
    screens = {}
    def passed(check):
        results.append(check)
        print('passed ' + check, flush=True)
    invalid_routes(binary)
    passed('default-route-preflight')
    for protocol in ['openai-chat-completions', 'openai-responses', 'anthropic-messages']:
        screens[protocol] = stream_journey(binary, protocol)
        passed(protocol + '-stream-resize-terminal-restore')
    mode_permission_matrix(binary)
    passed('four-modes-three-permissions')
    manual_tools_and_clarification(binary)
    passed('manual-read-search-background-query-and-clarification')
    mode_change_invalidates_approval(binary)
    passed('mode-change-invalidates-write-approval')
    session_restore_and_export(binary)
    passed('sessions-restore-and-export-without-replay')
    cancellation_and_incomplete_stream(binary)
    passed('cancellation-incomplete-stream-and-continuation')
    background_control_and_unknown_records(binary)
    passed('background-cancel-plan-cleanup-and-unknown-records')
    high_frequency_stream(binary)
    passed('high-frequency-complete-stream')
    slow_terminal_consumer(binary)
    passed('slow-terminal-consumer-raw-restore')
    signal_restore(binary)
    passed('signal-terminal-restore')
    report = {'schema_version': 1, 'binary': str(binary), 'checks': results, 'terminal_screens': screens, 'status': 'passed'}
    if args.report:
        args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print('native TUI journey passed:', ', '.join(results))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
