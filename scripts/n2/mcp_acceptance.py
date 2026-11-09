#!/usr/bin/env python3
"""Offline MCP acceptance through an immutable copy of the ordinary Saya CLI."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import pty
import re
import select
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, urlparse
from urllib.request import urlopen

REPOSITORY = Path(__file__).resolve().parents[2]
sys.dont_write_bytecode = True
sys.path.insert(0, str(REPOSITORY / 'scripts' / 'release'))
from tui_journey import Terminal, protocol_stream

TOKEN = 'n2-mcp-controlled-fixture-token'
ACCESS = 'n2-oauth-controlled-access-token'
REFRESH = 'n2-oauth-controlled-refresh-token'
INITIAL_ACCESS = 'n2-oauth-controlled-initial-access-token'
INITIAL_REFRESH = 'n2-oauth-controlled-initial-refresh-token'


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def source_digest():
    paths = subprocess.check_output(['git', 'ls-files', '-co', '--exclude-standard', '-z'], cwd=REPOSITORY)
    hashed = hashlib.sha256()
    for name in sorted(set(paths.split(b'\0')) - {b''}):
        path = REPOSITORY / os.fsdecode(name)
        if path.is_file():
            hashed.update(name + b'\0' + hashlib.sha256(path.read_bytes()).digest())
    return hashed.hexdigest()


def tool_catalog(scenario, lists):
    schema = {'type': 'object', 'properties': {'message': {'type': 'string'}},
              'required': ['message'], 'additionalProperties': False}
    if scenario == 'catalog_changed' and lists > 1:
        schema['properties']['message']['type'] = 'integer'
    if scenario == 'bad_schema':
        schema['type'] = 'invalid-type'
    return [{'name': 'echo', 'description': 'Controlled remote echo', 'inputSchema': schema},
            {'name': 'mutate', 'description': 'Unclassified external capability',
             'inputSchema': {'type': 'object'}}]


def mcp_result(request, scenario, lists):
    method = request.get('method')
    if method == 'server/discover':
        return {'resultType': 'complete', 'supportedVersions': ['2026-07-28'],
                'capabilities': {'tools': {}}, 'ttlMs': 0, 'cacheScope': 'private'}
    if method == 'tools/list':
        return {'resultType': 'complete', 'tools': tool_catalog(scenario, lists),
                'ttlMs': 0, 'cacheScope': 'private'}
    if method == 'tools/call':
        if scenario == 'needs_input':
            return {'resultType': 'input_required', 'inputRequests': {'name': {
                'method': 'elicitation/create', 'params': {'message': 'Need a user name',
                'requestedSchema': {'type': 'object', 'properties': {'name': {'type': 'string'}}}}}},
                'requestState': 'fixture-state'}
        text = 'x' * 20000 if scenario == 'large_output' else 'echo|' + TOKEN
        return {'resultType': 'complete', 'content': [{'type': 'text', 'text': text}], 'isError': False}
    return None


STDIO_SERVER = r'''import json,os,sys,time
pid_path,log_path,scenario=sys.argv[1:4]
open(pid_path+'.tmp','w').write(str(os.getpid()))
os.replace(pid_path+'.tmp',pid_path)
lists=0
for line in sys.stdin:
    request=json.loads(line)
    method=request.get('method')
    with open(log_path,'a') as log: log.write(method+'\n')
    if 'id' not in request: continue
    if (scenario=='slow_connect' and method=='server/discover') or (scenario=='slow_catalog' and method=='tools/list'):
        time.sleep(5 if scenario=='slow_catalog' else 3)
    if method=='server/discover':
        result={'resultType':'complete','supportedVersions':['2026-07-28'],'capabilities':{'tools':{}},'ttlMs':0,'cacheScope':'private'}
    elif method=='tools/list':
        lists+=1
        schema={'type':'object','properties':{'message':{'type':'string'}},'required':['message'],'additionalProperties':False}
        result={'resultType':'complete','tools':[{'name':'echo','description':'Controlled stdio echo','inputSchema':schema}],'ttlMs':0,'cacheScope':'private'}
    elif method=='tools/call':
        result={'resultType':'complete','content':[{'type':'text','text':'echo|'+os.environ['SERVER_TOKEN']}],'isError':False}
    else: continue
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
'''


class Fixture:
    def __init__(self, protocol='openai-chat-completions', scenario='normal'):
        self.protocol, self.scenario = protocol, scenario
        self.models, self.calls, self.lists, self.refreshes = [], 0, 0, 0
        self.oauth = False
        self.auth_requests = []
        self.tool_started = threading.Event()
        self.tool_release = threading.Event()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def reply(self, value, status=200, content_type='application/json'):
                data = value.encode() if isinstance(value, str) else json.dumps(value).encode()
                self.send_response(status)
                self.send_header('Content-Type', content_type)
                self.send_header('Content-Length', str(len(data)))
                self.send_header('Connection', 'close')
                if status == 401:
                    self.send_header('WWW-Authenticate', 'Bearer')
                self.end_headers()
                try:
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def do_GET(self):
                if 'oauth-protected-resource' in self.path:
                    self.reply({'resource': owner.endpoint,
                        'authorization_servers': [owner.origin + '/'], 'scopes_supported': ['read']})
                elif 'oauth-authorization-server' in self.path or 'openid-configuration' in self.path:
                    self.reply({'issuer': owner.origin + '/',
                        'authorization_endpoint': owner.origin + '/authorize',
                        'token_endpoint': owner.origin + '/token',
                        'registration_endpoint': owner.origin + '/register',
                        'response_types_supported': ['code'],
                        'grant_types_supported': ['authorization_code', 'refresh_token'],
                        'code_challenge_methods_supported': ['S256'],
                        'authorization_response_iss_parameter_supported': True})
                elif self.path.startswith('/authorize?'):
                    query = parse_qs(urlparse(self.path).query)
                    owner.auth_requests.append(query)
                    callback = query['redirect_uri'][0] + '?' + urlencode({
                        'code': 'n2-controlled-authorization-code', 'state': query['state'][0],
                        'iss': owner.origin + '/'})
                    self.send_response(302)
                    self.send_header('Location', callback)
                    self.send_header('Content-Length', '0')
                    self.end_headers()
                else:
                    self.reply({}, 404)

            def do_POST(self):
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                if self.path == '/register':
                    registration = json.loads(body)
                    self.reply({'client_id': 'saya-acceptance-public-client',
                                'redirect_uris': registration['redirect_uris']})
                    return
                if self.path == '/token':
                    form = parse_qs(body.decode())
                    refreshing = form['grant_type'] == ['refresh_token']
                    if refreshing:
                        owner.refreshes += 1
                    self.reply({'access_token': ACCESS if refreshing else INITIAL_ACCESS,
                        'refresh_token': REFRESH if refreshing else INITIAL_REFRESH,
                        'token_type': 'Bearer', 'scope': 'read',
                        'expires_in': 3600 if refreshing else 1})
                    return
                request = json.loads(body)
                if self.path.startswith('/v1/'):
                    owner.models.append(request)
                    tools = request.get('tools', [])
                    names = [tool.get('name') or tool.get('function', {}).get('name') for tool in tools]
                    name = next((name for name in names if name and name.startswith('mcp__') and '__echo_' in name), None)
                    call = ('controlled-call', name, {'message': 'hello'}) if len(owner.models) == 1 and name else None
                    response = protocol_stream(owner.protocol, text='MCP_ACCEPTED', call=call)
                    self.reply(response, content_type='text/event-stream')
                    return
                method = request.get('method')
                if ((owner.scenario == 'slow_connect' and method == 'server/discover')
                        or (owner.scenario == 'slow_catalog' and method == 'tools/list')):
                    time.sleep(3)
                if method == 'tools/list':
                    owner.lists += 1
                if method == 'tools/call':
                    owner.calls += 1
                    if owner.scenario == 'tui_hold':
                        owner.tool_started.set()
                        if not owner.tool_release.wait(15):
                            self.reply({'error': 'controlled TUI call was not released'}, 504)
                            return
                    if owner.scenario == 'disconnect':
                        self.close_connection = True
                        return
                    if owner.scenario == '401':
                        self.reply({}, 401)
                        return
                if owner.oauth and self.headers.get('Authorization') != 'Bearer ' + ACCESS:
                    self.reply({}, 401)
                    return
                result = mcp_result(request, owner.scenario, owner.lists)
                if owner.oauth and method == 'tools/call' and result and result.get('content'):
                    result['content'][0]['text'] = 'echo|' + ACCESS
                if 'id' not in request:
                    self.reply('', 202)
                elif result is None:
                    self.reply({'jsonrpc': '2.0', 'id': request['id'],
                                'error': {'code': -32601, 'message': 'unsupported fixture method'}})
                else:
                    self.reply({'jsonrpc': '2.0', 'id': request['id'], 'result': result})

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.origin = 'http://127.0.0.1:' + str(self.server.server_port)
        self.endpoint = self.origin + '/mcp'
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


def configuration(root, fixture, transport='http', permission='bypass', reference=None, active_budget=None):
    roots = sorted({str(Path(sys.base_prefix).resolve()), str(Path(sys.executable).resolve().parents[1])})
    for toolchain in ['/opt/homebrew', '/usr/local', '/Library/Developer/CommandLineTools']:
        if Path(toolchain).is_dir() and toolchain not in roots:
            roots.append(toolchain)
    lines = ['schema_version=1', '[defaults]', 'profile="fixture"',
             'permissions=' + json.dumps(permission), '[profiles.fixture]',
             'protocol=' + json.dumps(fixture.protocol), 'base_url=' + json.dumps(fixture.origin + '/v1'),
             'model="controlled-fixture"', 'auth="none"', 'context_window_tokens=128000', 'stream=true',
             '[sandbox]', 'read_roots=' + json.dumps(roots), '[mcp.servers.fixture]',
             'read_only_tools=["echo"]', '[mcp.servers.fixture.transport]', 'type=' + json.dumps(transport)]
    if transport == 'stdio':
        server = root / 'workspace' / 'server.py'
        server.write_text(STDIO_SERVER)
        argv = [str(Path(sys.executable).resolve()), '-u', str(server),
                str(root / 'workspace' / 'server.pid'), str(root / 'workspace' / 'server.log'), fixture.scenario]
        lines += ['argv=' + json.dumps(argv), 'env={SERVER_TOKEN="N2_MCP_TOKEN"}']
    else:
        lines += ['endpoint=' + json.dumps(fixture.endpoint), 'allow_insecure_http=true', 'scopes=["read"]']
        if reference:
            lines += ['credential_ref=' + json.dumps(reference)]
        elif not fixture.oauth:
            lines += ['bearer_env="N2_MCP_TOKEN"']
    lines += ['[mcp.servers.fixture.limits]', 'connect_timeout_seconds=5',
              'request_timeout_seconds=5', 'max_result_bytes=4096' if fixture.scenario == 'large_output' else 'max_result_bytes=65536']
    if active_budget is not None:
        lines += ['[limits]', 'active_run_timeout_seconds=' + str(active_budget)]
    path = root / 'config.toml'
    path.write_text('\n'.join(lines) + '\n')
    return path


def environment(root):
    return {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': str(root / 'home'),
            'TERM': 'xterm-256color', 'XDG_DATA_HOME': str(root / 'data'),
            'XDG_STATE_HOME': str(root / 'state'), 'XDG_CACHE_HOME': str(root / 'cache'),
            'N2_MCP_TOKEN': TOKEN, 'NO_PROXY': '127.0.0.1,localhost'}


def arguments(binary, root, config, output='json'):
    return [str(binary), '--config', str(config), '--cwd', str(root / 'workspace'), '--output', output]


def records(text):
    result = []
    for line in text.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            result.append(value)
    return result


def final_result(text):
    values = records(text)
    for value in reversed(values):
        if 'status' in value:
            return value
        if value.get('type') == 'run.finished':
            return value['data']
    raise AssertionError('CLI did not emit a final result')


def invoke(binary, root, config, extra, output='json'):
    result = subprocess.run(arguments(binary, root, config, output) + extra,
        cwd=root / 'workspace', env=environment(root), text=True, capture_output=True, timeout=40)
    require(TOKEN not in result.stdout + result.stderr, 'credential escaped CLI output')
    return result


def invoke_pty(argv, root, fixture=None):
    child, master = pty.fork()
    if child == 0:
        os.chdir(root / 'workspace')
        os.execve(argv[0], argv, environment(root))
    raw = bytearray()
    answers = 0
    opened = False
    browser_threads = []
    deadline = time.monotonic() + 40
    status = None
    try:
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], .05)
            if ready:
                try:
                    part = os.read(master, 65536)
                except OSError:
                    part = b''
                raw.extend(part)
            text = raw.decode(errors='replace')
            count = text.count('[y/N]')
            while answers < count:
                os.write(master, b'y\n')
                answers += 1
            if fixture and not opened:
                match = re.search(r'http://127\.0\.0\.1:\d+/authorize\?[^\r\n ]+', text)
                if match:
                    opened = True
                    browser = threading.Thread(target=lambda: urlopen(match.group(0), timeout=10).read(), daemon=True)
                    browser.start()
                    browser_threads.append(browser)
            waited, value = os.waitpid(child, os.WNOHANG)
            if waited:
                status = os.waitstatus_to_exitcode(value)
                # waitpid can win after the child flushed a final JSON line
                # while that line is still queued in the PTY master.
                for _ in range(16):
                    readable, _, _ = select.select([master], [], [], .01)
                    if not readable:
                        break
                    try:
                        remaining = os.read(master, 65536)
                    except OSError:
                        break
                    if not remaining:
                        break
                    raw.extend(remaining)
                break
        require(status is not None, 'PTY CLI did not finish before deadline')
    finally:
        if status is None:
            os.kill(child, signal.SIGKILL)
            os.waitpid(child, 0)
        os.close(master)
        for browser in browser_threads:
            browser.join(2)
    text = raw.decode(errors='replace').replace('\r\n', '\n')
    require(TOKEN not in text and ACCESS not in text and REFRESH not in text,
            'credential escaped PTY output')
    require(INITIAL_ACCESS not in text and INITIAL_REFRESH not in text, 'initial OAuth credential escaped PTY output')
    return status, text, answers


def stored(root):
    paths = list((root / 'data').rglob('sessions.sqlite3'))
    require(len(paths) == 1, 'normal CLI did not create exactly one session store')
    with sqlite3.connect(paths[0]) as connection:
        messages = [row[0] for row in connection.execute('SELECT item FROM messages ORDER BY ordinal')]
        tools = list(connection.execute('SELECT state,result FROM tool_calls'))
    joined = '\n'.join(messages + [str(row) for row in tools])
    require(not any(secret in joined for secret in [TOKEN, ACCESS, REFRESH, INITIAL_ACCESS, INITIAL_REFRESH]),
            'credential escaped SQLite history or result')
    return messages, tools


def local_methods(root):
    log = root / 'workspace' / 'server.log'
    return log.read_text().splitlines() if log.exists() else []


def validate_reaped(root):
    path = root / 'workspace' / 'server.pid'
    if path.exists():
        pid = int(path.read_text())
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        raise AssertionError('owned stdio server remained alive after CLI return')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=Path)
    parser.add_argument('--report-dir', type=Path, default=REPOSITORY / 'target/n2/acceptance')
    args = parser.parse_args()
    report_dir = args.report_dir.resolve()
    report_dir.mkdir(parents=True, exist_ok=True)
    original = args.binary.resolve(strict=True)
    expected_hash = digest(original)
    binary = report_dir / ('mcp-saya-' + expected_hash[:16])
    shutil.copy2(original, binary)
    binary.chmod(0o700)
    require(digest(binary) == expected_hash, 'acceptance executable copy hash differs')
    report = {'schema_version': 1, 'suite': 'mcp', 'binary_sha256': expected_hash,
              'binary': str(binary), 'source_commit': subprocess.check_output(
                  ['git', 'rev-parse', 'HEAD'], cwd=REPOSITORY, text=True).strip(),
              'platform': platform.platform(), 'architecture': platform.machine(), 'cases': []}
    report['source_tree_sha256'] = source_digest()
    report['source_dirty'] = bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=REPOSITORY))

    def case(name, operation):
        started = time.monotonic()
        with tempfile.TemporaryDirectory(prefix='saya-mcp-acceptance-') as temporary:
            root = Path(temporary).resolve()
            (root / 'workspace').mkdir()
            (root / 'home').mkdir()
            try:
                details = operation(root)
                require(digest(binary) == expected_hash, 'acceptance executable changed during case')
                report['cases'].append({'name': name, 'passed': True,
                    'elapsed_seconds': round(time.monotonic()-started, 3), **details})
                print('PASS ' + name, flush=True)
            except Exception as error:
                report['cases'].append({'name': name, 'passed': False,
                    'elapsed_seconds': round(time.monotonic()-started, 3), 'error': str(error)})
                print('FAIL ' + name + ' ' + str(error), flush=True)
        (report_dir / 'mcp-report.json').write_text(json.dumps(report, indent=2) + '\n')

    def roundtrip(root, protocol, transport, output='json'):
        fixture = Fixture(protocol)
        try:
            config = configuration(root, fixture, transport)
            result = invoke(binary, root, config, ['run', 'controlled MCP acceptance'], output)
            (report_dir / ('mcp-' + protocol + '-' + transport + '-' + output + '.log')).write_text(result.stdout + result.stderr)
            final = final_result(result.stdout)
            require(result.returncode == 0 and final['status'] == 'completed', 'MCP round trip failed ' + str(final))
            require(len(fixture.models) == 2, 'MCP round trip did not return tool result to model')
            calls = fixture.calls if transport == 'http' else local_methods(root).count('tools/call')
            require(calls == 1, 'MCP tool was replayed or skipped')
            messages, _ = stored(root)
            require('[REDACTED]' in '\n'.join(messages), 'MCP credential was not projected as redacted history')
            validate_reaped(root)
            if output == 'jsonl':
                states = [value['data']['servers'] for value in records(result.stdout) if value.get('type') == 'mcp.status']
                require(states and any(server['state'] == 'connected' and server['tools'] == 2
                    for state in states for server in state), 'normal JSONL omitted the actual registered MCP connection')
                require(states[-1][0]['state'] == 'stopped' and states[-1][0]['tools'] == 0,
                    'normal JSONL omitted confirmed MCP cleanup')
            return {'model_requests': len(fixture.models), 'tool_posts': calls, 'status': final['status']}
        finally:
            fixture.close()

    for protocol in ['openai-chat-completions', 'openai-responses', 'anthropic-messages']:
        for transport in ['stdio', 'http']:
            case(protocol + '-' + transport, lambda root, p=protocol, t=transport: roundtrip(root, p, t))
    case('jsonl-redaction', lambda root: roundtrip(root, 'openai-chat-completions', 'http', 'jsonl'))

    def lifecycle(root):
        fixture = Fixture()
        try:
            config = configuration(root, fixture)
            code, text, _ = invoke_pty(arguments(binary, root, config, 'jsonl') + ['run', 'PTY lifecycle events'], root)
            (report_dir / 'mcp-pty-lifecycle.log').write_text(text)
            require(code == 0 and final_result(text)['status'] == 'completed', 'PTY lifecycle run failed')
            events = [value['data']['servers'][0] for value in records(text) if value.get('type') == 'mcp.status']
            states = [value['state'] for value in events]
            require(states == ['configured', 'connecting', 'connected', 'stopped'],
                    'PTY did not receive the persisted real MCP lifecycle ' + str(states))
            require(events[2]['tools'] == 2 and events[3]['tools'] == 0, 'PTY lifecycle catalogue count was inaccurate')
            return {'states': states, 'tool_posts': fixture.calls, 'model_requests': len(fixture.models)}
        finally:
            fixture.close()
    case('pty-real-mcp-lifecycle', lifecycle)

    def tui_lifecycle(root):
        fixture = Fixture(scenario='tui_hold')
        terminal = None
        try:
            config = configuration(root, fixture)
            config.write_text(config.read_text().replace('request_timeout_seconds=5', 'request_timeout_seconds=15'))
            terminal = Terminal([str(binary), '--config', str(config), '--cwd', str(root / 'workspace')],
                                root / 'workspace', environment(root))
            terminal.resize(160, 40)
            terminal.title()

            def sidebar():
                return [terminal.screen.sidebar_row_text(row) for row in range(terminal.screen.rows)]

            def has_status(state, tools):
                return any('fixture' in row and state in row and str(tools) + 't' in row for row in sidebar())

            terminal.wait(lambda: has_status('Configured', 0), 'configured MCP sidebar before a run')
            terminal.settle()
            require(not fixture.models and fixture.calls == 0, 'idle TUI contacted a model or executed an MCP tool')
            (report_dir / 'mcp-tui-configured.txt').write_text(terminal.screen.text)
            terminal.line('Run the controlled remote echo tool')
            terminal.wait(lambda: fixture.tool_started.is_set() and has_status('Connected', 2),
                          'real connected MCP sidebar during a held remote tool', timeout=20)
            terminal.settle()
            connected = [row for row in sidebar() if 'fixture' in row and 'Connected' in row]
            require(connected and terminal.mode() == 'WORK', 'TUI connected sidebar or work mode was inaccurate')
            (report_dir / 'mcp-tui-connected.txt').write_text(terminal.screen.text)
            (report_dir / 'mcp-tui-connected-cells.json').write_text(json.dumps({
                'columns': 160, 'rows': 40, 'cells': terminal.screen.cells,
                'colors': terminal.screen.colors}, ensure_ascii=False) + '\n')
            fixture.tool_release.set()
            terminal.until('MCP_ACCEPTED')
            terminal.wait(lambda: has_status('Stopped', 0) and terminal.screen.status() == 'Completed',
                          'confirmed MCP cleanup and completed TUI task')
            terminal.settle()
            stopped = [row for row in sidebar() if 'fixture' in row and 'Stopped' in row]
            require(stopped and terminal.mode() == 'WORK', 'TUI final sidebar or work mode changed unexpectedly')
            (report_dir / 'mcp-tui-stopped.txt').write_text(terminal.screen.text)
            terminal.finish()
            raw = terminal.raw.decode(errors='replace')
            require(TOKEN not in raw, 'MCP credential escaped real TUI output')
            (report_dir / 'mcp-tui-raw.ansi').write_bytes(terminal.raw)
            stored(root)
            database = next((root / 'data').rglob('sessions.sqlite3'))
            with sqlite3.connect(database) as connection:
                runs = list(connection.execute('SELECT id,session_id,result FROM runs'))
                rows = list(connection.execute("SELECT run_id,session_id,reference FROM change_log WHERE kind='activity' ORDER BY seq"))
            require(len(runs) == 1 and json.loads(runs[0][2])['status'] == 'completed', 'TUI run was not durably completed')
            run_id, session_id, _ = runs[0]
            events = []
            for event_run, event_session, reference in rows:
                payload = json.loads(reference)
                if payload.get('type') == 'mcp.status':
                    require(event_run == run_id and event_session == session_id, 'MCP activity was paired with another run or session')
                    events.append(payload['data']['servers'][0])
            require([event['state'] for event in events] == ['configured', 'connecting', 'connected', 'stopped'],
                    'SQLite omitted or misordered the real TUI MCP lifecycle')
            require(events[2]['tools'] == 2 and events[3]['tools'] == 0, 'SQLite MCP catalogue counts differed from the sidebar')
            require(fixture.calls == 1 and len(fixture.models) == 2, 'TUI tool or model request count was unexpected')
            return {'columns': 160, 'rows': 40, 'connected_sidebar': connected,
                    'stopped_sidebar': stopped, 'sqlite_run_id': run_id,
                    'sqlite_states': [event['state'] for event in events],
                    'tool_posts': fixture.calls, 'model_requests': len(fixture.models),
                    'terminal_restoration': 'passed', 'credential_redaction': 'passed'}
        finally:
            fixture.tool_release.set()
            if terminal:
                terminal.close()
            fixture.close()
    case('real-tui-mcp-sidebar-lifecycle', tui_lifecycle)

    def manual(root, interactive):
        fixture = Fixture()
        try:
            config = configuration(root, fixture, 'stdio', 'manual')
            if interactive:
                code, text, approvals = invoke_pty(arguments(binary, root, config) + ['run', '--interactive', 'manual startup and tool approval'], root)
                (report_dir / 'mcp-manual-stdio.log').write_text(text)
                require(code == 0 and final_result(text)['status'] == 'completed', 'interactive Manual Approval failed')
                require(approvals == 2, 'Manual Approval did not approve startup and each MCP tool separately')
                require(local_methods(root).count('tools/call') == 1, 'manual MCP tool was skipped or replayed')
                validate_reaped(root)
                return {'approvals': approvals, 'tool_posts': 1}
            result = invoke(binary, root, config, ['run', 'manual noninteractive'])
            final = final_result(result.stdout)
            require(final['status'] == 'needs_confirmation', 'manual noninteractive startup did not stop for confirmation')
            require(not local_methods(root) and not fixture.models, 'unapproved stdio startup or model request occurred')
            return {'status': final['status'], 'model_requests': 0, 'server_starts': 0}
        finally:
            fixture.close()
    case('manual-stdio-startup-and-tool', lambda root: manual(root, True))
    case('manual-noninteractive', lambda root: manual(root, False))

    def plan(root, transport):
        fixture = Fixture()
        try:
            config = configuration(root, fixture, transport)
            result = invoke(binary, root, config, ['--mode', 'plan', 'run', 'readonly MCP acceptance'])
            final = final_result(result.stdout)
            require(final['status'] == 'completed', 'PLAN failed ' + str(final))
            if transport == 'stdio':
                require(not local_methods(root) and not (root / 'workspace/server.pid').exists(), 'PLAN launched a stdio process')
                require(len(fixture.models) == 1, 'PLAN stdio attempted a tool')
            else:
                require(fixture.calls == 1, 'PLAN failed to call a host-classified read-only HTTP tool')
                tools = fixture.models[0].get('tools', [])
                names = [tool.get('name') or tool.get('function', {}).get('name') for tool in tools]
                require(not any(name and '__mutate_' in name for name in names), 'PLAN exposed an unclassified external capability')
            return {'status': final['status'], 'server_starts': int(transport == 'http'), 'tool_posts': fixture.calls}
        finally:
            fixture.close()
    case('plan-stdio-never-starts', lambda root: plan(root, 'stdio'))
    case('plan-http-host-readonly', lambda root: plan(root, 'http'))

    def budget(root, transport, scenario):
        fixture = Fixture(scenario=scenario)
        try:
            # Native debug interpreter startup can consume close to one second.
            # Leave a complete second for the catalogue stage being tested.
            active_budget = 2 if transport == 'stdio' and scenario == 'slow_catalog' else 1
            delay = 5 if active_budget == 2 else 3
            config = configuration(root, fixture, transport, active_budget=active_budget)
            started = time.monotonic()
            result = invoke(binary, root, config, ['run', 'bounded MCP service startup'])
            elapsed = time.monotonic() - started
            final = final_result(result.stdout)
            require(elapsed < active_budget + 1.5, 'MCP service wait exceeded its active budget ' + str(elapsed))
            expected = 'outcome_unknown' if transport == 'stdio' and scenario == 'slow_connect' else 'limit_reached'
            require(final['stop_reason'] == expected, 'MCP budget expiry had an incorrect outcome ' + str(final))
            require(not fixture.models and fixture.calls == 0, 'MCP budget expiry dispatched a model or remote tool')
            require(local_methods(root).count('tools/call') == 0, 'MCP budget expiry dispatched a stdio tool')
            if transport == 'stdio' and scenario == 'slow_catalog':
                require('tools/list' in local_methods(root), 'native stdio did not reach the catalogue stage')
            validate_reaped(root)
            return {'run_elapsed_seconds': round(elapsed, 3), 'active_budget_seconds': active_budget,
                    'service_delay_seconds': delay, 'stop_reason': final['stop_reason'],
                    'model_requests': 0, 'tool_posts': 0}
        finally:
            fixture.close()
    for transport in ['stdio', 'http']:
        for scenario in ['slow_connect', 'slow_catalog']:
            case(transport + '-active-budget-' + scenario,
                 lambda root, t=transport, s=scenario: budget(root, t, s))

    def boundary(root, scenario):
        fixture = Fixture(scenario=scenario)
        try:
            config = configuration(root, fixture)
            result = invoke(binary, root, config, ['run', 'boundary ' + scenario])
            final = final_result(result.stdout)
            if scenario == 'disconnect':
                require(final['status'] == 'failed' and final['stop_reason'] == 'outcome_unknown', 'post-dispatch failure was not unknown ' + str(final))
                require(fixture.calls == 1 and len(fixture.models) == 1, 'uncertain tool was retried or model continued')
                _, tools = stored(root)
                require(any(state == 'unknown' for state, _ in tools), 'unknown outcome was not durable')
            elif scenario == 'large_output':
                require(final['status'] == 'completed' and fixture.calls == 1 and len(fixture.models) == 2,
                        'bounded long tool output did not complete one ordinary round trip')
                _, tools = stored(root)
                results = [json.loads(result) for _, result in tools if result]
                require(any(result['truncated'] and len(json.dumps(result)) < 8192 for result in results),
                        'long tool output was not bounded and labelled truncated')
            elif scenario == 'needs_input':
                require(final['status'] == 'needs_input' and fixture.calls == 1 and len(fixture.models) == 1, 'input_required did not stop the run')
            elif scenario == '401':
                require(final['status'] == 'needs_input' and fixture.calls == 1, 'HTTP 401 replayed tools/call')
            elif scenario == 'bad_schema':
                require(final['status'] == 'failed' and fixture.calls == 0 and not fixture.models, 'unsafe catalogue reached model or dispatch')
            else:
                require(final['status'] == 'completed' and fixture.calls == 0, 'changed input schema was not revalidated before dispatch')
            return {'status': final['status'], 'stop_reason': final.get('stop_reason'),
                    'tool_posts': fixture.calls, 'model_requests': len(fixture.models)}
        finally:
            fixture.close()
    for scenario in ['disconnect', 'needs_input', 'large_output', 'bad_schema', 'catalog_changed', '401']:
        case('http-' + scenario, lambda root, s=scenario: boundary(root, s))

    def resume(root):
        fixture = Fixture()
        try:
            config = configuration(root, fixture)
            first = invoke(binary, root, config, ['run', 'initial controlled task'])
            session = final_result(first.stdout)['session_id']
            require(fixture.calls == 1, 'initial tool call did not execute exactly once')
            second = invoke(binary, root, config, ['run', '--session', session, 'continue without replay'])
            final = final_result(second.stdout)
            require(final['status'] == 'completed' and fixture.calls == 1, 'session resume replayed prior MCP side effects')
            return {'session_reused': final['session_id'] == session, 'tool_posts': fixture.calls, 'model_requests': len(fixture.models)}
        finally:
            fixture.close()
    case('session-resume-does-not-replay', resume)

    def oauth(root):
        fixture = Fixture()
        fixture.oauth = True
        try:
            config = configuration(root, fixture)
            code, text, _ = invoke_pty(arguments(binary, root, config) + ['mcp', 'login', 'fixture', '--approve-scopes', '--client-id', 'saya-public-fixture'], root, fixture)
            require(code == 0, 'OAuth CLI login failed ' + text)
            values = records(text)
            reference = next(value['data']['reference'] for value in reversed(values) if value.get('data', {}).get('reference'))
            require(len(fixture.auth_requests) == 1 and fixture.auth_requests[0].get('scope') == ['read'], 'OAuth scopes expanded without user approval')
            config = configuration(root, fixture, reference=reference)
            result = invoke(binary, root, config, ['run', 'use authorized HTTP MCP'])
            require(final_result(result.stdout)['status'] == 'completed', 'authorized MCP run failed')
            require(fixture.refreshes == 1 and fixture.calls == 1, 'OAuth did not preflight refresh once before one tool POST')
            messages, _ = stored(root)
            require('[REDACTED]' in '\n'.join(messages), 'newly refreshed token was not redacted before history persistence')
            require(not any(secret in json.dumps(fixture.models) for secret in [ACCESS, REFRESH, INITIAL_ACCESS, INITIAL_REFRESH]),
                    'a fresh OAuth credential entered a subsequent model request')
            metadata = invoke(binary, root, config, ['mcp', 'credentials', 'list'])
            require(ACCESS not in metadata.stdout and REFRESH not in metadata.stdout, 'credential list leaked values')
            revoked = invoke(binary, root, config, ['mcp', 'credentials', 'revoke', reference])
            require(revoked.returncode == 0, 'credential revocation failed')
            return {'login_callbacks': len(fixture.auth_requests), 'refreshes': fixture.refreshes,
                    'tool_posts': fixture.calls, 'revoked': True}
        finally:
            fixture.close()
    case('oauth-cli-pkce-refresh-and-revoke', oauth)
    report['passed'] = all(item['passed'] for item in report['cases'])
    (report_dir / 'mcp-report.json').write_text(json.dumps(report, indent=2) + '\n')
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    sys.exit(main())
