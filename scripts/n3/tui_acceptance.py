#!/usr/bin/env python3
"""Real candidate PTY journeys for N3 memory management and compaction.

Requires an explicit executable SHA and runner-declared source commit. The
existing release PTY/VT observer supplies terminal restoration checks, while the
N3 provider supplies bounded local protocol responses and request evidence.
"""
import argparse
from contextlib import closing
import json
from pathlib import Path
import platform
import re
import sqlite3
import sys
import tempfile
import threading
import time

from acceptance import Candidate, source_digest, write_report
from fixture import PROTOCOLS, Provider, require, sha256

RELEASE = Path(__file__).resolve().parents[1] / "release"
sys.path.insert(0, str(RELEASE))
from tui_journey import Terminal

MODAL_TITLE = "Memory · explicit user administration"
DRAFT = "N3 retained unsent draft"
ORIGINAL = "TUI_ORIGINAL_PREFERENCE_00"
CORRECTED = "TUI_CORRECTED_PREFERENCE_00"
FIRST_TASK = "Complete the first controlled N3 task"
SECOND_TASK = "Complete the next controlled N3 task"


class Answer:
    def __init__(self, gated=False):
        self.started = threading.Event()
        self.release = threading.Event()
        if not gated:
            self.release.set()
        self.completed = False

    def respond(self, _request):
        self.started.set()
        require(self.release.wait(45), "TUI fixture was not released before its deadline")
        self.completed = True
        return {"text": "N3_FINAL_OBSERVATION\n" + "Earlier controlled observation. " * 1600}


def write_wire(path, requests):
    encoded = json.dumps(requests, ensure_ascii=False, indent=2).encode()
    require(len(encoded) <= 16 * 1024 * 1024, "TUI wire evidence exceeds its byte bound")
    path.write_bytes(encoded)


def sqlite_rows(candidate, query, parameters=()):
    path = candidate.data_dir / "sessions.sqlite3"
    if not path.exists():
        return []
    with closing(sqlite3.connect("file:" + str(path) + "?mode=ro", uri=True)) as connection:
        return connection.execute(query, parameters).fetchall()


def history(candidate, session_id):
    return sqlite_rows(candidate, "SELECT ordinal,item FROM messages WHERE session_id=? ORDER BY ordinal", (session_id,))


def user_texts(rows):
    return [item["data"] for _, encoded in rows
            for item in [json.loads(encoded)] if item.get("kind") == "user_text"]


def final_runs(candidate, session_id):
    rows = sqlite_rows(candidate, "SELECT session_id,finished_at,result FROM runs ORDER BY rowid")
    require(len(rows) == 4, "TUI journeys created an unexpected number of Runs")
    require(all(row[0] == session_id and row[1] is not None and row[2] is not None
                and json.loads(row[2]).get("status") == "completed" for row in rows),
            "TUI journeys left a failed, unfinished or unrelated Run")
    return len(rows)


def evidence_error(report, stage, cause):
    report["status"] = "failed"
    report.setdefault("evidence_errors", []).append(
        {"stage": stage, "type": type(cause).__name__, "message": str(cause)[:8192]})


def try_evidence(report, stage, operation):
    try:
        return operation()
    except Exception as cause:
        evidence_error(report, stage, cause)
        return None


def failed_journey(report, journey):
    report["failed_protocol"] = {"protocol": journey.candidate.provider.protocol,
        "journeys": journey.records, "screens": journey.screens,
        "terminal_restored": journey.restored, "request_counts": journey.candidate.provider.counts,
        "fixture_errors": journey.candidate.provider.errors}
    try:
        try_evidence(report, "failed_screen", lambda: journey.capture("failed"))
        try_evidence(report, "failed_terminal_log", lambda:
                     (journey.candidate.root / "terminal.raw").write_bytes(journey.terminal.raw))
        try_evidence(report, "failed_wire", lambda: write_wire(
            journey.candidate.root / "provider-requests.json", journey.candidate.provider.requests))
    finally:
        try_evidence(report, "terminal_cleanup", journey.close)


def finish_identity(report, binary, source, expected_sha, before_source):
    report["candidate"]["sha256_after"] = try_evidence(report, "candidate_identity", lambda: sha256(binary))
    report["source"]["after"] = try_evidence(report, "source_identity", lambda: source_digest(source))
    if report["candidate"]["sha256_after"] != expected_sha or report["source"]["after"] != before_source:
        report["status"] = "failed"
        report["identity_failure"] = "candidate or source changed or became unreadable during acceptance"


def seed(candidate, workspace):
    result = None
    for index in range(15):
        revision = candidate.invoke(workspace, ["memory", "version"], expected_status=None)["data"]["revision"]
        source = {"id": "src_tui_user", "kind": "user", "scope": {"kind": "global"},
                  "reference": "explicit-admin-input", "revision": "a" * 64,
                  "observed_at": "not_recorded", "description": "Controlled acceptance user source",
                  "metadata_withheld": False}
        fact = {"id": "fact_tui_%02d" % index, "kind": "explicit_preference",
                "text": ORIGINAL if index == 0 else "TUI_SCOPED_FACT_%02d" % index,
                "sources": [{"source_id": source["id"], "revision": source["revision"]}],
                "aliases": [], "derived_from": []}
        entry = {"id": "mem_tui_%02d" % index, "title": "TUI Memory Entry %02d" % index,
                 "scope": {"kind": "global"}, "updated_at": "not_recorded", "facts": [fact], "resources": []}
        change = candidate.root / ("seed-%02d.json" % index)
        change.write_text(json.dumps({"operation": "upsert", "entry": entry, "sources": [source]}))
        result = candidate.invoke(workspace, ["memory", "apply", "--change", str(change),
                                  "--expected-revision", revision, "--operation-id", "op_tui_seed_%02d" % index],
                                  expected_status=None)
        require(result["data"]["status"] == "committed", "CLI did not seed actual memory")
    return result["data"]["receipt"]["generation"]


class Journey:
    def __init__(self, candidate, workspace):
        self.candidate = candidate
        self.workspace = workspace
        self.records = []
        self.screens = []
        self.modal_input = ""
        self.restored = False
        self.closed = False
        environment = dict(candidate.env, TERM="xterm-256color")
        self.terminal = Terminal([str(candidate.binary), "--config", str(candidate.config),
                                  "--cwd", str(workspace), "--data-dir", str(candidate.data_dir)],
                                 workspace, environment)
        self.terminal.resize(140, 44)
        self.terminal.title()

    def capture(self, name):
        path = self.candidate.root / (name + ".screen.txt")
        path.write_text(self.terminal.screen.text)
        self.screens.append({"name": name, "path": str(path), "sha256": sha256(path)})

    def close(self):
        if not self.closed:
            self.terminal.close()
            self.closed = True

    def modal(self):
        return MODAL_TITLE in self.terminal.screen.row_text(1)

    def modal_body(self):
        screen = self.terminal.screen
        left = (screen.columns - min(screen.columns - 4, 112)) // 2 + 1
        width = min(screen.columns - 4, 112) - 2
        return "\n".join("".join(row[left:left + width]).rstrip()
                         for row in screen.cells[4:screen.rows - 5])

    def modal_input_text(self):
        screen = self.terminal.screen
        left = (screen.columns - min(screen.columns - 4, 112)) // 2 + 1
        width = min(screen.columns - 4, 112) - 2
        return "".join(screen.cells[screen.rows - 4][left:left + width]).strip()

    def hint(self):
        return self.terminal.screen.row_text(self.terminal.screen.rows - 5)

    def open_palette(self, command):
        terminal = self.terminal
        terminal.send(b"\x10")
        terminal.wait(lambda: terminal.screen.command_palette_selected() is not None, "command palette")
        for _ in range(64):
            # A Ratatui diff may recolor a row before replacing its label.
            # Observe the completed quiet frame before deciding what Enter runs.
            terminal.settle()
            selected = terminal.screen.command_palette_selected()
            require(selected is not None, "command palette lost its selection")
            text = selected[2][2:].rstrip()
            if text.split(maxsplit=1)[0] == command:
                terminal.send(b"\r")
                return
            terminal.send(b"\x1b[B")
            terminal.wait(lambda: terminal.screen.command_palette_selected() is not None
                          and terminal.screen.command_palette_selected() != selected, "next palette selection")
        raise AssertionError("command was absent from the real palette: " + command)

    def open_memory(self):
        self.open_palette("/memory")
        self.terminal.wait(self.modal, "memory administration modal")
        self.terminal.wait(lambda: "Working" not in self.hint() and self.modal_input_text() == ">"
                            and self.modal_body().lstrip().startswith("{"), "current memory view")
        self.terminal.settle()
        self.modal_input = ""

    def command(self, value, error=None):
        require(self.modal(), "memory command requires the real modal")
        if self.modal_input:
            self.terminal.send(b"\x1b[F" + b"\x7f" * len(self.modal_input))
            self.terminal.wait(lambda: self.modal_input_text() == ">", "cleared memory command")
        self.terminal.send(value)
        self.terminal.wait(lambda: self.modal_input_text().startswith(">")
                            and self.modal_input_text().endswith(value[-24:]), "typed memory command")
        self.terminal.settle()
        self.terminal.send(b"\r")
        if error:
            self.terminal.wait(lambda: error in self.modal_body(), "memory command rejection")
            self.terminal.settle()
            self.modal_input = value
        else:
            self.terminal.wait(lambda: self.modal_input_text() == ">" and "Working" not in self.hint(),
                               "completed memory command")
            self.terminal.settle()
            self.modal_input = ""

    def top(self):
        self.terminal.send(b"\x1b[5~" * 32)
        # A visible first JSON line proves the scroll request was consumed.
        self.terminal.wait(lambda: self.modal_body().lstrip().startswith("{"), "top of current JSON view")

    def scan(self, tokens=(), pattern=None):
        self.top()
        observed = ""
        for _ in range(40):
            current = self.modal_body()
            observed += "\n" + current
            if all(token in observed for token in tokens) and (pattern is None or re.search(pattern, observed)):
                return observed
            self.terminal.send(b"\x1b[6~")
            try:
                self.terminal.wait(lambda: self.modal_body() != current, "next memory viewport", timeout=2)
            except RuntimeError:
                require(self.terminal.process.poll() is None, "TUI exited while browsing memory")
                break
        raise AssertionError("memory view omitted required evidence: " + ", ".join(tokens))

    def stage(self, command, before, after=None):
        revision = self.candidate.invoke(self.workspace, ["memory", "version"], expected_status=None)["data"]["revision"]
        self.command(command)
        text = self.scan([before] + ([after] if after else []), r'"operation_id"\s*:\s*"op_[A-Za-z0-9_-]+"')
        identifier = re.search(r'"operation_id"\s*:\s*"(op_[A-Za-z0-9_-]+)"', text).group(1)
        require("Review the complete diff" in self.hint(), "mutation was not staged for human review")
        after_revision = self.candidate.invoke(self.workspace, ["memory", "version"], expected_status=None)["data"]["revision"]
        require(after_revision == revision, "staging the UI review wrote memory")
        return identifier, revision

    def close_memory(self):
        self.terminal.send(b"\x1b")
        self.terminal.wait(lambda: not self.modal(), "closed memory modal")
        self.modal_input = ""

    def wait_runs(self, count):
        terminal = self.terminal
        def finished():
            rows = sqlite_rows(self.candidate, "SELECT result FROM runs WHERE finished_at IS NOT NULL ORDER BY rowid")
            return len(rows) >= count and all(json.loads(row[0])["status"] == "completed" for row in rows[:count])
        terminal.wait(finished, "persisted N3 run completion", timeout=40)
        terminal.wait(lambda: terminal.screen.status() == "Completed", "idle completed TUI", timeout=30)


def memory_journeys(journey, generation):
    candidate, workspace, terminal = journey.candidate, journey.workspace, journey.terminal
    terminal.send(DRAFT)
    terminal.wait(lambda: terminal.editor_text() == DRAFT, "retained message draft")
    journey.open_memory()
    journey.scan(["mem_tui_00", '"page": 1'])
    journey.top()
    before = journey.modal_body()
    terminal.wheel(40, 12, down=True)
    terminal.wait(lambda: journey.modal_body() != before, "memory-only mouse wheel movement")
    journey.command("next")
    journey.scan(["mem_tui_12", '"page": 2'])
    journey.command("search TUI_ORIGINAL")
    journey.scan([ORIGINAL, '"scope"', '"snapshot_revision"'])
    journey.command("sources fact_tui_00")
    journey.scan(["explicit-admin-input", '"revision"', '"global"'])
    journey.close_memory()
    terminal.wait(lambda: terminal.editor_text() == DRAFT, "draft after memory browsing")
    journey.records.append({"name": "palette-memory-query-pagination-wheel-sources-draft", "status": "passed"})
    journey.capture("memory-browse-draft")

    journey.open_memory()
    identifier, revision = journey.stage("correct fact_tui_00 " + CORRECTED, ORIGINAL, CORRECTED)
    journey.command("confirm wrong wrong", error="Confirmation must match")
    unchanged = candidate.invoke(workspace, ["memory", "version"], expected_status=None)["data"]["revision"]
    require(unchanged == revision, "wrong UI confirmation changed memory")
    journey.command("confirm " + identifier + " " + revision)
    journey.scan(['"status": "committed"', '"receipt"'])
    receipt = candidate.invoke(workspace, ["memory", "operation", identifier], expected_status=None)["data"]["receipt"]
    require(receipt["generation"] == generation + 1, "UI correction was not exactly one committed operation")
    shown = candidate.invoke(workspace, ["memory", "show", "--fact-id", "fact_tui_00"], expected_status=None)
    require(shown["data"]["items"][0]["text"] == CORRECTED, "UI correction was not current")
    journey.records.append({"name": "complete-review-wrong-confirmation-exactly-once-correction", "status": "passed", "operation_id": identifier})
    journey.capture("memory-corrected")

    stale_id, stale_revision = journey.stage("forget fact_tui_01", "TUI_SCOPED_FACT_01")
    journey.command("set read off")
    require("read false" in terminal.screen.text, "read switch was not visibly disabled")
    require("TUI_SCOPED_FACT_01" not in journey.modal_body(), "disabled read retained pending review text")
    journey.command("confirm " + stale_id + " " + stale_revision, error="Prepare and review")
    require(candidate.invoke(workspace, ["memory", "version"], expected_status=None)["data"]["revision"] == stale_revision,
            "read-disabled stale confirmation changed memory")
    journey.command("set read on")
    journey.command("set inject off")
    journey.command("set write off")
    require("inject false" in terminal.screen.text and "write false" in terminal.screen.text,
            "independent memory switches are not visible")
    journey.command("set inject on")
    journey.command("set write on")
    journey.close_memory()
    terminal.wait(lambda: terminal.editor_text() == DRAFT, "draft after memory settings")
    journey.records.append({"name": "read-off-invalidates-review-and-process-switches", "status": "passed"})
    journey.capture("memory-settings-draft")
    require(not candidate.provider.requests, "user-side memory administration unexpectedly called a model")


def compact_journeys(journey):
    candidate, terminal, provider = journey.candidate, journey.terminal, journey.candidate.provider
    terminal.send(b"\x03")
    terminal.wait(lambda: terminal.editor_text() == "", "cleared retained draft for explicit task")
    first = Answer()
    provider.dialogue = first
    terminal.line(FIRST_TASK)
    terminal.wait(first.started.is_set, "first actual model request")
    journey.wait_runs(1)
    sessions = sqlite_rows(candidate, "SELECT id FROM sessions")
    require(len(sessions) == 1, "TUI created an unexpected session")
    session_id = sessions[0][0]
    before = history(candidate, session_id)
    counts = dict(provider.counts)
    terminal.line("/compact")
    journey.wait_runs(2)
    require(provider.counts["execution"] == counts["execution"], "idle compact invoked the execution model")
    require(provider.counts["compaction"] > counts["compaction"], "idle compact made no actual summary request")
    require(history(candidate, session_id) == before, "idle compact changed original user/tool history")
    checkpoints = sqlite_rows(candidate, "SELECT count(*) FROM change_log WHERE kind='context_checkpoint'")[0][0]
    require(checkpoints > 0, "idle compact did not publish a durable checkpoint")
    journey.records.append({"name": "idle-compact-summary-only-original-history-cas", "status": "passed", "session_id": session_id,
                            "checkpoint_records": checkpoints})
    journey.capture("idle-compact")

    final = Answer(gated=True)
    provider.dialogue = final
    try:
        terminal.line(SECOND_TASK)
        terminal.wait(final.started.is_set, "active final execution request")
        active_history = history(candidate, session_id)
        require(user_texts(active_history) == [FIRST_TASK, SECOND_TASK],
                "active request did not persist both original user messages")
        journey.open_memory()
        revision = candidate.invoke(journey.workspace, ["memory", "version"], expected_status=None)["data"]["revision"]
        journey.command("forget fact_tui_00", error="prepared and confirmed while idle")
        journey.command("set write off", error="settings can be changed while idle")
        require(candidate.invoke(journey.workspace, ["memory", "version"], expected_status=None)["data"]["revision"] == revision,
                "active-run memory controls wrote memory")
        journey.close_memory()
        terminal.line("/compact")
        terminal.until("Compaction requested for the next model boundary")
        require(not final.completed, "execution was released before queued compact was observed")
        final.release.set()
        journey.wait_runs(4)
    finally:
        final.release.set()
    after = history(candidate, session_id)
    require(after[:len(active_history)] == active_history,
            "queued compaction modified the original active-request history prefix")
    require(user_texts(after) == [FIRST_TASK, SECOND_TASK], "queued compaction appended or lost a user message")
    require(provider.counts["execution"] == counts["execution"] + 1, "queued compact added an execution model call")
    require(provider.counts["compaction"] >= counts["compaction"] + 2, "queued compact was lost at the final response")
    require(sqlite_rows(candidate, "SELECT count(*) FROM change_log WHERE kind='context_checkpoint'")[0][0] > checkpoints,
            "queued compact did not publish the next durable checkpoint")
    require(not provider.errors, "local provider rejected a real request: " + "; ".join(provider.errors))
    require(sqlite_rows(candidate, "SELECT count(*) FROM tool_calls")[0][0] == 0,
            "standalone compact or user memory controls executed an Agent tool")
    journey.records.append({"name": "active-memory-write-denial-and-final-response-compact-queue", "status": "passed",
                            "session_id": session_id, "original_user_messages": len(user_texts(after))})
    journey.capture("queued-compact")
    return session_id


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--expected-sha256", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--protocol", choices=PROTOCOLS, action="append")
    options = parser.parse_args()
    require(re.fullmatch(r"[a-f0-9]{64}", options.expected_sha256), "expected SHA must be complete lowercase SHA-256")
    require(re.fullmatch(r"[a-f0-9]{40,64}", options.source_commit), "source commit must be a complete object ID")
    binary = options.binary.resolve(strict=True)
    report_path = options.report.resolve()
    source = options.source_root.resolve(strict=True)
    before_source = source_digest(source)
    actual_sha = sha256(binary)
    report = {"schema_version": 1, "kind": "saya_n3_tui_acceptance", "status": "failed", "actual_journeys": 0,
              "platform": {"system": platform.system(), "release": platform.release(), "machine": platform.machine()},
              "candidate": {"path": str(binary), "expected_sha256": options.expected_sha256, "sha256_before": actual_sha},
              "source": {"declared_commit": options.source_commit, "before": before_source,
                         "build_binding": "runner-declared commit; current source digest independently recorded"},
              "harness": {str(path): sha256(path) for path in
                          [Path(__file__).resolve(), RELEASE / "tui_journey.py", RELEASE / "process_witness.py"]},
              "protocols": []}
    if actual_sha != options.expected_sha256:
        report["failure"] = {"type": "artifact_identity", "message": "candidate SHA differs from the selected artifact"}
        write_report(report_path, report)
        return 1
    root = options.work_dir.resolve() if options.work_dir else Path(tempfile.mkdtemp(prefix="saya-n3-tui-"))
    if options.work_dir:
        root.mkdir(parents=True, exist_ok=False)
    report["work_dir"] = str(root)
    started = time.monotonic()
    current = None
    try:
        for index, protocol in enumerate(options.protocol or PROTOCOLS):
            directory = root / str(index)
            directory.mkdir()
            provider = Provider(protocol)
            with provider.serving():
                candidate = Candidate(binary, directory, provider)
                # Make these explicit user compact requests independent of the
                # automatic threshold tested by the core/CLI acceptance matrix.
                with candidate.config.open("a") as config:
                    config.write("\n[context]\nauto_compact=false\n")
                workspace = directory / "workspace"
                workspace.mkdir()
                generation = seed(candidate, workspace)
                current = Journey(candidate, workspace)
                memory_journeys(current, generation)
                session_id = compact_journeys(current)
                current.terminal.finish()
                current.restored = True
                current.capture("terminal-restored")
                run_count = final_runs(candidate, session_id)
                require(sqlite_rows(candidate, "PRAGMA integrity_check")[0][0] == "ok", "SQLite integrity check failed")
                current.close()
                raw_path = directory / "terminal.raw"
                raw_path.write_bytes(current.terminal.raw)
                request_path = directory / "provider-requests.json"
                write_wire(request_path,provider.requests)
                report["protocols"].append({"protocol": protocol, "status": "passed", "journeys": current.records,
                    "screens": current.screens, "terminal_restored": current.restored,
                    "raw_log": {"path": str(raw_path), "sha256": sha256(raw_path), "retention_bytes": 2 * 1024 * 1024},
                    "request_counts": provider.counts, "wire": {"path": str(request_path), "sha256": sha256(request_path)},
                    "sqlite": {"path": str(candidate.data_dir / "sessions.sqlite3"), "sessions": len(sqlite_rows(candidate, "SELECT id FROM sessions")),
                               "runs": run_count, "all_runs_completed": True, "integrity": "ok"}})
                current = None
        report["status"] = "passed"
    except Exception as cause:
        report["failure"] = {"type": type(cause).__name__, "message": str(cause)[:8192]}
        if current:
            failed_journey(report, current)
    finally:
        report["actual_journeys"] = sum(len(item["journeys"]) for item in report["protocols"]) + (len(current.records) if current else 0)
        finish_identity(report, binary, source, options.expected_sha256, before_source)
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
        write_report(report_path, report)
    print(json.dumps({"status": report["status"], "report": str(report_path), "actual_journeys": report["actual_journeys"]}))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
