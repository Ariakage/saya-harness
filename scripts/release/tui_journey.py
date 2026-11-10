#!/usr/bin/env python3
"""Offline native TUI journey using a PTY and loopback protocol fixtures."""
import argparse
import base64
import codecs
from contextlib import ExitStack
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
from process_witness import ProcessLease


EDITOR_PLACEHOLDER = 'Describe a task…'
INPUT_STRIPE = '\U0001fbcf'
MODE_COLORS = {'WORK': (88, 166, 255), 'CODE': (125, 203, 155),
               'PLAN': (187, 154, 247), 'BUILD': (246, 173, 85)}


class Screen:
    """Small VT screen used to observe cursor-positioned Ratatui updates."""
    def __init__(self, columns=110, rows=32):
        self.foreground = self.background = None
        self.resize(columns, rows)
        self.pending = ''
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.saved = (0, 0)
        self.window_titles = []
        self.title_stack = []

    def resize(self, columns, rows):
        self.columns, self.rows = columns, rows
        self.cells = [[' '] * columns for _ in range(rows)]
        self.colors = [[(None, None)] * columns for _ in range(rows)]
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
                    payload = value[index + 2:index + 2 + end.start()]
                    if following == ']' and payload.startswith('2;'):
                        self.window_titles.append(payload[2:])
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
            elif ord(char) >= 32 and (char == INPUT_STRIPE or
                                     not unicodedata.category(char).startswith('C')):
                if unicodedata.combining(char):
                    if self.x:
                        self.cells[self.y][self.x - 1] += char
                else:
                    # Older Python Unicode tables treat this Unicode 17 glyph as
                    # unassigned and fullwidth; its terminal width is one cell.
                    width = 1
                    if char != INPUT_STRIPE and unicodedata.east_asian_width(char) in 'WF':
                        width = 2
                    if self.x >= self.columns:
                        self.x = 0
                        self.y = min(self.y + 1, self.rows - 1)
                    self.cells[self.y][self.x] = char
                    self.colors[self.y][self.x] = (self.foreground, self.background)
                    if width == 2 and self.x + 1 < self.columns:
                        self.cells[self.y][self.x + 1] = ''
                        self.colors[self.y][self.x + 1] = (self.foreground, self.background)
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
        elif final == 't' and numbers in ([22, 2], [23, 2]):
            self.title_stack.append('push' if numbers[0] == 22 else 'pop')
        elif final == 'm':
            index = 0
            while index < len(numbers):
                value = numbers[index]
                if value == 0:
                    self.foreground = self.background = None
                elif value == 39:
                    self.foreground = None
                elif value == 49:
                    self.background = None
                elif value in (38, 48) and numbers[index + 1:index + 2] == [2] and index + 4 < len(numbers):
                    color = tuple(numbers[index + 2:index + 5])
                    if value == 38:
                        self.foreground = color
                    else:
                        self.background = color
                    index += 4
                elif value in (38, 48) and numbers[index + 1:index + 2] == [5] and index + 2 < len(numbers):
                    color = ('indexed', numbers[index + 2])
                    if value == 38:
                        self.foreground = color
                    else:
                        self.background = color
                    index += 2
                index += 1

    @property
    def text(self):
        return '\n'.join(''.join(row).rstrip() for row in self.cells)

    def row_text(self, index):
        return ''.join(self.cells[index]).rstrip()

    def row_style(self, index, text, start=0, end=None):
        end = self.columns if end is None else end
        for column in range(start, end):
            if self.cells[index][column] and ''.join(self.cells[index][column:end]).startswith(text):
                return self.colors[index][column]
        return None

    def main_width(self):
        return next((index + 1 for index, cell in enumerate(self.cells[-1]) if cell == '┘'), None)

    def main_row_text(self, index):
        width = self.main_width()
        return ''.join(self.cells[index][:width]).rstrip() if width is not None else ''

    def sidebar_row_text(self, index):
        width = self.main_width()
        return ''.join(self.cells[index][width:]).strip() if width is not None else ''

    def message_row(self):
        width = self.main_width()
        status_row = self.status_row()
        if width is None or status_row is None or (self.rows == 6 and status_row == 3):
            return None
        index = status_row + 1
        label = ''.join(self.cells[index][3:width - 1]).strip()
        if label == 'Message' or label.startswith('Message '):
            return index
        return None

    def editor_text(self):
        start = self.message_row()
        if start is None and self.rows == 6 and self.status_row() == 3:
            start = 3
        if start is None:
            return None
        width = self.main_width()
        end = self.rows - (2 if self.rows - 1 - start > 2 else 1)
        content = [''.join(self.cells[index][3:width - 1]).rstrip()
                   for index in range(start + 1, end)]
        if (self.rows - 1 - start > 2 and content and content[0] == EDITOR_PLACEHOLDER and
                not any(content[1:]) and self.row_style(start + 1, EDITOR_PLACEHOLDER, 3, width - 1) ==
                ((129, 129, 139), (32, 32, 36))):
            return ''
        return '\n'.join(content).rstrip()

    def mode(self):
        width = self.main_width()
        if width is None:
            return None
        row = ''.join(self.cells[self.rows - 1][1:width - 1])
        match = re.search(r'(?:^|\s)(PLAN|BUILD|CODE|WORK)(?:\s|→|$)', row)
        return match.group(1) if match else None

    def permissions(self):
        status_row = self.status_row()
        if status_row is None:
            return None
        row = self.main_row_text(status_row).strip(' │')
        for label in ('Manual Approval', 'bypass permissions', 'manual', 'bypass', 'auto'):
            if row.endswith(label):
                return {'manual': 'Manual Approval', 'bypass': 'bypass permissions'}.get(label, label)
        return None

    def status(self):
        status_row = self.status_row()
        permission = self.permissions()
        if status_row is None or permission is None:
            return None
        label = permission
        if self.main_width() - 2 < 48:
            label = {'Manual Approval': 'manual', 'bypass permissions': 'bypass'}.get(permission, permission)
        row = self.main_row_text(status_row).strip(' │')
        status = row[:-len(label)].strip().lstrip('✓● ').rstrip(' █▓▒░')
        return {'Approval': 'Awaiting approval', 'Input': 'Awaiting input',
                'Cancel': 'Cancelling', 'Done': 'Completed'}.get(status, status)

    def status_row(self):
        if self.main_width() is None:
            return None
        labels = ('Manual Approval', 'bypass permissions', 'manual', 'bypass', 'auto')
        for index in range(self.rows - 3, max(1, self.rows - 9), -1):
            if self.cells[index][1] in ('✓', '●'):
                row = self.main_row_text(index).strip(' │')
                if any(row.endswith(label) for label in labels):
                    return index
        return None

    def details_text(self):
        end = self.status_row()
        width = self.main_width()
        if width is None:
            return ''
        column = 1
        if width >= 100:
            details_width = min(48, max(32, (width - 2) // 3))
            column = width - 1 - details_width
        return '\n'.join(''.join(self.cells[index][column:width - 1]).rstrip()
                         for index in range(2, max(2, end or 2)))

    def body_text(self):
        width = self.main_width()
        end = self.status_row()
        if width is None or end is None:
            return ''
        return '\n'.join(''.join(self.cells[index][1:width - 1]).rstrip() for index in range(2, end))

    def body_style(self, text):
        width = self.main_width()
        end = self.status_row()
        if width is None or end is None:
            return None
        for row in range(2, end):
            for column in range(1, width - 1):
                if self.cells[row][column] and ''.join(self.cells[row][column:width - 1]).startswith(text):
                    return self.colors[row][column]
        return None

    def fold_headers(self, role=None, details=False):
        width = self.main_width()
        end = self.status_row()
        if width is None or end is None:
            return []
        if details and width >= 100:
            width -= min(48, max(32, (width - 2) // 3))
        headers = []
        column = 2 if width - 2 >= 40 else 1
        for row in range(2, end):
            text = ''.join(self.cells[row][column:width - 1]).rstrip()
            thinking = text.startswith(('▶ Thinking', '▼ Thinking'))
            tool = text.startswith(('▶ Tool', '▼ Tool'))
            background = (9, 9, 11) if thinking else (23, 23, 27)
            if ((thinking or tool) and self.colors[row][column] == ((129, 129, 139), background) and
                    (role is None or text[2:].startswith(role))):
                headers.append((column, row, text))
        return headers

    def popup_text(self):
        width = self.main_width()
        if width is None:
            return ''
        end = max(2, self.status_row() or 2)
        for start in range(2, end):
            row = ''.join(self.cells[start][2:width - 1])
            if row.startswith('┌ Command suggestions '):
                lines = []
                for index in range(start, end):
                    lines.append(''.join(self.cells[index][2:min(width - 2, 82)]).rstrip())
                    if self.cells[index][2] == '└':
                        break
                return '\n'.join(lines)
        return ''

    def command_palette_rect(self):
        width, height = min(self.columns - 2, 88), min(self.rows, 14)
        if width < 4 or height < 4:
            return None
        left, top = (self.columns - width) // 2, (self.rows - height) // 2
        if (self.cells[top][left] != '╭' or self.cells[top][left + width - 1] != '╮' or
                self.cells[top + height - 1][left] != '╰' or self.cells[top + height - 1][left + width - 1] != '╯'):
            return None
        if self.row_style(top, 'Commands', left, left + width) != ((232, 232, 237), (20, 20, 22)):
            return None
        return left, top, width, height

    def command_palette_rows(self):
        rectangle = self.command_palette_rect()
        if rectangle is None:
            return []
        left, top, width, height = rectangle
        return [(left + 1, row, ''.join(self.cells[row][left + 1:left + width - 1]).rstrip())
                for row in range(top + 1, top + height - 2)]

    def command_palette_selected(self):
        return next((item for item in self.command_palette_rows() if item[2].startswith('› ') and
                     self.colors[item[1]][item[0]] == ((9, 9, 11), (88, 166, 255))), None)

    def command_palette_text(self):
        rectangle = self.command_palette_rect()
        if rectangle is None:
            return ''
        left, top, width, height = rectangle
        return '\n'.join(''.join(self.cells[row][left:left + width]).rstrip()
                         for row in range(top, top + height))


SUPERVISOR = r"""
import os
import signal
import subprocess
import sys

release, ready, status = map(int, sys.argv[1:4])
child = subprocess.Popen(sys.argv[4:], close_fds=True)
# Keep the observer alive after a terminal hangup. The child starts with the
# original disposition and owns its signal handling and tool cleanup.
signal.signal(signal.SIGHUP, signal.SIG_IGN)
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

    def close(self, drain=None):
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
            if drain is None:
                time.sleep(0.01)
            else:
                drain()
        if self.supervisor.poll() is None:
            self.supervisor.kill()
            deadline = time.monotonic() + 5
            while self.supervisor.poll() is None and time.monotonic() < deadline:
                if drain is None:
                    time.sleep(0.01)
                else:
                    drain()
            if self.supervisor.poll() is None:
                raise RuntimeError('terminal supervisor did not exit after bounded output drain and SIGKILL')


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
        if self.screen.columns != columns or self.screen.rows != rows:
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

    def settle(self):
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            ready, _, _ = select.select([self.master], [], [], 0.05)
            if not ready and not self.screen.pending and not self.screen.decoder.getstate()[0]:
                return
            self.pump(0)
        raise RuntimeError('terminal output did not reach a complete quiet frame\n' + self.screen.text)

    def title(self):
        self.wait(lambda: self.screen.main_row_text(0).startswith('Saya Harness'), 'TUI title in its header region')

    def editor_text(self):
        return self.screen.editor_text()

    def mode(self):
        return self.screen.mode()

    def permissions(self):
        return self.screen.permissions()

    def send(self, value):
        os.write(self.master, value.encode() if isinstance(value, str) else value)

    def line(self, value):
        self.send(value + '\r')

    def click(self, column, row):
        self.send(f'\x1b[<0;{column + 1};{row + 1}M\x1b[<0;{column + 1};{row + 1}m')

    def wheel(self, column, row, down=True):
        self.send(f'\x1b[<{65 if down else 64};{column + 1};{row + 1}M')

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
        self.process.close(lambda: self.pump(0.01))
        os.close(self.master)
        os.close(self.slave)


def protocol_stream(protocol, text='', call=None, pieces=None, chat_usage=None, chat_reasoning=None, thinking=None):
    pieces = pieces if pieces is not None else [text]
    if protocol == 'openai-chat-completions':
        delta = {'content': text}
        if call:
            identifier, name, arguments = call
            delta = {'tool_calls': [{'index': 0, 'id': identifier, 'type': 'function',
                                    'function': {'name': name, 'arguments': json.dumps(arguments)}}]}
        deltas = [delta] if call else [{'content': piece} for piece in pieces]
        reasoning = chat_reasoning if chat_reasoning is not None else thinking
        if reasoning is not None:
            deltas = [{'reasoning_content': piece} for piece in reasoning] + deltas
        events = [{'choices': [{'index': 0, 'delta': piece, 'finish_reason': None}]} for piece in deltas]
        events.append({'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'tool_calls' if call else 'stop'}]})
        if chat_usage is not None:
            events.append({'choices': [], 'usage': chat_usage})
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
        output = [item]
        if thinking is not None:
            reasoning = {'type': 'reasoning', 'id': 'reasoning-fixture',
                         'summary': [{'type': 'summary_text', 'text': ''.join(thinking)}],
                         'encrypted_content': 'OPAQUE_ENCRYPTED_FIXTURE'}
            for event in events:
                event['output_index'] = 1
            events = [{'type': 'response.reasoning_summary_text.delta', 'output_index': 0,
                       'summary_index': 0, 'delta': piece} for piece in thinking] + events
            events.append({'type': 'response.output_item.done', 'output_index': 0, 'item': reasoning})
            output.insert(0, reasoning)
        events += [{'type': 'response.output_item.done', 'output_index': len(output) - 1, 'item': item},
                   {'type': 'response.completed', 'response': {'status': 'completed', 'output': output}}]
        return ''.join('data: ' + json.dumps(event) + '\n\n' for event in events)
    if call:
        identifier, name, arguments = call
        block = {'type': 'tool_use', 'id': identifier, 'name': name, 'input': arguments}
        deltas = []
    else:
        block = {'type': 'text', 'text': ''}
        deltas = [{'type': 'content_block_delta', 'index': 0,
                   'delta': {'type': 'text_delta', 'text': piece}} for piece in pieces]
    prefix = []
    index = 0
    if thinking is not None:
        index = 1
        prefix = [{'type': 'content_block_start', 'index': 0,
                   'content_block': {'type': 'thinking', 'thinking': '', 'signature': ''}},
                  *[{'type': 'content_block_delta', 'index': 0,
                     'delta': {'type': 'thinking_delta', 'thinking': piece}} for piece in thinking],
                  {'type': 'content_block_delta', 'index': 0,
                   'delta': {'type': 'signature_delta', 'signature': 'OPAQUE_SIGNATURE_FIXTURE'}},
                  {'type': 'content_block_stop', 'index': 0}]
        for event in deltas:
            event['index'] = index
    events = [{'type': 'message_start', 'message': {'role': 'assistant', 'content': []}}, *prefix,
              {'type': 'content_block_start', 'index': index, 'content_block': block}, *deltas,
              {'type': 'content_block_stop', 'index': index},
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
                body = protocol_stream(protocol, reply.get('text', ''), reply.get('call'),
                                       reply.get('pieces'), reply.get('chat_usage'), reply.get('chat_reasoning'),
                                       reply.get('thinking')).encode()
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


def assert_mode_colors(terminal, mode, pending=None):
    terminal.settle()
    screen = terminal.screen
    color = MODE_COLORS[mode]
    footer_row = screen.rows - 1
    if terminal.mode() != mode or screen.row_style(footer_row, mode, 1, screen.main_width() - 1) != (color, (9, 9, 11)):
        raise RuntimeError('footer did not use the actual ' + mode + ' mode color')
    input_row = screen.message_row()
    if input_row is None and screen.rows == 6 and screen.status_row() == 3:
        input_row = 4
    if input_row is None:
        raise RuntimeError('mode color assertion could not locate the actual composer')
    for row in range(input_row, screen.rows - 1):
        if screen.cells[row][1] != INPUT_STRIPE or screen.colors[row][1] != (color, (32, 32, 36)):
            raise RuntimeError('composer stripe did not retain the actual ' + mode + ' mode color')
        if screen.colors[row][screen.main_width() - 2][1] != (9, 9, 11):
            raise RuntimeError('composer omitted its dark right margin')
    if pending is not None:
        if screen.row_style(footer_row, '→', 1, screen.main_width() - 1) != ((129, 129, 139), (9, 9, 11)):
            raise RuntimeError('pending-mode arrow did not retain its secondary color')
        if screen.row_style(footer_row, pending, 1, screen.main_width() - 1) != (MODE_COLORS[pending], (9, 9, 11)):
            raise RuntimeError('pending target mode did not use its own color')


def stream_journey(binary, protocol):
    markdown = '\n'.join(['## Heading', '**strong** and *emphasis* and `inline`', '- item', '> quote',
                          'https://example.test', '<b>literal html</b>', '```rust',
                          'let x = **literal**;', '```', 'RichTextEnd'])
    response = 'TUI_STREAM_你好_🌸\n' + markdown
    thought = 'PROTOCOL_THINKING_你好_FIXTURE'
    fixture = Fixture(protocol, [{'text': response, 'pieces': ['TUI_STREAM_', '你好_', '🌸', '\n' + markdown],
                                 'thinking': [thought]}])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-stream-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture)
            config.write_text(config.read_text() + 'thinking_effort = "max"\n')
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                terminal.wait(lambda: 'max' in terminal.screen.main_row_text(terminal.screen.rows - 1),
                              'configured thinking effort in the footer')
                if termios.tcgetattr(terminal.slave)[3] & termios.ICANON:
                    raise RuntimeError('TUI did not enable raw mode')
                expected_input = 'first 你好🌸\nsecond\nthird 粘贴'
                terminal.send('first 你好🌸')
                terminal.send(b'\x0a')  # Ctrl+J inserts a newline without sending.
                terminal.send('\x1b[200~second\nthird 粘贴\x1b[201~')
                terminal.send(b'\r')
                terminal.until('TUI_STREAM_你好_🌸')
                result = wait_idle(terminal, root)
                if result.get('text') != response:
                    raise RuntimeError('Markdown presentation changed the persisted model response')
                terminal.settle()
                headers = terminal.screen.fold_headers('Thinking')
                if len(headers) != 1 or headers[0][2] != '▶ Thinking · done':
                    raise RuntimeError('completed thinking did not use its concise final-state header')
                if thought in terminal.screen.body_text():
                    raise RuntimeError('provider plaintext thinking was expanded by default')
                terminal.send(b'\x0f')
                terminal.wait(lambda: thought in terminal.screen.body_text(), 'protocol thinking expanded through Ctrl+O')
                if 'OPAQUE_SIGNATURE_FIXTURE' in terminal.screen.body_text() or 'OPAQUE_ENCRYPTED_FIXTURE' in terminal.screen.body_text():
                    raise RuntimeError('opaque provider metadata appeared in expanded thinking')
                terminal.send(b'\x0f')
                terminal.wait(lambda: thought not in terminal.screen.body_text(), 'protocol thinking collapsed through Ctrl+O')
                terminal.settle()
                terminal.resize(48, 16)
                terminal.title()
                terminal.resize(110, 32)
                terminal.until('TUI_STREAM_你好_🌸')
                terminal.wait(lambda: 'max' in terminal.screen.main_row_text(terminal.screen.rows - 1),
                              'thinking effort footer after resize')
                for text, foreground, background in [
                        ('Heading', (187, 154, 247), (9, 9, 11)),
                        ('strong', (246, 173, 85), (9, 9, 11)),
                        ('emphasis', (246, 173, 85), (9, 9, 11)),
                        ('inline', (125, 203, 155), (23, 23, 27)),
                        ('- item', (246, 173, 85), (9, 9, 11)),
                        ('quote', (129, 129, 139), (9, 9, 11)),
                        ('╭─ rust', (48, 48, 56), (23, 23, 27))]:
                    if terminal.screen.body_style(text) != (foreground, background):
                        raise RuntimeError('Markdown style was missing or incorrect for ' + text)
                code = 'let x = **literal**;'
                code_style = terminal.screen.body_style(code)
                if code_style is None or code_style[1] != (23, 23, 27) or code_style[0] == (196, 196, 204):
                    raise RuntimeError('Rust code was missing its keyword highlight or code background')
                visible = terminal.screen.body_text()
                if '## Heading' in visible or '**strong**' in visible or '`inline`' in visible:
                    raise RuntimeError('complete Markdown delimiters remained in the styled assistant body')
                if 'https://example.test' not in visible or '<b>literal html</b>' not in visible or '╰─' not in visible:
                    raise RuntimeError('Markdown changed literal URLs, HTML or the closing code card')
                snapshot = terminal.screen.text
                terminal.finish()
                if len(fixture.requests) != 1 or fixture.requests[0].get('stream') is not True:
                    raise RuntimeError('TUI stream did not use one streamed model request')
                request = fixture.requests[0]
                if protocol == 'openai-chat-completions':
                    effort = request.get('reasoning_effort')
                elif protocol == 'openai-responses':
                    effort = request.get('reasoning', {}).get('effort')
                else:
                    effort = request.get('output_config', {}).get('effort')
                if effort != 'max':
                    raise RuntimeError('configured thinking effort did not reach the model protocol')
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



def markdown_table_links_journey(binary):
    pieces = [
        '## MD_START\n\n**ASTERISK_BOLD** and __UNDERSCORE_BOLD__\n\n| Name | State | Details |\n| :--- |',
        ' :---: | ---: |\n| CJK | **Ready** | 中文 |\n| Link | [Guide](https://e.test) | docs |\n',
        ''.join(f'| row{row} | ok | value{row} |\n' for row in range(8)) + '\nMD_END',
    ]
    response = ''.join(pieces)
    fixture = Fixture('openai-chat-completions', [
        {'text': response, 'pieces': pieces, 'stream_delay': 0.02}])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-markdown-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                terminal.resize(80, 48)
                terminal.title()
                terminal.line('Show the controlled Markdown table')
                terminal.until('MD_END')
                result = wait_idle(terminal, root)
                terminal.settle()
                if result.get('text') != response:
                    raise RuntimeError('Markdown table presentation changed the saved response source')
                visible = terminal.screen.body_text()
                for text in ('MD_START', 'ASTERISK_BOLD', 'UNDERSCORE_BOLD', '中文',
                             'Guide (https://e.test)', 'row7', 'value7'):
                    if text not in visible:
                        raise RuntimeError('Markdown table content was missing at 80 columns: ' + text)
                if not all(marker in visible for marker in ('┌', '├', '└', '│')):
                    raise RuntimeError('Markdown table was not rendered with cell borders')
                if any(marker in visible for marker in ('**ASTERISK_BOLD**', '__UNDERSCORE_BOLD__',
                                                        '[Guide](', ':---')):
                    raise RuntimeError('completed Markdown delimiters remained in the formatted table')
                for text, color in [('ASTERISK_BOLD', (246, 173, 85)),
                                    ('UNDERSCORE_BOLD', (246, 173, 85)),
                                    ('Guide', (88, 166, 255))]:
                    style = terminal.screen.body_style(text)
                    if style is None or style[0] != color:
                        raise RuntimeError('Markdown emphasis or link color was missing: ' + text)

                terminal.resize(40, 48)
                terminal.title()
                terminal.wait(lambda: 'MD_END' in terminal.screen.body_text() and
                              'row7' in terminal.screen.body_text(), 'reflowed 40-column Markdown table')
                terminal.settle()
                if ':---' in terminal.screen.body_text():
                    raise RuntimeError('table separator became literal after a narrower resize')

                terminal.resize(20, 48)
                terminal.title()
                terminal.wait(lambda: 'MD_END' in terminal.screen.body_text(), 'narrow Markdown table end')
                terminal.settle()
                bottom = terminal.screen.body_text()
                for _ in range(40):
                    if 'MD_START' in terminal.screen.body_text():
                        break
                    terminal.wheel(5, 8, down=False)
                    terminal.settle()
                if 'MD_START' not in terminal.screen.body_text() or terminal.screen.body_text() == bottom:
                    raise RuntimeError('mouse scrolling did not reach the beginning of the narrow table')
                for _ in range(40):
                    if 'MD_END' in terminal.screen.body_text():
                        break
                    terminal.wheel(5, 8)
                    terminal.settle()
                if 'MD_END' not in terminal.screen.body_text():
                    raise RuntimeError('mouse scrolling did not return to the end of the narrow table')

                terminal.resize(140, 48)
                terminal.title()
                terminal.wait(lambda: 'Guide (https://e.test)' in terminal.screen.body_text() and
                              'MD_END' in terminal.screen.body_text(), 'wide Markdown table after sidebar reflow')
                terminal.finish()
                if len(fixture.requests) != 1 or fixture.requests[0].get('stream') is not True:
                    raise RuntimeError('Markdown resize or scrolling replayed a model request')
            finally:
                terminal.close()
    finally:
        fixture.close()


def markdown_source_and_footnotes_journey(binary):
    response = (
        'Markdown controls\n=================\n\nREFERENCE_NOTE[^support].\n\n'
        '***both*** and ~~removed~~ and $x^2$.\n\n'
        '- [x] task done\n  - nested item\n\n> quoted text\n\n'
        '<small>source HTML</small>\n\n![sample](https://example.test/sample.png)\n\n---\n\n'
        + ''.join(f'FILLER_{index:02d}\n\n' for index in range(32))
        + '[^support]: TARGET_NOTE with a return reference.\n')
    fixture = Fixture('openai-chat-completions', [{'text': response}])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-markdown-source-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                def reveal(marker):
                    for _ in range(80):
                        if marker in terminal.screen.body_text():
                            terminal.settle()
                            return
                        terminal.wheel(5, 8, down=False)
                        terminal.settle()
                    raise RuntimeError('Markdown scroll did not reveal ' + marker)

                def note_point(marker):
                    terminal.settle()
                    for row in range(1, terminal.screen.rows - 6):
                        text = terminal.screen.main_row_text(row)
                        if marker in text and '[1]' in text:
                            return text.index('[1]'), row
                    raise RuntimeError('semantic footnote marker was not visible next to ' + marker)

                terminal.resize(80, 48)
                terminal.title()
                terminal.line('Show Markdown source controls and the numbered footnote')
                terminal.until('TARGET_NOTE')
                saved = wait_idle(terminal, root)
                if saved.get('text') != response:
                    raise RuntimeError('Markdown rendering changed the saved answer source')
                reveal('REFERENCE_NOTE')
                terminal.click(*note_point('REFERENCE_NOTE'))
                terminal.until('TARGET_NOTE')
                terminal.click(*note_point('TARGET_NOTE'))
                terminal.until('REFERENCE_NOTE')

                terminal.resize(40, 48)
                terminal.title()
                reveal('REFERENCE_NOTE')
                terminal.click(*note_point('REFERENCE_NOTE'))
                terminal.until('TARGET_NOTE')
                terminal.line('/footnote back')
                terminal.until('REFERENCE_NOTE')

                terminal.resize(80, 48)
                terminal.title()
                terminal.line('/markdown source')
                reveal('=================')
                source = terminal.screen.body_text()
                for marker in ('REFERENCE_NOTE[^support]', '***both***', '~~removed~~', '$x^2$',
                               '<small>source HTML</small>', '![sample]('):
                    if marker not in source:
                        raise RuntimeError('source presentation lost literal Markdown ' + marker)
                terminal.line('/markdown render')
                reveal('REFERENCE_NOTE')
                terminal.settle()
                rendered = terminal.screen.body_text()
                if '***both***' in rendered or '~~removed~~' in rendered or 'x²' not in rendered:
                    raise RuntimeError('render presentation did not restore Markdown and math styling')
                terminal.line('/footnote 1')
                terminal.until('TARGET_NOTE')
                terminal.line('/footnote back')
                terminal.until('REFERENCE_NOTE')
                if len(fixture.requests) != 1:
                    raise RuntimeError('view settings or footnote navigation submitted another model request')
                if wait_idle(terminal, root).get('text') != response:
                    raise RuntimeError('view commands replaced the original persisted answer')
                terminal.finish()
            finally:
                terminal.close()
    finally:
        fixture.close()


def command_completion_journey(binary):
    call = ('completion-read', 'read_file', {'path': 'fact.txt', 'start_line': 1, 'line_count': 1})
    def denied(request):
        result = tool_result(request, call[0])
        if result.get('ok') or result.get('error', {}).get('code') != 'permission_denied':
            raise RuntimeError('second Esc did not reject the pending tool approval')
        return {'text': 'COMPLETION_APPROVAL_DONE'}
    fixture = Fixture('openai-chat-completions', [
        {'text': 'COMPLETION_MULTILINE_DONE'}, {'text': 'COMPLETION_LITERAL_DONE'},
        {'call': call}, denied])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-completion-') as directory:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            (workspace / 'fact.txt').write_text('completion fixture\n')
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                editor_text = terminal.editor_text
                mode = terminal.mode
                permissions = terminal.permissions

                def no_completion_effects(expected_mode, expected_permissions):
                    if mode() != expected_mode or permissions() != expected_permissions:
                        raise RuntimeError('completion changed the active mode or permissions before Enter')
                    if fixture.requests or (root / 'data').exists() or (root / 'state').exists():
                        raise RuntimeError('completion invoked a model or accessed persistent session storage')
                    if list(workspace.iterdir()) != [workspace / 'fact.txt']:
                        raise RuntimeError('completion changed workspace files')

                def clear_draft():
                    terminal.send(b'\x03')
                    terminal.wait(lambda: editor_text() == '' and 'Command suggestions' not in terminal.screen.popup_text(),
                                  'empty draft after Ctrl+C')

                terminal.title()
                terminal.send('/')
                terminal.wait(lambda: bool(terminal.screen.popup_text()), 'command suggestions popup')
                terminal.send('mo')
                terminal.wait(lambda: '/mode plan' in terminal.screen.popup_text(), 'mode prefix suggestions')
                terminal.send(b'\t')
                terminal.wait(lambda: editor_text() == '/mode plan', 'Tab completed mode draft')
                no_completion_effects('WORK', 'auto')
                terminal.send(b'\r')
                terminal.wait(lambda: mode() == 'PLAN' and editor_text() == '', 'explicit mode command execution')

                terminal.send('/permissions ')
                terminal.wait(lambda: bool(terminal.screen.popup_text()), 'command suggestions popup')
                terminal.send(b'\x1b[B')
                terminal.wait(lambda: any('› /permissions manual' in row for row in terminal.screen.popup_text().splitlines()),
                              'highlighted permissions suggestion')
                terminal.send(b'\t')
                terminal.wait(lambda: editor_text() == '/permissions manual', 'selected permissions completion')
                no_completion_effects('PLAN', 'auto')
                terminal.send(b'\r')
                terminal.wait(lambda: permissions() == 'Manual Approval' and editor_text() == '',
                              'explicit permissions command execution')

                terminal.send('/mo')
                terminal.wait(lambda: bool(terminal.screen.popup_text()), 'command suggestions popup')
                terminal.send(b'\x1b')
                terminal.wait(lambda: editor_text() == '/mo' and 'Command suggestions' not in terminal.screen.popup_text(),
                              'Esc hid suggestions and ghost while preserving draft')
                terminal.send(b'\x10')
                terminal.wait(lambda: bool(terminal.screen.command_palette_rect()), 'centered command palette frame')
                terminal.send(b'\x1b')
                terminal.wait(lambda: editor_text() == '/mo' and not terminal.screen.command_palette_rect(),
                              'existing command palette closed with draft intact')
                clear_draft()

                terminal.send('/resume-a')
                terminal.wait(lambda: bool(terminal.screen.popup_text()), 'command suggestions popup')
                terminal.send(b'\t')
                terminal.send('NOT_A_SESSION_ID')
                terminal.wait(lambda: editor_text() == '/resume-accept NOT_A_SESSION_ID',
                              'scaffold completion retained its trailing argument space')
                no_completion_effects('PLAN', 'Manual Approval')
                clear_draft()

                terminal.send('/mode wo')
                terminal.wait(lambda: bool(terminal.screen.popup_text()), 'command suggestions popup')
                terminal.send(b'\x1b[C')
                terminal.send(' ')
                terminal.wait(lambda: editor_text() == '/mode work' and 'Command suggestions' not in terminal.screen.popup_text(),
                              'Right accepted actual draft before additional typing')
                no_completion_effects('PLAN', 'Manual Approval')
                clear_draft()

                terminal.send('普通中文')
                terminal.send(b'\x0a')
                terminal.send('第二行')
                terminal.wait(lambda: editor_text() == '普通中文\n第二行' and
                              'Command suggestions' not in terminal.screen.popup_text(), 'ordinary multiline draft without completion')
                no_completion_effects('PLAN', 'Manual Approval')
                terminal.send(b'\r')
                terminal.until('COMPLETION_MULTILINE_DONE')
                wait_idle(terminal, root)
                if not any(message.get('role') == 'user' and message.get('content') == '普通中文\n第二行'
                           for message in fixture.requests[0]['messages']):
                    raise RuntimeError('ordinary multiline input was changed by command completion')

                terminal.send('//literal')
                terminal.wait(lambda: editor_text() == '//literal' and 'Command suggestions' not in terminal.screen.popup_text(),
                              'escaped slash draft without completion')
                terminal.send(b'\r')
                terminal.until('COMPLETION_LITERAL_DONE')
                wait_idle(terminal, root, count=2)
                if not any(message.get('role') == 'user' and message.get('content') == '/literal'
                           for message in fixture.requests[1]['messages']):
                    raise RuntimeError('escaped slash input was executed as a command')

                terminal.line('Request the manual read fixture')
                terminal.wait(lambda: call[0] in terminal.screen.details_text() and 'Approval required' in terminal.screen.details_text(),
                              'pending manual approval')
                terminal.send('/mo')
                terminal.wait(lambda: bool(terminal.screen.popup_text()), 'command suggestions popup')
                terminal.send(b'\x1b')
                terminal.wait(lambda: editor_text() == '/mo' and 'Command suggestions' not in terminal.screen.popup_text()
                              and 'Approval required' in terminal.screen.details_text(), 'first Esc preserved tool approval')
                if len(fixture.requests) != 3:
                    raise RuntimeError('first completion Esc replied to a pending tool approval')
                terminal.send(b'\x1b')
                terminal.until('COMPLETION_APPROVAL_DONE')
                wait_idle(terminal, root, count=3)
                if len(fixture.requests) != 4:
                    raise RuntimeError('completion approval interaction duplicated model requests')
                clear_draft()
                terminal.finish()
            finally:
                terminal.close()
    finally:
        fixture.close()


def empty_tab_mode_journey(binary):
    active_call = ('tab-active-read', 'read_file', {'path': 'fact.txt', 'start_line': 1, 'line_count': 1})
    manual_call = ('tab-manual-read', 'read_file', {'path': 'fact.txt', 'start_line': 1, 'line_count': 1})
    def stale_manual(request):
        result = tool_result(request, manual_call[0])
        if result.get('ok') or result.get('error', {}).get('code') != 'stale_approval':
            raise RuntimeError('empty Tab approved a tool or failed to invalidate its previous mode approval')
        return {'text': 'TabManualDone'}
    fixture = Fixture('openai-chat-completions', [
        {'call': active_call, 'delay': 3}, expect_tool(active_call[0], {'text': 'TabActiveDone'}),
        {'call': manual_call}, stale_manual])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-empty-tab-') as directory:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            (workspace / 'fact.txt').write_text('tab fixture\n')
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                def no_dispatch(expected_mode):
                    if terminal.mode() != expected_mode or fixture.requests:
                        raise RuntimeError('Tab unexpectedly changed the mode or dispatched a model request')
                    if (root / 'data').exists() or (root / 'state').exists():
                        raise RuntimeError('idle Tab initialized persistent session storage')
                    if list(workspace.iterdir()) != [workspace / 'fact.txt']:
                        raise RuntimeError('idle Tab changed workspace files')
                    assert_mode_colors(terminal, expected_mode)

                def clear_draft():
                    terminal.send(b'\x03')
                    terminal.wait(lambda: terminal.editor_text() == '', 'empty Tab fixture draft cleared')

                terminal.title()
                terminal.settle()
                input_row = terminal.screen.message_row() + 1
                if terminal.screen.row_style(input_row, EDITOR_PLACEHOLDER, 3,
                                             terminal.screen.main_width() - 1) != ((129, 129, 139), (32, 32, 36)):
                    raise RuntimeError('fresh empty draft did not display its muted placeholder')
                if terminal.editor_text() != '':
                    raise RuntimeError('placeholder text was treated as user input')
                terminal.send(EDITOR_PLACEHOLDER)
                terminal.wait(lambda: terminal.editor_text() == EDITOR_PLACEHOLDER, 'literal placeholder text retained as a user draft')
                terminal.settle()
                if terminal.screen.row_style(input_row, EDITOR_PLACEHOLDER, 3,
                                             terminal.screen.main_width() - 1) != ((212, 212, 216), (32, 32, 36)):
                    raise RuntimeError('literal placeholder draft did not use the user-input foreground')
                terminal.send(b'\t')
                terminal.send('X')
                terminal.wait(lambda: terminal.editor_text() == EDITOR_PLACEHOLDER + '    X',
                              'Tab edits a literal placeholder draft without cycling mode')
                no_dispatch('WORK')
                clear_draft()

                terminal.send('\x1b[200~' + 'x' * 66000 + '\x1b[201~')
                terminal.wait(lambda: terminal.screen.message_row() is not None and
                              'Message · paste truncated at 64 KiB' in terminal.screen.main_row_text(
                                  terminal.screen.message_row()), 'paste limit warning in the composer caption')
                terminal.settle()
                caption = terminal.screen.message_row()
                if terminal.screen.row_style(caption, 'Message · paste truncated', 3,
                                             terminal.screen.main_width() - 1) != ((246, 173, 85), (32, 32, 36)):
                    raise RuntimeError('paste limit warning did not use its warning hierarchy')
                if 'Enter send' in terminal.screen.main_row_text(caption):
                    raise RuntimeError('composer hint displaced the paste truncation warning')
                no_dispatch('WORK')
                clear_draft()

                for mode in ['CODE', 'PLAN', 'BUILD', 'WORK']:
                    terminal.send(b'\t')
                    terminal.wait(lambda: terminal.mode() == mode and terminal.editor_text() == '',
                                  'idle empty Tab mode ' + mode)
                    no_dispatch(mode)
                    save_interactive_screen(terminal, mode.lower() + '-110x32')

                terminal.send(b'\x10')
                terminal.wait(lambda: bool(terminal.screen.command_palette_rect()), 'Tab fixture command palette')
                terminal.send(b'\t')
                no_dispatch('WORK')
                terminal.send(b'\x1b')
                terminal.wait(lambda: not terminal.screen.command_palette_rect(), 'Tab fixture command palette closed')
                no_dispatch('WORK')
                clear_draft()

                for before, after in [('tab 中文', 'X'), (' ', 'space'), ('\n', 'multi')]:
                    terminal.send(before if before != '\n' else b'\x0a')
                    terminal.send(b'\t')
                    terminal.send(after)
                    expected = before + '    ' + after
                    terminal.wait(lambda: terminal.editor_text() == expected, 'nonempty draft retained its Tab editing behavior')
                    no_dispatch('WORK')
                    clear_draft()

                terminal.send('/mo')
                terminal.wait(lambda: bool(terminal.screen.popup_text()), 'Tab fixture slash suggestions')
                terminal.send(b'\t')
                terminal.send(' ')
                terminal.wait(lambda: terminal.editor_text() == '/mode plan' and not terminal.screen.popup_text(),
                              'nonempty slash Tab completed without changing mode')
                no_dispatch('WORK')
                clear_draft()

                terminal.line('Start the active Tab mode fixture')
                terminal.wait(lambda: len(fixture.requests) == 1 and terminal.screen.status() == 'Thinking',
                              'model request before active empty Tab')
                terminal.send(b'\t')
                terminal.wait(lambda: 'WORK→CODE' in terminal.screen.main_row_text(terminal.screen.rows - 1),
                              'active Tab pending mode in the main footer')
                if terminal.mode() != 'WORK' or len(fixture.requests) != 1:
                    raise RuntimeError('active empty Tab interrupted the current model call')
                assert_mode_colors(terminal, 'WORK', pending='CODE')
                save_interactive_screen(terminal, 'pending-work-code-110x32')
                terminal.until('TabActiveDone')
                wait_idle(terminal, root)
                if terminal.mode() != 'CODE' or session_data(binary, root, config)[0]['work_mode'].lower() != 'code':
                    raise RuntimeError('active Tab did not apply and persist its mode at the next call boundary')
                assert_mode_colors(terminal, 'CODE')

                terminal.line('/permissions manual')
                terminal.wait(lambda: terminal.permissions() == 'Manual Approval', 'Tab fixture manual permissions')
                terminal.line('Request the manual Tab mode fixture')
                terminal.wait(lambda: manual_call[0] in terminal.screen.details_text() and
                              terminal.screen.status() == 'Awaiting approval', 'manual approval before draft Tab')
                assert_mode_colors(terminal, 'CODE')
                if terminal.screen.colors[terminal.screen.status_row()][1] != ((246, 173, 85), (9, 9, 11)):
                    raise RuntimeError('mode color replaced the independent approval status color')
                terminal.send('/mo')
                terminal.wait(lambda: bool(terminal.screen.popup_text()), 'manual draft Tab suggestions')
                terminal.send(b'\t')
                terminal.send(' ')
                terminal.wait(lambda: terminal.editor_text() == '/mode plan' and not terminal.screen.popup_text(),
                              'manual draft Tab completion')
                if terminal.mode() != 'CODE' or terminal.screen.status() != 'Awaiting approval' or len(fixture.requests) != 3:
                    raise RuntimeError('nonempty approval Tab executed or approved the pending operation')
                terminal.send(b'\x7f' * len('/mode plan '))
                terminal.wait(lambda: terminal.editor_text() == '', 'manual Tab draft emptied')
                terminal.send(b'\t')
                terminal.until('TabManualDone')
                wait_idle(terminal, root, count=2)
                if terminal.mode() != 'PLAN' or len(fixture.requests) != 4:
                    raise RuntimeError('empty approval Tab did not complete the explicit mode transition')
                if session_data(binary, root, config)[0]['work_mode'].lower() != 'plan':
                    raise RuntimeError('empty approval Tab mode was not persisted')
                assert_mode_colors(terminal, 'PLAN')
                terminal.finish()
            finally:
                terminal.close()
    finally:
        fixture.close()


def reference_layout_and_title_journey(binary):
    first_prompt = '整理 Saya 验收🌸'
    approval_prompt = '核对文件\n只读审批'
    tiny_prompt = '窄屏中文'
    malicious_prompt = '  项目\u202e\x1b]2;FORGED\x07 标题🌸\t第一项任务 ' + '长中文' * 60
    call = ('compact-read', 'read_file', {'path': 'fact.txt', 'start_line': 1, 'line_count': 1})
    fixture = Fixture('openai-chat-completions', [
        {'text': 'MODEL_BODY | PLAN | Manual Approval | Completed | input 99% | Context · Estimated | Skills 999 discovered',
         'chat_usage': {'prompt_tokens': 64000, 'completion_tokens': 5}},
        {'call': call, 'chat_usage': {'prompt_tokens': 1024, 'completion_tokens': 5}},
        expect_tool(call[0], {'text': 'COMPACT_APPROVED',
                            'chat_usage': {'prompt_tokens': 512, 'completion_tokens': 5}}),
        {'text': 'COMPACT_TINY_DONE', 'chat_usage': {'prompt_tokens': 12800, 'completion_tokens': 5}},
        {'text': 'COMPACT_SAFE_TITLE_DONE'}])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-compact-') as directory:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            (workspace / 'fact.txt').write_text('reference fixture\n')
            (workspace / 'AGENTS.md').write_text('Use the reference UI fixture instructions and preserve fact.txt.\n')
            skill = workspace / '.saya' / 'skills' / 'example'
            skill.mkdir(parents=True)
            (skill / 'SKILL.md').write_text('---\nname: example\ndescription: Offline reference UI acceptance fixture.\n---\nRead fact.txt when requested.\n')
            config = configuration(root, fixture)
            config.write_text(config.read_text() + 'thinking_effort = "max"\n[plugins]\nenabled = true\n')
            terminal = invoke(binary, root, config)
            try:
                def title():
                    return terminal.screen.window_titles[-1] if terminal.screen.window_titles else None

                def footer():
                    return terminal.screen.main_row_text(terminal.screen.rows - 1)

                def reference_ready(columns, rows, expected_message_row):
                    terminal.resize(columns, rows)
                    terminal.title()
                    sidebar_width = min(40, max(28, columns // 4)) if columns >= 120 and rows >= 24 else 0
                    terminal.wait(lambda: terminal.screen.message_row() == expected_message_row and
                                  terminal.screen.main_width() == columns - sidebar_width and
                                  terminal.screen.status() is not None and terminal.mode() is not None and
                                  'input' in footer(),
                                  'reference Message region at its fixed row')
                    terminal.settle()
                    screen = terminal.screen
                    if screen.colors[0][0][1] != (9, 9, 11):
                        raise RuntimeError('reference header did not retain the main background')
                    if screen.row_style(0, 'Saya Harness', 0, screen.main_width()) != ((232, 232, 237), (9, 9, 11)):
                        raise RuntimeError('reference brand did not use its stronger foreground hierarchy')
                    if not screen.main_row_text(1).startswith('┌') or '/' not in screen.main_row_text(1):
                        raise RuntimeError('reference workspace was not embedded in the outer top border')
                    if 'Ctrl+P' not in screen.main_row_text(0):
                        raise RuntimeError('reference command hint was missing from the top row')
                    if screen.main_width() >= 44 and '(v' not in screen.main_row_text(0):
                        raise RuntimeError('reference header omitted the package version')
                    input_row = expected_message_row if expected_message_row is not None else 4
                    for index in range(input_row, rows - 1):
                        if screen.cells[index][1] != INPUT_STRIPE:
                            raise RuntimeError('reference input did not use its one-third mode stripe')
                        color = MODE_COLORS[terminal.mode()]
                        if screen.colors[index][1] != (color, (32, 32, 36)):
                            raise RuntimeError('reference input stripe did not use the actual mode color for its full height')
                        if screen.colors[index][screen.main_width() - 2][1] != (9, 9, 11):
                            raise RuntimeError('reference input omitted its dark right margin')
                    if screen.colors[input_row][3][1] != (32, 32, 36):
                        raise RuntimeError('reference Message region did not use its gray background')
                    if expected_message_row is not None:
                        inner_width = screen.main_width() - 5
                        hint = ('Enter send · Alt+Enter newline · Ctrl+O fold' if inner_width >= 64 else
                                'Enter send · Ctrl+O fold' if inner_width >= 38 else None)
                        message = screen.main_row_text(input_row)
                        if hint is not None and hint not in message:
                            raise RuntimeError('Message region omitted its width-appropriate input hint')
                        if hint is not None:
                            keys = ['Enter', 'Ctrl+O'] if inner_width < 64 else ['Enter', 'Alt+Enter', 'Ctrl+O']
                            for key in keys:
                                if screen.row_style(input_row, key, 3, screen.main_width() - 1) != ((232, 232, 237), (23, 23, 27)):
                                    raise RuntimeError('input hint did not use its keycap hierarchy for ' + key)
                            if screen.row_style(input_row, 'send', 3, screen.main_width() - 1) != ((129, 129, 139), (32, 32, 36)):
                                raise RuntimeError('input hint action did not use its secondary composer color')
                        if hint is None and 'Enter send' in message:
                            raise RuntimeError('input hint displaced the narrow Message label')
                        if screen.row_style(input_row, 'Message', 3, screen.main_width() - 1) != ((232, 232, 237), (32, 32, 36)):
                            raise RuntimeError('Message caption did not use its stronger foreground')
                        show_placeholder = (rows - 1 - input_row > 2 and screen.status() not in
                                            ('Awaiting input', 'NeedsInput', 'Awaiting approval', 'NeedsConfirmation'))
                        placeholder_style = screen.row_style(input_row + 1, EDITOR_PLACEHOLDER, 3, screen.main_width() - 1)
                        if show_placeholder and placeholder_style != ((129, 129, 139), (32, 32, 36)):
                            raise RuntimeError('empty composer omitted its style-distinct placeholder')
                        if not show_placeholder and placeholder_style is not None:
                            raise RuntimeError('placeholder occupied a tiny composer or pending human response')
                        if rows - 1 - input_row > 2 and screen.colors[rows - 2][3] != ((42, 42, 48), (32, 32, 36)):
                            raise RuntimeError('composer padding did not contain its subtle bottom edge')
                    if not any(ring in footer() for ring in ('○', '◔', '◑', '◕', '●')):
                        raise RuntimeError('reference footer omitted the labeled input ring')
                    if screen.main_width() >= 44:
                        terminal.wait(lambda: 'max' in footer(), 'reference configured thinking effort')
                        column = screen.main_row_text(rows - 1).index('max')
                        if screen.colors[rows - 1][column][0] != (246, 173, 85):
                            raise RuntimeError('reference thinking effort did not use its orange accent')
                    if sidebar_width:
                        terminal.wait(lambda: 'Context' in screen.sidebar_row_text(2) and
                                      'Window' in screen.sidebar_row_text(3) and
                                      'Extensions' in screen.sidebar_row_text(15),
                                      'sidebar fixed context and extension regions')
                        if screen.colors[0][screen.main_width() + 1][1] != (20, 20, 22):
                            raise RuntimeError('sidebar did not use its reference gray background')
                        if screen.sidebar_row_text(16) != 'MCP  No servers':
                            raise RuntimeError('sidebar did not report the empty MCP configuration')
                        if screen.sidebar_row_text(17) != 'Plugins  Enabled in config':
                            raise RuntimeError('sidebar did not report the configured plugin enablement')
                        if 'Skills' not in screen.sidebar_row_text(18):
                            raise RuntimeError('sidebar omitted its actual skill discovery state')
                        for row, label in [(2, 'Context'), (15, 'Extensions'), (20, 'Tasks')]:
                            if screen.row_style(row, label, screen.main_width()) != ((232, 232, 237), (20, 20, 22)):
                                raise RuntimeError('sidebar section did not retain its stronger heading color for ' + label)
                    elif any(screen.sidebar_row_text(index) for index in range(rows)):
                        raise RuntimeError('sidebar remained visible below its size threshold')

                def assert_user_input(request_index, expected):
                    if not any(message.get('role') == 'user' and message.get('content') == expected
                               for message in fixture.requests[request_index]['messages']):
                        raise RuntimeError('reference editor changed the submitted user text')

                def assert_sidebar_context(nonzero):
                    labels = ['System', 'Instructions', 'Skills', 'Preferences', 'Memory',
                              'Conversation', 'Files', 'Tools', 'Protocol']
                    populated = {}
                    terminal.wait(lambda: terminal.screen.sidebar_row_text(2) == 'Context · Estimated' and
                                  terminal.screen.sidebar_row_text(18) == 'Skills 1 discovered',
                                  'actual execution context estimates and discovered skill')
                    terminal.settle()
                    if terminal.screen.sidebar_row_text(0) != first_prompt:
                        raise RuntimeError('sidebar session title did not match the actual first task')
                    if terminal.screen.sidebar_row_text(3) != 'Window 128k':
                        raise RuntimeError('sidebar context window did not match the configured model')
                    if not terminal.screen.sidebar_row_text(13).startswith('Total ~'):
                        raise RuntimeError('sidebar did not label its total context as estimated')
                    for index, label in enumerate(labels, 4):
                        row = terminal.screen.sidebar_row_text(index)
                        if not row.startswith(label + ' '):
                            raise RuntimeError('sidebar omitted context category ' + label)
                        metric = re.search(r'\s([0-9]+(?:\.[0-9]+)?)[kMGT]?\s+[<>]?[0-9]+(?:\.[0-9]+)?%$', row)
                        if metric is None:
                            raise RuntimeError('sidebar did not display a bounded numeric estimate for ' + label)
                        populated[label] = float(metric.group(1)) > 0
                        token = metric.group(0).strip().split()[0]
                        foreground = (232, 232, 237) if populated[label] else (129, 129, 139)
                        if terminal.screen.row_style(index, token, terminal.screen.main_width()) != (foreground, (20, 20, 22)):
                            raise RuntimeError('sidebar numeric hierarchy did not match the actual value for ' + label)
                        if label in nonzero and not populated[label]:
                            raise RuntimeError('sidebar did not estimate the actual ' + label + ' context')
                    return populated

                terminal.title()
                terminal.wait(lambda: title() == 'Saya | New session', 'initial window title')
                for columns, rows, message_row in [
                        (110, 32, 27), (120, 40, 35),
                        (119, 24, 19), (120, 24, 19), (119, 24, 19),
                        (120, 23, 18), (120, 24, 19), (120, 23, 18),
                        (99, 24, 19), (100, 24, 19), (99, 24, 19),
                        (100, 23, 18), (100, 24, 19), (100, 23, 18),
                        (80, 24, 19), (60, 18, 13), (20, 6, 3),
                        (160, 45, 40), (120, 40, 35), (110, 32, 27), (80, 24, 19)]:
                    reference_ready(columns, rows, message_row)
                    if terminal.mode() != 'WORK' or terminal.permissions() != 'auto' or terminal.screen.status() != 'Ready':
                        raise RuntimeError('resizing changed the reference control state')
                    if terminal.screen.main_width() < columns and 'Unknown' not in terminal.screen.sidebar_row_text(2):
                        raise RuntimeError('sidebar invented context estimates before a model request')
                    if terminal.screen.main_width() < columns and terminal.screen.sidebar_row_text(18) != 'Skills —':
                        raise RuntimeError('sidebar invented skill discovery before the first request')
                save_interactive_screen(terminal, 'idle-composer-80x24')
                if fixture.requests:
                    raise RuntimeError('reference layout resizing called the model')
                if terminal.mode() != 'WORK' or terminal.permissions() != 'auto' or terminal.screen.status() != 'Ready':
                    raise RuntimeError('reference initial control state was not displayed in fixed regions')
                terminal.wait(lambda: 'input —/128k (—)' in footer(), 'unknown initial reference input usage')
                terminal.line(first_prompt)
                wait_idle(terminal, root)
                terminal.wait(lambda: title() == 'Saya | ' + first_prompt, 'first-task window title')
                assert_user_input(0, first_prompt)
                if fixture.requests[0].get('reasoning_effort') != 'max':
                    raise RuntimeError('reference configured thinking effort was not sent to the model')
                if terminal.mode() != 'WORK' or terminal.permissions() != 'auto':
                    raise RuntimeError('model body text spoofed the reference control observer')
                terminal.wait(lambda: '◑ input 64k/128k (50%)' in footer(), 'execution input usage in reference meter')
                identifier = session_data(binary, root, config)[0]['id']
                reference_ready(120, 40, 35)
                if assert_sidebar_context({'Instructions', 'Skills'})['Files']:
                    raise RuntimeError('sidebar invented file output context before a file tool result')

                reference_ready(60, 18, 13)
                terminal.send('/mo')
                terminal.wait(lambda: 'Command suggestions' in terminal.screen.popup_text(),
                              'reference completion popup')
                terminal.send(b'\t')
                terminal.send(' ')
                terminal.wait(lambda: terminal.editor_text() == '/mode plan' and
                              'Command suggestions' not in terminal.screen.popup_text(), 'reference accepted command draft')
                if terminal.mode() != 'WORK' or len(fixture.requests) != 1:
                    raise RuntimeError('reference completion executed before Enter')
                terminal.send(b'\r')
                terminal.wait(lambda: terminal.mode() == 'PLAN' and terminal.editor_text() == '',
                              'reference explicit mode change')
                terminal.line('/permissions manual')
                terminal.wait(lambda: terminal.permissions() == 'Manual Approval' and terminal.editor_text() == '',
                              'reference explicit permissions change')
                terminal.send('核对文件')
                terminal.send(b'\x0a')
                terminal.send('只读审批')
                terminal.wait(lambda: terminal.editor_text() == approval_prompt, 'reference multiline editor draft')
                terminal.send(b'\r')
                terminal.wait(lambda: terminal.screen.status() == 'Awaiting approval' and
                              call[0] in terminal.screen.details_text(), 'reference manual approval')
                assert_user_input(1, approval_prompt)
                if terminal.permissions() != 'Manual Approval' or len(fixture.requests) != 2:
                    raise RuntimeError('reference approval did not pause the tool request')
                for columns, rows, message_row in [
                        (99, 24, 19), (100, 24, 19), (99, 24, 19),
                        (110, 32, 27), (119, 24, 19), (120, 24, 19),
                        (120, 23, 18), (120, 40, 35), (160, 45, 40), (80, 24, 19)]:
                    reference_ready(columns, rows, message_row)
                    terminal.wait(lambda: 'Approval required' in terminal.screen.details_text() and
                                  call[0] in terminal.screen.details_text(), 'reference approval in its actual details region')
                    if terminal.screen.status() != 'Awaiting approval' or len(fixture.requests) != 2:
                        raise RuntimeError('resizing replied to a pending reference tool approval')
                reference_ready(20, 6, None)
                reveal_details(terminal, lambda: call[0] in terminal.screen.details_text(), 'tiny approval call ID')
                for _ in range(10):
                    terminal.send(b'\x1b[5~')
                terminal.wait(lambda: 'Tool read_file' in terminal.screen.details_text() and
                              terminal.screen.status() == 'Awaiting approval', 'tiny approval tool metadata')
                reference_ready(80, 24, 19)
                terminal.send('y')
                wait_idle(terminal, root, count=2)
                terminal.wait(lambda: '○ input 512/128k (0%)' in footer(), 'latest execution input usage without accumulation')
                if title() != 'Saya | ' + first_prompt:
                    raise RuntimeError('a later task replaced the first-task window title')
                reference_ready(120, 40, 35)
                assert_sidebar_context({'Instructions', 'Skills', 'Files'})

                reference_ready(20, 6, 3)
                if terminal.mode() != 'PLAN' or terminal.permissions() != 'Manual Approval' or terminal.screen.status() != 'Completed':
                    raise RuntimeError('tiny reference state did not preserve mode, permissions and completion')
                terminal.send(tiny_prompt)
                terminal.wait(lambda: terminal.editor_text() == tiny_prompt, 'tiny reference Unicode editor')
                terminal.send(b'\r')
                wait_idle(terminal, root, count=3)
                assert_user_input(3, tiny_prompt)
                terminal.wait(lambda: 'input 10%' in footer(), 'labeled tiny reference input percentage')

                reference_ready(80, 24, 19)
                terminal.line('/new')
                terminal.wait(lambda: title() == 'Saya | New session' and terminal.screen.status() == 'Ready',
                              'new session title reset')
                terminal.wait(lambda: 'input —/128k (—)' in footer(), 'new session input meter reset')
                title_count = len(terminal.screen.window_titles)
                terminal.send('\x1b[200~' + malicious_prompt + '\x1b[201~')
                terminal.send(b'\r')
                wait_idle(terminal, root, count=4)
                assert_user_input(4, malicious_prompt)
                titles = terminal.screen.window_titles[title_count:]
                if len(titles) != 1 or not titles[0].startswith('Saya | 项目]2;FORGED 标题🌸 第一项任务 '):
                    raise RuntimeError('hostile task text injected a second OSC or changed its sanitized title prefix')
                if len(titles[0]) > 87 or len(titles[0].encode()) > 512:
                    raise RuntimeError('long first-task window title exceeded its limit')
                if any(unicodedata.category(character) == 'Cc' or character in '\u061c\u200e\u200f\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069'
                       for character in titles[0]):
                    raise RuntimeError('window title retained terminal controls or bidi overrides')
                terminal.line('/resume ' + identifier)
                terminal.wait(lambda: title() == 'Saya | ' + first_prompt and terminal.screen.status() == 'Ready',
                              'resumed first-task window title')
                if len(fixture.requests) != 5:
                    raise RuntimeError('title changes or session browsing made an extra model request')
                terminal.finish()
                if terminal.screen.title_stack != ['push', 'pop']:
                    raise RuntimeError('window title was not saved and restored through the terminal title stack')
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
    status = outcome[0].get('status')
    if expected_status and status != expected_status:
        raise RuntimeError('unexpected persisted run status: ' + json.dumps(outcome[0]))
    ui_status = ''.join(part.capitalize() for part in status.split('_'))
    # Persistence finishes before App.finish consumes the run outcome. Only the
    # final state region proves that the UI has released its active run.
    def ui_finished():
        return terminal.screen.status() == ui_status
    terminal.wait(ui_finished, 'idle UI status ' + ui_status)
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
                        label = {'auto': 'auto', 'manual': 'Manual Approval', 'bypass': 'bypass permissions'}[permission]
                        terminal.wait(lambda: terminal.permissions() == label, 'configured permission label before model input')
                        assert_mode_colors(terminal, mode.upper())
                        terminal.settle()
                        foreground = (129, 129, 139) if permission == 'auto' else (187, 166, 133)
                        if terminal.screen.row_style(terminal.screen.status_row(), label, 1,
                                                     terminal.screen.main_width() - 1) != (foreground, (9, 9, 11)):
                            raise RuntimeError('permission label did not use its actual-mode color hierarchy')
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
        text = terminal.screen.details_text()
        lower = text.lower()
        marker = 'background-' if call_id == 'manual-start' else call_id
        return marker in text and ('approval required' in lower or 'approve' in lower or 'y/n' in lower or 'allow' in lower)
    terminal.wait(visible, 'approval for ' + call_id)
    if len(fixture.requests) != expected_requests:
        raise RuntimeError('model continued before approval for ' + call_id)
    terminal.send('y')


def manual_tools_and_clarification(binary):
    reasoning_parts = ['OPAQUE_REASONING_FIXTURE_', 'retained across tools']
    reasoning = ''.join(reasoning_parts)
    read = ('manual-read', 'read_file', {'path': 'fact.txt', 'start_line': 1, 'line_count': 2})
    search = ('manual-search', 'search_text', {'path': '.', 'pattern': 'TUI_PRIVATE_FIXTURE'})
    start = ('manual-start', 'background_start', {'tool': 'read_file',
              'arguments': {'path': 'fact.txt', 'start_line': 1, 'line_count': 2}})
    ask = ('manual-question', 'ask_user', {'question': 'TUI_QUESTION', 'options': ['first', 'second']})
    def assert_reasoning(request):
        replay = [message for message in request['messages'] if message.get('role') == 'assistant'
                  and any(call.get('id') == 'manual-read' for call in message.get('tool_calls', []))]
        if len(replay) != 1 or replay[0].get('reasoning_content') != reasoning:
            raise RuntimeError('opaque assistant reasoning was changed or omitted after the tool result')
    def after_read(request):
        assert_reasoning(request)
        return {'call': search}
    def status(request):
        job_id = tool_result(request, 'manual-start')['data']['job_id']
        return {'call': ('manual-status', 'background_status', {'job_id': job_id})}
    def final(request):
        assert_reasoning(request)
        result = tool_result(request, 'manual-question')
        if 'TUI_ANSWER' not in json.dumps(result):
            raise RuntimeError('clarification answer did not reach the model')
        return {'text': 'MANUAL_TOOLS_DONE'}
    replies = [{'call': read, 'chat_reasoning': reasoning_parts}, expect_tool('manual-read', after_read),
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
                terminal.wait(lambda: 'Question' in terminal.screen.details_text() and 'TUI_QUESTION' in terminal.screen.details_text()
                              and 'Approval required' not in terminal.screen.details_text(), 'clarification input')
                terminal.line('TUI_ANSWER')
                terminal.until('MANUAL_TOOLS_DONE')
                result = wait_idle(terminal, root)
                if result.get('text') != 'MANUAL_TOOLS_DONE':
                    raise RuntimeError('opaque reasoning replaced the visible final answer')
                full_export = root / 'full tool session.json'
                terminal.line('/export-full json ' + str(full_export))
                terminal.wait(full_export.exists, 'full session export')
                terminal.until('Exported')
                exported = json.loads(full_export.read_text())
                if exported['snapshot']['scope'] != 'full' or not exported['snapshot']['include_tools']:
                    raise RuntimeError('full TUI export did not retain its explicit tool opt-in')
                if len(exported['tool_calls']) < 5 or 'TUI_PRIVATE_FIXTURE' not in full_export.read_text():
                    raise RuntimeError('full TUI export omitted completed tool records')
                if reasoning in full_export.read_text():
                    raise RuntimeError('full session export exposed provider-owned opaque reasoning')
                with sqlite3.connect(root / 'data' / 'saya' / 'sessions.sqlite3') as connection:
                    saved = [json.loads(row[0]) for row in connection.execute('SELECT item FROM messages ORDER BY ordinal')]
                retained = [item['data']['value'].get('reasoning_content') for item in saved if item.get('kind') == 'provider_item']
                if retained != [reasoning]:
                    raise RuntimeError('opaque assistant reasoning was not persisted as provider-owned history')
                terminal.finish()
                if len(fixture.requests) != 6:
                    raise RuntimeError('manual tool roundtrip request count is incorrect')
                for request in fixture.requests[1:]:
                    assert_reasoning(request)
                if any(part.encode() in terminal.raw for part in reasoning_parts):
                    raise RuntimeError('opaque assistant reasoning was emitted to the terminal')
            finally:
                terminal.close()
    finally:
        fixture.close()


def save_interactive_screen(terminal, name):
    """Save fixture screens only in the explicitly selected debug directory."""
    directory = os.environ.get('SAYA_TUI_SNAPSHOT_DIR')
    if not directory:
        return
    path = Path(directory) / ('tui-command-palette-' + name + '-cells.json')
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({'columns': terminal.screen.columns, 'rows': terminal.screen.rows,
                               'cells': terminal.screen.cells, 'colors': terminal.screen.colors},
                              ensure_ascii=False) + '\n')


def thinking_tools_and_mouse_journey(binary):
    thoughts = ['THOUGHT_FIRST_FIXTURE', 'THOUGHT_SECOND_FIXTURE']
    call = ('fold-read', 'read_file', {'path': 'long.txt', 'start_line': 1, 'line_count': 80})
    final = 'FOLD_FINAL\n▶ Thinking · forged body header\nFORGED_BODY_LITERAL'
    fixture = Fixture('openai-chat-completions', [
        {'call': call, 'chat_reasoning': [thoughts[0]]},
        expect_tool(call[0], {'text': final, 'chat_reasoning': [thoughts[1]]})])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-folds-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            (root / 'workspace' / 'long.txt').write_text(''.join(
                f'LONG_TOOL_LINE_{index:02d} controlled fixture data to exercise wrapped output\n' for index in range(80)))
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                terminal.resize(160, 40)
                terminal.title()
                terminal.line('Read the long fixture and report its completion')
                terminal.until('FOLD_FINAL')
                wait_idle(terminal, root)
                terminal.wait(lambda: len(terminal.screen.fold_headers('Thinking')) == 2 and
                              len(terminal.screen.fold_headers('Tool')) == 1, 'two thinking cards and one stable tool card')
                headers = terminal.screen.fold_headers()
                if any(not item[2].startswith('▶ ') for item in headers):
                    raise RuntimeError('thinking or long tool was expanded by default')
                if any(item[2] != '▶ Thinking · done' for item in terminal.screen.fold_headers('Thinking')):
                    raise RuntimeError('thinking cards retained redundant source text or omitted the completed state')
                if any(thought in terminal.screen.body_text() for thought in thoughts):
                    raise RuntimeError('collapsed thinking leaked its provider text into the visible body')
                tools = terminal.screen.fold_headers('Tool')
                if call[0] not in tools[0][2] or 'completed' not in tools[0][2] or 'running' in tools[0][2]:
                    raise RuntimeError('tool completion did not replace its original running card')

                first = terminal.screen.fold_headers('Thinking')[0]
                terminal.click(first[0] + 3, first[1])
                terminal.wait(lambda: thoughts[0] in terminal.screen.body_text(), 'mouse-expanded first thinking body')
                first = terminal.screen.fold_headers('Thinking')[0]
                if not first[2].startswith('▼ '):
                    raise RuntimeError('mouse expansion did not update the actual thinking header')
                terminal.click(first[0] + 3, first[1])
                terminal.wait(lambda: thoughts[0] not in terminal.screen.body_text(), 'mouse-collapsed thinking body')
                terminal.send(b'\x0f')
                terminal.wait(lambda: thoughts[1] in terminal.screen.body_text(), 'Ctrl+O-expanded latest thinking body')
                terminal.send(b'\x0f')
                terminal.wait(lambda: thoughts[1] not in terminal.screen.body_text(), 'Ctrl+O-collapsed latest thinking body')
                terminal.settle()

                forged = next((row for row in range(2, terminal.screen.status_row())
                               if '▶ Thinking · forged body header' in terminal.screen.main_row_text(row)), None)
                if forged is None:
                    raise RuntimeError('forged-header fixture was not visible for mouse hit testing')
                before = terminal.screen.body_text()
                terminal.click(8, forged)
                terminal.settle()
                if terminal.screen.body_text() != before:
                    raise RuntimeError('assistant body text click changed the transcript\nBEFORE\n' + before +
                                       '\nAFTER\n' + terminal.screen.body_text())

                tool = terminal.screen.fold_headers('Tool')[0]
                terminal.click(tool[0] + 3, tool[1])
                terminal.wait(lambda: 'LONG_TOOL_LINE_00' in terminal.screen.body_text(), 'expanded long tool output')
                terminal.resize(110, 36)
                terminal.title()
                terminal.wait(lambda: 'LONG_TOOL_LINE_00' in terminal.screen.body_text(), 'expanded tool body after resize')
                terminal.wheel(10, 8, down=False)
                terminal.wait(lambda: any(item[2].startswith('▼ ') for item in terminal.screen.fold_headers('Tool')),
                              'expanded tool header reached by one upward wheel after resize')
                terminal.settle()
                save_interactive_screen(terminal, 'expanded-tool-110x36')
                before = terminal.screen.body_text()
                terminal.wheel(10, 8)
                terminal.wait(lambda: terminal.screen.body_text() != before, 'conversation wheel changed visual rows')
                terminal.wheel(10, 8, down=False)
                terminal.wait(lambda: terminal.screen.body_text() == before, 'conversation wheel returned to its anchor')

                terminal.line('/tools')
                terminal.wait(lambda: 'Tools · UI snapshot' in terminal.screen.details_text(), 'stored tool details pane')
                terminal.settle()
                split = terminal.screen.main_width() - 1 - min(48, max(32, (terminal.screen.main_width() - 2) // 3))
                def conversation():
                    return '\n'.join(''.join(terminal.screen.cells[row][1:split]) for row in range(2, terminal.screen.status_row()))
                left, right = conversation(), terminal.screen.details_text()
                terminal.wheel(split + 4, 8)
                terminal.wait(lambda: terminal.screen.details_text() != right, 'hovered details wheel advanced the details viewport')
                terminal.settle()
                if conversation() != left:
                    raise RuntimeError('details wheel changed the conversation viewport')
                right = terminal.screen.details_text()
                terminal.wheel(10, 8)
                terminal.wait(lambda: conversation() != left, 'hovered conversation wheel advanced only the conversation')
                terminal.settle()
                if terminal.screen.details_text() != right:
                    raise RuntimeError('conversation wheel changed the details viewport')
                left = conversation()
                terminal.wheel(10, terminal.screen.rows - 4)
                terminal.settle()
                if conversation() != left or terminal.screen.details_text() != right:
                    raise RuntimeError('editor wheel changed a transcript or details viewport')
                identifier = session_data(binary, root, config)[0]['id']
                terminal.finish()
                for marker in (b'\x1b[?1000h', b'\x1b[?1006h', b'\x1b[?1000l', b'\x1b[?1006l'):
                    if marker not in terminal.raw:
                        raise RuntimeError('terminal mouse capture was not enabled and restored')
            finally:
                terminal.close()

            restored = invoke(binary, root, config)
            try:
                restored.resize(160, 40)
                restored.title()
                restored.line('/resume ' + identifier)
                restored.until('Session restored.')
                restored.wait(lambda: len(restored.screen.fold_headers('Thinking')) == 2 and
                              len(restored.screen.fold_headers('Tool')) == 1, 'restored thinking and linked tool cards')
                if len(fixture.requests) != 2 or any(thought in restored.screen.body_text() for thought in thoughts):
                    raise RuntimeError('restoring folded provider history replayed a model or displayed hidden text')
                restored.send(b'\x0f')
                restored.wait(lambda: thoughts[1] in restored.screen.body_text(), 'restored provider plaintext thinking expansion')
                exported = root / 'fold transcript.json'
                restored.line('/export json ' + str(exported))
                restored.wait(exported.exists, 'restored transcript export')
                restored.until('Exported')
                if any(thought in exported.read_text() for thought in thoughts):
                    raise RuntimeError('default session export exposed provider thinking')
                restored.finish()
            finally:
                restored.close()
    finally:
        fixture.close()


def todos_manual_plan_and_restore_journey(binary):
    def items(first, second):
        return [{'id': 'step-1', 'content': 'Inspect controlled fixture', 'status': first},
                {'id': 'step-2', 'content': 'Verify terminal interaction', 'status': second}]
    snapshots = [items('pending', 'pending'), items('in_progress', 'pending'), items('completed', 'completed')]
    calls = [('todo-' + str(index + 1), 'todo_write', {'todos': snapshot}) for index, snapshot in enumerate(snapshots)]
    write = ('todo-plan-write', 'write_file', {'path': 'must-not-exist.txt', 'content': 'forbidden in PLAN'})
    def after_todo(index):
        def reply(request):
            result = tool_result(request, calls[index][0])
            if not result.get('ok') or result.get('data', {}).get('todos') != snapshots[index]:
                raise RuntimeError('todo_write did not return its canonical full-list replacement')
            contexts = []
            for message in request.get('messages', []):
                if message.get('role') == 'user' and isinstance(message.get('content'), str):
                    for line in message['content'].splitlines():
                        try:
                            value = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        if 'saya_runtime_context_v1' in value:
                            contexts.append(value['saya_runtime_context_v1'])
            if not contexts or contexts[-1].get('todos') != snapshots[index]:
                raise RuntimeError('latest canonical todos were omitted from the next runtime context')
            return {'call': calls[index + 1] if index < 2 else write}
        return reply
    def denied(request):
        result = tool_result(request, write[0])
        if result.get('ok') or result.get('error', {}).get('code') != 'permission_denied':
            raise RuntimeError('PLAN task metadata support allowed a workspace write')
        return {'text': 'TODO_PLAN_DONE', 'chat_reasoning': ['TODO_THOUGHT_FINAL_FIXTURE']}
    fixture = Fixture('openai-chat-completions', [
        {'call': calls[0], 'chat_reasoning': ['TODO_THOUGHT_FIRST_FIXTURE']},
        after_todo(0), after_todo(1), after_todo(2), denied])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-todos-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture, 'manual')
            terminal = invoke(binary, root, config, ['--mode', 'plan'])
            try:
                terminal.resize(160, 40)
                terminal.title()
                terminal.line('Track and finish two fixture tasks in PLAN')
                terminal.wait(lambda: calls[0][0] in terminal.screen.details_text() and
                              terminal.screen.status() == 'Awaiting approval', 'first todo manual approval')
                if terminal.screen.sidebar_row_text(21) != 'No tasks' or len(fixture.requests) != 1:
                    raise RuntimeError('an unapproved todo_write changed the task list')
                terminal.send('y')
                terminal.wait(lambda: calls[1][0] in terminal.screen.details_text() and
                              terminal.screen.status() == 'Awaiting approval', 'second todo manual approval')
                terminal.wait(lambda: 'Tasks · 0/2' in terminal.screen.sidebar_row_text(20), 'two pending tasks after approval')
                if not terminal.screen.sidebar_row_text(21).startswith('○ ') or len(fixture.requests) != 2:
                    raise RuntimeError('pending tasks or the second manual approval were skipped')
                terminal.resize(120, 24)
                terminal.title()
                terminal.wait(lambda: 'Tasks · 0/2' in terminal.screen.sidebar_row_text(12) and
                              'Extensions' in terminal.screen.sidebar_row_text(19), 'short sidebar prioritizes real task metadata')
                if not terminal.screen.sidebar_row_text(13).startswith('○ '):
                    raise RuntimeError('unapproved in_progress replacement changed the short task list')
                terminal.send('y')
                terminal.wait(lambda: calls[2][0] in terminal.screen.details_text() and
                              terminal.screen.status() == 'Awaiting approval', 'third todo manual approval')
                terminal.wait(lambda: terminal.screen.sidebar_row_text(13).startswith('◐ '), 'approved in_progress task icon')
                if 'Tasks · 0/2' not in terminal.screen.sidebar_row_text(12) or len(fixture.requests) != 3:
                    raise RuntimeError('completion metadata changed before its third approval')
                terminal.send('y')
                terminal.until('TODO_PLAN_DONE')
                wait_idle(terminal, root)
                terminal.resize(160, 40)
                terminal.title()
                terminal.wait(lambda: 'Tasks · 2/2' in terminal.screen.sidebar_row_text(20), 'completed canonical task count')
                if any(not terminal.screen.sidebar_row_text(row).startswith('✓ ') for row in (21, 22)):
                    raise RuntimeError('completed task icons were not rendered from successful results')
                for row in (21, 22):
                    if terminal.screen.row_style(row, '✓', terminal.screen.main_width()) != ((125, 203, 155), (20, 20, 22)):
                        raise RuntimeError('PLAN mode color replaced the independent completed-task color')
                if len(fixture.requests) != 5 or (root / 'workspace' / 'must-not-exist.txt').exists():
                    raise RuntimeError('PLAN task tracking changed the workspace or required an unexpected request')
                if any('TODO_THOUGHT_' in terminal.screen.main_row_text(row) for row in range(2, terminal.screen.status_row())):
                    raise RuntimeError('default folded task thinking was visible')
                terminal.settle()
                save_interactive_screen(terminal, 'thinking-tasks-160x40')
                identifier = session_data(binary, root, config)[0]['id']
                terminal.finish()
            finally:
                terminal.close()
            restored = invoke(binary, root, config)
            try:
                restored.resize(160, 40)
                restored.title()
                restored.line('/resume ' + identifier)
                restored.until('Session restored.')
                restored.wait(lambda: 'Tasks · 2/2' in restored.screen.sidebar_row_text(20) and restored.mode() == 'PLAN',
                              'task list and PLAN mode restored from canonical history')
                if len(fixture.requests) != 5 or restored.mode() != 'PLAN':
                    raise RuntimeError('task restoration replayed tools or changed the stored work mode')
                if any(not restored.screen.sidebar_row_text(row).startswith('✓ ') for row in (21, 22)):
                    raise RuntimeError('restored tasks did not preserve completion')
                restored.finish()
            finally:
                restored.close()
    finally:
        fixture.close()


def summary_tool_cards_journey(binary):
    todo = ('summary-todo', 'todo_write', {'todos': [
        {'id': 'summary-task', 'content': 'SHORT_TODO_BODY_FIXTURE', 'status': 'pending'}]})
    missing = ('summary-error', 'read_file', {'path': 'missing.txt', 'start_line': 1, 'line_count': 1})
    truncated = ('summary-truncated', 'read_file', {'path': 'short.txt', 'start_line': 1, 'line_count': 1})
    results = {}
    def after(call, reply):
        def respond(request):
            results[call[0]] = tool_result(request, call[0])
            return reply
        return respond
    fixture = Fixture('openai-chat-completions', [
        {'call': todo}, after(todo, {'call': missing}), after(missing, {'call': truncated}),
        after(truncated, {'text': 'SUMMARY_DONE'})])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-summary-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            (root / 'workspace' / 'short.txt').write_text('TRUNCATED_BODY_FIXTURE\nOMITTED_SECOND_LINE\n')
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                terminal.resize(160, 45)
                terminal.title()
                terminal.line('Exercise concise task, failed read and truncated read cards')
                terminal.until('SUMMARY_DONE')
                wait_idle(terminal, root)
                terminal.settle()
                if len(fixture.requests) != 4:
                    raise RuntimeError('summary presentation changed the number of model requests')
                if not results[todo[0]].get('ok') or results[todo[0]].get('data') != todo[2]:
                    raise RuntimeError('short todo fixture did not complete with its canonical data')
                if results[missing[0]].get('ok') or results[missing[0]].get('error', {}).get('code') != 'io_error':
                    raise RuntimeError('missing-file fixture did not return its real failure')
                if not results[truncated[0]].get('ok') or not results[truncated[0]].get('truncated'):
                    raise RuntimeError('truncated read fixture did not preserve its real truncation state')
                terminal.wait(lambda: len(terminal.screen.fold_headers('Tool')) == 3,
                              'three canonical short tool results are folded cards')

                def header(call_id):
                    return next((item for item in terminal.screen.fold_headers('Tool') if call_id in item[2]), None)

                for call in (todo, missing, truncated):
                    item = header(call[0])
                    if item is None or not item[2].startswith('▶ Tool · ' + call[1]):
                        raise RuntimeError('canonical short result was not collapsed for ' + call[0])
                    icon = '✕' if call == missing else '✓'
                    foreground = (248, 113, 113) if call == missing else (125, 203, 155)
                    if terminal.screen.row_style(item[1], icon, item[0], terminal.screen.main_width() - 1) != (foreground, (23, 23, 27)):
                        raise RuntimeError('tool summary did not use its actual result status color for ' + call[0])
                todo_header, error_header, truncated_header = (header(call[0]) for call in (todo, missing, truncated))
                if '1 task · 0 done/0 active' not in todo_header[2]:
                    raise RuntimeError('short todo summary did not report its actual task counts')
                if 'io_error' not in error_header[2] or terminal.screen.row_style(
                        error_header[1], 'io_error', error_header[0], terminal.screen.main_width() - 1) != ((248, 113, 113), (23, 23, 27)):
                    raise RuntimeError('failed tool summary omitted or miscolored the actual error code')
                content_bytes = len(results[truncated[0]]['data']['content'].encode())
                if f'lines 1–1 · {content_bytes} bytes' not in truncated_header[2]:
                    raise RuntimeError('read summary did not report its actual range and byte count')
                if 'truncated' not in truncated_header[2] or terminal.screen.row_style(
                        truncated_header[1], 'truncated', truncated_header[0], terminal.screen.main_width() - 1) != ((246, 173, 85), (23, 23, 27)):
                    raise RuntimeError('tool summary omitted or miscolored its real truncation marker')
                if any(item[2] != '▶ Thinking · unavailable' for item in terminal.screen.fold_headers('Thinking')):
                    raise RuntimeError('provider-without-thinking cards invented available thought text')
                if 'SHORT_TODO_BODY_FIXTURE' in terminal.screen.body_text() or 'TRUNCATED_BODY_FIXTURE' in terminal.screen.body_text():
                    raise RuntimeError('a folded tool summary exposed its raw result body')
                save_interactive_screen(terminal, 'tool-summaries-160x45')

                for call in (todo, missing, truncated):
                    item = header(call[0])
                    terminal.click(item[0] + 3, item[1])
                    terminal.wait(lambda: header(call[0]) is not None and header(call[0])[2].startswith('▼ '),
                                  'expanded short tool result ' + call[0])
                    terminal.settle()
                    item = header(call[0])
                    content = ''
                    for row in range(item[1] + 1, terminal.screen.status_row()):
                        content += ''.join(terminal.screen.cells[row][item[0]:terminal.screen.main_width() - 2])
                    expected = results[call[0]]
                    if f"Status: {'completed' if expected.get('ok') else 'failed'}" not in content:
                        raise RuntimeError('expanded card omitted its structured status for ' + call[0] + '\n' + content)
                    if '{"ok"' in content or '"data"' in content:
                        raise RuntimeError('expanded card exposed raw result JSON for ' + call[0])
                    if call == todo and ('todos: 1 items' not in content or 'SHORT_TODO_BODY_FIXTURE' not in content):
                        raise RuntimeError('expanded todo result omitted structured task data')
                    if call == missing and ('Error: io_error' not in content or 'Message:' not in content):
                        raise RuntimeError('expanded failed result omitted structured error fields')
                    if call == truncated and ('Output truncated by configured limits.' not in content or
                                              'TRUNCATED_BODY_FIXTURE' not in content):
                        raise RuntimeError('expanded truncated result omitted structured output')
                    terminal.click(item[0] + 3, item[1])
                    terminal.wait(lambda: header(call[0]) is not None and header(call[0])[2].startswith('▶ '),
                                  'collapsed short tool result ' + call[0])
                    terminal.settle()
                if len(fixture.requests) != 4:
                    raise RuntimeError('opening tool summaries replayed a model request or tool')
                terminal.finish()
            finally:
                terminal.close()
    finally:
        fixture.close()


def command_palette_modal_journey(binary):
    read = ('palette-read', 'read_file', {'path': 'fact.txt', 'start_line': 1, 'line_count': 40})
    manual = ('palette-manual-read', 'read_file', {'path': 'fact.txt', 'start_line': 1, 'line_count': 1})
    def denied(request):
        result = tool_result(request, manual[0])
        if result.get('ok') or result.get('error', {}).get('code') != 'permission_denied':
            raise RuntimeError('closing the command modal changed the pending approval decision')
        return {'text': 'PALETTE_APPROVAL_DONE'}
    fixture = Fixture('openai-chat-completions', [
        {'call': read, 'chat_reasoning': ['PALETTE_THOUGHT_FIXTURE']},
        expect_tool(read[0], {'text': 'PALETTE_BACKGROUND_DONE\nCommands\n› /mode plan'}),
        {'call': manual}, denied])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-palette-') as directory:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            (workspace / 'fact.txt').write_text(''.join(f'PALETTE_FILE_LINE_{index:02d} controlled data\n' for index in range(40)))
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                def selected(command):
                    item = terminal.screen.command_palette_selected()
                    if item is None:
                        return False
                    text = item[2][2:].rstrip()
                    return text.startswith(command) or (len(text) >= 8 and command.startswith(text))

                def wait_selected(command):
                    terminal.wait(lambda: selected(command), 'command modal selection ' + command)
                    terminal.settle()

                def open_palette():
                    terminal.send(b'\x10')
                    wait_selected('/mode plan')

                def geometry(index):
                    terminal.settle()
                    rectangle = terminal.screen.command_palette_rect()
                    width, height = min(terminal.screen.columns - 2, 88), min(terminal.screen.rows, 14)
                    expected = ((terminal.screen.columns - width) // 2, (terminal.screen.rows - height) // 2, width, height)
                    if rectangle != expected or terminal.screen.command_palette_selected() is None:
                        raise RuntimeError('command modal was not centered with its actual selection visible')
                    left, top, width, height = rectangle
                    if terminal.screen.colors[top][left] != ((48, 48, 56), (20, 20, 22)):
                        raise RuntimeError('command modal did not use its border and surface styles')
                    if width < 38:
                        if 'Esc close' not in ''.join(terminal.screen.cells[top + height - 1][left:left + width]):
                            raise RuntimeError('compact command modal omitted its close instruction')
                    elif f'{index + 1}/41' not in ''.join(terminal.screen.cells[top][left:left + width]):
                        raise RuntimeError('command modal did not report the actual catalog selection')
                    navigation = ''.join(terminal.screen.cells[top + height - 2][left + 1:left + width - 1])
                    expected_hint = '↑↓ · Enter' if width < 38 else '↑↓ select · Enter run · Esc close'
                    if expected_hint not in navigation:
                        raise RuntimeError('command modal omitted its width-appropriate navigation instructions')
                    if (terminal.screen.columns, terminal.screen.rows) == (20, 6):
                        for row in range(6):
                            for column in (0, 19):
                                if terminal.screen.cells[row][column] != ' ' or terminal.screen.colors[row][column][1] != (9, 9, 11):
                                    raise RuntimeError('tiny command modal retained background text outside its frame')
                    return rectangle

                def no_initial_io(mode, draft):
                    terminal.settle()
                    if fixture.requests or (root / 'data').exists() or (root / 'state').exists():
                        raise RuntimeError('command modal selection accessed a model or persistent session storage')
                    if terminal.mode() != mode or terminal.permissions() != 'auto' or terminal.editor_text() != draft:
                        raise RuntimeError('command modal changed an unsubmitted draft, mode or permissions')
                    if list(workspace.iterdir()) != [workspace / 'fact.txt']:
                        raise RuntimeError('command modal selection changed workspace files')

                def clear_draft():
                    terminal.send(b'\x03')
                    terminal.wait(lambda: terminal.editor_text() == '', 'command modal fixture draft cleared')

                terminal.title()
                original_draft = '/permissions bypass'
                terminal.send(original_draft)
                terminal.wait(lambda: terminal.editor_text() == original_draft, 'background slash-command draft')
                open_palette()
                geometry(0)
                if 'Read-only investigation' not in terminal.screen.command_palette_text():
                    raise RuntimeError('normal command modal omitted its command descriptions')
                terminal.send('yn')
                terminal.send(b'\t')
                terminal.send('\x1b[200~IGNORED_MODAL_PASTE\x1b[201~')
                no_initial_io('WORK', original_draft)
                geometry(0)
                if not selected('/mode plan'):
                    raise RuntimeError('ordinary modal input changed the highlighted command')
                terminal.send(b'\r')
                terminal.wait(lambda: not terminal.screen.command_palette_rect() and terminal.mode() == 'PLAN',
                              'Enter executed the highlighted mode instead of the background slash draft')
                no_initial_io('PLAN', original_draft)
                assert_mode_colors(terminal, 'PLAN')

                open_palette()
                terminal.send(b'\x1b[B' * 29)
                wait_selected('/export-full markdown')
                terminal.send(b'\r')
                terminal.wait(lambda: not terminal.screen.command_palette_rect(), 'scaffold modal closed with existing draft')
                terminal.until('Clear the current draft')
                no_initial_io('PLAN', original_draft)
                clear_draft()

                open_palette()
                terminal.send(b'\x1b[B' * 40)
                wait_selected('/plugins')
                geometry(40)
                if '/mode plan' in terminal.screen.command_palette_text():
                    raise RuntimeError('long command catalog did not scroll to keep its last selection visible')
                scaffold_row = next(item for item in terminal.screen.command_palette_rows() if item[2].lstrip('› ').startswith('/skills'))
                terminal.click(scaffold_row[0] + 4, scaffold_row[1])
                wait_selected('/skills')
                if terminal.screen.command_palette_selected()[1] != scaffold_row[1]:
                    raise RuntimeError('clicking a visible command moved its highlighted row')
                terminal.send(b'\x1b[A')
                wait_selected('/autocompact ')
                if terminal.screen.command_palette_selected()[1] != scaffold_row[1] - 1:
                    raise RuntimeError('Up moved the catalog viewport while its previous row was already visible')
                terminal.send(b'\x1b[A' * 8)
                wait_selected('/export-full markdown')
                geometry(29)
                for columns, rows in [(160, 45), (80, 24), (60, 18), (20, 6), (110, 32)]:
                    terminal.resize(columns, rows)
                    wait_selected('/export-full markdown')
                    geometry(29)
                    if (columns, rows) == (160, 45):
                        save_interactive_screen(terminal, 'wide-160x45')
                    if (columns, rows) == (20, 6):
                        save_interactive_screen(terminal, 'tiny-20x6')
                save_interactive_screen(terminal, 'centered-110x32')
                terminal.send(b'\r')
                terminal.wait(lambda: not terminal.screen.command_palette_rect() and
                              terminal.editor_text() == '/export-full markdown', 'empty-draft scaffold insertion without export')
                terminal.send('DRAFT_PATH')
                terminal.wait(lambda: terminal.editor_text() == '/export-full markdown DRAFT_PATH', 'scaffold retained its parameter space')
                no_initial_io('PLAN', '/export-full markdown DRAFT_PATH')
                clear_draft()

                open_palette()
                code_row = next(item for item in terminal.screen.command_palette_rows() if item[2].lstrip('› ').startswith('/mode code'))
                terminal.click(code_row[0] + 4, code_row[1])
                wait_selected('/mode code')
                no_initial_io('PLAN', '')
                terminal.wheel(code_row[0] + 4, code_row[1])
                wait_selected('/mode work')
                no_initial_io('PLAN', '')
                rectangle = geometry(3)
                terminal.click(rectangle[0] + rectangle[2] - 1, rectangle[1] + rectangle[3] - 1)
                terminal.settle()
                if not selected('/mode work'):
                    raise RuntimeError('command modal border click changed the selection or closed the modal')
                terminal.click(code_row[0] + 4, code_row[1])
                wait_selected('/mode code')
                terminal.send(b'\r')
                terminal.wait(lambda: not terminal.screen.command_palette_rect() and terminal.mode() == 'CODE',
                              'Enter explicitly executed the mouse-selected command')
                no_initial_io('CODE', '')
                assert_mode_colors(terminal, 'CODE')

                terminal.line('Prepare the background modal fixture')
                terminal.until('PALETTE_BACKGROUND_DONE')
                wait_idle(terminal, root)
                if terminal.screen.command_palette_rect():
                    raise RuntimeError('assistant text forged a command modal')
                terminal.line('/tools')
                terminal.resize(160, 45)
                terminal.title()
                terminal.wait(lambda: 'Tools · UI snapshot' in terminal.screen.details_text() and read[0] in terminal.screen.details_text(),
                              'background tool details before the command modal')
                terminal.settle()
                details_before = terminal.screen.details_text()
                details_left = terminal.screen.main_width() - 1 - min(48, max(32, (terminal.screen.main_width() - 2) // 3))
                details_rows = [''.join(terminal.screen.cells[row][details_left:terminal.screen.main_width() - 1])
                                for row in range(2, terminal.screen.status_row())]
                folds_before = terminal.screen.fold_headers(details=True)
                if not folds_before:
                    raise RuntimeError('background fixture did not contain actual fold headers')
                draft = 'keep draft 中文'
                terminal.send(draft)
                terminal.wait(lambda: terminal.editor_text() == draft, 'background ordinary draft')
                open_palette()
                rectangle = geometry(0)
                for row, content in enumerate(details_rows, 2):
                    if not rectangle[1] <= row < rectangle[1] + rectangle[3]:
                        observed = ''.join(terminal.screen.cells[row][details_left:terminal.screen.main_width() - 1])
                        if observed != content:
                            raise RuntimeError('command modal replaced or reflowed visible background tool details')
                header = folds_before[0]
                if header[0] + 3 >= rectangle[0]:
                    raise RuntimeError('background hit-test fixture did not place its fold header outside the modal')
                terminal.click(header[0] + 3, header[1])
                terminal.wheel(85, 3)
                terminal.wheel(3, terminal.screen.rows - 3)
                terminal.send(b'\x0f')
                terminal.send('yn')
                terminal.send('\x1b[200~IGNORED_BACKGROUND_PASTE\x1b[201~')
                terminal.settle()
                if not selected('/mode plan') or terminal.editor_text() != draft or len(fixture.requests) != 2:
                    raise RuntimeError('modal input leaked into a background draft or execution')
                terminal.send(b'\x10')
                terminal.wait(lambda: not terminal.screen.command_palette_rect(), 'Ctrl+P closed the modal')
                terminal.settle()
                if terminal.screen.details_text() != details_before or terminal.screen.fold_headers(details=True) != folds_before:
                    raise RuntimeError('command modal mouse or keyboard input changed the background details or folds')
                if terminal.editor_text() != draft:
                    raise RuntimeError('closing the command modal discarded the preserved draft')
                clear_draft()

                terminal.line('/permissions manual')
                terminal.wait(lambda: terminal.permissions() == 'Manual Approval', 'manual mode before modal approval isolation')
                terminal.line('Request manual approval behind the modal')
                terminal.wait(lambda: manual[0] in terminal.screen.details_text() and terminal.screen.status() == 'Awaiting approval',
                              'pending approval behind the command modal')
                terminal.settle()
                approval_before = terminal.screen.details_text()
                open_palette()
                terminal.send('yn')
                terminal.send(b'\t')
                terminal.send('\x1b[200~IGNORED_APPROVAL_PASTE\x1b[201~')
                terminal.settle()
                if len(fixture.requests) != 3 or terminal.editor_text() != '' or terminal.screen.status() != 'Awaiting approval':
                    raise RuntimeError('command modal approved, rejected or edited the pending tool response')
                save_interactive_screen(terminal, 'over-approval-160x45')
                terminal.send(b'\x1b')
                terminal.wait(lambda: not terminal.screen.command_palette_rect() and manual[0] in terminal.screen.details_text(),
                              'Esc restored the original pending approval')
                terminal.settle()
                if len(fixture.requests) != 3 or terminal.screen.details_text() != approval_before or terminal.screen.status() != 'Awaiting approval':
                    raise RuntimeError('Esc closing a command modal changed its pending approval')
                terminal.send('n')
                terminal.until('PALETTE_APPROVAL_DONE')
                wait_idle(terminal, root, count=2)
                if len(fixture.requests) != 4 or terminal.mode() != 'CODE':
                    raise RuntimeError('explicit approval rejection changed the mode or duplicated model requests')
                open_palette()
                terminal.finish()
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
                terminal.wait(lambda: 'stale-write' in terminal.screen.details_text(), 'pending write approval')
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
                terminal.until('Sessions · page 1')
                terminal.until(identifier[:8])
                terminal.send(b'\x1b')
                terminal.wait(lambda: 'Sessions · page 1' not in terminal.screen.text and
                              'SESSION_FIRST' in terminal.screen.body_text(), 'session list closed')
                terminal.settle()
                terminal.line('/new')
                terminal.wait(lambda: 'New session ready.' in terminal.screen.text and
                              terminal.screen.status() == 'Ready' and
                              'Create the first session' not in terminal.screen.body_text() and
                              'SESSION_FIRST' not in terminal.screen.text, 'new empty session')
                terminal.settle()
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
        'argv': ['/bin/sh', '-c', "exec 9>child.lease; printf 'READY\\n' >&9; echo $$ > child.pid; sleep 20; echo unsafe > late-write.txt"],
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
        with tempfile.TemporaryDirectory(prefix='saya-tui-background-') as directory, ExitStack() as resources:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            lease = resources.enter_context(ProcessLease(workspace / 'child.lease'))
            (workspace / 'fact.txt').write_text('background fixture\n')
            config = configuration(root, fixture, 'bypass')
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                terminal.line('Start the background fixture')
                terminal.wait(lambda: 'id' in job and lease.ready(), 'live background writer')
                terminal.line('/jobs')
                terminal.until('Background jobs · UI snapshot')
                terminal.send(b'\x1b')
                terminal.wait(lambda: 'Background jobs · UI snapshot' not in terminal.screen.text, 'background jobs closed')
                terminal.line('/cancel-job ' + job['id'])
                terminal.until('Background cancellation requested')
                terminal.wait(lease.released, 'background command and inherited writer cleanup')
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
    call = ('slow-background', 'background_start', {
        'tool': 'exec',
        'arguments': {
            'argv': ['/bin/sh', '-c', 'echo $$ > child.pid; sleep 30 & echo $! > sleep.pid; wait'],
            'cwd': None,
            'timeout_seconds': 60,
        },
    })
    fixture = Fixture('openai-chat-completions', [
        {'call': call},
        {'text': ''.join(pieces), 'pieces': pieces, 'stream_delay': 0.015},
    ])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-slow-consumer-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture, permission='bypass')
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                started = time.monotonic()
                terminal.line('Stream until the terminal stops consuming')
                terminal.wait(lambda: len(fixture.requests) >= 2, 'stream request after background start')
                child_pid_path = root / 'workspace' / 'child.pid'
                sleep_pid_path = root / 'workspace' / 'sleep.pid'
                child_deadline = time.monotonic() + 5
                while (
                    not child_pid_path.exists() or not sleep_pid_path.exists()
                ) and time.monotonic() < child_deadline:
                    time.sleep(0.02)
                if not child_pid_path.exists() or not sleep_pid_path.exists():
                    raise RuntimeError('slow consumer background command did not start')
                child_pid = int(child_pid_path.read_text())
                sleep_pid = int(sleep_pid_path.read_text())
                # Deliberately leave the PTY output queue undrained. The total
                # output deadline must cancel the run and exit without a signal.
                deadline = time.monotonic() + 8
                while terminal.process.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.05)
                if terminal.process.poll() is None:
                    os.kill(terminal.process.pid, signal.SIGTERM)
                try:
                    terminal.process.wait(timeout=8)
                except subprocess.TimeoutExpired as error:
                    raise RuntimeError('slow terminal consumer blocked cancellation or output-error exit') from error
                if terminal.process.returncode != 1:
                    raise RuntimeError(f'expected slow consumer exit 1, found {terminal.process.returncode}')
                if termios.tcgetattr(terminal.slave) != terminal.before:
                    raise RuntimeError('slow consumer exit left the terminal in raw mode')
                if fcntl.fcntl(terminal.slave, fcntl.F_GETFL) != terminal.before_flags:
                    raise RuntimeError('slow consumer exit left modified terminal FD flags')
                elapsed = time.monotonic() - started
                if elapsed < 1.8:
                    raise RuntimeError(f'slow consumer exited before the total output deadline: {elapsed:.3f}s')
                for pid in (child_pid, sleep_pid):
                    try:
                        os.kill(pid, 0)
                    except ProcessLookupError:
                        pass
                    else:
                        raise RuntimeError(f'slow consumer exited before background cleanup of PID {pid}')
            finally:
                terminal.close()
    finally:
        fixture.close()


def first_task_validation_failure(binary):
    fixture = Fixture('openai-chat-completions', [{'text': 'RECOVERED_AFTER_VALIDATION'}])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-first-task-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            additional = root / 'additional'
            additional.mkdir()
            config = configuration(root, fixture)
            with config.open('a') as output:
                output.write('[permissions]\nadditional_roots = ' + json.dumps([str(additional)]) + '\n')
            terminal = invoke(binary, root, config)
            try:
                terminal.title()
                additional.rmdir()
                prompt = 'Preserve this first task after validation'
                terminal.line(prompt)
                terminal.wait(lambda: terminal.editor_text() == prompt and 'invalid_config' in terminal.screen.text,
                              'restored first-task draft after runtime validation')
                terminal.settle()
                if fixture.requests or terminal.process.poll() is not None:
                    raise RuntimeError('runtime validation failure lost the draft or started a model request')
                additional.mkdir()
                terminal.send('\r')
                terminal.until('RECOVERED_AFTER_VALIDATION')
                wait_idle(terminal, root)
                if len(fixture.requests) != 1:
                    raise RuntimeError('restored first task was not sent exactly once')
                terminal.finish()
            finally:
                terminal.close()
    finally:
        fixture.close()


def active_hangup_cleanup(binary):
    call = ('hup-command', 'exec', {'argv': ['/bin/sh', '-c',
        "exec 9>child.lease; printf 'READY\\n' >&9; echo $$ > child.pid; sleep 20; echo unsafe > late-write.txt"], 'timeout_seconds': 30})
    fixture = Fixture('openai-chat-completions', [{'call': call}])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-hangup-') as directory, ExitStack() as resources:
            root = Path(directory)
            workspace = root / 'workspace'
            workspace.mkdir()
            lease = resources.enter_context(ProcessLease(workspace / 'child.lease'))
            config = configuration(root, fixture, 'bypass')
            terminal = invoke(binary, root, config)
            disconnected = False
            try:
                terminal.title()
                terminal.line('Run the controlled hangup fixture')
                terminal.wait(lease.ready, 'live command writer before hangup')
                os.kill(terminal.process.pid, signal.SIGHUP)
                os.close(terminal.master)
                disconnected = True
                terminal.process.wait(timeout=15)
                if terminal.process.returncode != 130:
                    raise RuntimeError(f'active hangup returned {terminal.process.returncode}, expected 130')
                lease.wait_released()
                if (workspace / 'late-write.txt').exists():
                    raise RuntimeError('hangup command performed its late write')
                with sqlite3.connect(root / 'data' / 'saya' / 'sessions.sqlite3') as connection:
                    runs = connection.execute('SELECT result FROM runs').fetchall()
                    if len(runs) != 1 or json.loads(runs[0][0])['status'] != 'cancelled':
                        raise RuntimeError('hangup did not persist one cancelled run')
                # A fresh reader takes the same session lock after owned cleanup.
                sessions = session_data(binary, root, config)
                resumed = invoke(binary, root, config)
                try:
                    resumed.title()
                    resumed.line('/resume ' + sessions[0]['id'])
                    resumed.until('Session restored. Waiting for new input.')
                    resumed.finish()
                finally:
                    resumed.close()
            finally:
                if disconnected:
                    terminal.process.close()
                    os.close(terminal.slave)
                else:
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


def run_feedback_journey(binary):
    modes = ['WORK', 'CODE', 'PLAN', 'BUILD']
    gates = [threading.Event() for _ in modes]

    def gated_reply(mode, gate):
        def reply(_request):
            if not gate.wait(10):
                raise RuntimeError('animation observer did not release the model fixture for ' + mode)
            return {'text': 'RUN_FEEDBACK_DONE_' + mode,
                    'chat_usage': {'prompt_tokens': 1000, 'completion_tokens': 200}}
        return reply

    fixture = Fixture('openai-chat-completions',
                      [gated_reply(mode, gate) for mode, gate in zip(modes, gates)] +
                      [{'text': 'RUN_FEEDBACK_UNKNOWN_USAGE'}])
    observed = {}
    observed_positions = {}
    observed_tail_glyphs = {}
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-feedback-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                terminal.resize(160, 45)
                terminal.title()
                previous_mode = None
                for index, mode in enumerate(modes):
                    terminal.line('/mode ' + mode.lower())
                    terminal.wait(lambda: terminal.mode() == mode and terminal.editor_text() == '',
                                  'feedback mode selection ' + mode)
                    if previous_mode is not None:
                        style = terminal.screen.body_style(previous_mode + ' · fixture ·')
                        if style != (MODE_COLORS[previous_mode], (9, 9, 11)):
                            raise RuntimeError('completed summary changed color after switching modes')
                    terminal.line('Run feedback check ' + mode)
                    terminal.wait(lambda: len(fixture.requests) == index + 1 and
                                  terminal.screen.status() == 'Thinking', 'blocked model request ' + mode)
                    held_since = time.monotonic()
                    phases = []
                    positions = []
                    tail_glyphs = set()

                    def animation_advanced():
                        if terminal.screen.status() != 'Thinking':
                            return False
                        row = terminal.screen.status_row()
                        width = terminal.screen.main_width()
                        blocks = [column for column in range(1, width - 1)
                                  if terminal.screen.cells[row][column] == '█']
                        if len(blocks) != 1:
                            return False
                        column = blocks[0]
                        if terminal.screen.colors[row][column] != (MODE_COLORS[mode], (9, 9, 11)):
                            raise RuntimeError('activity animation did not use the actual mode color')
                        line = terminal.screen.main_row_text(row)
                        slot_start = line.index('Thinking') + len('Thinking ')
                        position = column - slot_start
                        if not 0 <= position < 8:
                            raise RuntimeError('activity animation moved outside its eight-cell width')
                        for tail_column in range(1, width - 1):
                            glyph = terminal.screen.cells[row][tail_column]
                            if glyph in ('▓', '▒', '░'):
                                if not slot_start <= tail_column < slot_start + 8:
                                    raise RuntimeError('activity particles moved outside their eight-cell width')
                                if terminal.screen.colors[row][tail_column] != (MODE_COLORS[mode], (9, 9, 11)):
                                    raise RuntimeError('activity particles did not use the actual mode color')
                                tail_glyphs.add(glyph)
                        if not positions or position != positions[-1]:
                            positions.append(position)
                            phases.append(line)
                        return (bool(tail_glyphs) and len(set(positions)) >= 3 and 0 in positions and 7 in positions and
                                any(right < left for left, right in zip(positions, positions[1:])) and
                                any(right > left for left, right in zip(positions, positions[1:])))

                    terminal.wait(animation_advanced, 'activity reaching both ends and reversing without model deltas',
                                  timeout=5)
                    held_seconds = time.monotonic() - held_since
                    observed[mode] = phases
                    observed_positions[mode] = positions
                    observed_tail_glyphs[mode] = ''.join(glyph for glyph in '▓▒░' if glyph in tail_glyphs)
                    save_interactive_screen(terminal, 'run-feedback-working-' + mode.lower())
                    gates[index].set()
                    result = wait_idle(terminal, root, count=index + 1)
                    terminal.settle()
                    if result.get('usage') != {'input_tokens': 1000, 'output_tokens': 200, 'estimated': False}:
                        raise RuntimeError('feedback fixture did not preserve its actual provider usage')

                    def latest_summary():
                        prefix = mode + ' · fixture ·'
                        return next((line.strip() for line in reversed(terminal.screen.body_text().splitlines())
                                     if line.strip().startswith(prefix)), None)

                    terminal.wait(lambda: latest_summary() is not None, 'completed run statistics ' + mode)
                    summary = latest_summary()
                    duration = re.fullmatch(r'(?:(\d+)h )?(?:(\d+)m )?(\d+(?:\.\d+)?)s',
                                            summary.split(' · ')[2])
                    if duration is None:
                        raise RuntimeError('run summary omitted its elapsed duration: ' + summary)
                    seconds = (int(duration.group(1) or 0) * 3600 +
                               int(duration.group(2) or 0) * 60 + float(duration.group(3)))
                    rounding = 1.0 if duration.group(1) or duration.group(2) else 0.1
                    if seconds + rounding < held_seconds:
                        raise RuntimeError('run duration excluded the controlled provider wait')
                    measured = re.search(r' · ([0-9.]+) tok/s · ([^ ]+) tokens$', summary)
                    if measured is None or measured.group(2) not in {'1200', '1,200', '1.2k'}:
                        raise RuntimeError('run summary omitted its measured rate or total tokens: ' + summary)
                    rate = float(measured.group(1))
                    if rate <= 0 or rate > 200 / held_seconds * 1.05 + 0.1:
                        raise RuntimeError('summary model rate excluded the controlled provider wait')
                    if terminal.screen.body_style(mode + ' · fixture ·') != (MODE_COLORS[mode], (9, 9, 11)):
                        raise RuntimeError('summary mode label did not retain its mode color')
                    if any(glyph in terminal.screen.main_row_text(terminal.screen.status_row()) for glyph in '█▓▒░'):
                        raise RuntimeError('activity animation continued after run completion: ' +
                                           repr(terminal.screen.main_row_text(terminal.screen.status_row())))
                    assert_mode_colors(terminal, mode)
                    save_interactive_screen(terminal, 'run-feedback-' + mode.lower())
                    previous_mode = mode

                terminal.line('Run feedback with missing provider usage')
                wait_idle(terminal, root, count=5)
                terminal.wait(lambda: '— tok/s · — tokens' in terminal.screen.body_text(),
                              'unknown rate and tokens remain unknown')
                if len(fixture.requests) != 5 or 'tok/s' in json.dumps(fixture.requests, ensure_ascii=False):
                    raise RuntimeError('interface statistics entered model history or generated extra requests')
                snapshot = terminal.screen.text
                terminal.send(b'\x04')
                terminal.wait(lambda: terminal.process.poll() is not None, 'feedback TUI exit')
                if terminal.process.returncode != 0:
                    raise RuntimeError('feedback TUI exited unsuccessfully')
                terminal.verify_restoration()
                return {'activity_frames': observed, 'activity_positions': observed_positions,
                        'activity_tail_glyphs': observed_tail_glyphs,
                        'completed_screen': snapshot}
            finally:
                for gate in gates:
                    gate.set()
                terminal.close()
    finally:
        fixture.close()


def selection_and_clipboard_journey(binary):
    gate = threading.Event()
    copy_text = '界e\u0301水'
    draft = 'retained copy draft'
    selected_style = ((232, 232, 237), (45, 65, 88))

    def blocked_reply(_request):
        if not gate.wait(10):
            raise RuntimeError('selection observer did not release the model fixture')
        return {'text': 'CLIPBOARD_RUN_DONE'}

    fixture = Fixture('openai-chat-completions', [
        {'text': 'COPY_START ' + copy_text + ' COPY_END'}, blocked_reply,
    ])
    try:
        with tempfile.TemporaryDirectory(prefix='saya-tui-selection-') as directory:
            root = Path(directory)
            (root / 'workspace').mkdir()
            config = configuration(root, fixture)
            terminal = invoke(binary, root, config)
            try:
                terminal.resize(160, 45)
                terminal.title()
                terminal.line('Show text for the selection check')
                terminal.until('COPY_START')
                wait_idle(terminal, root)
                terminal.settle()
                if b'\x1b[?1002h' not in terminal.raw:
                    raise RuntimeError('terminal did not enable drag reporting')

                def clipboard_messages():
                    return [base64.b64decode(match.group(1), validate=True).decode('utf-8')
                            for match in re.finditer(rb'\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)',
                                                     bytes(terminal.raw))]

                def locate(text):
                    width = terminal.screen.main_width()
                    bottom = terminal.screen.status_row()
                    if width is None or bottom is None:
                        return None
                    return next(((column, row) for row in range(2, bottom)
                                 for column in range(1, width - 1)
                                 if terminal.screen.cells[row][column] and
                                 ''.join(terminal.screen.cells[row][column:width - 1]).startswith(text)), None)

                def has_selection():
                    return any(background == selected_style[1]
                               for row in terminal.screen.colors for _foreground, background in row)

                def drag(text, columns):
                    terminal.wait(lambda: locate(text) is not None, 'visible selection text ' + text)
                    column, row = locate(text)
                    terminal.send(f'\x1b[<0;{column + 1};{row + 1}M'
                                  f'\x1b[<32;{column + columns};{row + 1}M'
                                  f'\x1b[<0;{column + columns};{row + 1}m')
                    terminal.wait(lambda: terminal.screen.body_style(text) == selected_style,
                                  'visible dragged selection')
                    terminal.settle()
                    return column, row

                if clipboard_messages():
                    raise RuntimeError('terminal emitted clipboard data before user selection')
                terminal.send(draft)
                terminal.wait(lambda: terminal.editor_text() == draft, 'copy draft')
                drag(copy_text, 5)
                if clipboard_messages():
                    raise RuntimeError('drag selection copied without Ctrl+C')
                terminal.send(b'\x03')
                terminal.wait(lambda: clipboard_messages() == [copy_text], 'Unicode OSC 52 copy')
                terminal.wait(lambda: not has_selection(), 'copied selection cleared')
                if terminal.editor_text() != draft:
                    raise RuntimeError('selection copy changed the input draft')

                drag(copy_text, 5)
                terminal.send(b'\x1b')
                terminal.wait(lambda: not has_selection(), 'Esc cleared selection')
                terminal.settle()
                drag(copy_text, 5)
                terminal.resize(100, 32)
                terminal.wait(lambda: terminal.screen.main_width() == 100 and
                              locate(copy_text) is not None and not has_selection(),
                              'resize cleared selection after repaint')
                terminal.settle()
                column, row = drag(copy_text, 5)
                terminal.wheel(column, row, down=False)
                terminal.wait(lambda: not has_selection(), 'scroll cleared selection')
                terminal.settle()
                if clipboard_messages() != [copy_text] or terminal.editor_text() != draft:
                    raise RuntimeError('selection clearing copied data or changed the draft')

                terminal.send(b'\x7f' * len(draft))
                terminal.wait(lambda: terminal.editor_text() == '', 'cleared copy draft')
                terminal.line('/profiles')
                terminal.until('Configured profiles · UI')
                terminal.settle()
                details_title = 'Configured profiles · UI'
                drag(details_title, len(details_title))
                if clipboard_messages() != [copy_text]:
                    raise RuntimeError('details selection copied without Ctrl+C')
                terminal.send(b'\x03')
                terminal.wait(lambda: clipboard_messages() == [copy_text, details_title], 'details OSC 52 copy')
                terminal.wait(lambda: not has_selection(), 'copied details selection cleared')
                terminal.send(b'\x1b')
                terminal.wait(lambda: 'Configured profiles · UI' not in terminal.screen.details_text() and
                              locate(copy_text) is not None, 'details closed')
                terminal.settle()

                terminal.line('Keep the model request active during selection')
                terminal.wait(lambda: len(fixture.requests) == 2 and terminal.screen.status() == 'Thinking',
                              'active model request before copy')
                terminal.send(draft)
                terminal.wait(lambda: terminal.editor_text() == draft, 'active-run copy draft')
                drag(copy_text, 5)
                if clipboard_messages() != [copy_text, details_title]:
                    raise RuntimeError('active selection copied without Ctrl+C')
                terminal.send(b'\x03')
                terminal.wait(lambda: clipboard_messages() == [copy_text, details_title, copy_text],
                              'active-run OSC 52 copy')
                terminal.settle()
                if terminal.screen.status() != 'Thinking' or terminal.editor_text() != draft:
                    raise RuntimeError('selection copy cancelled the active run or changed its draft')
                gate.set()
                terminal.until('CLIPBOARD_RUN_DONE')
                result = wait_idle(terminal, root, count=2)
                if result['status'] != 'completed' or len(fixture.requests) != 2:
                    raise RuntimeError('selection copy interrupted or replayed the model request')
                terminal.send(b'\x7f' * len(draft))
                terminal.wait(lambda: terminal.editor_text() == '', 'cleared active-run draft')
                terminal.finish()
                if b'\x1b[?1002l' not in terminal.raw:
                    raise RuntimeError('terminal did not restore drag reporting')
                copied = clipboard_messages()
                if copied != [copy_text, details_title, copy_text]:
                    raise RuntimeError('terminal emitted unexpected clipboard data during run completion or exit')
                return {'clipboard_texts': copied, 'drag_reporting_restored': True}
            finally:
                gate.set()
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
    screens['run_feedback'] = run_feedback_journey(binary)
    passed('mode-activity-animation-run-summary-and-unknown-usage')
    screens['selection_clipboard'] = selection_and_clipboard_journey(binary)
    passed('drag-selection-unicode-osc52-draft-and-active-run')
    mode_permission_matrix(binary)
    passed('four-modes-three-permissions')
    command_completion_journey(binary)
    passed('command-completion-editing-and-approval-priority')
    empty_tab_mode_journey(binary)
    passed('empty-tab-mode-cycle-pending-and-approval')
    reference_layout_and_title_journey(binary)
    passed('reference-layout-input-meter-and-window-title')
    markdown_table_links_journey(binary)
    passed('markdown-table-links-stream-resize-scroll-and-source')
    markdown_source_and_footnotes_journey(binary)
    passed('markdown-source-render-footnotes-and-original-history')
    manual_tools_and_clarification(binary)
    passed('manual-read-search-background-query-and-clarification')
    thinking_tools_and_mouse_journey(binary)
    passed('thinking-tool-folds-mouse-scroll-and-restore')
    todos_manual_plan_and_restore_journey(binary)
    passed('todos-manual-plan-sidebar-and-restore')
    summary_tool_cards_journey(binary)
    passed('short-tool-summaries-failure-truncation-and-original-json')
    command_palette_modal_journey(binary)
    passed('centered-command-modal-navigation-mouse-and-approval-isolation')
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
    first_task_validation_failure(binary)
    passed('first-task-validation-draft-and-retry')
    active_hangup_cleanup(binary)
    passed('active-hangup-owned-command-cleanup')
    report = {'schema_version': 1, 'binary': str(binary), 'checks': results, 'terminal_screens': screens, 'status': 'passed'}
    if args.report:
        args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print('native TUI journey passed:', ', '.join(results))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
