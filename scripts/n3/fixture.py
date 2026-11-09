"""Bounded local provider and subprocess fixtures for N3 candidate acceptance."""
from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import subprocess
import threading
import time

MAX_CAPTURE = 1024 * 1024
MAX_REQUEST = 8 * 1024 * 1024
MAX_REQUESTS = 256
PROTOCOLS = ("openai-chat-completions", "openai-responses", "anthropic-messages")


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)
    elif isinstance(value, dict):
        for item in value.values():
            yield from strings(item)


def content_text(value):
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return "".join(block.get("text", "") for block in value if isinstance(block, dict))
    return ""


def results(request):
    found = {}
    for item in request.get("messages", []) + request.get("input", []):
        if not isinstance(item, dict):
            continue
        if item.get("role") == "tool":
            found[item["tool_call_id"]] = json.loads(content_text(item["content"]))
        if item.get("type") == "function_call_output":
            found[item["call_id"]] = json.loads(content_text(item["output"]))
        if isinstance(item.get("content"), list):
            for block in item["content"]:
                if block.get("type") == "tool_result":
                    found[block["tool_use_id"]] = json.loads(content_text(block["content"]))
    return found


def reply(protocol, text=None, call=None):
    if protocol == PROTOCOLS[0]:
        message = {"role": "assistant", "content": text}
        if call:
            identifier, name, arguments = call
            message["tool_calls"] = [{"id": identifier, "type": "function", "function": {
                "name": name, "arguments": json.dumps(arguments, ensure_ascii=False)}}]
        return {"choices": [{"index": 0, "finish_reason": "tool_calls" if call else "stop", "message": message}]}
    if protocol == PROTOCOLS[1]:
        if call:
            identifier, name, arguments = call
            output = [{"type": "function_call", "id": "fc_" + identifier, "call_id": identifier,
                       "name": name, "arguments": json.dumps(arguments, ensure_ascii=False), "status": "completed"}]
        else:
            output = [{"type": "message", "id": "fixture-final", "status": "completed", "role": "assistant",
                       "content": [{"type": "output_text", "text": text, "annotations": []}]}]
        return {"status": "completed", "output": output}
    if call:
        identifier, name, arguments = call
        blocks = [{"type": "tool_use", "id": identifier, "name": name, "input": arguments}]
    else:
        blocks = [{"type": "text", "text": text}]
    return {"id": "fixture-message", "type": "message", "role": "assistant", "model": "fixture",
            "content": blocks, "stop_reason": "tool_use" if call else "end_turn",
            "usage": {"input_tokens": 100, "output_tokens": 20}}


def summary(data):
    value = {"schema_version": 1, "covered_range": data["assigned_source_range"],
             "objective": [], "constraints": [], "decisions": [], "completed": [],
             "remaining": [], "uncertainties": [], "required_ids": [], "references": []}
    users=set()
    observations=set()
    for unit in data["source_units"]:
        for source in unit:
            if "prior_summary_data" in source:
                previous = source["prior_summary_data"]
                previous = previous.get("summary", previous)
                for category in ("objective", "constraints", "decisions", "completed", "remaining", "uncertainties"):
                    value[category].extend(previous[category])
                    for fact in previous[category]:
                        if fact["evidence"]=="user_request": users.update(fact["source_ids"])
                        elif fact["evidence"]=="observed": observations.update(fact["source_ids"])
            elif source.get("role") == "user":
                users.add(source["source_id"])
                value["objective"].append({"text": "Recorded user task", "source_ids": [source["source_id"]],
                                           "evidence": "user_request", "status": "active"})
            elif source.get("role") in ("tool","provider","mechanical"):
                observations.add(source["source_id"])
    categories = {"objective": "objective", "constraint": "constraints", "remaining": "remaining", "recovery": "uncertainties"}
    for item in data["required_items"]:
        evidence="user_request" if set(item["source_ids"]).issubset(users) else "observed" if set(item["source_ids"]).issubset(observations) else "claim"
        value[categories[item["kind"]]].append({"text": item["text"], "source_ids": item["source_ids"],
                                                "evidence": evidence, "status": item["status"]})
        value["required_ids"].append(item["id"])
    value["references"] = [reference["id"] for reference in data["references"]]
    return value


class Dialogue:
    """One explicit operation at a time, with result assertions before advancing."""
    def __init__(self, prefix, steps, retired_result=None):
        self.prefix = prefix
        self.steps = steps
        self.index = 0
        self.observed = {}
        self.arguments = {}
        self.tool_ids = []
        self.completed = False
        self.retired_result = retired_result

    def respond(self, request):
        observed = results(request)
        if self.index:
            identifier = self.tool_ids[-1]
            previous = self.steps[self.index - 1]
            if identifier in observed:
                result = observed[identifier]
            else:
                require(self.retired_result is not None and self.index == len(self.steps)
                        and previous[1] == "memory_forget", "provider request omitted the preceding paired tool result")
                result = self.retired_result(request, identifier, previous[1], self.arguments[previous[0]])
            self.observed[previous[0]] = result
            checker = previous[3] if len(previous) > 3 else None
            if checker:
                checker(result, self)
            else:
                require(result.get("ok") is True, "tool failed: " + previous[1] + " " + json.dumps(result)[:2048])
        if self.index == len(self.steps):
            self.completed = True
            return {"text": "N3 fixture completed"}
        label, name, arguments = self.steps[self.index][:3]
        arguments = arguments(self) if callable(arguments) else arguments
        identifier = "n3-" + self.prefix + "-" + str(self.index)
        self.arguments[label] = arguments
        self.tool_ids.append(identifier)
        self.index += 1
        return {"call": (identifier, name, arguments)}


class Provider:
    def __init__(self, protocol):
        self.protocol = protocol
        self.dialogue = None
        self.requests = []
        self.errors = []
        self.counts = {"execution": 0, "review": 0, "compaction": 0}
        self.server = None

    def respond(self, request):
        require(len(self.requests) < MAX_REQUESTS, "fixture request budget exceeded")
        self.requests.append(request)
        system = content_text(request.get("system", "")) + request.get("instructions", "")
        for message in request.get("messages", []):
            if message.get("role") == "system":
                system += content_text(message.get("content"))
        if "# Saya: independent operation review v1" in system:
            self.counts["review"] += 1
            return reply(self.protocol, text=json.dumps({"decision": "allow", "reason": "Controlled N3 fixture"}))
        for text in strings(request.get("messages", []) + request.get("input", [])):
            try:
                data = json.loads(text)
            except (ValueError, TypeError):
                continue
            if isinstance(data, dict) and {"assigned_source_range", "source_units", "required_items", "output_schema"}.issubset(data):
                self.counts["compaction"] += 1
                return reply(self.protocol, text=json.dumps(summary(data), ensure_ascii=False))
        self.counts["execution"] += 1
        require(self.dialogue is not None, "unexpected execution request outside a journey")
        return reply(self.protocol, **self.dialogue.respond(request))

    @contextmanager
    def serving(self):
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                    require(0 < length <= MAX_REQUEST, "model request exceeds the fixture byte limit")
                    request = json.loads(self.rfile.read(length))
                    response = owner.respond(request)
                    body = json.dumps(response, ensure_ascii=False).encode()
                    self.send_response(200)
                except Exception as cause:
                    owner.errors.append(str(cause)[:4096])
                    body = b'{"error":"controlled fixture assertion failed"}'
                    self.send_response(500)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        try:
            yield self
        finally:
            self.server.shutdown()
            thread.join(timeout=5)
            self.server.server_close()


class ProcessFailure(AssertionError):
    def __init__(self,message,code,outputs,elapsed):
        super().__init__(message)
        self.record={"exit_code":code,"stdout":bytes(outputs[0]).decode("utf-8","replace")[:16384],
                     "stderr":bytes(outputs[1]).decode("utf-8","replace")[:16384],"elapsed_seconds":round(elapsed,3)}


def bounded_process(arguments, cwd, env, timeout=120):
    process = subprocess.Popen(arguments, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    outputs = [bytearray(), bytearray()]
    exceeded = threading.Event()

    def drain(source, target):
        while True:
            chunk = source.read(4096)
            if not chunk:
                break
            capacity = MAX_CAPTURE - len(target)
            target.extend(chunk[:max(capacity, 0)])
            if len(chunk) > capacity:
                exceeded.set()
                try:
                    process.send_signal(signal.SIGINT)
                except ProcessLookupError:
                    pass
                break
        source.close()

    readers = [threading.Thread(target=drain, args=(stream, output), daemon=True)
               for stream, output in zip((process.stdout, process.stderr), outputs)]
    for reader in readers:
        reader.start()
    started = time.monotonic()
    timed_out=False
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out=True
        process.send_signal(signal.SIGINT)
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
    finally:
        for reader in readers:
            reader.join(timeout=5)
    if timed_out or exceeded.is_set() or any(reader.is_alive() for reader in readers):
        reason="candidate process timed out" if timed_out else "candidate output exceeded the bounded capture" if exceeded.is_set() else "candidate output pipes did not close"
        raise ProcessFailure(reason,process.returncode,outputs,time.monotonic()-started)
    return process.returncode, *(bytes(output).decode("utf-8", "replace") for output in outputs), time.monotonic() - started
