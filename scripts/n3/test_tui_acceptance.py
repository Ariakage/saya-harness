"""Offline regressions for the PTY acceptance harness, without launching Saya."""
from contextlib import closing
import json
from pathlib import Path
import sqlite3
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import tui_acceptance


class TuiAcceptanceFixtureTests(unittest.TestCase):
    def test_source_user_count_excludes_provider_mirrors_and_other_items(self):
        rows = [(0, json.dumps({"kind": "user_text", "data": "Original task"})),
                (1, json.dumps({"kind": "assistant_text", "data": "Observed response"})),
                (2, json.dumps({"kind": "provider_item", "data": {"type": "message", "role": "assistant"}})),
                (3, json.dumps({"kind": "user_text", "data": "Current correction"}))]
        self.assertEqual(tui_acceptance.user_texts(rows), ["Original task", "Current correction"])

    def test_final_response_gate_remains_owned_until_explicit_release(self):
        answer = tui_acceptance.Answer(gated=True)
        values = []
        worker = threading.Thread(target=lambda: values.append(answer.respond({})))
        worker.start()
        try:
            self.assertTrue(answer.started.wait(2))
            self.assertFalse(answer.completed)
            self.assertEqual(values, [])
        finally:
            answer.release.set()
            worker.join(2)
        self.assertFalse(worker.is_alive())
        self.assertTrue(answer.completed)
        self.assertTrue(values[0]["text"].startswith("N3_FINAL_OBSERVATION"))

    def test_wire_capture_is_complete_json_and_byte_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "wire.json"
            requests = [{"input": "controlled source", "text": "Unicode 中文"}]
            tui_acceptance.write_wire(path, requests)
            self.assertEqual(json.loads(path.read_bytes()), requests)
            before = path.read_bytes()
            with self.assertRaisesRegex(AssertionError, "byte bound"):
                tui_acceptance.write_wire(path, [{"text": "x" * (16 * 1024 * 1024)}])
            self.assertEqual(path.read_bytes(), before)

    def test_final_runs_reject_extra_failed_unfinished_and_cross_session_runs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidate = SimpleNamespace(data_dir=root)
            with closing(sqlite3.connect(root / "sessions.sqlite3")) as connection, connection:
                connection.execute("CREATE TABLE runs(session_id TEXT,finished_at TEXT,result TEXT)")
                connection.executemany("INSERT INTO runs VALUES(?,?,?)",
                    [("session-a", "finished", '{"status":"completed"}')] * 4)
            self.assertEqual(tui_acceptance.final_runs(candidate, "session-a"), 4)
            for finished, result in [("finished", '{"status":"failed"}'), (None, None)]:
                with closing(sqlite3.connect(root / "sessions.sqlite3")) as connection, connection:
                    connection.execute("INSERT INTO runs VALUES(?,?,?)", ("session-a", finished, result))
                with self.assertRaisesRegex(AssertionError, "unexpected number"):
                    tui_acceptance.final_runs(candidate, "session-a")
                with closing(sqlite3.connect(root / "sessions.sqlite3")) as connection, connection:
                    connection.execute("DELETE FROM runs WHERE rowid=5")
            for session, finished, result in [("session-a", "finished", '{"status":"failed"}'),
                                              ("session-a", None, None),
                                              ("session-other", "finished", '{"status":"completed"}')]:
                with closing(sqlite3.connect(root / "sessions.sqlite3")) as connection, connection:
                    connection.execute("UPDATE runs SET session_id=?,finished_at=?,result=? WHERE rowid=4",
                                       (session, finished, result))
                with self.assertRaisesRegex(AssertionError, "failed, unfinished or unrelated"):
                    tui_acceptance.final_runs(candidate, "session-a")

    def test_identity_read_failures_still_produce_a_structured_failure_report(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = {"status": "passed", "candidate": {}, "source": {}}
            with patch.object(tui_acceptance, "source_digest", side_effect=OSError("unreadable source")):
                tui_acceptance.finish_identity(report, root / "deleted-program", root, "a" * 64,
                                               {"sha256": "b" * 64})
            path = root / "report.json"
            tui_acceptance.write_report(path, report)
            actual = json.loads(path.read_bytes())
            self.assertEqual(actual["status"], "failed")
            self.assertIsNone(actual["candidate"]["sha256_after"])
            self.assertIsNone(actual["source"]["after"])
            self.assertEqual([item["stage"] for item in actual["evidence_errors"]],
                             ["candidate_identity", "source_identity"])
            self.assertIn("identity_failure", actual)

    def test_failed_capture_does_not_skip_other_evidence_or_terminal_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            closed = []
            provider = SimpleNamespace(protocol="controlled", counts={}, errors=[], requests=[])
            def unavailable_screen(_name):
                raise OSError("screen unavailable")
            journey = SimpleNamespace(candidate=SimpleNamespace(root=Path(directory) / "missing", provider=provider),
                records=[], screens=[], restored=False, terminal=SimpleNamespace(raw=b"bounded raw"),
                capture=unavailable_screen,
                close=lambda: closed.append(True))
            report = {"status": "failed"}
            with patch.object(tui_acceptance, "write_wire", side_effect=OSError("wire unavailable")):
                tui_acceptance.failed_journey(report, journey)
            self.assertEqual(closed, [True])
            self.assertEqual([item["stage"] for item in report["evidence_errors"]],
                             ["failed_screen", "failed_terminal_log", "failed_wire"])
            self.assertEqual(report["failed_protocol"]["protocol"], "controlled")


if __name__ == "__main__":
    unittest.main()
