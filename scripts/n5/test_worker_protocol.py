#!/usr/bin/env python3
import argparse
import http.client
import io
import json
import re
from urllib.parse import urlsplit
import os
from pathlib import Path
import selectors
import shlex
import signal
import subprocess
import tempfile
import time
import unittest


REQUEST_SECONDS = 10
TOTAL_SECONDS = 120
STDOUT_LIMIT = 2 * 1024 * 1024
STDERR_LIMIT = 256 * 1024
PRIVATE_METHOD = "saya/browser/processInfo"
PRIVATE_CAPTURE_METHOD = "saya/browser/captureScreenshot"
PRIVATE_METHODS = (PRIVATE_METHOD, PRIVATE_CAPTURE_METHOD)


class Worker:
    def __init__(self, node, cli, directory, deadline, flags=("--isolated",)):
        self.deadline = deadline
        self.pending = bytearray()
        self.stderr = bytearray()
        self.stdout_bytes = 0
        self.request_id = 0
        self.selector = selectors.DefaultSelector()
        self.process = None
        self.marker = directory / "browser-executed"
        sentinel = directory / "browser-sentinel"
        sentinel.write_text(
            "#!/bin/sh\n"
            f"printf 'executed\\n' > {shlex.quote(str(self.marker))}\n"
            "exit 97\n",
            encoding="utf-8",
        )
        sentinel.chmod(0o700)
        env = {"PATH": "/usr/bin:/bin", "LANG": "C", "LC_ALL": "C"}
        for key, name in (
            ("HOME", "home"), ("TMPDIR", "tmp"), ("TMP", "tmp"),
            ("TEMP", "tmp"), ("XDG_CACHE_HOME", "cache"),
            ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"), ("XDG_RUNTIME_DIR", "run"),
        ):
            path = directory / name
            path.mkdir(mode=0o700, exist_ok=True)
            env[key] = str(path)
        output = directory / "output"
        output.mkdir(mode=0o700)
        try:
            self.process = subprocess.Popen(
                [str(node), str(cli), *flags, "--headless",
                 "--ignore-roots", "--no-webmcp", "--executable-path",
                 str(sentinel), "--output-dir", str(output)],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, cwd=directory, env=env,
                start_new_session=True, bufsize=0,
            )
            for stream, name in ((self.process.stdout, "stdout"),
                                 (self.process.stderr, "stderr")):
                os.set_blocking(stream.fileno(), False)
                self.selector.register(stream, selectors.EVENT_READ, name)
            os.set_blocking(self.process.stdin.fileno(), False)
        except BaseException:
            self.close()
            raise

    def fail(self, message):
        diagnostic = self.stderr[-4000:].decode("utf-8", errors="replace")
        raise AssertionError(f"{message}; worker stderr: {diagnostic!r}")

    def remaining(self, deadline):
        remaining = min(deadline, self.deadline) - time.monotonic()
        if remaining <= 0:
            self.fail("JSON-RPC request or total deadline exceeded")
        return remaining

    def pump(self, deadline):
        events = self.selector.select(self.remaining(deadline))
        if not events:
            self.fail("JSON-RPC request or total deadline exceeded")
        for key, _ in events:
            if key.data == "stdin":
                continue
            try:
                chunk = os.read(key.fd, 65536)
            except BlockingIOError:
                continue
            if not chunk:
                self.selector.unregister(key.fileobj)
                if key.data == "stdout":
                    self.fail("worker stdout closed before response")
                continue
            if key.data == "stderr":
                if len(self.stderr) + len(chunk) > STDERR_LIMIT:
                    self.fail("worker stderr exceeded byte limit")
                self.stderr.extend(chunk)
            else:
                self.stdout_bytes += len(chunk)
                if self.stdout_bytes > STDOUT_LIMIT:
                    self.fail("worker stdout exceeded byte limit")
                self.pending.extend(chunk)

    def send(self, message, deadline):
        payload = memoryview((json.dumps(message) + "\n").encode("utf-8"))
        stream = self.process.stdin
        self.selector.register(stream, selectors.EVENT_WRITE, "stdin")
        try:
            while payload:
                self.remaining(deadline)
                try:
                    written = os.write(stream.fileno(), payload)
                except BlockingIOError:
                    self.pump(deadline)
                    continue
                except BrokenPipeError:
                    self.fail("worker stdin closed before request")
                if written == 0:
                    self.fail("worker stdin made no progress")
                payload = payload[written:]
        finally:
            self.selector.unregister(stream)

    def request(self, method, params):
        deadline = min(self.deadline, time.monotonic() + REQUEST_SECONDS)
        self.request_id += 1
        request_id = self.request_id
        self.send({"jsonrpc": "2.0", "id": request_id,
                   "method": method, "params": params}, deadline)
        while True:
            self.remaining(deadline)
            if b"\n" not in self.pending:
                self.pump(deadline)
                continue
            line, _, rest = self.pending.partition(b"\n")
            self.pending = bytearray(rest)
            try:
                response = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                self.fail("worker emitted invalid JSON")
            if not isinstance(response, dict) or response.get("jsonrpc") != "2.0":
                self.fail(f"invalid JSON-RPC envelope: {response!r}")
            if "id" not in response and isinstance(response.get("method"), str):
                continue
            if type(response.get("id")) is not int or response["id"] != request_id:
                self.fail(f"unexpected response id: {response!r}")
            if ("result" in response) == ("error" in response):
                self.fail(f"expected exactly one result or error: {response!r}")
            return response

    def initialize(self):
        response = self.request("initialize", {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "saya-worker-protocol-test", "version": "1"},
        })
        if response.get("result", {}).get("protocolVersion") != "2025-11-25":
            self.fail(f"initialization failed: {response!r}")
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"},
                  min(self.deadline, time.monotonic() + REQUEST_SECONDS))

    def close(self):
        process = self.process
        self.process = None
        error = None
        try:
            if process is not None:
                process.stdin.close()
                try:
                    process.wait(timeout=0.5)
                except subprocess.TimeoutExpired:
                    if process.returncode is None:
                        try:
                            os.killpg(process.pid, signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                    try:
                        process.wait(timeout=0.5)
                    except subprocess.TimeoutExpired:
                        if process.returncode is None:
                            try:
                                os.killpg(process.pid, signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                        process.wait(timeout=2)
                try:
                    os.killpg(process.pid, 0)
                except ProcessLookupError:
                    pass
                else:
                    raise AssertionError(
                        f"cleanup failed: process group {process.pid} still exists"
                    )
        except BaseException as caught:
            error = caught
        finally:
            if process is not None:
                for stream in (process.stdin, process.stdout, process.stderr):
                    if stream is not None:
                        try:
                            stream.close()
                        except BaseException as caught:
                            if error is None:
                                error = caught
            try:
                self.selector.close()
            except BaseException as caught:
                if error is None:
                    error = caught
        if error is not None:
            raise error


class DeadlineHTTPReader(io.RawIOBase):
    def __init__(self, sock, worker, deadline):
        super().__init__()
        self.sock = sock
        self.stream = sock.makefile("rb", buffering=0)
        self.worker = worker
        self.deadline = deadline
        self.received = 0
        self.headers = bytearray()
        self.headers_complete = False

    def readable(self):
        return True

    def close(self):
        try:
            self.stream.close()
        finally:
            super().close()

    def readinto(self, buffer):
        self.sock.settimeout(self.worker.remaining(self.deadline))
        chunk = self.stream.read(min(len(buffer), 8192))
        self.worker.remaining(self.deadline)
        self.received += len(chunk)
        if self.received > STDOUT_LIMIT:
            self.worker.fail("HTTP response exceeded byte limit")
        if not self.headers_complete:
            self.headers.extend(chunk)
            end = self.headers.find(b"\r\n\r\n")
            if (end < 0 and len(self.headers) > 65536) or end > 65532:
                self.worker.fail("HTTP headers exceeded byte limit")
            if end >= 0:
                self.headers_complete = True
                self.headers.clear()
        buffer[:len(chunk)] = chunk
        return len(chunk)


class HTTPWorker(Worker):
    def initialize(self):
        deadline = min(self.deadline, time.monotonic() + REQUEST_SECONDS)
        while True:
            self.remaining(deadline)
            match = re.search(rb"(?:^|\n)Listening on ([^\r\n]+)\r?\n", self.stderr)
            if match:
                url = urlsplit(match[1].decode("ascii"))
                if (url.scheme != "http" or url.hostname != "127.0.0.1"
                        or url.username is not None or url.password is not None
                        or url.path not in ("", "/", "/mcp")
                        or url.query or url.fragment or not url.port):
                    self.fail("HTTP CLI advertised a non-loopback HTTP endpoint")
                self.port = url.port
                break
            self.pump(deadline)
        self.session_id = None
        response = self.request("initialize", {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "saya-worker-protocol-test", "version": "1"},
        })
        if response.get("result", {}).get("protocolVersion") != "2025-11-25":
            self.fail(f"HTTP initialization failed: {response!r}")
        if not self.session_id:
            self.fail("HTTP initialization omitted Mcp-Session-Id")
        self.post({"jsonrpc": "2.0", "method": "notifications/initialized"},
                  notification=True)

    def post(self, message, notification=False):
        deadline = min(self.deadline, time.monotonic() + REQUEST_SECONDS)
        connection = http.client.HTTPConnection(
            "127.0.0.1", self.port, timeout=self.remaining(deadline),
        )

        def response_factory(sock, **kwargs):
            response = http.client.HTTPResponse(sock, **kwargs)
            response.fp.close()
            response.fp = io.BufferedReader(DeadlineHTTPReader(sock, self, deadline))
            return response

        connection.response_class = response_factory
        headers = {"Content-Type": "application/json",
                   "Accept": "application/json, text/event-stream",
                   "MCP-Protocol-Version": "2025-11-25"}
        if self.session_id is not None:
            headers["Mcp-Session-Id"] = self.session_id
        try:
            connection.connect()
            original_send = connection.send

            def send_before_deadline(data):
                connection.sock.settimeout(self.remaining(deadline))
                original_send(data)
                self.remaining(deadline)

            connection.send = send_before_deadline
            connection.request("POST", "/mcp", json.dumps(message), headers)
            self.remaining(deadline)
            with connection.getresponse() as response:
                expected_status = 202 if notification else 200
                if response.status != expected_status:
                    self.fail(f"HTTP status {response.status}, expected {expected_status}")
                session_id = response.getheader("Mcp-Session-Id")
                if session_id is not None:
                    if self.session_id is not None and session_id != self.session_id:
                        self.fail("HTTP response changed Mcp-Session-Id")
                    self.session_id = session_id
                body = bytearray()
                while True:
                    self.remaining(deadline)
                    chunk = response.read1(8192)
                    if not chunk:
                        break
                    body.extend(chunk)
                    if len(body) > STDOUT_LIMIT:
                        self.fail("HTTP body exceeded byte limit")
                self.remaining(deadline)
                if notification:
                    if body:
                        self.fail("HTTP initialized notification returned a body")
                    return None
                content_type = response.getheader("Content-Type", "").split(";", 1)[0].strip()
                if content_type == "application/json":
                    return json.loads(body)
                if content_type != "text/event-stream":
                    self.fail(f"unexpected HTTP content type: {content_type!r}")
                events = []
                data = []
                for line in body.decode("utf-8").replace("\r\n", "\n").split("\n"):
                    if line == "":
                        if data:
                            events.append(json.loads("\n".join(data)))
                            data = []
                    elif line.startswith("data:"):
                        value = line[5:]
                        data.append(value[1:] if value.startswith(" ") else value)
                if data or len(events) != 1:
                    self.fail("expected one complete HTTP SSE response event")
                return events[0]
        finally:
            connection.close()

    def request(self, method, params):
        self.request_id += 1
        response = self.post({"jsonrpc": "2.0", "id": self.request_id,
                              "method": method, "params": params})
        if (not isinstance(response, dict) or response.get("jsonrpc") != "2.0"
                or type(response.get("id")) is not int
                or response["id"] != self.request_id
                or ("result" in response) == ("error" in response)):
            self.fail(f"invalid HTTP JSON-RPC response: {response!r}")
        return response


class WorkerProtocolTests(unittest.TestCase):
    def setUp(self):
        if time.monotonic() >= self.deadline:
            self.fail("total test deadline exceeded")
        temporary = tempfile.TemporaryDirectory(
            prefix="worker-protocol-", dir=self.temp_root,
        )
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)

    def start_worker(self, flags=("--isolated",), http=False):
        worker_type = HTTPWorker if http else Worker
        self.worker = worker_type(self.node, self.cli, self.directory,
                                  self.deadline, flags=flags)
        self.addCleanup(self.stop_worker_and_check_no_browser)
        self.worker.initialize()

    def stop_worker_and_check_no_browser(self):
        try:
            self.worker.close()
        finally:
            self.assertFalse(self.worker.marker.exists(),
                             "browser sentinel executed: request launched a browser")

    def assert_rpc_error(self, response, code):
        self.assertNotIn("result", response, response)
        self.assertIsInstance(response.get("error"), dict, response)
        self.assertEqual(response["error"].get("code"), code, response)
        return response["error"]

    def test_empty_params_report_browser_not_started_without_launch(self):
        self.start_worker()
        for method in PRIVATE_METHODS:
            with self.subTest(method=method):
                response = self.worker.request(method, {})
                error = self.assert_rpc_error(response, -32000)
                self.assertIsInstance(error.get("data"), dict, response)
                self.assertEqual(error["data"].get("reason"), "browser_not_started", response)

    def test_nonempty_object_params_are_rejected_without_launch(self):
        self.start_worker()
        cases = [
            {"method": "Runtime.evaluate"},
            {"selector": "body"},
            {"script": "1 + 1"},
            {"processId": 1},
            {"method": "SystemInfo.getProcessInfo"},
            {"params": {}},
            {"params": {"method": "Runtime.evaluate", "script": "1 + 1"}},
        ]
        for method in PRIVATE_METHODS:
            for params in cases:
                with self.subTest(method=method, params=params):
                    self.assert_rpc_error(self.worker.request(method, params), -32602)

    def test_reserved_progress_metadata_reports_browser_not_started_without_launch(self):
        self.start_worker()
        cases = [
            {},
            {"_meta": {}},
            {"_meta": {"progressToken": 0}},
            {"_meta": {"progressToken": 4294967295}},
            {"_meta": {"progressToken": "role-check"}},
        ]
        for method in PRIVATE_METHODS:
            for params in cases:
                with self.subTest(method=method, params=params):
                    response = self.worker.request(method, params)
                    error = self.assert_rpc_error(response, -32000)
                    self.assertIsInstance(error.get("data"), dict, response)
                    self.assertEqual(error["data"].get("reason"), "browser_not_started", response)
                    self.assertFalse(self.worker.marker.exists(), "browser sentinel executed")

    def test_progress_metadata_does_not_allow_business_parameters_without_launch(self):
        self.start_worker()
        cases = [
            {"_meta": {"progressToken": 0, "method": "SystemInfo.getProcessInfo"}},
            {"_meta": {"progressToken": 0, "script": "1 + 1"}},
            {"_meta": {"progressToken": 0, "selector": "body"}},
            {"_meta": {"progressToken": 0, "processId": 1}},
            {"_meta": {"progressToken": 0}, "method": "SystemInfo.getProcessInfo"},
            {"_meta": {"progressToken": 0}, "script": "1 + 1"},
            {"_meta": {"progressToken": 0}, "selector": "body"},
            {"_meta": {"progressToken": 0}, "processId": 1},
        ]
        for method in PRIVATE_METHODS:
            for params in cases:
                with self.subTest(method=method, params=params):
                    self.assert_rpc_error(self.worker.request(method, params), -32602)
                    self.assertFalse(self.worker.marker.exists(), "browser sentinel executed")

    def test_tools_directory_keeps_private_process_queries_hidden(self):
        self.start_worker()
        response = self.worker.request("tools/list", {})
        self.assertNotIn("error", response, response)
        tools = response.get("result", {}).get("tools")
        self.assertIsInstance(tools, list, response)
        names = set()
        for tool in tools:
            self.assertIsInstance(tool, dict)
            self.assertIsInstance(tool.get("name"), str)
            names.add(tool["name"])
        self.assertIn("browser_navigate", names)
        self.assertIn("browser_snapshot", names)
        for method in PRIVATE_METHODS:
            self.assertNotIn(method, names)
        self.assertFalse(
            {name for name in names if name.startswith("saya/")
             or "process" in name.lower()},
            f"private process tools exposed: {names!r}",
        )

    def test_unknown_private_method_stays_method_not_found(self):
        self.start_worker()
        response = self.worker.request("saya/browser/notProcessInfo", {})
        self.assert_rpc_error(response, -32601)

    def test_nonisolated_stdio_rejects_private_method_without_launch(self):
        self.start_worker(flags=())
        for method in PRIVATE_METHODS:
            self.assert_rpc_error(self.worker.request(method, {}), -32601)

    def test_shared_context_stdio_rejects_private_method_without_launch(self):
        self.start_worker(flags=("--isolated", "--shared-browser-context"))
        for method in PRIVATE_METHODS:
            self.assert_rpc_error(self.worker.request(method, {}), -32601)

    def test_isolated_http_rejects_private_method_without_launch(self):
        self.start_worker(flags=("--isolated", "--port", "0", "--host", "127.0.0.1"),
                          http=True)
        for method in PRIVATE_METHODS:
            self.assert_rpc_error(self.worker.request(method, {}), -32601)


def main():
    parser = argparse.ArgumentParser(description="Real fixed-worker JSON-RPC contract tests")
    parser.add_argument("--node", required=True, type=Path)
    parser.add_argument("--worker-root", required=True, type=Path)
    parser.add_argument("--temp-root", required=True, type=Path)
    args = parser.parse_args()
    node = args.node.resolve()
    worker_root = args.worker_root.resolve()
    temp_root = args.temp_root.resolve()
    target = (Path(__file__).resolve().parents[2] / "target").resolve()
    if not node.is_file() or not os.access(node, os.X_OK):
        parser.error("--node must name an executable file")
    cli = worker_root / "node_modules/@playwright/mcp/cli.js"
    if not cli.is_file():
        parser.error("--worker-root must contain node_modules/@playwright/mcp/cli.js")
    for package, version in (("@playwright/mcp", "0.0.85"),
                             ("playwright-core", "1.66.0-alpha-2026-11-09")):
        manifest = worker_root / "node_modules" / package / "package.json"
        try:
            actual = json.loads(manifest.read_text(encoding="utf-8"))["version"]
        except (OSError, ValueError, KeyError, TypeError) as error:
            parser.error(f"cannot read {package} version: {error}")
        if actual != version:
            parser.error(f"{package} must be {version}, got {actual!r}")
    if not temp_root.is_relative_to(target) or temp_root == target:
        parser.error("--temp-root must be a dedicated directory under repository target/")
    temp_root.mkdir(parents=True, exist_ok=True)
    WorkerProtocolTests.node = node
    WorkerProtocolTests.cli = cli
    WorkerProtocolTests.temp_root = temp_root
    WorkerProtocolTests.deadline = time.monotonic() + TOTAL_SECONDS
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(WorkerProtocolTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
