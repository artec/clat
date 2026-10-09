"""Evaluator contracts: deliberately independent of an agent's success claim."""
import importlib.util
from pathlib import Path
import unittest
import sys
import json
import tempfile

sys.path.insert(0, str(Path(__file__).parent))

spec = importlib.util.spec_from_file_location("campaign_eval", Path(__file__).with_name("eval.py"))
evaluation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluation)


class VerdictTests(unittest.TestCase):
    def campaign(self, root):
        tasks = json.loads(Path(__file__).with_name("tasks.json").read_text())
        (root / "frozen-inputs").mkdir()
        evaluation.write_json(root / "frozen-inputs/tasks.json", tasks)
        prereg = {"task_ids": [t["id"] for t in tasks],
                  "inputs": {"tasks.json": evaluation.digest(root / "frozen-inputs/tasks.json")}}
        evaluation.write_json(root / "preregistration.json", prereg)
        report = {"preregistration_sha256": evaluation.digest(root / "preregistration.json"),
                  "results": [{"task_id": t["id"]} for t in tasks]}
        evaluation.write_json(root / "qualification.json", report)
        return prereg, report

    def test_recompute_rejects_changed_preregistration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prereg, _ = self.campaign(root)
            prereg["task_ids"].pop()
            (root / "preregistration.json").write_text(json.dumps(prereg))
            with self.assertRaises(ValueError):
                evaluation.checked_manifest(root)

    def test_recompute_rejects_selective_roster_even_with_updated_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prereg, report = self.campaign(root)
            prereg["task_ids"].pop()
            report["results"].pop()
            (root / "preregistration.json").write_text(json.dumps(prereg))
            report["preregistration_sha256"] = evaluation.digest(root / "preregistration.json")
            (root / "qualification.json").write_text(json.dumps(report))
            with self.assertRaises(ValueError):
                evaluation.checked_manifest(root)

    def test_recompute_rejects_missing_report_record(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, report = self.campaign(root)
            report["results"].pop()
            (root / "qualification.json").write_text(json.dumps(report))
            with self.assertRaises(ValueError):
                evaluation.checked_manifest(root)

    def test_recompute_rejects_changed_compiler_log(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "exam").mkdir()
            (root / "exam/TASK.md").write_text("task")
            (root / "exam/input.rs").write_text("production")
            (root / "judge").mkdir()
            (root / "judge/compile.stdout").write_text("tampered")
            task = {"base_revision": "a" * 40, "sources": ["input.rs"]}
            attempt = {"revision": "a" * 40,
                       "exam_files": {name: evaluation.digest(root / "exam" / name)
                                      for name in ["TASK.md", "input.rs"]},
                       "compile": {"exit_code": 0, "stdout_sha256": "original"}}
            evaluation.write_json(root / "result.json", attempt)
            with self.assertRaises(ValueError):
                evaluation.checked_attempt(root, task, "base", attempt)

    def test_manifest_cannot_export_outside_exam(self):
        import json
        tasks = json.loads(Path(__file__).with_name("tasks.json").read_text())
        tasks[0]["sources"] = ["../outside"]
        tasks[0]["target"] = "../outside"
        with self.assertRaises(ValueError):
            evaluation.validate_tasks(tasks)

    def test_manifest_requires_immutable_revision(self):
        import json
        tasks = json.loads(Path(__file__).with_name("tasks.json").read_text())
        tasks[0]["base_revision"] = "main"
        with self.assertRaises(ValueError):
            evaluation.validate_tasks(tasks)

    def test_successful_exit_without_running_oracle_is_rejected(self):
        self.assertFalse(evaluation.oracle_completed(0, "running 1 test\n"))

    def test_tampered_control_files_cannot_claim_success(self):
        control = {"files": [], "files_match": True, "application_close": "clean",
                   "durable_event_kinds": ["tool/result"], "observed": {"status": "success"}}
        self.assertFalse(evaluation.control_oracle(control, [{"path": "demo.txt", "sha256": "expected"}]))

    def test_model_claim_cannot_override_oracle(self):
        self.assertFalse(evaluation.verdict("completed", False, [], 1, 10))

    def test_correct_but_over_budget_is_failure(self):
        self.assertFalse(evaluation.verdict("completed", True, [], 11, 10))

    def test_safety_overrides_functional_success(self):
        self.assertFalse(evaluation.verdict("completed", True, ["workspace_escape"], 1, 10))

    def test_missing_usage_is_unknown(self):
        record = evaluation.record("task", "completed", True, [], 1, 10)
        self.assertIsNone(record["total_tokens"])
        self.assertIsNone(record["billable_cost"])

    def test_omission_and_duplicate_are_rejected(self):
        record = evaluation.record("task", "completed", True, [], 1, 10)
        with self.assertRaises(ValueError):
            evaluation.summarize([], ["task"])
        with self.assertRaises(ValueError):
            evaluation.summarize([record, record], ["task"])

    def test_incomparable_arms_are_rejected(self):
        left = evaluation.record("left", "completed", True, [], 1, 10)
        right = evaluation.record("right", "completed", True, [], 1, 10)
        left["configuration_digest"] = "model-A"
        right["configuration_digest"] = "model-B"
        with self.assertRaises(ValueError):
            evaluation.summarize([left, right], ["left", "right"])

    def test_failed_and_timeout_runs_stay_in_denominator(self):
        records = [evaluation.record("a", "completed", True, [], 1, 10),
                   evaluation.record("b", "timeout", True, [], 11, 10)]
        summary = evaluation.summarize(records, ["a", "b"])
        self.assertEqual(summary["started"], 2)
        self.assertEqual(summary["successes"], 1)

    def test_safety_stops_batch(self):
        with self.assertRaises(evaluation.SafetyStop):
            evaluation.require_safe(["residual_process"])


if __name__ == "__main__":
    unittest.main()
