#!/usr/bin/env python3
"""Offline native acceptance for the supplied Saya image through normal CLI entry points."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import uuid


CAPTURE_BYTES = 1024 * 1024
MAX_REQUESTS = 64


PROBE_SOURCE = r"""
#ifdef __APPLE__
#define _DARWIN_C_SOURCE 1
#endif
#define _POSIX_C_SOURCE 200809L
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#ifdef __APPLE__
#include <sys/sysctl.h>
#endif
#include <time.h>
#include <unistd.h>

static int denied(void) { printf("DENIED errno=%d\n", errno); return 0; }
static int tcp(int family, int port) {
    int fd = socket(family, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    struct timeval limit = {1, 0};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &limit, sizeof(limit));
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &limit, sizeof(limit));
    int result;
    if (family == AF_INET) {
        struct sockaddr_in address = {0};
        address.sin_family = AF_INET;
        address.sin_port = htons((unsigned short)port);
        inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);
        result = connect(fd, (struct sockaddr *)&address, sizeof(address));
    } else {
        struct sockaddr_in6 address = {0};
        address.sin6_family = AF_INET6;
        address.sin6_port = htons((unsigned short)port);
        inet_pton(AF_INET6, "::1", &address.sin6_addr);
        result = connect(fd, (struct sockaddr *)&address, sizeof(address));
    }
    if (result < 0) { int saved = errno; close(fd); errno = saved; return -1; }
    return fd;
}
static int read_file(const char *path) {
    FILE *file = fopen(path, "rb");
    if (!file) return denied();
    char text[257] = {0};
    size_t count = fread(text, 1, sizeof(text) - 1, file);
    fclose(file);
    printf("READ %.*s\n", (int)count, text);
    return 0;
}
static int write_file(const char *path) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return denied();
    const char text[] = "N2_WRITE_PROBE";
    ssize_t count = write(fd, text, sizeof(text) - 1);
    close(fd);
    if (count != (ssize_t)(sizeof(text) - 1)) return denied();
    puts("WROTE");
    return 0;
}
static int udp(int family, int port) {
    int fd = socket(family, SOCK_DGRAM, 0);
    if (fd < 0) return denied();
    const char packet[] = "N2_RAW_DNS_TRANSPORT_PROBE";
    ssize_t sent;
    if (family == AF_INET) {
        struct sockaddr_in address = {0};
        address.sin_family = AF_INET;
        address.sin_port = htons((unsigned short)port);
        inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);
        sent = sendto(fd, packet, sizeof(packet), 0,
                      (struct sockaddr *)&address, sizeof(address));
    } else {
        struct sockaddr_in6 address = {0};
        address.sin6_family = AF_INET6;
        address.sin6_port = htons((unsigned short)port);
        inet_pton(AF_INET6, "::1", &address.sin6_addr);
        sent = sendto(fd, packet, sizeof(packet), 0,
                      (struct sockaddr *)&address, sizeof(address));
    }
    int saved = errno;
    close(fd);
    errno = saved;
    if (sent < 0) return denied();
    puts("SENT");
    return 0;
}
static int unix_socket(const char *path) {
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return denied();
    struct sockaddr_un address = {0};
    address.sun_family = AF_UNIX;
    if (strlen(path) >= sizeof(address.sun_path)) return 64;
    memcpy(address.sun_path, path, strlen(path) + 1);
    int result = connect(fd, (struct sockaddr *)&address, sizeof(address));
    int saved = errno;
    close(fd);
    errno = saved;
    if (result < 0) return denied();
    puts("CONNECTED");
    return 0;
}
static int proxy_port(void) {
    const char *configured = getenv("HTTP_PROXY");
    if (!configured) configured = getenv("http_proxy");
    char host[64] = {0};
    int port = 0;
    if (!configured || sscanf(configured, "http://%63[^:]:%d", host, &port) != 2
        || strcmp(host, "127.0.0.1")) return 0;
    return port;
}
static int network_error(int measured) {
    if (!measured) return denied();
    printf("NETWORK_FAILED errno=%d\n", errno);
    return 70;
}
static int loopback_origin_port(const char *origin, int tunnel) {
    const char *prefix = tunnel ? "https://127.0.0.1:" : "http://127.0.0.1:";
    size_t prefix_length = strlen(prefix);
    if (strnlen(origin, 129) > 128 || strncmp(origin, prefix, prefix_length)) return 0;
    const char *number = origin + prefix_length;
    if (*number < '0' || *number > '9') return 0;
    unsigned int port = 0;
    for (const char *cursor = number; *cursor; cursor++) {
        if (*cursor < '0' || *cursor > '9') return 0;
        port = port * 10 + (unsigned int)(*cursor - '0');
        if (port > 65535) return 0;
    }
    return (int)port;
}
static int proxy(const char *origin, int tunnel, int measured, int direct) {
    int target_port = measured ? loopback_origin_port(origin, tunnel) : 0;
    if (measured && !target_port) return 64;
    const char *authority = strstr(origin, "://");
    if (!authority) return 64;
    authority += 3;
    int port = direct ? target_port : proxy_port();
    if (port <= 0 || port > 65535) {
        if (measured) return 70;
        puts("NO_PROXY"); return 0;
    }
    struct timespec started = {0}, finished = {0};
    if (measured && clock_gettime(CLOCK_MONOTONIC, &started) < 0) return 70;
    int fd = tcp(AF_INET, port);
    if (fd < 0) return network_error(measured);
    char request[2048];
    int length;
    if (tunnel) {
        length = snprintf(request, sizeof(request),
            "CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n", authority, authority);
    } else if (direct) {
        length = snprintf(request, sizeof(request),
            "GET /probe HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n", authority);
    } else {
        length = snprintf(request, sizeof(request),
            "GET %s/probe HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n",
            origin, authority);
    }
    if (length < 0 || length >= (int)sizeof(request)
        || write(fd, request, (size_t)length) != length) { close(fd); return network_error(measured); }
    char response[2048] = {0};
    size_t used = 0;
    if (tunnel) {
        while (used < sizeof(response) - 1 && !strstr(response, "\r\n\r\n")) {
            ssize_t count = read(fd, response + used, 1);
            if (count <= 0) break;
            used += (size_t)count;
        }
        if (!strstr(response, "\r\n\r\n") || !strstr(response, " 200 ")
            || (measured && strncmp(response, "HTTP/1.1 200 ", 13)
                && strncmp(response, "HTTP/1.0 200 ", 13))) {
            close(fd);
            if (measured) return 70;
            puts("PROXY_DENIED"); return 0;
        }
        length = snprintf(request, sizeof(request),
            "GET /probe HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n", authority);
        if (length < 0 || length >= (int)sizeof(request)
            || write(fd, request, (size_t)length) != length) { close(fd); return network_error(measured); }
        memset(response, 0, sizeof(response));
        used = 0;
    }
    while (used < sizeof(response) - 1) {
        ssize_t count = read(fd, response + used, sizeof(response) - 1 - used);
        if (count < 0 && measured) { close(fd); return network_error(1); }
        if (count <= 0) break;
        used += (size_t)count;
    }
    int closed = close(fd);
    if (measured) {
        if (closed < 0 || clock_gettime(CLOCK_MONOTONIC, &finished) < 0) return 70;
        const char *body = strstr(response, "\r\n\r\n");
        if ((strncmp(response, "HTTP/1.0 200 ", 13) && strncmp(response, "HTTP/1.1 200 ", 13))
            || !body || used != (size_t)(body - response) + 4 + 16
            || memcmp(body + 4, "N2_PROXY_ALLOWED", 16)
            || !strstr(response, "\r\nContent-Length: 16\r\n")) return 70;
        double elapsed = (double)(finished.tv_sec - started.tv_sec) * 1000.0
            + (double)(finished.tv_nsec - started.tv_nsec) / 1000000.0;
        if (elapsed <= 0.0 || elapsed > 10000.0) return 70;
        printf("N2_NETWORK_MS=%.6f\n", elapsed);
        return 0;
    }
    puts(strstr(response, " 200 ") && strstr(response, "N2_PROXY_ALLOWED")
         ? "PROXY_ALLOWED" : "PROXY_DENIED");
    return 0;
}
static int hold(const char *path) {
    pid_t child = fork();
    if (child < 0) return 70;
    if (child == 0) {
        for (int index = 0; index < 100; index++) {
            int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0600);
            if (fd < 0 || write(fd, ".", 1) != 1) _exit(71);
            close(fd);
            struct timespec delay = {0, 100000000};
            nanosleep(&delay, NULL);
        }
        _exit(0);
    }
    int status = 0;
    while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
    return 0;
}
static int host_process_metadata(int pid) {
#ifdef __APPLE__
    int mib[] = {CTL_KERN, KERN_PROCARGS2, pid};
    char data[32768];
    size_t length = sizeof(data);
    if (sysctl(mib, 3, data, &length, NULL, 0) < 0) return denied();
    memset(data, 0, sizeof(data));
#else
    char path[128];
    snprintf(path, sizeof(path), "/proc/%d/environ", pid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return denied();
    char data = 0;
    ssize_t count = read(fd, &data, 1);
    close(fd);
    if (count < 0) return denied();
    data = 0;
#endif
    puts("HOST_PROCESS_METADATA_READABLE");
    return 0;
}
static int gate(const char *ready, const char *release) {
    int fd = open(ready, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0 || write(fd, "READY", 5) != 5) return 71;
    close(fd);
    for (int index = 0; index < 200; index++) {
        if (access(release, F_OK) == 0) return 0;
        struct timespec delay = {0, 50000000};
        nanosleep(&delay, NULL);
    }
    return 75;
}
int main(int argc, char **argv) {
    signal(SIGPIPE, SIG_IGN);
    if (argc < 2) return 64;
    if (!strcmp(argv[1], "read") && argc == 3) return read_file(argv[2]);
    if (!strcmp(argv[1], "write") && argc == 3) return write_file(argv[2]);
    if (!strcmp(argv[1], "unix") && argc == 3) return unix_socket(argv[2]);
    if (!strcmp(argv[1], "dns_udp") && argc == 3) return udp(AF_INET, atoi(argv[2]));
    if (!strcmp(argv[1], "dns_udp6") && argc == 3) return udp(AF_INET6, atoi(argv[2]));
    if ((!strcmp(argv[1], "tcp4") || !strcmp(argv[1], "tcp6")) && argc == 3) {
        int fd = tcp(!strcmp(argv[1], "tcp4") ? AF_INET : AF_INET6, atoi(argv[2]));
        if (fd < 0) return denied();
        close(fd);
        puts("CONNECTED");
        return 0;
    }
    if (!strcmp(argv[1], "proxy") && argc == 3) return proxy(argv[2], 0, 0, 0);
    if (!strcmp(argv[1], "connect") && argc == 3) return proxy(argv[2], 1, 0, 0);
    if (!strcmp(argv[1], "http_latency") && argc == 3) return proxy(argv[2], 0, 1, 1);
    if (!strcmp(argv[1], "proxy_latency") && argc == 3) return proxy(argv[2], 0, 1, 0);
    if (!strcmp(argv[1], "connect_latency") && argc == 3) return proxy(argv[2], 1, 1, 0);
    if (!strcmp(argv[1], "proxy_port")) { printf("PROXY_PORT=%d\n", proxy_port()); return 0; }
    if (!strcmp(argv[1], "hold") && argc == 3) return hold(argv[2]);
    if (!strcmp(argv[1], "host_process_metadata") && argc == 3)
        return host_process_metadata(atoi(argv[2]));
    if (!strcmp(argv[1], "gate") && argc == 4) return gate(argv[2], argv[3]);
    if (!strcmp(argv[1], "open_files")) {
        int count = 0;
        while (count < 256 && open("/dev/null", O_RDONLY) >= 0) count++;
        printf("OPEN_COUNT=%d errno=%d\n", count, errno);
        return 0;
    }
    return 64;
}
"""


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as source:
        for part in iter(lambda: source.read(1024 * 1024), b''):
            value.update(part)
    return value.hexdigest()


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def write_evidence(path, evidence):
    encoded = json.dumps(evidence, indent=2, ensure_ascii=False) + '\n'
    require(len(encoded.encode()) <= 256 * 1024, 'Acceptance evidence exceeded 256 KiB')
    temporary = path.with_name(path.name + '.tmp-' + uuid.uuid4().hex)
    try:
        temporary.write_text(encoded)
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def failure_diagnostics(case):
    return {'model_requests': len(case.model.requests), 'invocations': [
        {key: (value[:4096] if isinstance(value, str) else value)
         for key, value in invocation.outcome.items() if key != 'result'}
        for invocation in case.invocations if invocation.outcome is not None][:4]}


def reply(message, reason='stop'):
    return {'choices': [{'index': 0, 'finish_reason': reason, 'message': message}]}


def tool_results(requests):
    results = {}
    for request in requests:
        for message in request.get('messages', []):
            if message.get('role') == 'tool':
                results[message['tool_call_id']] = json.loads(message['content'])
    return results


class ModelFixture:
    def __init__(self):
        self.requests = []
        self.observations = []
        self.steps = []
        self.position = 0
        self.lock = threading.Lock()
        self.block_after_tool = False
        self.blocked = threading.Event()
        self.release = threading.Event()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_arguments):
                pass

            def do_POST(self):
                length = int(self.headers.get('Content-Length', '0'))
                if length > CAPTURE_BYTES:
                    self.send_error(413)
                    return
                encoded_request = self.rfile.read(length)
                received_ns = time.perf_counter_ns()
                request = json.loads(encoded_request)
                with fixture.lock:
                    if len(fixture.requests) >= MAX_REQUESTS:
                        self.send_error(429)
                        return
                    fixture.requests.append(request)
                    if not request.get('tools'):
                        value = reply({'role': 'assistant', 'content': json.dumps({
                            'decision': 'allow', 'reason': 'Controlled offline acceptance fixture'})})
                    elif fixture.position < len(fixture.steps):
                        name, arguments = fixture.steps[fixture.position]
                        fixture.position += 1
                        if callable(arguments):
                            arguments = arguments(tool_results(fixture.requests))
                        value = reply({'role': 'assistant', 'content': None, 'tool_calls': [{
                            'id': f'n2-{fixture.position}', 'type': 'function',
                            'function': {'name': name, 'arguments': json.dumps(arguments)}}]}, 'tool_calls')
                    else:
                        value = reply({'role': 'assistant', 'content': 'N2_FIXTURE_DONE'})
                    calls = value['choices'][0]['message'].get('tool_calls', [])
                    observation = {'request_index': len(fixture.requests) - 1,
                        'request_received_ns': received_ns, 'response_sent_ns': None,
                        'tool_call_id': calls[0]['id'] if calls else None}
                    fixture.observations.append(observation)
                if fixture.block_after_tool and tool_results([request]):
                    fixture.blocked.set()
                    fixture.release.wait(20)
                body = json.dumps(value).encode()
                try:
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    self.wfile.flush()
                    sent_ns = time.perf_counter_ns()
                    with fixture.lock:
                        observation['response_sent_ns'] = sent_ns
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever,
                                       kwargs={'poll_interval': 0.02}, daemon=True)
        self.thread.start()

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.thread.join(5)
        self.server.server_close()


class Invocation:
    def __init__(self, argv, environment, cwd):
        self.stdout = tempfile.TemporaryFile()
        self.stderr = tempfile.TemporaryFile()
        self.started = time.monotonic()
        self.process = subprocess.Popen(argv, env=environment, cwd=cwd,
            stdin=subprocess.DEVNULL, stdout=self.stdout, stderr=self.stderr, start_new_session=True)
        self.outcome = None

    def finish(self, timeout=40):
        if self.outcome is not None:
            return self.checked_outcome()
        deadline = self.started + timeout
        timed_out = False
        while True:
            pid, status, usage = os.wait4(self.process.pid, os.WNOHANG)
            if pid:
                self.process.returncode = os.waitstatus_to_exitcode(status)
                break
            if time.monotonic() >= deadline:
                if not timed_out:
                    self.process.send_signal(signal.SIGTERM)
                    deadline = time.monotonic() + 15
                    timed_out = True
                else:
                    self.process.kill()
            time.sleep(0.01)
        elapsed = time.monotonic() - self.started
        captured = []
        truncated = False
        stdout_valid = True
        for index, stream in enumerate((self.stdout, self.stderr)):
            stream.seek(0)
            data = stream.read(CAPTURE_BYTES + 1)
            stream.close()
            truncated |= len(data) > CAPTURE_BYTES
            data = data[:CAPTURE_BYTES]
            if index == 0:
                try:
                    data.decode('utf-8')
                except UnicodeDecodeError:
                    stdout_valid = False
            captured.append(data.decode('utf-8', errors='replace'))
        self.outcome = {'exit_code': self.process.returncode, 'stdout': captured[0], 'stderr': captured[1],
                'elapsed_ms': elapsed * 1000, 'user_cpu_ms': usage.ru_utime * 1000,
                'system_cpu_ms': usage.ru_stime * 1000,
                'peak_rss_bytes': usage.ru_maxrss * (1 if platform.system() == 'Darwin' else 1024),
                'timed_out': timed_out, 'capture_truncated': truncated, 'stdout_valid_utf8': stdout_valid}
        return self.checked_outcome()

    def checked_outcome(self):
        require(not self.outcome['timed_out'], 'CLI exceeded acceptance timeout and required termination')
        require(not self.outcome['capture_truncated'], 'CLI output exceeded its acceptance capture limit')
        require(self.outcome['stdout_valid_utf8'], 'CLI machine output contained invalid UTF-8')
        return self.outcome

    def close(self):
        if self.process.returncode is None:
            try:
                self.process.send_signal(signal.SIGTERM)
            except ProcessLookupError:
                pass
            self.finish()


class CaseFixture:
    def __init__(self, binary, probe, base):
        self.directory = tempfile.TemporaryDirectory(prefix='c-', dir=base)
        self.root = Path(self.directory.name).resolve()
        self.workspace = self.root / 'workspace'
        self.workspace.mkdir()
        runtime = self.root / 'r'
        runtime.mkdir()
        self.probe = self.workspace / 'n2-probe'
        shutil.copy2(probe, self.probe)
        self.model = ModelFixture()
        self.config = self.root / 'config.toml'
        self.binary = binary
        self.invocations = []
        self.environment = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(self.root),
            'TMPDIR': str(runtime),
            'XDG_DATA_HOME': str(self.root / 'data'), 'XDG_STATE_HOME': str(self.root / 'state'),
            'XDG_CACHE_HOME': str(self.root / 'cache'), 'NO_PROXY': '127.0.0.1,localhost'}
        self.configure()

    def configure(self, extra=''):
        self.config.write_text('\n'.join([
            'schema_version=1', '[defaults]', 'profile="fixture"', '[profiles.fixture]',
            'protocol="openai-chat-completions"',
            f'base_url="http://127.0.0.1:{self.model.server.server_port}/v1"',
            'model="n2-offline-fixture"', 'auth="none"', 'context_window_tokens=128000',
            'stream=false', '[limits]', 'max_model_requests=24', 'max_tool_calls=24',
            'active_run_timeout_seconds=45', 'command_timeout_seconds=15',
            'max_command_timeout_seconds=30', extra, '']), encoding='utf-8')

    def start(self, steps=(), mode='code', permissions='bypass', session=None):
        self.model.steps = list(steps)
        self.model.position = 0
        argv = [str(self.binary), '--config', str(self.config), '--cwd', str(self.workspace),
                '--mode', mode, '--permissions', permissions, '--output', 'json', 'run']
        if session:
            argv += ['--session', session]
        argv.append('Run the controlled N2 acceptance fixture')
        invocation = Invocation(argv, self.environment, self.workspace)
        self.invocations.append(invocation)
        return invocation

    def run(self, steps=(), **options):
        outcome = self.start(steps, **options).finish()
        try:
            outcome['result'] = json.loads(outcome['stdout'])
        except ValueError as error:
            raise AssertionError('CLI did not emit a JSON result. ' + outcome['stderr'][:1500]) from error
        return outcome

    def command(self, *arguments):
        return ('exec', {'argv': [str(self.probe), *map(str, arguments)]})

    def facts(self):
        marker = '{"saya_execution_environment_v1"'
        for request in self.model.requests:
            for message in request.get('messages', []):
                content = message.get('content')
                if isinstance(content, str) and marker in content:
                    value, _ = json.JSONDecoder().raw_decode(content[content.index(marker):])
                    return value['saya_execution_environment_v1']
        raise AssertionError('Normal model request did not include execution-environment facts')

    def native(self):
        facts = self.facts()
        expected = 'seatbelt' if platform.system() == 'Darwin' else 'bubblewrap'
        require(facts['identity']['backend'] == expected, 'Default execution backend was not native')
        require(facts['capabilities']['filesystem'] == 'hard', 'Native filesystem was not hard')
        require(facts['capabilities']['network'] == 'hard', 'Native network was not hard')
        return facts

    def results(self):
        return tool_results(self.model.requests)

    def close(self):
        self.model.release.set()
        failure = None
        callbacks = [invocation.close for invocation in self.invocations]
        callbacks.extend([self.model.close, self.directory.cleanup])
        for callback in callbacks:
            try:
                callback()
            except Exception as error:
                if failure is None:
                    failure = error
        if failure is not None:
            raise failure


@contextmanager
def fixture(binary, probe, base):
    value = CaseFixture(binary, probe, base)
    try:
        yield value
    finally:
        value.close()


def complete(outcome):
    require(outcome['exit_code'] == 0 and outcome['result']['status'] == 'completed',
            'Run did not complete. ' + json.dumps(outcome.get('result', {}))[:1800]
            + outcome['stderr'][:1000])


def stdout(result):
    require(result.get('ok'), 'Exec tool failed. ' + json.dumps(result)[:1500])
    return result['data']['stdout']


def files_case(case):
    path = case.workspace / 'file.txt'
    patch = '--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-seed\n+revised\n'
    outcome = case.run([
        ('write_file', {'path': 'file.txt', 'content': 'seed\n', 'expected_version': None}),
        ('apply_patch', lambda results: {'path': 'file.txt', 'patch': patch,
            'expected_version': results['n2-1']['data']['version']}),
        ('read_file', {'path': 'file.txt', 'start_line': 1, 'line_count': 4}),
        ('list_dir', {'path': '.'}), ('find_files', {'path': '.', 'glob': '*.txt'}),
        ('search_text', {'path': '.', 'pattern': 'revised'}),
        ('write_file', {'path': 'file.txt', 'content': 'must not replace', 'expected_version': None}),
        ('write_file', lambda results: {'path': 'file.txt', 'content': 'must not replace',
            'expected_version': results['n2-1']['data']['version']}),
    ], permissions='auto')
    complete(outcome)
    results = case.results()
    require(all(results[f'n2-{index}']['ok'] for index in range(1, 7)), 'One of six file tools failed')
    require(not results['n2-7']['ok'] and not results['n2-8']['ok'],
            'Missing or stale expected version replaced an existing file')
    require(path.read_text() == 'revised\n', 'Write/patch/version checks produced wrong file content')
    require(results['n2-3']['data']['content'] == 'revised\n', 'Read result did not match disk')
    require(results['n2-2']['data']['version'] == results['n2-3']['data']['version'], 'Patch/read versions differed')
    require(results['n2-1']['data']['version'] != results['n2-2']['data']['version'], 'Changed contents kept the old version')
    for index in (4, 5, 6):
        require('file.txt' in json.dumps(results[f'n2-{index}']['data']), 'Discovery omitted the created file')
    require('revised' in json.dumps(results['n2-6']['data']), 'Content search omitted the matching line')
    return {'backend': case.native(), 'tools': list(results), 'version_replacement_refused': True}


def plan_case(case):
    before = sorted(path.name for path in case.workspace.iterdir())
    outcome = case.run([
        ('write_file', {'path': 'forbidden.txt', 'content': 'blocked', 'expected_version': None}),
        ('exec', {'argv': ['/usr/bin/touch', 'forbidden-exec']}),
    ], mode='plan')
    complete(outcome)
    require(set(case.results()) == {'n2-1', 'n2-2'}, 'PLAN did not close both requested tool calls')
    require(all(not result['ok'] for result in case.results().values()), 'PLAN permitted a mutation')
    require(all(result.get('error', {}).get('code') in ('permission_denied', 'mode_denied')
                for result in case.results().values()), 'PLAN failed for an unrelated tool error')
    require(before == sorted(path.name for path in case.workspace.iterdir()), 'PLAN changed the workspace')
    return {'backend': case.native(), 'workspace_unchanged': True, 'denied': len(case.results())}


def deny_case(case):
    case.configure('[[permissions.rules]]\neffect="deny"\ntool="write_file"\npath_glob='
                   + json.dumps(str(case.workspace / 'denied.txt')))
    outcome = case.run([('write_file', {'path': 'denied.txt', 'content': 'blocked', 'expected_version': None})])
    complete(outcome)
    require(not case.results()['n2-1']['ok'], 'bypass ignored an explicit deny')
    require(case.results()['n2-1'].get('error', {}).get('code') == 'permission_denied',
            'Explicit deny failed for an unrelated reason')
    require(not (case.workspace / 'denied.txt').exists(), 'Explicitly denied file was written')
    return {'denied_under_bypass': True, 'backend': case.native()}


def protected_case(case):
    control = case.workspace / '.saya' / 'local'
    control.mkdir(parents=True)
    secret = control / 'synthetic.txt'
    secret.write_text('N2_SYNTHETIC_PROTECTED')
    outside = case.root / 'outside.txt'
    outside.write_text('N2_SYNTHETIC_OUTSIDE')
    outcome = case.run([
        case.command('read', secret), case.command('write', secret), case.command('read', outside),
        ('read_file', {'path': str(secret), 'start_line': 1, 'line_count': 2}),
        ('write_file', {'path': str(secret), 'content': 'blocked',
                        'expected_version': hashlib.sha256(secret.read_bytes()).hexdigest()}),
    ])
    complete(outcome)
    results = case.results()
    for index in range(1, 4):
        require('DENIED' in stdout(results[f'n2-{index}']), 'Native exec reached a protected/outside file')
    require(not results['n2-4']['ok'] and not results['n2-5']['ok'], 'File worker bypassed OS protection')
    require(secret.read_text() == 'N2_SYNTHETIC_PROTECTED', 'Protected file was mutated')
    require(outside.read_text() == 'N2_SYNTHETIC_OUTSIDE', 'Outside file was mutated')
    return {'backend': case.native(), 'exec_and_worker_denied': True}


def skill_case(case):
    workspace_skill = case.workspace / '.saya' / 'skills' / 'example' / 'SKILL.md'
    workspace_skill.parent.mkdir(parents=True)
    workspace_body = ('---\nname: example\ndescription: N2 workspace acceptance skill\n---\n'
                      'N2_WORKSPACE_SKILL_BODY\n')
    workspace_skill.write_text(workspace_body)
    global_skill = case.root / '.saya' / 'skills' / 'global-example' / 'SKILL.md'
    global_skill.parent.mkdir(parents=True)
    global_body = ('---\nname: global-example\ndescription: N2 global acceptance skill\n---\n'
                   'N2_GLOBAL_SKILL_BODY\n')
    global_skill.write_text(global_body)
    (case.workspace / '.saya' / 'AGENTS.md').write_text('N2_PROJECT_INSTRUCTION_FIXTURE\n')
    (case.workspace / '.saya' / 'config.toml').write_text('not valid runtime configuration\n')
    outcome = case.run([
        ('read_file', {'path': str(workspace_skill), 'start_line': 1, 'line_count': 20}),
        ('read_file', {'path': str(global_skill), 'start_line': 1, 'line_count': 20}),
    ], permissions='auto')
    complete(outcome)
    first_request = json.dumps(case.model.requests[0])
    for marker in ('N2 workspace acceptance skill', 'N2 global acceptance skill',
                   'N2_PROJECT_INSTRUCTION_FIXTURE'):
        require(marker in first_request, 'Project instructions or Skill metadata were not discovered')
    results = case.results()
    require(results['n2-1']['ok'] and results['n2-1']['data']['content'] == workspace_body,
            'Default native execution blocked a discovered workspace Skill body')
    require(not results['n2-2']['ok'] and results['n2-2'].get('error', {}).get('code') == 'permission_denied',
            'Global Skill discovery expanded file permissions without an explicit root')
    case.configure('[permissions]\nadditional_roots=[' + json.dumps(str(global_skill.parent)) + ']')
    outcome = case.run([('read_file', {'path': str(global_skill), 'start_line': 1, 'line_count': 20})],
                       permissions='auto')
    complete(outcome)
    result = case.results()['n2-1']
    require(result['ok'] and result['data']['content'] == global_body,
            'Explicitly authorized global Skill root was unavailable in the native environment')
    return {'backend': case.native(), 'workspace_skill_read': True, 'global_explicit_root_read': True,
            'discovery_did_not_expand_roots': True, 'implicit_runtime_config_was_ignored': True}


def host_case(case):
    outside = case.root / 'outside.txt'
    outside.write_text('N2_SYNTHETIC_HOST_CONTROL')
    case.configure('[sandbox]\nbackend="host"')
    outcome = case.run([case.command('read', outside)])
    complete(outcome)
    require('READ N2_SYNTHETIC_HOST_CONTROL' in stdout(case.results()['n2-1']), 'Host control could not read fixture')
    facts = case.facts()
    require(facts['identity']['backend'] == 'host', 'Explicit Host was not reported')
    require(facts['capabilities']['filesystem'] == 'unsupported', 'Host claimed filesystem isolation')
    return {'host_control_read': True, 'backend': facts}


@contextmanager
def listener(family, root):
    stream = socket.socket(family, socket.SOCK_STREAM)
    try:
        if family == socket.AF_UNIX:
            endpoint = str(root / 'host.sock')
        else:
            endpoint = ('127.0.0.1' if family == socket.AF_INET else '::1', 0)
        stream.bind(endpoint)
        stream.listen(16)
        yield stream.getsockname()
    finally:
        stream.close()


def network_case(case, family):
    with listener(family, case.root) as endpoint:
        action = 'unix' if family == socket.AF_UNIX else ('tcp4' if family == socket.AF_INET else 'tcp6')
        target = endpoint if family == socket.AF_UNIX else endpoint[1]
        control = subprocess.run([str(case.probe), action, str(target)], capture_output=True, timeout=5, check=True)
        require(b'CONNECTED' in control.stdout, 'Host listener positive control failed')
        outcome = case.run([case.command(action, target)])
        complete(outcome)
        output = stdout(case.results()['n2-1'])
        require('DENIED' in output and 'CONNECTED' not in output, 'Native payload connected directly to a host listener')
        return {'backend': case.native(), 'host_control_connected': True, 'payload': output.strip()}


def require_udp_isolation(output, received):
    require(not received, 'Host received a native UDP bypass packet')
    result = output.strip()
    denied = re.fullmatch(r'DENIED errno=[1-9][0-9]*', result) is not None
    # A send into Linux's private network namespace can succeed locally. The
    # host listener is the boundary witness, with a separate host positive control.
    require(denied or (platform.system() == 'Linux' and result == 'SENT'),
            'Unexpected native UDP result. ' + repr(result))


def dns_case(case):
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as server:
        server.bind(('127.0.0.1', 0))
        server.settimeout(1)
        port = server.getsockname()[1]
        control = subprocess.run([str(case.probe), 'dns_udp', str(port)], capture_output=True, timeout=5, check=True)
        require(b'SENT' in control.stdout, 'Host raw DNS transport control failed')
        require(b'N2_RAW_DNS_TRANSPORT_PROBE' in server.recv(512), 'Host UDP listener did not receive control')
        outcome = case.run([case.command('dns_udp', port)])
        complete(outcome)
        output = stdout(case.results()['n2-1'])
        try:
            unexpected = server.recv(512)
        except socket.timeout:
            unexpected = b''
        require_udp_isolation(output, unexpected)
        return {'backend': case.native(), 'raw_udp_dns_transport_blocked': True,
                'host_positive_control': True, 'host_packets_received': 0, 'payload_result': output.strip()}


@contextmanager
def http_target():
    seen = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_arguments):
            pass

        def do_GET(self):
            seen.append(self.path)
            body = b'N2_PROXY_ALLOWED'
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.02}, daemon=True)
    thread.start()
    try:
        yield server.server_port, seen
    finally:
        server.shutdown()
        thread.join(5)
        server.server_close()


def proxy_case(case, tunnel=False):
    with http_target() as (allowed_port, allowed), http_target() as (denied_port, denied):
        scheme = 'https' if tunnel else 'http'
        origin = f'{scheme}://127.0.0.1:{allowed_port}'
        other = f'{scheme}://127.0.0.1:{denied_port}'
        case.configure('[sandbox.network]\nmode="proxy"\nallowed_origins=[' + json.dumps(origin)
                       + ']\nlocalhost_development_origin=' + json.dumps(origin))
        action = 'connect' if tunnel else 'proxy'
        outcome = case.run([case.command(action, origin), case.command(action, other)])
        complete(outcome)
        require('PROXY_ALLOWED' in stdout(case.results()['n2-1']), 'Approved origin could not use broker')
        require('PROXY_DENIED' in stdout(case.results()['n2-2']), 'Unapproved origin used broker')
        require(allowed == ['/probe'] and not denied, 'Broker forwarded an unauthorized request')
        return {'backend': case.native(), 'approved_requests': len(allowed), 'denied_requests': len(denied),
                'transport': 'opaque CONNECT tunnel' if tunnel else 'HTTP absolute form'}


def proxy_udp_case(case):
    case.configure('[sandbox.network]\nmode="proxy"\nallowed_origins=[]')
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as ipv4, \
            socket.socket(socket.AF_INET6, socket.SOCK_DGRAM) as ipv6:
        ipv6.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
        ipv4.settimeout(0.2)
        ipv6.settimeout(0.2)

        def bind_and_send(results):
            port = int(stdout(results['n2-1']).strip().split('PROXY_PORT=')[1])
            require(port > 0, 'Native payload did not receive the controlled proxy endpoint')
            ipv4.bind(('127.0.0.1', port))
            ipv6.bind(('::1', port))
            for action, host_listener in [('dns_udp', ipv4), ('dns_udp6', ipv6)]:
                control = subprocess.run([str(case.probe), action, str(port)],
                                         capture_output=True, timeout=5, check=True)
                require(control.stdout.strip() == b'SENT', 'Host proxy-port UDP control failed')
                require(b'N2_RAW_DNS_TRANSPORT_PROBE' in host_listener.recv(512),
                        'Host proxy-port UDP listener did not receive its positive control')
            return {'argv': [str(case.probe), 'dns_udp', str(port)]}

        outcome = case.run([case.command('proxy_port'), ('exec', bind_and_send),
            ('exec', lambda results: {'argv': [str(case.probe), 'dns_udp6',
                stdout(results['n2-1']).strip().split('PROXY_PORT=')[1]]})])
        complete(outcome)
        received = []
        for family, listener_socket in (('ipv4', ipv4), ('ipv6', ipv6)):
            try:
                packet = listener_socket.recv(512)
            except socket.timeout:
                packet = b''
            if packet:
                received.append(family)
        payload_results = []
        for index in (2, 3):
            output = stdout(case.results()[f'n2-{index}'])
            require_udp_isolation(output, received)
            payload_results.append(output.strip())
        return {'backend': case.native(), 'ipv4_udp_same_proxy_port_denied': True,
                'ipv6_udp_same_proxy_port_denied': True, 'host_packets_received': 0,
                'host_positive_controls': ['ipv4', 'ipv6'], 'payload_results': payload_results}


def process_metadata_case(case):
    sentinel = subprocess.Popen(['/bin/sleep', '30'], env={'N2_SYNTHETIC_ENV': 'synthetic-only-sentinel'},
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        control = subprocess.run([str(case.probe), 'host_process_metadata', str(sentinel.pid)],
                                 capture_output=True, timeout=5, check=True)
        require(b'HOST_PROCESS_METADATA_READABLE' in control.stdout,
                'Host process metadata positive control was unavailable')
        outcome = case.run([case.command('host_process_metadata', sentinel.pid)])
        complete(outcome)
        output = stdout(case.results()['n2-1'])
        require('DENIED' in output and 'HOST_PROCESS_METADATA_READABLE' not in output,
                'Native payload could read another host process metadata/environment')
        return {'backend': case.native(), 'host_positive_control': True,
                'target': 'owned sleep process with a clean synthetic environment',
                'mechanism': 'KERN_PROCARGS2' if platform.system() == 'Darwin' else 'procfs environ',
                'payload': output.strip()}
    finally:
        sentinel.terminate()
        sentinel.wait(timeout=5)


def cancel_case(case):
    heartbeat = case.workspace / 'heartbeat'
    invocation = case.start([case.command('hold', heartbeat)])
    deadline = time.monotonic() + 20
    while (not heartbeat.exists() or heartbeat.stat().st_size == 0) and time.monotonic() < deadline:
        time.sleep(0.02)
    require(heartbeat.exists() and heartbeat.stat().st_size > 0, 'Native child did not publish its heartbeat')
    invocation.process.send_signal(signal.SIGINT)
    outcome = invocation.finish()
    value = json.loads(outcome['stdout'])
    require(value['status'] == 'cancelled', 'SIGINT did not cancel the Run')
    length = heartbeat.stat().st_size
    time.sleep(0.5)
    require(heartbeat.stat().st_size == length, 'Child continued writing after cancellation completed')
    require(length < 100, 'Child completed its full lifetime before cancellation')
    return {'backend': case.native(), 'run_status': value['status'], 'heartbeat_quiescent': True,
            'heartbeat_bytes': length, 'observation': 'host-visible heartbeat, independent of guest PID numbering'}


def recovery_case(case):
    target = case.workspace / 'persisted.txt'
    case.model.block_after_tool = True
    invocation = case.start([('write_file', {'path': 'persisted.txt', 'content': 'N2_PERSISTED', 'expected_version': None})])
    require(case.model.blocked.wait(20), 'Model did not observe the persisted write before crash')
    invocation.process.kill()
    killed = invocation.finish()
    require(killed['exit_code'] == -signal.SIGKILL, 'Crash fixture did not terminate the CLI')
    require(target.read_text() == 'N2_PERSISTED', 'Observed write did not reach disk before crash')
    listed = Invocation([str(case.binary), '--config', str(case.config), '--cwd', str(case.workspace),
                         '--output', 'json', 'session', 'list'], case.environment, case.workspace).finish()
    session = json.loads(listed['stdout'])['data'][0]['id']
    case.model.block_after_tool = False
    case.model.release.set()
    count = len(case.model.requests)
    outcome = case.run(session=session)
    complete(outcome)
    require(len(case.model.requests) == count + 1, 'Recovery replayed a tool or made extra model requests')
    require(target.read_text() == 'N2_PERSISTED', 'Recovery altered the completed write')
    require('recovered' in outcome['stderr'] and 'no tools were replayed' in outcome['stderr'],
            'Resume did not report interrupted-run recovery')
    return {'observed_write_preserved': True, 'tool_replay_count': 0, 'new_model_requests': 1,
            'crash_boundary': 'model request after completed and persisted file operation'}


def resources_case(case, cgroup_root=None):
    if cgroup_root:
        with delegated_group(Path(cgroup_root)) as delegated:
            runtime_root = delegated / 'runtime-root'
            escape = delegated / 'escape'
            runtime_root.mkdir()
            escape.mkdir()
            (runtime_root / 'cgroup.subtree_control').write_text('+cpu +memory +pids')
            root_setting = 'cgroup_root=' + json.dumps(str(runtime_root)) + '\n'
            resource_settings = ('[sandbox.resources]\ntotal_memory_bytes=536870912\ntotal_processes=64\n'
                '[sandbox.resources.cpu_quota]\nquota_micros=50000\nperiod_micros=100000')
            negative_controls = {}
            for name, configuration, expected_error in [
                ('writable_sibling_cgroup', '[sandbox]\n' + root_setting
                    + 'write_roots=' + json.dumps([str(escape)]) + '\n' + resource_settings,
                    'unsupported_fs_policy'),
                ('host_delegated_aggregate', '[sandbox]\nbackend="host"\n' + root_setting
                    + resource_settings, 'sandbox_capability_unavailable'),
                ('host_aggregate_without_delegation', '[sandbox]\nbackend="host"\n'
                    + resource_settings, 'sandbox_capability_unavailable'),
                ('host_delegation_without_aggregate', '[sandbox]\nbackend="host"\n'
                    + root_setting, 'sandbox_capability_unavailable'),
            ]:
                case.configure(configuration)
                before = len(case.model.requests)
                rejected = case.run()
                require(rejected['exit_code'] != 0 and rejected['result']['status'] == 'failed',
                        name + ' did not fail before model dispatch')
                error = rejected['result'].get('error', {})
                require(error.get('code') == expected_error, name + ' failed for an unrelated reason')
                require(len(case.model.requests) == before, name + ' reached the model')
                groups = [path for path in runtime_root.iterdir() if path.is_dir()]
                require(not groups, name + ' created a Run cgroup before rejecting the configuration')
                negative_controls[name] = {'failed_before_model_dispatch': True, 'error': error,
                    'model_requests': len(case.model.requests) - before, 'created_run_cgroups': len(groups)}
            case.configure('[sandbox]\n' + root_setting + resource_settings)
            ready = case.workspace / 'resource-ready'
            release = case.workspace / 'resource-release'
            invocation = case.start([case.command('gate', ready, release)])
            try:
                deadline = time.monotonic() + 20
                while (not ready.exists() or ready.stat().st_size == 0) and time.monotonic() < deadline:
                    time.sleep(0.02)
                require(ready.exists() and ready.read_text() == 'READY', 'Cgroup payload did not become active')
                groups = [path for path in runtime_root.iterdir() if path.is_dir()]
                require(len(groups) == 1, 'Native Run did not create exactly one owned cgroup')
                members = (groups[0] / 'cgroup.procs').read_text().split()
                require(members and str(invocation.process.pid) not in members,
                        'Owned payload cgroup was empty or incorrectly included the host CLI')
                actual = {name: (groups[0] / name).read_text().strip()
                          for name in ('memory.max', 'pids.max', 'cpu.max')}
                require(actual == {'memory.max': '536870912', 'pids.max': '64', 'cpu.max': '50000 100000'},
                        'Kernel cgroup controllers did not retain requested limits')
            finally:
                release.touch()
            outcome = invocation.finish()
            outcome['result'] = json.loads(outcome['stdout'])
            complete(outcome)
            require(not any(path.is_dir() for path in runtime_root.iterdir()), 'Run left an owned cgroup after cleanup')
            facts = case.native()
            for capability in ('total_memory', 'total_processes', 'cpu_bandwidth'):
                require(facts['capabilities'][capability] == 'hard', 'Delegated cgroup capability was not hard')
            return {'backend': facts, 'delegated_limits_configured': True, 'kernel_readback': actual,
                    'measurement_scope': 'kernel limit readback and active membership; resource exhaustion not exercised',
                    'payload_active_member_count': len(members), 'owned_run_cgroup_removed': True,
                    'negative_controls': negative_controls}
    case.configure('[sandbox.resources]\ntotal_memory_bytes=536870912')
    outcome = case.run()
    require(outcome['exit_code'] != 0 and outcome['result']['status'] == 'failed',
            'Unavailable mandatory resource constraint did not fail closed')
    require(outcome['result'].get('error', {}).get('code') == 'sandbox_capability_unavailable',
            'Resource requirement failed for an unrelated reason')
    require(not case.model.requests, 'Missing resource capability failed after model dispatch')
    return {'failed_before_model_dispatch': True, 'error': outcome['result'].get('error')}


@contextmanager
def delegated_group(root):
    owned = root.resolve(strict=True) / ('saya_n2_accept_' + uuid.uuid4().hex)
    owned.mkdir()
    try:
        available = (owned / 'cgroup.controllers').read_text().split()
        require(all(name in available for name in ('cpu', 'memory', 'pids')),
                'Supplied cgroup subtree does not delegate cpu, memory and pids')
        (owned / 'cgroup.subtree_control').write_text('+cpu +memory +pids')
        yield owned
    finally:
        descendants = sorted((path for path in owned.rglob('*') if path.is_dir()),
                             key=lambda path: len(path.parts), reverse=True)
        for directory in descendants:
            (directory / 'cgroup.kill').write_text('1')
            deadline = time.monotonic() + 5
            while 'populated 0' not in (directory / 'cgroup.events').read_text():
                require(time.monotonic() < deadline, 'Acceptance-owned cgroup remained populated')
                time.sleep(0.02)
            directory.rmdir()
        require('populated 0' in (owned / 'cgroup.events').read_text(), 'Delegated acceptance group remained populated')
        owned.rmdir()


def file_descriptors_case(case):
    case.configure('[sandbox.resources]\nopen_files=32')
    outcome = case.run([case.command('open_files')])
    complete(outcome)
    text = stdout(case.results()['n2-1'])
    count = int(text.split('OPEN_COUNT=')[1].split()[0])
    require(0 < count < 32, 'Configured file descriptor limit did not constrain payload')
    return {'backend': case.native(), 'additional_fds_before_failure': count, 'payload': text.strip()}


def validate_benchmark_sample(case, backend, kind, expected_tools=1):
    if backend == 'auto':
        facts = case.native()
    else:
        facts = case.facts()
        require(facts['identity']['backend'] == 'host', 'Host benchmark used a different backend')
        require(all(facts['capabilities'][name] == 'unsupported' for name in ('filesystem', 'network')),
                'Host benchmark reported native isolation capabilities')
    results = case.results()
    proof = {'kind': kind, 'backend': facts['identity']['backend'], 'tool_calls': len(results)}
    if kind == 'cold_start':
        require(not results, 'Cold-start benchmark unexpectedly executed a tool')
        return proof
    expected_ids = [f'n2-{index}' for index in range(1, expected_tools + 1)]
    require(set(results) == set(expected_ids), 'Benchmark did not complete its requested tools')
    require(all(result.get('ok') is True for result in results.values()),
            'Benchmark tool failed instead of measuring a successful operation')
    if kind == 'warm_command':
        require(2 <= expected_tools <= 21, 'Warm benchmark requires one warmup and 1..20 samples')
        require(len(case.invocations) == 1, 'Warm benchmark used more than one CLI invocation')
        requests = case.model.requests
        require(len(requests) == expected_tools + 1, 'Warm benchmark has unexpected model request count')
        identity = facts['identity']
        require(isinstance(identity.get('id'), str) and bool(identity['id']),
                'Warm benchmark omitted its prepared-environment identity')
        marker = '{"saya_execution_environment_v1"'
        for request in requests:
            identities = []
            for message in request.get('messages', []):
                content = message.get('content')
                if isinstance(content, str) and marker in content:
                    value, _ = json.JSONDecoder().raw_decode(content[content.index(marker):])
                    identities.append(value['saya_execution_environment_v1']['identity'])
            require(identities and all(value == identity for value in identities),
                    'Warm benchmark changed its prepared environment between tool calls')
        observations = case.model.observations
        require(len(observations) == len(requests), 'Warm benchmark omitted model time observations')
        intervals = []
        for index, call_id in enumerate(expected_ids):
            result = results[call_id]
            require(result['data']['exit_code'] == 0, 'Warm benchmark command failed')
            sent = observations[index]
            received = observations[index + 1]
            require(sent['request_index'] == index and received['request_index'] == index + 1
                    and sent['tool_call_id'] == call_id, 'Warm benchmark timing belongs to a different call')
            require(tool_results([requests[index + 1]]).get(call_id) == result,
                    'Warm benchmark next model request omitted its actual tool result')
            start = sent['response_sent_ns']
            end = received['request_received_ns']
            require(type(start) is int and type(end) is int and 0 < start < end,
                    'Warm benchmark did not observe a complete response-to-tool-result interval')
            require(end - start <= 40_000_000_000,
                    'Warm benchmark interval exceeded the bounded CLI invocation')
            intervals.append({'call_id': call_id, 'response_sent_ns': start,
                'tool_result_request_received_ns': end, 'elapsed_ms': (end - start) / 1_000_000,
                'proof': {'kind': kind, 'backend': proof['backend'],
                          'environment_id': identity['id'], 'exit_code': 0}})
        proof.update({'environment_identity': identity, 'model_requests': len(requests),
                      'request_environment_ids': [identity['id']] * len(requests),
                      'invocation_count': len(case.invocations),
                      'call_ids': expected_ids, 'call_intervals': intervals})
        return proof
    require(expected_tools == 1, 'Single-operation benchmark expected multiple tools')
    result = results['n2-1']
    if kind == 'file_roundtrip':
        require(result['data']['content'] == 'N2_BENCH\n', 'Benchmark read returned the wrong file content')
        proof.update({'content_sha256': hashlib.sha256(b'N2_BENCH\n').hexdigest(), 'bytes': 9})
    elif kind == 'file_write':
        path = case.workspace / 'written.txt'
        require(path.is_file() and path.read_bytes() == b'N2_BENCH_WRITE\n',
                'Benchmark write did not create the expected file content')
        require(Path(result['data']['path']).resolve() == path.resolve()
                and result['data']['created'] is True and result['data']['version'] == digest(path),
                'Benchmark write result did not describe the created file')
        proof.update({'path': str(path.resolve()), 'content_sha256': digest(path), 'bytes': path.stat().st_size})
    elif kind == 'search_text':
        data = result['data']
        items = data['items']
        require(data.get('truncated') is False and len(items) == 1,
                'Benchmark search did not return its complete expected match')
        hit = items[0]
        require(Path(hit['path']).resolve() == (case.workspace / 'bench.txt').resolve()
                and hit['line'] == 1 and hit['text'] == 'N2_BENCH',
                'Benchmark search returned the wrong path, line or matching text')
        proof.update({'path': str(Path(hit['path']).resolve()), 'line': 1, 'text': hit['text'], 'matches': 1})
    elif kind == 'short_command':
        require(result['data']['exit_code'] == 0, 'Benchmark command did not exit successfully')
        proof['exit_code'] = 0
    else:
        raise AssertionError('Unknown benchmark operation ' + kind)
    return proof


def proxy_elapsed_ms(result):
    output = stdout(result)
    require(result['data'].get('exit_code') == 0, 'Network latency probe did not exit successfully')
    match = re.fullmatch(r'N2_NETWORK_MS=([0-9]+\.[0-9]{6})\n', output)
    require(match is not None, 'Network latency probe returned an invalid timing record')
    elapsed = float(match.group(1))
    require(math.isfinite(elapsed) and 0 < elapsed <= 10000,
            'Network latency probe returned an out-of-range duration')
    return elapsed


def benchmark_proxy(binary, probe, base, samples):
    require(isinstance(samples, int) and not isinstance(samples, bool) and 1 <= samples <= 20,
            'Proxy benchmark sample count is outside its supported bound')
    result = {
        'samples_per_variant': samples,
        'percentile': 'nearest-rank',
        'clock': 'payload CLOCK_MONOTONIC',
        'scope': 'fresh socket creation, connect, HTTP request, complete 16-byte response body and close; '
                 'CONNECT also includes tunnel negotiation; plaintext HTTP inside the tunnel, no TLS; '
                 'excludes CLI, model, backend preparation and process startup',
        'whole_cli_rusage_scope': 'separate whole CLI invocation wall time and aggregate wait4 CPU counters; '
                                  'kernel ru_maxrss, not aggregate simultaneous Run memory',
        'variants': {},
    }
    with http_target() as (port, seen):
        for variant, action, scheme in [
                ('host_direct', 'http_latency', 'http'),
                ('native_http', 'proxy_latency', 'http'),
                ('native_connect', 'connect_latency', 'https')]:
            origin = f'{scheme}://127.0.0.1:{port}'
            timings = []
            resources = []
            for _ in range(samples):
                before = len(seen)
                with fixture(binary, probe, base) as case:
                    if variant == 'host_direct':
                        case.configure('[sandbox]\nbackend="host"')
                    else:
                        case.configure('[sandbox.network]\nmode="proxy"\nallowed_origins=['
                                       + json.dumps(origin) + ']\nlocalhost_development_origin='
                                       + json.dumps(origin))
                    outcome = case.run([case.command(action, origin)])
                    complete(outcome)
                    facts = case.facts() if variant == 'host_direct' else case.native()
                    if variant == 'host_direct':
                        require(facts['identity']['backend'] == 'host'
                                and all(facts['capabilities'][name] == 'unsupported'
                                        for name in ('filesystem', 'network')),
                                'Direct network baseline did not use explicit Host execution')
                    results = case.results()
                    require(set(results) == {'n2-1'}, 'Proxy benchmark did not complete its one exec tool')
                    timings.append({'elapsed_ms': proxy_elapsed_ms(results['n2-1'])})
                    require(seen[before:] == ['/probe'],
                            'Proxy benchmark target did not receive exactly its authorized request')
                    resources.append({key: outcome[key] for key in (
                        'elapsed_ms', 'user_cpu_ms', 'system_cpu_ms', 'peak_rss_bytes')})
            ordered = sorted(sample['elapsed_ms'] for sample in timings)
            result['variants'][variant] = {
                'backend': facts['identity']['backend'],
                'origin': origin,
                'target_path': '/probe',
                'target_requests': samples,
                'p50_ms': ordered[math.ceil(0.5 * samples) - 1],
                'p95_ms': ordered[math.ceil(0.95 * samples) - 1],
                'samples': timings,
                'whole_cli_rusage': resources,
            }
        require(seen == ['/probe'] * (3 * samples),
                'Proxy benchmark target observed an unexpected request or sample count')
    baseline = result['variants']['host_direct']
    result['native_over_host'] = {}
    for variant in ('native_http', 'native_connect'):
        measured = result['variants'][variant]
        result['native_over_host'][variant] = {
            'p50_extra_ms': measured['p50_ms'] - baseline['p50_ms'],
            'p95_extra_ms': measured['p95_ms'] - baseline['p95_ms'],
            'p50_ratio': measured['p50_ms'] / baseline['p50_ms'],
            'p95_ratio': measured['p95_ms'] / baseline['p95_ms'],
        }
    return result


def benchmark(binary, probe, base, samples):
    require(type(samples) is int and 1 <= samples <= 20, 'Benchmark sample count must be 1..20')
    result = {'measurement_version': 2, 'samples_per_variant': samples, 'percentile': 'nearest-rank',
              'resource_measurement': 'wait4 rusage for each CLI child',
              'peak_rss_scope': 'kernel ru_maxrss, not an aggregate simultaneous Run memory peak',
              'cold_start_scope': 'new CLI process and new Run/environment, OS page cache retained',
              'variants': {}}
    for backend in ('auto', 'host'):
        variants = {}
        for kind in ('cold_start', 'file_roundtrip', 'file_write', 'search_text', 'short_command'):
            measured = []
            for _ in range(samples):
                with fixture(binary, probe, base) as case:
                    case.configure('[sandbox]\nbackend=' + json.dumps(backend))
                    (case.workspace / 'bench.txt').write_text('N2_BENCH\n')
                    steps = []
                    if kind == 'file_roundtrip':
                        steps = [('read_file', {'path': 'bench.txt', 'start_line': 1, 'line_count': 1})]
                    elif kind == 'file_write':
                        steps = [('write_file', {'path': 'written.txt', 'content': 'N2_BENCH_WRITE\n',
                                                 'expected_version': None})]
                    elif kind == 'search_text':
                        steps = [('search_text', {'path': '.', 'pattern': 'N2_BENCH'})]
                    elif kind == 'short_command':
                        steps = [('exec', {'argv': ['/usr/bin/true']})]
                    outcome = case.run(steps)
                    complete(outcome)
                    proof = validate_benchmark_sample(case, backend, kind)
                    measured.append({**{key: outcome[key] for key in
                        ('elapsed_ms', 'user_cpu_ms', 'system_cpu_ms', 'peak_rss_bytes')}, 'proof': proof})
            ordered = sorted(sample['elapsed_ms'] for sample in measured)
            variants[kind] = {'scope': 'new CLI process and Run; successful operation including setup, dispatch, persistence and loopback model fixture',
                'p50_ms': ordered[math.ceil(0.5 * samples) - 1],
                'p95_ms': ordered[math.ceil(0.95 * samples) - 1], 'samples': measured}
        with fixture(binary, probe, base) as case:
            case.configure('[sandbox]\nbackend=' + json.dumps(backend))
            outcome = case.run([('exec', {'argv': ['/usr/bin/true']})] * (samples + 1))
            complete(outcome)
            proof = validate_benchmark_sample(case, backend, 'warm_command', samples + 1)
            proof['run_id'] = outcome['result']['run_id']
            intervals = proof.pop('call_intervals')
            measured = intervals[1:]
            ordered = sorted(sample['elapsed_ms'] for sample in measured)
            variants['warm_command'] = {
                'scope': 'same CLI Run and PreparedEnvironment after one exec warmup; model response fully sent to next actual tool-result request received, including dispatch, process launch, persistence and loopback overhead',
                'warmup_calls': 1, 'warmup_sample': intervals[0], 'operation_proof': proof,
                'p50_ms': ordered[math.ceil(0.5 * samples) - 1],
                'p95_ms': ordered[math.ceil(0.95 * samples) - 1], 'samples': measured,
                'whole_cli_rusage': {**{key: outcome[key] for key in
                    ('elapsed_ms', 'user_cpu_ms', 'system_cpu_ms', 'peak_rss_bytes')},
                    'scope': 'whole CLI invocation wait4 rusage including warmup and all samples; CPU/RSS are not measured separately for each warm tool call'}}
        def concurrent_run(_index):
            with fixture(binary, probe, base) as case:
                case.configure('[sandbox]\nbackend=' + json.dumps(backend))
                outcome = case.run([('exec', {'argv': ['/usr/bin/true']})])
                complete(outcome)
                proof = validate_benchmark_sample(case, backend, 'short_command')
                return {**{key: outcome[key] for key in
                    ('elapsed_ms', 'user_cpu_ms', 'system_cpu_ms', 'peak_rss_bytes')}, 'proof': proof}
        batches = []
        for _ in range(samples):
            started = time.monotonic()
            with ThreadPoolExecutor(max_workers=4) as executor:
                resources = list(executor.map(concurrent_run, range(4)))
            batches.append({'elapsed_ms': (time.monotonic() - started) * 1000,
                'run_elapsed_ms': [run['elapsed_ms'] for run in resources], 'run_resources': resources,
                'user_cpu_ms': sum(run['user_cpu_ms'] for run in resources),
                'system_cpu_ms': sum(run['system_cpu_ms'] for run in resources),
                'max_child_peak_rss_bytes': max(run['peak_rss_bytes'] for run in resources)})
        ordered = sorted(batch['elapsed_ms'] for batch in batches)
        variants['concurrent_runs'] = {'scope': 'four-worker batch of independent CLI Runs, including fixture setup and cleanup',
            'resource_scope': 'CPU sums four CLI wait4 samples, excluding model fixture; max_child_peak_rss_bytes is max of four kernel child peaks and is not simultaneous aggregate Run memory',
            'p50_ms': ordered[math.ceil(0.5 * samples) - 1],
            'p95_ms': ordered[math.ceil(0.95 * samples) - 1], 'batches': batches}
        result['variants'][backend] = variants
    result['native_over_host'] = {}
    for kind in result['variants']['auto']:
        native = result['variants']['auto'][kind]
        host = result['variants']['host'][kind]
        result['native_over_host'][kind] = {
            'p50_extra_ms': native['p50_ms'] - host['p50_ms'],
            'p95_extra_ms': native['p95_ms'] - host['p95_ms'],
            'p50_ratio': native['p50_ms'] / host['p50_ms'],
            'p95_ratio': native['p95_ms'] / host['p95_ms'],
        }
    return result


def tool_evidence(path, arguments=()):
    executable = shutil.which(str(path))
    if not executable:
        return {'available': False}
    resolved = Path(executable).resolve()
    output = subprocess.run([executable, *arguments], capture_output=True, timeout=5)
    return {'available': True, 'path': str(resolved), 'sha256': digest(resolved),
            'exit_code': output.returncode, 'version': (output.stdout + output.stderr)[:2048].decode(errors='replace')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--output', type=Path, default=Path('target/n2/acceptance/native-evidence.json'))
    parser.add_argument('--benchmark-samples', type=int, default=3, choices=range(1, 21))
    parser.add_argument('--skip-benchmarks', action='store_true')
    parser.add_argument('--cgroup-root', type=Path, default=os.environ.get('SAYA_N2_CGROUP_ROOT'))
    parser.add_argument('--require-cgroup', action='store_true')
    options = parser.parse_args()
    options.output.parent.mkdir(parents=True, exist_ok=True)
    binary = options.binary.resolve(strict=True)
    root = Path(__file__).resolve().parents[2]
    source = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=root, capture_output=True, text=True, check=True).stdout.strip()
    files = subprocess.run(['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z', '--',
        'crates', 'scripts', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo', '.github'],
        cwd=root, capture_output=True, check=True).stdout.split(b'\0')
    manifest = hashlib.sha256()
    source_files = 0
    for name in sorted(set(name for name in files if name)):
        path = root / os.fsdecode(name)
        if path.is_file() and '__pycache__' not in path.parts and path.suffix != '.pyc':
            manifest.update(name + b'\0' + digest(path).encode() + b'\n')
            source_files += 1
    evidence = {'schema_version': 1, 'started_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
        'binary': {'input_path': str(binary), 'sha256': digest(binary)}, 'source_sha': source,
        'source_observation': {'manifest_sha256': manifest.hexdigest(), 'files': source_files,
            'scope': 'observed checkout source and build files, independent of caller build provenance'},
        'script_sha256': digest(__file__), 'platform': {'system': platform.system(),
            'release': platform.release(), 'version': platform.version(), 'machine': platform.machine()},
        'tools': {}, 'cases': [], 'passed': False, 'phase': 'initializing',
        'scope': 'actual supplied CLI image, local model fixture, synthetic files and loopback services',
        'supplemental_coverage': ['Rust execution_backend, exec_tools, network_broker and permissions suites',
            'CST/security fuzz, syscall filtering, schema and credential adversarial suites'],
        'unmeasured': ['real external DNS resolution', 'encrypted TLS contents inside CONNECT',
            'abrupt host death while an external tool is still running', 'cross-platform results from this invocation',
            'resource exhaustion pressure under configured cgroup limits'],
        'options': {'benchmark_samples': options.benchmark_samples, 'skip_benchmarks': options.skip_benchmarks,
            'require_cgroup': options.require_cgroup, 'delegated_cgroup_supplied': bool(options.cgroup_root)}}
    write_evidence(options.output, evidence)
    try:
        evidence['tools']['cc'] = tool_evidence('cc', ['--version'])
        for name, arguments in [('bwrap', ['--version']), ('socat', ['-V'])]:
            if platform.system() == 'Linux':
                evidence['tools'][name] = tool_evidence(name, arguments)
        if platform.system() == 'Darwin':
            evidence['tools']['sandbox-exec'] = tool_evidence('/usr/bin/sandbox-exec', ['-h'])
        image = options.output.parent.resolve() / ('saya-' + evidence['binary']['sha256'])
        if not image.exists():
            shutil.copy2(binary, image)
            image.chmod(0o500)
        require(digest(image) == evidence['binary']['sha256'], 'Input image changed while copying')
        evidence['binary']['executed_copy_path'] = str(image)
        with tempfile.TemporaryDirectory(prefix='saya-n2-', dir='/var/tmp') as directory:
            base = Path(directory).resolve()
            probe_source = base / 'probe.c'
            probe_source.write_text(PROBE_SOURCE)
            probe = base / 'probe'
            subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-O2', str(probe_source), '-o', str(probe)],
                           check=True, capture_output=True, timeout=60)
            evidence['probe_sha256'] = digest(probe)
            evidence['probe_source_sha256'] = hashlib.sha256(PROBE_SOURCE.encode()).hexdigest()
            tests = [('six_file_tools_versions', files_case), ('plan_readonly', plan_case),
                ('explicit_deny_under_bypass', deny_case), ('bypass_os_protected_files', protected_case),
                ('project_instructions_and_skill_permissions', skill_case),
                ('explicit_host_positive_control', host_case),
                ('native_ipv4_direct_denied', lambda case: network_case(case, socket.AF_INET)),
                ('native_ipv6_direct_denied', lambda case: network_case(case, socket.AF_INET6)),
                ('native_unix_socket_denied', lambda case: network_case(case, socket.AF_UNIX)),
                ('native_raw_dns_transport_denied', dns_case), ('proxy_http_allow_deny', proxy_case),
                ('proxy_connect_allow_deny', lambda case: proxy_case(case, True)),
                ('proxy_udp_same_port_bypass_denied', proxy_udp_case),
                ('host_process_metadata_denied', process_metadata_case),
                ('exec_child_cancel', cancel_case), ('crash_after_write_recovery', recovery_case),
                ('missing_resource_fail_closed', resources_case), ('per_process_fd_limit', file_descriptors_case)]
            if platform.system() == 'Linux' and options.cgroup_root:
                tests.append(('delegated_cgroup_resources', lambda case: resources_case(case, options.cgroup_root)))
            else:
                evidence['cases'].append({'name': 'delegated_cgroup_resources', 'status': 'not_run',
                    'reason': 'No Linux delegated cgroup was supplied for this invocation'})
            for name, test in tests:
                started = time.monotonic()
                case = None
                try:
                    with fixture(image, probe, base) as case:
                        details = test(case)
                    value = {'name': name, 'status': 'passed', 'details': details}
                except (AssertionError, OSError, ValueError, KeyError, IndexError, subprocess.SubprocessError) as error:
                    value = {'name': name, 'status': 'failed', 'error_type': type(error).__name__, 'message': str(error)[:4096]}
                    if case is not None:
                        value['diagnostics'] = failure_diagnostics(case)
                value['elapsed_ms'] = (time.monotonic() - started) * 1000
                evidence['cases'].append(value)
                write_evidence(options.output, evidence)
                print(name, value['status'], flush=True)
            if not options.skip_benchmarks:
                try:
                    evidence['performance'] = benchmark(image, probe, base, options.benchmark_samples)
                    evidence['performance']['proxy_latency'] = benchmark_proxy(
                        image, probe, base, options.benchmark_samples)
                except (AssertionError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
                    evidence['cases'].append({'name': 'performance', 'status': 'failed', 'message': str(error)[:4096]})
            evidence['binary']['executed_copy_sha256_after'] = digest(image)
            require(evidence['binary']['executed_copy_sha256_after'] == evidence['binary']['sha256'], 'Executed binary changed during acceptance')
        evidence['passed'] = not any(case['status'] == 'failed' for case in evidence['cases'])
        if options.require_cgroup and not any(case['name'] == 'delegated_cgroup_resources'
                                            and case['status'] == 'passed' for case in evidence['cases']):
            evidence['passed'] = False
        evidence['phase'] = 'complete'
    except Exception as error:
        evidence['passed'] = False
        evidence['phase'] = 'failed'
        evidence['fatal'] = {'error_type': type(error).__name__, 'message': str(error)[:4096]}
        detail = getattr(error, 'stderr', None)
        if detail:
            evidence['fatal']['stderr'] = (detail.decode(errors='replace') if isinstance(detail, bytes) else str(detail))[:4096]
        raise
    finally:
        evidence['finished_utc'] = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
        write_evidence(options.output, evidence)
    return 0 if evidence['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
