"""Offline regression tests. No Saya, Cargo, Git, native backend or API is used."""
import json
import hashlib
from contextlib import redirect_stdout
import io
from pathlib import Path
import shutil
import sqlite3
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

from fixture import Dialogue, PROTOCOLS, Provider, bounded_process, reply, results, summary
import acceptance


class FixtureTests(unittest.TestCase):
    def identity_removal_report(self, target):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "candidate"
            binary.write_bytes(b"controlled test image")
            source = root / "source"
            source.mkdir()
            (source / "Cargo.toml").write_text("controlled source snapshot")
            report = root / "report.json"
            initial_digest = acceptance.source_digest
            removed = False

            def snapshot(path):
                nonlocal removed
                digest = initial_digest(path)
                if not removed:
                    removed = True
                    if target == "candidate":
                        binary.unlink()
                    else:
                        shutil.rmtree(source)
                return digest

            arguments = ["acceptance.py", str(binary), "--expected-sha256", hashlib.sha256(binary.read_bytes()).hexdigest(),
                         "--source-commit", "a" * 40, "--source-root", str(source), "--report", str(report),
                         "--work-dir", str(root / "work")]
            with patch.object(sys, "argv", arguments), patch.object(acceptance, "PROTOCOLS", ()), \
                 patch.object(acceptance, "source_digest", side_effect=snapshot), redirect_stdout(io.StringIO()):
                code = acceptance.main()
            self.assertEqual(code, 1)
            return json.loads(report.read_bytes())

    def test_candidate_disappearing_at_finally_still_writes_a_failed_report(self):
        report = self.identity_removal_report("candidate")
        self.assertEqual(report["status"], "failed")
        self.assertIsNone(report["candidate"]["sha256_after"])
        self.assertEqual(report["source"]["before"], report["source"]["after"])
        self.assertEqual(report["evidence_errors"][0]["stage"], "candidate_identity")
        self.assertEqual(report["evidence_errors"][0]["type"], "FileNotFoundError")
        self.assertIn("identity_failure", report)

    def test_source_disappearing_at_finally_still_writes_a_failed_report(self):
        report = self.identity_removal_report("source")
        self.assertEqual(report["status"], "failed")
        self.assertIsNone(report["source"]["after"])
        self.assertEqual(report["candidate"]["sha256_before"], report["candidate"]["sha256_after"])
        self.assertEqual(report["evidence_errors"][0]["stage"], "source_identity")
        self.assertEqual(report["evidence_errors"][0]["message"], "source snapshot root is unavailable")
        self.assertIn("identity_failure", report)

    def test_waiting_confirmation_audit_requires_real_result_run_and_approval(self):
        with tempfile.TemporaryDirectory() as directory:
            candidate = acceptance.Candidate.__new__(acceptance.Candidate)
            candidate.data_dir = Path(directory)
            connection = sqlite3.connect(candidate.data_dir / "sessions.sqlite3")
            connection.executescript("CREATE TABLE sessions(id); CREATE TABLE messages(session_id,ordinal,item); CREATE TABLE runs(id,result); "
                                     "CREATE TABLE tool_calls(call,state,result,run_id); CREATE TABLE approval_requests(run_id,request); CREATE TABLE change_log(kind);")
            call = {"call_id": "manual-call", "name": "memory_read", "arguments": "{}"}
            result = {"ok": False, "error": {"code": "needs_confirmation"}}
            connection.execute("INSERT INTO sessions VALUES('session')")
            connection.executemany("INSERT INTO messages VALUES('session',?,?)", enumerate(map(json.dumps, [
                {"kind": "tool_call", "data": call}, {"kind": "tool_result", "data": {"call_id": "manual-call", "result": result}}])))
            connection.execute("INSERT INTO runs VALUES('run',?)", (json.dumps({"status": "needs_confirmation"}),))
            connection.execute("INSERT INTO tool_calls VALUES(?,'waiting_confirmation',?,'run')", (json.dumps(call), json.dumps(result)))
            connection.execute("INSERT INTO approval_requests VALUES('run',?)", (json.dumps({"call_id": "manual-call", "run_id": "run"}),))
            connection.commit()
            self.assertEqual(candidate.durable_audit()["waiting_confirmation_outcomes"], 1)
            connection.execute("UPDATE runs SET result=?", (json.dumps({"status": "completed"}),))
            connection.commit()
            with self.assertRaisesRegex(AssertionError, "stopped Run"):
                candidate.durable_audit()
            connection.execute("UPDATE runs SET result=?", (json.dumps({"status": "needs_confirmation"}),))
            connection.execute("DELETE FROM approval_requests")
            connection.commit()
            with self.assertRaisesRegex(AssertionError, "approval request"):
                candidate.durable_audit()
            connection.close()

    def test_forgotten_identity_lookup_requires_exact_missing_memory_error(self):
        acceptance.missing_memory({"ok": False, "error": {"code": "memory_not_found"}}, None)
        for result in ({"ok": True, "data": {"items": []}}, {"ok": False, "error": {"code": "invalid_args"}}, {"ok": False, "error": {"code": "memory_conflict"}}):
            with self.assertRaisesRegex(AssertionError, "exact missing-memory"):
                acceptance.missing_memory(result, None)

    def test_retired_forget_requires_exact_projection_and_real_successful_durable_pair(self):
        with tempfile.TemporaryDirectory() as directory:
            candidate = acceptance.Candidate.__new__(acceptance.Candidate)
            candidate.data_dir = Path(directory)
            connection = sqlite3.connect(candidate.data_dir / "sessions.sqlite3")
            connection.executescript("CREATE TABLE messages(session_id,ordinal,item); CREATE TABLE runs(id,session_id); CREATE TABLE tool_calls(run_id,call_id,call,state,result);")
            arguments = {"operation_id": "forget-fixture"}
            call = {"call_id": "forget-call", "name": "memory_forget", "arguments": json.dumps(arguments)}
            result = {"ok": True, "data": {"current_revision": "e" * 64}}
            items = [{"kind": "tool_call", "data": call}, {"kind": "tool_result", "data": {"call_id": "forget-call", "result": result}}]
            raw = [json.dumps(item, separators=(",", ":")) for item in items]
            connection.executemany("INSERT INTO messages VALUES('session',?,?)", enumerate(raw))
            connection.execute("INSERT INTO runs VALUES('run','session')")
            connection.execute("INSERT INTO tool_calls VALUES('run','forget-call',?,'succeeded',?)", (json.dumps(call), json.dumps(result)))
            connection.commit()
            envelope = {"trust": "historical_source_data", "reason": "memory_context_changed", "source_range": {"start": 0, "end": 2},
                        "source_sha256": hashlib.sha256(("[" + ",".join(raw) + "]").encode()).hexdigest(),
                        "current_memory_revision": "e" * 64, "observations": []}
            request = lambda: {"messages": [{"role": "user", "content": json.dumps({"saya_retired_history_group_v1": envelope})}]}
            dialogue = Dialogue("retired", [("forget", "memory_forget", arguments)], retired_result=candidate.retired_result)
            dialogue.respond({})
            # The test uses the dialogue's real generated identity throughout the DB.
            identifier = dialogue.tool_ids[0]
            self.assertNotEqual(identifier, call["call_id"])
            self.assertEqual(candidate.retired_result(request(), "forget-call", "memory_forget", arguments), result)
            envelope["source_sha256"] = "0" * 64
            with self.assertRaisesRegex(AssertionError, "stale"):
                candidate.retired_result(request(), "forget-call", "memory_forget", arguments)
            envelope["source_sha256"] = hashlib.sha256(("[" + ",".join(raw) + "]").encode()).hexdigest()
            connection.execute("UPDATE tool_calls SET state='unknown'")
            connection.commit()
            with self.assertRaisesRegex(AssertionError, "unknown"):
                candidate.retired_result(request(), "forget-call", "memory_forget", arguments)
            connection.close()

    def test_dialogue_retirement_exception_cannot_cover_ordinary_or_nonfinal_operations(self):
        for steps in ([('read','memory_read',{})], [('forget','memory_forget',{}),('read','memory_read',{})]):
            dialogue = Dialogue("strict", steps, retired_result=lambda *_args: {"ok": True})
            dialogue.respond({})
            with self.assertRaisesRegex(AssertionError, "paired"):
                dialogue.respond({})

    def test_offline_query_error_is_never_accepted_as_a_successful_version(self):
        candidate=acceptance.Candidate.__new__(acceptance.Candidate)
        candidate.binary=Path("/candidate")
        candidate.config=Path("/config")
        candidate.data_dir=Path("/data")
        candidate.env={}
        candidate.processes=[]
        candidate.provider=SimpleNamespace(errors=[])
        with patch("acceptance.bounded_process",return_value=(2,json.dumps({"error":{"code":"invalid_config"}}),"",0.01)):
            with self.assertRaisesRegex(AssertionError,"offline CLI query failed"):
                candidate.invoke(Path("/workspace"),["memory","version"],expected_status=None)

    def test_plan_probe_uses_the_successfully_observed_current_revision(self):
        revision="e"*64
        arguments=acceptance.plan_probe(revision,"fixture")
        self.assertEqual(arguments["expected_revision"],revision)
        self.assertEqual(arguments["facts"][0]["evidence"],[{"kind":"user_message","ordinal":1}])
        self.assertEqual(arguments["facts"][0]["kind"],"explicit_preference")
        with self.assertRaises(AssertionError):
            acceptance.plan_probe("not-a-revision","fixture")

    def test_checkpoint_oracle_modifies_accepted_file_then_restores_the_new_baseline(self):
        dialogue=acceptance.checkpoint_dialogue(False)
        labels=[step[0] for step in dialogue.steps]
        self.assertLess(labels.index("accept"),labels.index("alter-accepted"))
        self.assertLess(labels.index("alter-accepted"),labels.index("restore"))
        dialogue.observed["accept"]={"data":{"checkpoint":{"token":{"id":"accepted","sha256":"a"*64}}}}
        restore=next(step for step in dialogue.steps if step[0]=="restore")[2](dialogue)
        self.assertEqual(restore["checkpoint"]["id"],"accepted")
        self.assertEqual(restore["paths"],["selected.txt","created.txt"])
        self.assertNotIn("untouched.txt",restore["paths"])

    def test_three_protocols_emit_only_the_selected_call_and_exact_arguments(self):
        call = ("controlled-call", "memory_read", {"max_items": 3, "text": "中文"})
        chat = reply(PROTOCOLS[0], call=call)
        self.assertEqual(json.loads(chat["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]), call[2])
        responses = reply(PROTOCOLS[1], call=call)
        self.assertEqual(responses["output"][0]["call_id"], call[0])
        anthropic = reply(PROTOCOLS[2], call=call)
        self.assertEqual(anthropic["content"][0]["input"], call[2])

    def test_paired_result_extraction_matches_all_three_wire_forms(self):
        result = {"ok": True, "data": {"revision": "a" * 64}, "error": None, "truncated": False}
        encoded = json.dumps(result)
        requests = [
            {"messages": [{"role": "tool", "tool_call_id": "call", "content": encoded}]},
            {"input": [{"type": "function_call_output", "call_id": "call", "output": encoded}]},
            {"messages": [{"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call", "content": [{"type": "text", "text": encoded}]}]}]},
        ]
        for request in requests:
            self.assertEqual(results(request), {"call": result})

    def test_dialogue_rejects_missing_or_failed_preceding_result(self):
        dialogue = Dialogue("fixture", [("read", "memory_read", {})])
        first = dialogue.respond({"messages": []})
        self.assertEqual(first["call"][1], "memory_read")
        with self.assertRaisesRegex(AssertionError, "paired"):
            dialogue.respond({"messages": []})
        bad = {"messages": [{"role": "tool", "tool_call_id": dialogue.tool_ids[0], "content": json.dumps({"ok": False})}]}
        with self.assertRaisesRegex(AssertionError, "failed"):
            dialogue.respond(bad)

    def test_review_requests_do_not_consume_the_execution_dialogue(self):
        provider = Provider(PROTOCOLS[0])
        provider.dialogue = Dialogue("fixture", [("read", "memory_read", {})])
        response = provider.respond({"messages": [{"role": "system", "content": "# Saya: independent operation review v1"}]})
        self.assertEqual(json.loads(response["choices"][0]["message"]["content"])["decision"], "allow")
        self.assertEqual(provider.dialogue.index, 0)
        self.assertEqual(provider.counts, {"execution": 0, "review": 1, "compaction": 0})

    def test_summary_preserves_required_identity_and_uses_actual_source_roles(self):
        data = {"assigned_source_range": {"start": 0, "end": 3},
                "source_units": [[{"source_id": "history:0", "role": "user"}], [{"source_id": "history:2", "role": "mechanical"}]],
                "required_items": [{"id": "required-user", "kind": "objective", "text": "Exact constraint", "source_ids": ["history:0"], "status": "active"},
                                   {"id": "required-observation", "kind": "recovery", "text": "Exact retired result", "source_ids": ["history:2"], "status": "uncertain"}],
                "references": [{"id": "output:controlled"}]}
        value = summary(data)
        self.assertEqual(value["required_ids"], ["required-user", "required-observation"])
        self.assertEqual(value["objective"][-1]["text"], "Exact constraint")
        self.assertEqual(value["uncertainties"][0]["evidence"], "observed")
        self.assertEqual(value["references"], ["output:controlled"])

    def test_process_capture_and_source_hash_are_bounded_and_content_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            code, stdout, stderr, _elapsed = bounded_process([sys.executable, "-c", "print('fixture')"], root, {})
            self.assertEqual((code, stdout, stderr), (0, "fixture\n", ""))
            (root / "crates").mkdir()
            path = root / "crates" / "lib.rs"
            path.write_text("first")
            first = acceptance.source_digest(root)
            path.write_text("second")
            self.assertNotEqual(first["sha256"], acceptance.source_digest(root)["sha256"])
            report = root / "report.json"
            acceptance.write_report(report, {"status": "passed"})
            acceptance.write_report(report, {"status": "failed"})
            self.assertEqual(json.loads(report.read_text())["status"], "failed")


if __name__ == "__main__":
    unittest.main()
