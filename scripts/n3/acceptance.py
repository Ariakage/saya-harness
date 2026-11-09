#!/usr/bin/env python3
"""Offline N3 journeys against one explicitly selected real Saya CLI image.

The runner supplies build provenance. This report separately records the
declared commit, current source-tree digest, candidate hash and actual journeys.
Failed workspaces and bounded provider/process diagnostics remain reviewable.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import sqlite3
import sys
import tempfile
import time
import uuid

from fixture import Dialogue, PROTOCOLS, Provider, ProcessFailure, bounded_process, require, sha256, strings

MAX_REPORT = 1024 * 1024


def source_digest(root):
    require(root.is_dir(), "source snapshot root is unavailable")
    selected = [root / name for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "justfile")]
    selected += sorted((root / "crates").rglob("*.rs"))
    selected += sorted((root / "crates").rglob("Cargo.toml"))
    selected += sorted((root / "crates").rglob("*.md"))
    selected += sorted((root / "crates").rglob("*.json"))
    selected += sorted((root / "docs" / "schemas").glob("*.json"))
    selected += sorted((root / "scripts" / "n3").glob("*.py"))
    digest = hashlib.sha256()
    total = 0
    files = 0
    for path in sorted(set(selected)):
        if not path.exists():
            continue
        require(path.is_file() and not path.is_symlink(), "source snapshot contains an unexpected linked file")
        size = path.stat().st_size
        require(size <= 8 * 1024 * 1024, "source file exceeds snapshot bound")
        total += size
        require(total <= 128 * 1024 * 1024, "source snapshot exceeds its byte bound")
        digest.update(str(path.relative_to(root)).encode() + b"\0" + bytes.fromhex(sha256(path)))
        files += 1
    return {"sha256": digest.hexdigest(), "files": files, "bytes": total}


def data(dialogue, label):
    return dialogue.observed[label]["data"]


def ok(result, _dialogue):
    require(result.get("ok") is True, "expected successful result: " + json.dumps(result)[:2048])


def rejected(result, _dialogue):
    require(result.get("ok") is False and result.get("error"), "hard boundary unexpectedly allowed the tool")


def missing_memory(result, _dialogue):
    require(result.get("ok") is False and result.get("error", {}).get("code") == "memory_not_found",
            "forgotten fact lookup did not return the exact missing-memory contract")


class Candidate:
    def __init__(self, binary, root, provider):
        self.binary = binary
        self.root = root
        self.provider = provider
        self.data_dir = root / "data"
        self.memory_root = root / "home" / ".saya"
        self.config = root / "config.toml"
        (root / "home").mkdir()
        self.env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(root / "home"),
                    "XDG_DATA_HOME": str(root / "xdg-data"), "XDG_STATE_HOME": str(root / "state"),
                    "XDG_CACHE_HOME": str(root / "cache"), "NO_PROXY": "127.0.0.1,localhost",
                    "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": str(root / "absent-git-config")}
        self.processes = []
        self.journeys = []
        self.write_config()

    def write_config(self, deny=None, plugins=False):
        text = ["schema_version=1", "[defaults]", 'profile="fixture"', "[profiles.fixture]",
                "protocol=" + json.dumps(self.provider.protocol),
                "base_url=" + json.dumps("http://127.0.0.1:" + str(self.provider.server.server_port) + "/v1"),
                'model="n3-fixture"', 'auth="none"', "stream=false", "context_window_tokens=128000",
                "[memory]", "root=" + json.dumps(str(self.memory_root)), "read=true", "inject=true", "write=true",
                "[plugins]", "enabled=" + str(plugins).lower()]
        if deny:
            text += ["[[permissions.rules]]", 'effect="deny"', "tool=" + json.dumps(deny)]
        self.config.write_text("\n".join(text) + "\n")

    def invoke(self, workspace, arguments, permissions="auto", mode="work", expected_status="completed"):
        command = [str(self.binary), "--config", str(self.config), "--cwd", str(workspace),
                   "--data-dir", str(self.data_dir), "--output", "json", "--permissions", permissions,
                   "--mode", mode, *arguments]
        try:
            code, stdout, stderr, elapsed = bounded_process(command, workspace, self.env)
        except ProcessFailure as cause:
            self.processes.append({"arguments":arguments[:1],"permissions":permissions,"mode":mode,**cause.record})
            raise
        record = {"arguments": arguments[:1], "permissions": permissions, "mode": mode,
                  "exit_code": code, "elapsed_seconds": round(elapsed, 3),
                  "stdout": stdout[:16384], "stderr": stderr[:16384]}
        self.processes.append(record)
        value = json.loads(stdout)
        if expected_status is None:
            require(code==0 and value.get("error") is None,"offline CLI query failed: "+json.dumps(value)[:2048])
        if expected_status is not None:
            require(value.get("status") == expected_status,
                    "candidate status mismatch: " + json.dumps(value)[:4096] + " stderr=" + stderr[:2048])
            require(code == (3 if expected_status == "needs_confirmation" else 0), "candidate exit code mismatch")
        require(not self.provider.errors, "provider assertion failed: " + "; ".join(self.provider.errors))
        return value

    def run(self, name, workspace, dialogue, prompt, session=None, permissions="auto", mode="work", expected_status="completed"):
        self.provider.dialogue = dialogue
        start_counts = dict(self.provider.counts)
        arguments = ["run", prompt]
        if session:
            arguments += ["--session", session]
        journey={"name":name,"status":"failed"}
        self.journeys.append(journey)
        try:
            value = self.invoke(workspace, arguments, permissions, mode, expected_status)
            if expected_status == "completed":
                require(dialogue.completed, "journey did not finish its requested tool steps")
            journey.update({"status":"passed","session_id":value.get("session_id"),"run_id":value.get("run_id")})
        finally:
            journey.update({"tools_requested":dialogue.index,"tools_observed":len(dialogue.observed),
                            "requests":{key:self.provider.counts[key]-start_counts[key] for key in start_counts}})
        return value

    def history(self, session):
        connection = sqlite3.connect("file:" + str(self.data_dir / "sessions.sqlite3") + "?mode=ro", uri=True)
        try:
            rows = connection.execute("SELECT item FROM messages WHERE session_id=? ORDER BY ordinal", (session,)).fetchall()
            return [json.loads(row[0]) for row in rows]
        finally:
            connection.close()

    def retired_result(self, request, identifier, name, arguments):
        """Observe a real committed outcome only through its exact host retirement proof."""
        connection = sqlite3.connect("file:" + str(self.data_dir / "sessions.sqlite3") + "?mode=ro", uri=True)
        try:
            rows = connection.execute("SELECT session_id FROM messages WHERE json_extract(item,'$.kind')='tool_call' "
                                      "AND json_extract(item,'$.data.call_id')=?", (identifier,)).fetchall()
            require(len(rows) == 1, "retired tool call has no unique durable session identity")
            session = rows[0][0]
            raw = [row[0] for row in connection.execute("SELECT item FROM messages WHERE session_id=? ORDER BY ordinal", (session,))]
            items = [json.loads(item) for item in raw]
            call_indexes = [index for index, item in enumerate(items) if item.get("kind") == "tool_call"
                            and item["data"]["call_id"] == identifier]
            result_indexes = [index for index, item in enumerate(items) if item.get("kind") == "tool_result"
                              and item["data"]["call_id"] == identifier]
            require(len(call_indexes) == len(result_indexes) == 1, "retired tool lacks its durable history pair")
            call_index, result_index = call_indexes[0], result_indexes[0]
            call, result = items[call_index]["data"], items[result_index]["data"]["result"]
            require(call["name"] == name == "memory_forget" and json.loads(call["arguments"]) == arguments
                    and result_index > call_index, "retired history differs from the requested operation")
            records = connection.execute("SELECT t.call,t.state,t.result FROM tool_calls t JOIN runs r ON r.id=t.run_id "
                                         "WHERE r.session_id=? AND t.call_id=?", (session, identifier)).fetchall()
            require(len(records) == 1 and records[0][1] == "succeeded" and json.loads(records[0][0]) == call
                    and json.loads(records[0][2]) == result and result.get("ok") is True,
                    "retired outcome is absent, failed, unknown or differs from its durable intent")
            for text in strings(request.get("messages", []) + request.get("input", [])):
                try:
                    envelope = json.loads(text).get("saya_retired_history_group_v1")
                except (ValueError, AttributeError):
                    continue
                if not isinstance(envelope, dict):
                    continue
                selected = envelope.get("source_range", {})
                start, end = selected.get("start"), selected.get("end")
                if not (type(start) is int and type(end) is int and 0 <= start <= call_index < result_index < end <= len(raw)):
                    continue
                digest = hashlib.sha256(("[" + ",".join(raw[start:end]) + "]").encode()).hexdigest()
                require(envelope.get("trust") == "historical_source_data" and envelope.get("reason") == "memory_context_changed"
                        and envelope.get("source_sha256") == digest and envelope.get("observations") == []
                        and envelope.get("current_memory_revision") == result.get("data", {}).get("current_revision"),
                        "retirement projection is stale or differs from the durable source group")
                return result
            raise AssertionError("missing exact host retirement envelope for the committed forget outcome")
        finally:
            connection.close()

    def durable_audit(self):
        connection = sqlite3.connect("file:" + str(self.data_dir / "sessions.sqlite3") + "?mode=ro", uri=True)
        try:
            require(connection.execute("PRAGMA integrity_check").fetchone()[0] == "ok", "SQLite integrity check failed")
            pending = connection.execute("SELECT count(*) FROM tool_calls WHERE state IN ('running','unknown')").fetchone()[0]
            require(pending == 0, "acceptance left an unfinished or unknown tool intent")
            calls = connection.execute("SELECT call,state,result,run_id FROM tool_calls").fetchall()
            waiting = 0
            for raw, state, encoded_result, run_id in calls:
                require(encoded_result is not None and state in ("succeeded", "failed", "denied", "waiting_confirmation"), "unpaired durable tool outcome")
                call, result = json.loads(raw), json.loads(encoded_result)
                if state == "waiting_confirmation":
                    run = connection.execute("SELECT result FROM runs WHERE id=?", (run_id,)).fetchone()
                    approvals = [json.loads(row[0]) for row in connection.execute("SELECT request FROM approval_requests WHERE run_id=?", (run_id,))]
                    require(result.get("ok") is False and result.get("error", {}).get("code") == "needs_confirmation"
                            and run and run[0] and json.loads(run[0]).get("status") == "needs_confirmation"
                            and any(item.get("call_id") == call["call_id"] and item.get("run_id") == run_id for item in approvals),
                            "waiting tool outcome lacks its exact confirmation result, stopped Run or approval request")
                    waiting += 1
            sessions = connection.execute("SELECT id FROM sessions").fetchall()
            history_calls = history_results = 0
            for (session,) in sessions:
                items = [json.loads(row[0]) for row in connection.execute("SELECT item FROM messages WHERE session_id=? ORDER BY ordinal", (session,))]
                requested = {}
                paired = {}
                for item in items:
                    if item.get("kind") == "tool_call":
                        call = item.get("data", item.get("call", {}))
                        requested[call["call_id"]] = requested.get(call["call_id"], 0) + 1
                    elif item.get("kind") == "tool_result":
                        result = item.get("data", item)
                        paired[result["call_id"]] = paired.get(result["call_id"], 0) + 1
                require(requested == paired and all(count == 1 for count in requested.values()), "session tool history is not exactly paired")
                history_calls += sum(requested.values())
                history_results += sum(paired.values())
            require(history_calls>0,"candidate produced no auditable paired tool history")
            output_records = connection.execute("SELECT count(*) FROM change_log WHERE kind='output_refs'").fetchone()[0]
            approvals = connection.execute("SELECT count(*) FROM approval_requests").fetchone()[0]
            return {"sessions": len(sessions), "durable_tool_outcomes": len(calls), "history_tool_calls": history_calls,
                    "history_tool_results": history_results, "output_reference_records": output_records,
                    "approval_requests": approvals, "waiting_confirmation_outcomes": waiting, "unfinished_intents": pending, "integrity": "ok"}
        finally:
            connection.close()


def memory_journeys(candidate, token):
    workspace = candidate.root / "workspace-a"
    workspace.mkdir()
    other = candidate.root / "workspace-b"
    other.mkdir()
    local_text = "workspace-observation-" + token
    global_text = "global-preference-" + token
    corrected = "corrected-preference-" + token
    (workspace / "evidence.txt").write_text(local_text + "\n")
    global_id, local_id = "fact_global_" + token, "fact_local_" + token

    def global_args(dialogue):
        return {"operation_id": "op_global_" + token, "expected_revision": data(dialogue, "initial")["revision"],
                "scope": "global", "entry_id": None, "title": "Global fixture preference",
                "facts": [{"id": global_id, "kind": "explicit_preference", "text": global_text,
                           "aliases": ["global-alias-" + token], "evidence": [{"kind": "user_message", "ordinal": 1}]}]}

    def absent_read(result,dialogue):
        ok(result,dialogue)
        require(not candidate.memory_root.exists(),"read-only missing memory access created its root")

    first = Dialogue("memory-seed-" + token, [
        ("file", "read_file", {"path": "evidence.txt", "start_line": 1, "line_count": 5}),
        ("evidence", "memory_evidence", {}),
        ("initial", "memory_read", {}, absent_read),
        ("global", "memory_update", global_args),
        ("revision", "memory_read", {}),
        ("local", "memory_update", lambda dialogue: {
            "operation_id": "op_local_" + token, "expected_revision": data(dialogue, "revision")["revision"],
            "scope": "workspace", "entry_id": None, "title": "Workspace observation",
            "facts": [{"id": local_id, "kind": "observed_fact", "text": local_text,
                       "evidence": [{"kind": "tool_result", "call_id": dialogue.tool_ids[0]}]}]}),
        ("read", "memory_read", {}),
        ("search", "memory_search", {"query": "global-alias-" + token}),
    ])
    initial = candidate.run("memory evidence/update/read/search", workspace, first,
                            "Remember my global preference " + global_text + ". Record the workspace fact from evidence.txt separately.")
    require(len(data(first, "read")["items"]) == 2, "global and workspace facts were not persisted")
    require(data(first, "search")["matches"], "memory alias search missed the new fact")
    require(any(entry["selector"].get("kind") == "tool_result" for entry in data(first, "evidence")["entries"]), "actual tool evidence selector missing")
    require(candidate.provider.counts["review"] >= 2, "auto memory mutations skipped independent model review")
    original_update = first.arguments["global"]
    global_entry=next(item["reference"]["entry_id"] for item in data(first,"read")["items"] if item["reference"]["fact_id"]==global_id)

    def only_global(result, _dialogue):
        ok(result, _dialogue)
        require([item["reference"]["fact_id"] for item in result["data"]["items"]] == [global_id], "workspace-scoped fact leaked into another workspace")

    second = Dialogue("memory-other-" + token, [
        ("read", "memory_read", {}, only_global),
        ("correct", "memory_update", lambda dialogue: {
            "operation_id": "op_correct_" + token, "expected_revision": data(dialogue, "read")["revision"],
            "scope": "global", "entry_id": global_entry, "title": "Global fixture preference",
            "facts": [{"id": global_id, "kind": "user_correction", "text": corrected,
                       "evidence": [{"kind": "user_message", "ordinal": 1}]}]}),
        ("corrected", "memory_read", {"fact_id": global_id}),
        ("forget", "memory_forget", lambda dialogue: {"operation_id": "op_forget_" + token,
            "expected_revision": data(dialogue, "corrected")["revision"], "scope": "global", "fact_ids": [global_id]}),
    ], retired_result=candidate.retired_result)
    candidate.run("cross-workspace correction and explicit forgetting", other, second,
                  "Correct my global preference to " + corrected + ", verify it, and then explicitly forget that global fact.")
    require(data(second, "corrected")["items"][0]["text"] == corrected, "correction did not become current")
    forgotten = Dialogue("memory-forgotten-" + token, [
        ("missing", "memory_read", {"fact_id": global_id}, missing_memory),
        ("empty", "memory_read", {}),
    ])
    candidate.run("new Run confirms explicit forgetting", other, forgotten, "Inspect current memory after the explicit forgetting operation.")
    require(not data(forgotten, "empty")["items"], "explicitly forgotten fact remained current")

    def no_replay(result, _dialogue):
        ok(result, _dialogue)
        require(result["data"]["status"] == "already_committed" and result["data"]["applied_now"] is False,
                "old mutation retry was reapplied after forgetting")

    resumed = Dialogue("memory-resume-" + token, [
        ("read", "memory_read", {}),
        ("retry", "memory_update", original_update, no_replay),
        ("after", "memory_read", {}),
    ])
    candidate.run("resume preserves workspace fact and never replays forgotten mutation", workspace, resumed,
                  "Check current memory and safely retry the old operation receipt without restoring forgotten facts.", initial["session_id"])
    require([item["reference"]["fact_id"] for item in data(resumed, "after")["items"]] == [local_id], "forgotten global fact was revived or workspace fact was lost")
    require(data(resumed, "read")["revision"] == data(resumed, "after")["revision"], "receipt retry changed memory")

    before = candidate.history(initial["session_id"])
    counts = dict(candidate.provider.counts)
    candidate.provider.dialogue = None
    candidate.invoke(workspace, ["compact", initial["session_id"]])
    require(candidate.provider.counts["compaction"] > counts["compaction"], "compact made no structured-summary request")
    require(candidate.provider.counts["execution"] == counts["execution"], "compact unexpectedly called the execution model")
    require(candidate.history(initial["session_id"]) == before, "compact edited immutable raw session history")
    candidate.journeys.append({"name": "manual compact preserves raw history", "status": "passed", "session_id": initial["session_id"],
                               "requests": {key: candidate.provider.counts[key] - counts[key] for key in counts}})
    continuation = Dialogue("memory-continued-" + token, [("read", "memory_read", {})])
    candidate.run("continue after compact preserves forgetting", workspace, continuation, "Continue and inspect current memory.", initial["session_id"])
    require([item["reference"]["fact_id"] for item in data(continuation, "read")["items"]] == [local_id], "compact continuation resurrected a forgotten fact")
    return workspace


def plan_probe(revision,token):
    require(re.fullmatch(r"[a-f0-9]{64}",revision),"PLAN probe requires the actual current memory revision")
    return {"operation_id":"op_forbidden_"+token,"expected_revision":revision,"scope":"global","entry_id":None,"title":"Forbidden",
            "facts":[{"id":None,"kind":"explicit_preference","text":"forbidden","evidence":[{"kind":"user_message","ordinal":1}]}]}


def permission_journeys(candidate, workspace, token):
    manual = Dialogue("manual-" + token, [("read", "memory_read", {})])
    candidate.run("Manual Approval requires human before even a read", workspace, manual, "Inspect memory", permissions="manual", expected_status="needs_confirmation")
    require(not manual.observed, "Manual Approval read executed without a human answer")
    before = candidate.invoke(workspace, ["memory", "version"], expected_status=None)["data"]
    plan = Dialogue("plan-" + token, [("write", "memory_update",plan_probe(before["revision"],token),rejected)])
    candidate.run("PLAN rejects a valid current-version memory update under bypass",workspace,plan,
                  "Store my global preference forbidden in global memory.",permissions="bypass",mode="plan")
    after = candidate.invoke(workspace, ["memory", "version"], expected_status=None)["data"]
    require(before == after, "PLAN attempt mutated memory revisions")
    candidate.write_config(deny="memory_read")
    try:
        deny = Dialogue("deny-" + token, [("read", "memory_read", {}, rejected)])
        candidate.run("explicit deny remains effective under bypass", workspace, deny, "Inspect memory", permissions="bypass")
    finally:
        candidate.write_config()


def checkpoint_dialogue(git):
    return Dialogue("checkpoint-" + ("git" if git else "plain"), [
        ("read", "read_file", {"path": "selected.txt", "start_line": 1, "line_count": 5}),
        ("untouched", "read_file", {"path": "untouched.txt", "start_line": 1, "line_count": 5}),
        ("create", "checkpoint_create", {"paths": ["selected.txt", "created.txt", "untouched.txt"], "label": "N3 original"}),
        ("write", "write_file", lambda dialog: {"path": "selected.txt", "content": "changed\n", "expected_version": data(dialog, "read")["version"]}),
        ("new", "write_file", {"path": "created.txt", "content": "accepted\n", "expected_version": None}),
        ("newversion", "read_file", {"path":"created.txt","start_line":1,"line_count":5}),
        ("change-unselected", "write_file", lambda dialog:{"path":"untouched.txt","content":"leave-this-change\n","expected_version":data(dialog,"untouched")["version"]}),
        ("diff", "checkpoint_diff", lambda dialog: {"checkpoint": data(dialog, "create")["checkpoint"]["token"]}),
        ("accept", "checkpoint_accept", lambda dialog: {"checkpoint": data(dialog, "create")["checkpoint"]["token"], "paths": ["created.txt"]}),
        ("alter-accepted", "write_file", lambda dialog:{"path":"created.txt","content":"changed-after-accept\n","expected_version":data(dialog,"newversion")["version"]}),
        ("restore", "checkpoint_restore", lambda dialog: {"checkpoint": data(dialog, "accept")["checkpoint"]["token"], "paths": ["selected.txt","created.txt"]}),
        ("list", "checkpoint_list", {}),
    ])


def checkpoint_journey(candidate, git=False):
    workspace = candidate.root / ("checkpoint-git" if git else "checkpoint-directory")
    workspace.mkdir()
    if git:
        executable = shutil.which("git", path=candidate.env["PATH"])
        require(executable is not None, "Git checkpoint acceptance requires the host git executable")
        code, _, stderr, _ = bounded_process([executable, "-c", "init.templateDir=", "init", "--quiet", str(workspace)], workspace, candidate.env)
        require(code == 0, "isolated git fixture initialization failed: " + stderr[:1024])
    controls = {str(path.relative_to(workspace)): sha256(path) for path in (workspace / ".git").rglob("*") if path.is_file()} if git else {}
    (workspace / "selected.txt").write_text("checkpoint original\n")
    (workspace / "untouched.txt").write_text("unselected-original\n")
    dialogue=checkpoint_dialogue(git)
    candidate.run("checkpoint " + ("Git" if git else "ordinary directory") + " capture/diff/accept/restore", workspace, dialogue,
                  "Capture selected files, edit them, accept created.txt, edit it again, then restore selected.txt and created.txt while preserving the unselected change.", permissions="bypass")
    require((workspace / "selected.txt").read_text() == "checkpoint original\n", "checkpoint restore did not restore the selected file")
    require((workspace / "created.txt").read_text() == "accepted\n", "accepted baseline did not restore its selected file")
    require((workspace / "untouched.txt").read_text()=="leave-this-change\n","checkpoint restore changed an unselected file")
    if git:
        after = {str(path.relative_to(workspace)): sha256(path) for path in (workspace / ".git").rglob("*") if path.is_file()}
        require(controls == after, "checkpoint operations changed Git control files")


def skill_output_journey(candidate):
    workspace = candidate.root / "skill-output"
    skill = workspace / ".saya" / "skills" / "n3-guide"
    skill.mkdir(parents=True)
    (skill / "SKILL.md").write_text("---\nname: n3-guide\ndescription: Controlled N3 Skill fixture\n---\nUse the bounded notes resource.\n")
    (skill / "notes.txt").write_text("N3_SKILL_RESOURCE_CONTENT\n")

    def selection(dialog):
        entry = next(entry for entry in data(dialog, "catalog")["entries"] if entry["name"].endswith("n3-guide"))
        return {"name": entry["name"], "source_sha256": entry["source_sha256"]}

    def output_reference(dialog):
        return next(output["output_ref"] for output in data(dialog, "exec")["output_refs"] if output["scope"]["stream"] == "stdout")

    dialogue = Dialogue("skill-output", [
        ("catalog", "skill_catalog", {}),
        ("activate", "skill_activate", selection),
        ("resource", "skill_read_resource", lambda dialog: {**selection(dialog), "resource": "notes.txt", "expected_resource_sha256": sha256(skill / "notes.txt")}),
        ("deactivate", "skill_deactivate", selection),
        ("exec", "exec", {"argv": ["/usr/bin/awk", 'BEGIN {for(i=0;i<12000;i++) printf "row-%05d payload payload payload\\n",i; print "N3_OUTPUT_NEEDLE_TAIL"}']}),
        ("search", "output_search", lambda dialog: {"output_ref": output_reference(dialog), "pattern": "N3_OUTPUT_NEEDLE_TAIL", "max_bytes": 1024, "context_lines": 1}),
        ("read", "output_read", lambda dialog: {"output_ref": output_reference(dialog), "cursor": data(dialog, "search")["matches"][0]["cursor"], "max_bytes": 1024}),
    ])
    candidate.run("Skill catalog/activate/resource/deactivate and retained long-output retrieval", workspace, dialogue,
                  "Use n3-guide, inspect its notes, deactivate it, run the controlled output fixture and retrieve its retained tail.", permissions="bypass")
    require("N3_SKILL_RESOURCE_CONTENT" in json.dumps(data(dialogue, "resource")), "active Skill resource was not read")
    metadata = next(output for output in data(dialogue, "exec")["output_refs"] if output["scope"]["stream"] == "stdout")
    require(metadata["capture_limit_bytes"] == 65536 and metadata["artifact_complete"] is False and metadata["gaps"], "long output did not report the effective capture limit and missing interval")
    require("N3_OUTPUT_NEEDLE_TAIL" in json.dumps(data(dialogue, "read")), "retained tail did not survive bounded output retrieval")


def plugin_skill_journey(candidate):
    workspace = candidate.root / "plugin-workspace"
    workspace.mkdir()
    package = candidate.root / "plugin-package"
    skill = package / "skills" / "guide"
    skill.mkdir(parents=True)
    plugin_id = "io.saya.n3-cli"
    qualified = plugin_id + "/guide"
    body_marker = "N3_PLUGIN_ACTIVE_BODY_ONLY"
    (package / "saya-plugin.toml").write_text('''schema_version=1
[plugin]
id="io.saya.n3-cli"
version="1.0.0"
description="Normal CLI Skill extension acceptance"
license="MIT"
[compatibility]
requires_saya=">=0.2.0, <1.0.0"
requires_features=["plugin-manifest-v1","skill-directory-v1"]
[capabilities]
requested=["workspace.read"]
[[skills]]
id="guide"
path="skills/guide"
''')
    (skill / "SKILL.md").write_text("---\nname: cli-guide\ndescription: Controlled CLI extension Skill\n---\n" + body_marker + "\nUse notes.txt as source material.\n")
    (skill / "notes.txt").write_text("N3_PLUGIN_RESOURCE_CONTENT\n")
    candidate.write_config(plugins=True)
    try:
        installed = candidate.invoke(workspace, ["plugin", "install", "--path", str(package)], expected_status=None)
        digest = installed["data"]["package"]["digest"]
        require(re.fullmatch(r"[a-f0-9]{64}", digest) and installed["data"]["package"]["enabled_scope_count"] == 0,
                "CLI plugin installation did not seal an initially disabled full digest")
        candidate.invoke(workspace, ["plugin", "enable", plugin_id, "--digest", digest, "--scope", "workspace"], expected_status=None)

        def selection(dialogue):
            matches = [entry for entry in data(dialogue, "catalog")["entries"] if entry["name"] == qualified]
            require(len(matches) == 1, "enabled CLI package did not assemble a unique qualified Skill")
            return {"name": qualified, "source_sha256": matches[0]["source_sha256"]}

        dialogue = Dialogue("cli-plugin-skill", [
            ("catalog", "skill_catalog", {}),
            ("activate", "skill_activate", selection),
            ("resource", "skill_read_resource", lambda dialog: {**selection(dialog), "resource": "notes.txt", "expected_resource_sha256": sha256(skill / "notes.txt")}),
            ("deactivate", "skill_deactivate", selection),
            ("catalog-after", "skill_catalog", {}),
        ])
        start = len(candidate.provider.requests)
        candidate.run("normal CLI install/enable assembles a leased plugin Skill", workspace, dialogue,
                      "Discover the installed qualified Skill, activate it, read its notes and deactivate it.", permissions="bypass")
        requests = candidate.provider.requests[start:]
        require(len(requests) == 6, "plugin Skill journey made an unexpected model request")
        injected = [body_marker in json.dumps(request) for request in requests]
        require(injected == [False, False, True, True, False, False],
                "plugin Skill body injection differs from its exact active lifetime")
        require("N3_PLUGIN_RESOURCE_CONTENT" in json.dumps(data(dialogue, "resource")), "CLI App plugin resource dispatch missed the sealed resource")
        candidate.invoke(workspace, ["plugin", "disable", plugin_id, "--scope", "workspace"], expected_status=None)
        disabled = Dialogue("cli-plugin-disabled", [("catalog", "skill_catalog", {})])
        start = len(candidate.provider.requests)
        candidate.run("disabled CLI plugin disappears from a new Run", workspace, disabled,
                      "List currently available Skills.", permissions="bypass")
        require(all(entry["name"] != qualified for entry in data(disabled, "catalog")["entries"]), "disabled plugin remained discoverable in a new Run")
        require(all(body_marker not in json.dumps(request) for request in candidate.provider.requests[start:]), "disabled plugin body remained injected")
        inspected = candidate.invoke(workspace, ["plugin", "inspect", plugin_id, "--digest", digest], expected_status=None)["data"]["package"]
        health = inspected["lease_health"]
        require(inspected["enabled_scope_count"] == 0 and health["running"] == health["stop_requested"] == health["unknown"] == 0,
                "CLI plugin lifecycle left an active or uncertain lease")
    finally:
        candidate.write_config()


def write_report(path, report):
    encoded = json.dumps(report, ensure_ascii=False, indent=2).encode() + b"\n"
    require(len(encoded) <= MAX_REPORT, "acceptance report exceeds its byte bound")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name("." + path.name + "." + uuid.uuid4().hex + ".tmp")
    with temporary.open("xb") as destination:
        destination.write(encoded)
        destination.flush()
        os.fsync(destination.fileno())
    temporary.replace(path)


def finish_identity(report, binary, source, expected_sha, before_source):
    checks = (("candidate_identity", report["candidate"], "sha256_after", lambda: sha256(binary)),
              ("source_identity", report["source"], "after", lambda: source_digest(source)))
    for stage, destination, field, operation in checks:
        try:
            destination[field] = operation()
        except Exception as cause:
            destination[field] = None
            report.setdefault("evidence_errors", []).append(
                {"stage": stage, "type": type(cause).__name__, "message": str(cause)[:8192]})
    if report["candidate"]["sha256_after"] != expected_sha or report["source"]["after"] != before_source:
        report["status"] = "failed"
        report["identity_failure"] = "candidate or source changed or became unreadable during acceptance"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--expected-sha256", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path)
    options = parser.parse_args()
    require(re.fullmatch(r"[a-f0-9]{64}", options.expected_sha256), "expected candidate SHA must be a complete lowercase SHA-256")
    require(re.fullmatch(r"[a-f0-9]{40,64}", options.source_commit), "source commit must be a complete object ID")
    binary = options.binary.resolve(strict=True)
    source = options.source_root.resolve(strict=True)
    report_path = options.report.resolve()
    actual_sha=sha256(binary)
    if actual_sha!=options.expected_sha256:
        write_report(report_path,{"schema_version":1,"kind":"saya_n3_normal_entry_acceptance","status":"failed","actual_journeys":0,
                    "candidate":{"path":str(binary),"expected_sha256":options.expected_sha256,"sha256_before":actual_sha},
                    "failure":{"type":"artifact_identity","message":"candidate SHA does not match the explicitly selected artifact"}})
        return 1
    before_source = source_digest(source)
    root = options.work_dir.resolve() if options.work_dir else Path(tempfile.mkdtemp(prefix="saya-n3-acceptance-"))
    if options.work_dir:
        root.mkdir(parents=True, exist_ok=False)
    report = {"schema_version": 1, "kind": "saya_n3_normal_entry_acceptance", "status": "failed",
              "platform": {"system": platform.system(), "release": platform.release(), "machine": platform.machine()},
              "candidate": {"path": str(binary), "sha256_before": options.expected_sha256},
              "source": {"declared_commit": options.source_commit, "before": before_source,
                         "build_binding": "runner-declared commit; current source digest independently recorded"},
              "work_dir": str(root), "protocols": [], "actual_journeys": 0}
    started = time.monotonic()
    current = None
    try:
        for index, protocol in enumerate(PROTOCOLS):
            directory = root / str(index)
            directory.mkdir()
            provider = Provider(protocol)
            with provider.serving():
                current = Candidate(binary, directory, provider)
                workspace = memory_journeys(current, "p" + str(index) + uuid.uuid4().hex[:8])
                permission_journeys(current, workspace, "p" + str(index))
                if index == 0:
                    checkpoint_journey(current)
                    checkpoint_journey(current, git=True)
                    skill_output_journey(current)
                    plugin_skill_journey(current)
                audit = current.durable_audit()
                require(audit["approval_requests"] > 0, "Manual Approval request was not durably recorded")
                report["protocols"].append({"protocol": protocol, "status": "passed", "journeys": current.journeys,
                                            "request_counts": provider.counts, "sqlite": audit,
                                            "processes": current.processes})
                report["actual_journeys"] += len(current.journeys)
                current = None
        report["status"] = "passed"
    except Exception as cause:
        report["failure"] = {"type": type(cause).__name__, "message": str(cause)[:8192]}
        if current:
            report["failed_protocol"] = {"protocol": current.provider.protocol, "journeys": current.journeys,
                                          "processes": current.processes, "request_counts": current.provider.counts,
                                          "fixture_errors": current.provider.errors[:8]}
            requests = json.dumps(current.provider.requests, ensure_ascii=False)
            (current.root / "provider-requests.json").write_text(requests[:16 * 1024 * 1024])
    finally:
        all_journeys=[journey for protocol in report["protocols"] for journey in protocol["journeys"]]
        if current: all_journeys+=current.journeys
        report["actual_journeys"]=len(all_journeys)
        report["passed_journeys"]=sum(journey["status"]=="passed" for journey in all_journeys)
        report["actual_tool_requests"]=sum(journey.get("tools_requested",0) for journey in all_journeys)
        report["observed_tool_results"]=sum(journey.get("tools_observed",0) for journey in all_journeys)
        finish_identity(report, binary, source, options.expected_sha256, before_source)
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
        write_report(report_path, report)
    print(json.dumps({"status": report["status"], "report": str(report_path), "actual_journeys": report["actual_journeys"]}))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
