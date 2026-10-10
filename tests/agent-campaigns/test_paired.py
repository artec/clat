import copy
import unittest
import paired


class PairedContracts(unittest.TestCase):
    def setUp(self):
        self.registration = paired.register(["one", "two"], 2, 104,
                                            {"model": "fixed", "budget_ms": 600000},
                                            "Classify validation failures.")
        self.records = [dict(run, started=True, success=run["arm"] == "B",
                             provider_total_tokens=3, billable_cost=None)
                        for run in self.registration["runs"]]

    def test_repetitions_do_not_become_independent_tasks(self):
        result = paired.score(self.registration, self.records, 1000)
        self.assertEqual(len(result["tasks"]), 2)
        self.assertEqual(result["delta"], 1)
        self.assertEqual(result["descriptive_interval_95"], [1, 1])
        self.assertIsNone(result["billable_cost"])
        self.assertEqual(result["capability_improvement"], "unverified")

    def test_started_failure_stays_in_denominator(self):
        result = paired.score(self.registration, self.records, 1000)
        self.assertEqual(result["started"], 8)
        self.assertTrue(all(row["A"]["started"] == 2 for row in result["tasks"]))
        self.assertTrue(all(row["A"]["successes"] == 0 for row in result["tasks"]))

    def test_partial_cannot_make_paired_improvement_claim(self):
        result = paired.score(self.registration, self.records[:-1], 1000)
        self.assertFalse(result["complete"])
        self.assertEqual(len(result["unstarted"]), 1)
        self.assertNotIn("delta", result)

    def test_duplicate_and_wrong_arm_are_rejected(self):
        with self.assertRaises(ValueError):
            paired.score(self.registration, self.records + self.records[:1], 1000)
        changed = copy.deepcopy(self.records)
        changed[0]["arm"] = "B" if changed[0]["arm"] == "A" else "A"
        with self.assertRaises(ValueError):
            paired.score(self.registration, changed, 1000)

    def test_missing_provider_usage_is_unknown(self):
        self.records[0]["provider_total_tokens"] = None
        self.assertIsNone(paired.score(self.registration, self.records, 1000)
                          ["provider_total_tokens"])

    def test_unstarted_campaign_has_no_observed_usage_or_cost(self):
        result = paired.score(self.registration, [], 1000)
        self.assertIsNone(result["provider_total_tokens"])
        self.assertIsNone(result["billable_cost"])

    def test_nonfinite_observed_cost_is_rejected(self):
        for value in (float("nan"), float("inf"), float("-inf")):
            with self.subTest(cost=value):
                self.records[0]["billable_cost"] = value
                with self.assertRaises(ValueError):
                    paired.score(self.registration, self.records, 1000)

    def test_finite_costs_cannot_overflow_the_total(self):
        for record in self.records:
            record["billable_cost"] = 1e308
        with self.assertRaises(ValueError):
            paired.score(self.registration, self.records, 1000)

    def test_modified_roster_is_rejected_even_if_rehashed(self):
        changed = copy.deepcopy(self.registration)
        changed["runs"].pop()
        changed.pop("registration_sha256")
        changed["registration_sha256"] = paired.digest(changed)
        with self.assertRaises(ValueError):
            paired.validate_registration(changed)

    def test_only_one_prompt_suffix_may_differ(self):
        left = {"prompt": "fix the public contract", "source": "abc", "budget": 10}
        right = dict(left, prompt=left["prompt"] + "\n\n" + "check")
        paired.validate_pair_inputs(left, right, "check")
        for field, value in (("source", "changed"), ("budget", 11),
                             ("prompt", "fix the public contract\ncheck")):
            changed = dict(right)
            changed[field] = value
            with self.assertRaises(ValueError):
                paired.validate_pair_inputs(left, changed, "check")

    def test_positive_and_negative_tasks_are_preserved(self):
        for record in self.records:
            record["success"] = record["arm"] == ("A" if record["task_id"] == "two" else "B")
        result = paired.score(self.registration, self.records, 1000)
        self.assertEqual(result["delta"], 0)
        self.assertEqual({row["delta"] for row in result["tasks"]}, {-1, 1})
        self.assertEqual(result["descriptive_interval_95"], [-1, 1])


if __name__ == "__main__":
    unittest.main()
