#!/usr/bin/env python3
"""Offline paired-campaign registration and task-clustered result analysis.

This module makes no model calls and executes no candidate code. The OS-fenced
live runner and independent judge remain separate admission requirements.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import random


def digest(value):
    data = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(data).hexdigest()


def register(tasks, repetitions, seed, configuration, intervention):
    if not tasks or len(tasks) != len(set(tasks)):
        raise ValueError("a nonempty unique task roster is required")
    if repetitions < 2 or not intervention.strip():
        raise ValueError("repeated samples and one intervention are required")
    if any(not isinstance(task, str) or not task for task in tasks):
        raise ValueError("task IDs must be nonempty strings")
    randomizer = random.Random(seed)
    blocks = [(task, repetition) for task in tasks for repetition in range(repetitions)]
    randomizer.shuffle(blocks)
    runs = []
    for task, repetition in blocks:
        arms = ["A", "B"]
        randomizer.shuffle(arms)
        for arm in arms:
            runs.append({"run_id": f"{task}-{repetition}-{arm}", "task_id": task,
                         "repetition": repetition, "arm": arm})
    registration = {"version": 1, "tasks": tasks, "repetitions": repetitions,
                    "seed": seed, "configuration": configuration,
                    "configuration_sha256": digest(configuration),
                    "intervention": intervention, "runs": runs,
                    "denominator": "every started run, including all failures",
                    "inference_unit": "task, not repeated attempt"}
    registration["registration_sha256"] = digest(registration)
    return registration


def validate_registration(registration):
    body = dict(registration)
    checksum = body.pop("registration_sha256", None)
    if checksum != digest(body):
        raise ValueError("registration drift")
    expected = register(body["tasks"], body["repetitions"], body["seed"],
                        body["configuration"], body["intervention"])
    if registration != expected:
        raise ValueError("invalid registration or allocation")


def validate_pair_inputs(left, right, intervention):
    """Input bundles include prompt, source hashes, config, tools and budgets."""
    expected = dict(left)
    expected["prompt"] = left["prompt"] + "\n\n" + intervention
    if right != expected:
        raise ValueError("pair differs beyond the registered prompt suffix")


def score(registration, records, bootstrap_samples=10000):
    validate_registration(registration)
    if bootstrap_samples < 1000:
        raise ValueError("at least 1000 resamples required")
    allowed = {run["run_id"]: run for run in registration["runs"]}
    seen = set()
    grouped = {task: {"A": [], "B": []} for task in registration["tasks"]}
    cost_known = bool(records)
    tokens_known = bool(records)
    tokens = 0
    cost = 0
    for record in records:
        run_id = record["run_id"]
        if run_id in seen or run_id not in allowed:
            raise ValueError("duplicate or unregistered run")
        seen.add(run_id)
        run = allowed[run_id]
        if any(record[key] != run[key] for key in ("task_id", "arm", "repetition")):
            raise ValueError("allocation mismatch")
        if type(record["success"]) is not bool or record.get("started") is not True:
            raise ValueError("started runs require a boolean observed outcome")
        grouped[run["task_id"]][run["arm"]].append(record["success"])
        value = record.get("provider_total_tokens")
        if value is None:
            tokens_known = False
        elif type(value) is not int or value < 0:
            raise ValueError("invalid provider token count")
        else:
            tokens += value
        value = record.get("billable_cost")
        if value is None:
            cost_known = False
        elif type(value) not in (int, float) or value < 0:
            raise ValueError("invalid observed cost")
        else:
            cost += value
            if type(cost) is float and not math.isfinite(cost):
                raise ValueError("observed cost total is not finite")
    rows = []
    complete = seen == set(allowed)
    for task, arms in grouped.items():
        row = {"task_id": task}
        for arm, values in arms.items():
            row[arm] = {"started": len(values), "successes": sum(values),
                        "rate": sum(values) / len(values) if values else None}
        row["delta"] = row["B"]["rate"] - row["A"]["rate"] if complete else None
        rows.append(row)
    result = {"registered": len(allowed), "started": len(seen), "complete": complete,
              "unstarted": [run["run_id"] for run in registration["runs"]
                            if run["run_id"] not in seen], "tasks": rows,
              "provider_total_tokens": tokens if tokens_known else None,
              "billable_cost": cost if cost_known else None,
              "capability_improvement": "unverified"}
    if complete:
        deltas = [row["delta"] for row in rows]
        rng = random.Random(registration["seed"])
        samples = sorted(sum(rng.choice(deltas) for _ in deltas) / len(deltas)
                         for _ in range(bootstrap_samples))
        result.update(delta=sum(deltas) / len(deltas),
                      descriptive_interval_95=[samples[int(.025 * bootstrap_samples)],
                                               samples[int(.975 * bootstrap_samples)]],
                      interval_method="paired task-cluster percentile bootstrap",
                      interval_limit="small fixed task set; not population-level proof")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("registration", type=Path)
    parser.add_argument("records", type=Path)
    args = parser.parse_args()
    print(json.dumps(score(json.loads(args.registration.read_text()),
                           json.loads(args.records.read_text())), indent=2))


if __name__ == "__main__":
    main()
