#!/usr/bin/env python3
"""Offline unit tests for scripts/prompt_eval.py; no model or network calls."""
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from contextlib import redirect_stdout

sys.path.insert(0, str(Path(__file__).resolve().parent))
import prompt_eval


class PromptEvalTest(unittest.TestCase):
    def test_fixtures_reject_buggy_sources_and_accept_repaired_sources(self):
        cases = {case["id"]: case for case in prompt_eval.SCENARIOS}
        self.assertGreaterEqual(len(cases), 8)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for scenario_id, transform in (
                (
                    "patch_multi_hunk",
                    lambda text: text.replace("(len(values) - 1)", "len(values)").replace(
                        "ordered = list(values)", "ordered = sorted(values)"
                    ),
                ),
                (
                    "code_scope_injection",
                    lambda text: text.replace("start = page * size", "start = (page - 1) * size"),
                ),
            ):
                case = cases[scenario_id]
                events = [{"type": "tool.finished", "data": {"name": "apply_patch", "result": {"ok": True}}}]
                buggy_ws = root / f"{scenario_id}-buggy"
                buggy_ws.mkdir()
                for name, content in case["files"].items():
                    (buggy_ws / name).write_text(content, encoding="utf-8")
                before = prompt_eval.snapshot(buggy_ws)
                self.assertTrue(prompt_eval.evaluate(buggy_ws, case["checks"], events, before))

                fixed_ws = root / f"{scenario_id}-fixed"
                fixed_ws.mkdir()
                for name, content in case["files"].items():
                    updated = transform(content) if name in ("stats.py", "app.py") else content
                    (fixed_ws / name).write_text(updated, encoding="utf-8")
                self.assertEqual(prompt_eval.evaluate(fixed_ws, case["checks"], events, before), [])

    def test_preserve_and_unchanged_line_checks_detect_collateral_edits(self):
        cases = {case["id"]: case for case in prompt_eval.SCENARIOS}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case = cases["code_scope_injection"]
            for name, content in case["files"].items():
                (root / name).write_text(content, encoding="utf-8")
            before = prompt_eval.snapshot(root)
            (root / "app.py").write_text(
                case["files"]["app.py"].replace("start = page * size", "start = (page - 1) * size"),
                encoding="utf-8",
            )
            (root / "legacy.py").unlink()
            errors = prompt_eval.evaluate(root, case["checks"], [], before)
            self.assertIn("legacy.py was changed", errors)

    def test_sales_and_incident_validators_check_content_and_order(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "summary.json").write_text(
                json.dumps({"total": 1655, "by_region": {"north": 260, "south": 660, "east": 270, "west": 465}}),
                encoding="utf-8",
            )
            (root / "top.md").write_text("1. desk: 780\n2. chair: 335\n3. lamp: 255\n", encoding="utf-8")
            self.assertEqual(prompt_eval.sales_checks(root), [])
            (root / "top.md").write_text("1. chair\n2. desk\n3. lamp\n", encoding="utf-8")
            self.assertTrue(prompt_eval.sales_checks(root))
            (root / "incident.txt").write_text("code=E4721 line=4377\n", encoding="utf-8")
            self.assertEqual(prompt_eval.incident_checks(root), [])

    def test_usage_totals_report_per_turn_cache_and_token_consumption(self):
        events = [
            {"type": "model.finished", "data": {"ok": True, "usage": {"input_tokens": 100, "cached_input_tokens": 40, "output_tokens": 10}}},
            {"type": "model.finished", "data": {"ok": False, "usage": {"input_tokens": 999, "cached_input_tokens": 999, "output_tokens": 999}}},
            {"type": "model.finished", "data": {"ok": True, "usage": {"input_tokens": 200, "cached_input_tokens": 150, "output_tokens": 20}}},
        ]
        usage = prompt_eval.usage_totals(events)
        self.assertEqual(usage["input_tokens"], 300)
        self.assertEqual(usage["cached_input_tokens"], 190)
        self.assertEqual(usage["uncached_input_tokens"], 110)
        self.assertEqual(usage["output_tokens"], 30)
        self.assertEqual(usage["total_tokens"], 330)
        self.assertEqual(usage["cache_hit_rate_pct"], 63.33)
        self.assertEqual(usage["turn1_cache_hit_rate_pct"], 40.0)
        self.assertEqual(usage["turn2plus_cache_hit_rate_pct"], 75.0)
        self.assertEqual([turn["uncached_input_tokens"] for turn in usage["turns"]], [60, 50])

    def test_summarise_and_compare_aggregate_multi_round_reports(self):
        records = [
            {"id": "skill_route", "iteration": 1, "passed": True, "tool_errors": 1, "model_requests": 5,
             "usage": {"input_tokens": 60000, "cached_input_tokens": 30000, "output_tokens": 400,
                       "turn1_input_tokens": 20000, "turn1_cached_input_tokens": 0,
                       "turn2plus_input_tokens": 40000, "turn2plus_cached_input_tokens": 30000}},
            {"id": "skill_route", "iteration": 2, "passed": True, "tool_errors": 0, "model_requests": 4,
             "usage": {"input_tokens": 50000, "cached_input_tokens": 25000, "output_tokens": 350,
                       "turn1_input_tokens": 20000, "turn1_cached_input_tokens": 0,
                       "turn2plus_input_tokens": 30000, "turn2plus_cached_input_tokens": 25000}},
        ]
        summary = prompt_eval.summarise(records)
        self.assertEqual(summary["skill_route"]["runs"], 2)
        self.assertEqual(summary["skill_route"]["passed"], 2)
        self.assertEqual(summary["skill_route"]["tool_errors"], 1)
        self.assertEqual(summary["skill_route"]["runs_with_tool_errors"], 1)
        overall = summary["__all__"]
        self.assertEqual(overall["mean_model_requests"], 4.5)
        self.assertTrue(overall["cache_observed"])
        legacy = prompt_eval.summarise([
            {"id": "legacy", "passed": True, "tool_errors": 0, "model_requests": 1,
             "usage": {"input_tokens": 100, "output_tokens": 10}}
        ])
        self.assertFalse(legacy["__all__"]["cache_observed"])
        self.assertEqual(overall["total_input_tokens"], 110000)
        self.assertEqual(overall["total_cached_input_tokens"], 55000)
        self.assertEqual(overall["total_uncached_input_tokens"], 55000)
        self.assertEqual(overall["total_output_tokens"], 750)
        self.assertEqual(overall["total_tokens"], 110750)
        self.assertEqual(overall["cache_hit_rate_pct"], 50.0)
        self.assertEqual(overall["turn1_input_tokens"], 40000)
        self.assertEqual(overall["turn1_cached_input_tokens"], 0)
        self.assertEqual(overall["turn1_uncached_input_tokens"], 40000)
        self.assertEqual(overall["turn2plus_input_tokens"], 70000)
        self.assertEqual(overall["turn2plus_cached_input_tokens"], 55000)
        self.assertEqual(overall["turn2plus_uncached_input_tokens"], 15000)
        self.assertEqual(overall["turn1_cache_hit_rate_pct"], 0.0)
        self.assertEqual(overall["turn2plus_cache_hit_rate_pct"], 78.57)
        self.assertEqual(overall["input_share_pct"], 99.32)
        self.assertEqual(overall["output_share_pct"], 0.68)
        self.assertEqual(overall["cached_total_share_pct"], 49.66)
        self.assertEqual(overall["uncached_total_share_pct"], 49.66)
        with tempfile.TemporaryDirectory() as directory:
            report_path = Path(directory) / "report.json"
            report_path.write_text(
                json.dumps({"label": "sample", "summary": summary, "scenarios": records}),
                encoding="utf-8",
            )
            buffer = io.StringIO()
            with redirect_stdout(buffer):
                self.assertEqual(prompt_eval.compare([report_path]), 0)
            self.assertIn(
                "| skill_route | 2/2 pass; req 4.5; err 1 (1 runs); in 55000; cache 50.0% (t2+ 78.6%) |",
                buffer.getvalue(),
            )


if __name__ == "__main__":
    unittest.main()
