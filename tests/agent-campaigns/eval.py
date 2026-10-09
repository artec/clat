#!/usr/bin/env python3
"""Offline, preregistered historical-function exams. Never calls a model."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent


class SafetyStop(RuntimeError):
    pass


def require_safe(violations):
    if violations:
        raise SafetyStop(", ".join(violations))


def verdict(terminal, oracle_pass, safety, wall_ms, budget_ms):
    return terminal == "completed" and oracle_pass is True and not safety and wall_ms <= budget_ms


def record(task_id, terminal, oracle_pass, safety, wall_ms, budget_ms):
    return {"task_id": task_id, "terminal_outcome": terminal,
            "oracle_pass": oracle_pass, "safety_violations": safety,
            "wall_ms": wall_ms, "budget_ms": budget_ms,
            "success": verdict(terminal, oracle_pass, safety, wall_ms, budget_ms),
            "total_tokens": None, "billable_cost": None,
            "configuration_digest": "offline-scripted-control"}


def summarize(records, expected_ids):
    ids = [row["task_id"] for row in records]
    if sorted(ids) != sorted(expected_ids) or len(set(ids)) != len(ids):
        raise ValueError("missing, duplicated or unregistered run; refusing a selective score")
    if len({row["configuration_digest"] for row in records}) > 1:
        raise ValueError("configuration drift; comparison is not admissible")
    for row in records:
        require_safe(row["safety_violations"])
    successes = sum(verdict(row["terminal_outcome"], row["oracle_pass"],
                            row["safety_violations"], row["wall_ms"], row["budget_ms"])
                    for row in records)
    return {"evidence": "offline-mechanism-only", "started": len(records),
            "successes": successes, "failures": len(records) - successes,
            "rate": successes / len(records) if records else None,
            "capability_improvement": "unverified", "billable_cost": None}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, ensure_ascii=False)
        stream.write("\n")


def git_bytes(repo, revision, path):
    return subprocess.check_output(["git", "show", f"{revision}:{path}"], cwd=repo, timeout=15)


def export_task(repo, task, revision, workspace):
    """No Git history, fixes, manifests, or hidden tests enter the exam."""
    workspace.mkdir()
    for name in task["sources"]:
        path = workspace / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(git_bytes(repo, revision, name))
    (workspace / "TASK.md").write_text(task["prompt"] + "\n", encoding="utf-8")


def validate_tasks(tasks):
    from probes import ORACLES
    ids = set()
    for task in tasks:
        if not re.fullmatch(r"AGEVAL-[0-9]{2}", task["id"]) or task["id"] in ids:
            raise ValueError("invalid/duplicate task id")
        ids.add(task["id"])
        if task["kind"] not in ORACLES or task["target"] not in task["sources"] or not task["prompt"].strip():
            raise ValueError("invalid task definition")
        for name in task["sources"]:
            path = PurePosixPath(name)
            if path.is_absolute() or ".." in path.parts or "\\" in name:
                raise ValueError("source path escapes exam workspace")
        for key in ["base_revision", "reference_fix_revision"]:
            if not re.fullmatch(r"[0-9a-f]{40}", task[key]):
                raise ValueError("task revisions must be immutable full commit hashes")


def bounded_command(args, cwd, evidence, timeout, environment):
    """Logs stream to files, not an unbounded communicate buffer; own one group."""
    start = time.monotonic()
    with evidence.with_suffix(".stdout").open("xb") as stdout, evidence.with_suffix(".stderr").open("xb") as stderr:
        child = subprocess.Popen(args, cwd=cwd, env=environment, stdout=stdout,
                                 stderr=stderr, start_new_session=True)
        try:
            code = child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, 9)
            child.wait(timeout=5)
            code = None
        finally:
            try:
                os.killpg(child.pid, 0)
            except ProcessLookupError:
                pass
            else:
                os.killpg(child.pid, 9)
                raise SafetyStop("residual verifier process group")
    for suffix in [".stdout", ".stderr"]:
        if evidence.with_suffix(suffix).stat().st_size > 65536:
            raise SafetyStop("verifier output bound exceeded")
    return {"exit_code": code, "wall_ms": round((time.monotonic() - start) * 1000),
            "stdout_sha256": digest(evidence.with_suffix(".stdout")),
            "stderr_sha256": digest(evidence.with_suffix(".stderr"))}


def dependencies(directory):
    """Exact prebuilt dependency bytes are frozen in preregistration."""
    result = {}
    for name in ["serde_json", "cap_std", "libc"]:
        choices = list(directory.glob(f"lib{name}-*.rlib"))
        if not choices:
            raise ValueError(f"build the ordinary Rust tests first: missing {name}")
        result[name] = max(choices, key=lambda path: path.stat().st_mtime_ns).resolve()
    return result


def prepare_campaign(args):
    repo = args.repo.resolve()
    out = args.out.resolve()
    if repo == out or repo in out.parents or out in [Path.home(), Path.home() / ".clat"] or Path.home() / ".clat" in out.parents:
        raise ValueError("output must be a fresh isolated directory outside the repo and ~/.clat")
    out.mkdir(mode=0o700)
    tasks = json.loads((HERE / "tasks.json").read_text(encoding="utf-8"))
    validate_tasks(tasks)
    deps = dependencies(args.dependencies.resolve())
    rustc = subprocess.check_output(["rustup", "which", "rustc"], cwd=repo, timeout=10).decode().strip()
    prereg = {"schema": 1, "campaign": "ag-eval-1-offline-v1", "paid_calls": 0,
              "scope": "historical-production-function-microtasks", "task_ids": [t["id"] for t in tasks],
              "rules": {"compile_failure": "infra_invalid_not_bug_red", "retry": "none",
                        "qualify": "base-assertion-red/reference-green/decoy-assertion-red",
                        "seconds_per_command": 30, "safety": "stop_batch",
                        "minimum_qualified": 10, "reference_red": "retain_in_investigation_pool"},
              "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo).decode().strip(),
              "rustc": subprocess.check_output([rustc, "--version"], timeout=10).decode().strip(),
              "inputs": {p.name: digest(p) for p in [HERE / "eval.py", HERE / "probes.py", HERE / "tasks.json"]},
              "dependencies": {name: digest(path) for name, path in deps.items()}}
    write_json(out / "preregistration.json", prereg)
    prereg_digest = digest(out / "preregistration.json")
    frozen_inputs = out / "frozen-inputs"
    frozen_inputs.mkdir(mode=0o700)
    for name in prereg["inputs"]:
        (frozen_inputs / name).write_bytes((HERE / name).read_bytes())
    environment = {"PATH": os.defpath, "HOME": str(out / "home"), "TMPDIR": str(out / "tmp")}
    for directory in [out / "home", out / "tmp"]:
        directory.mkdir(mode=0o700)
    return repo, out, tasks, deps, rustc, environment, prereg_digest


def run_arm(args, context, task, arm):
    from probes import source_for
    repo, out, _, deps, rustc, environment, _ = context
    root = out / task["id"] / arm
    root.mkdir(parents=True)
    exam, judge = root / "exam", root / "judge"
    revision = task["base_revision"] if arm == "base" else task["reference_fix_revision"]
    export_task(repo, task, revision, exam)
    judge.mkdir(mode=0o700)
    source = source_for(exam, task, decoy=arm == "decoy")
    (judge / "probe.rs").write_text(source, encoding="utf-8")
    cmd = [rustc, "--edition=2024", "-C", "opt-level=1", "-C", "overflow-checks=yes",
           "--test", str(judge / "probe.rs"), "-o", str(judge / "probe"),
           "-L", f"dependency={args.dependencies.resolve()}"]
    for name, path in deps.items():
        cmd.extend(["--extern", f"{name}={path}"])
    build = bounded_command(cmd, judge, judge / "compile", 30, environment)
    attempt = {"revision": revision, "compile": build,
               "probe_sha256": digest(judge / "probe.rs"),
               "exam_files": {str(p.relative_to(exam)): digest(p) for p in exam.rglob("*") if p.is_file()}}
    write_json(root / "build.json", attempt)
    if build["exit_code"] != 0:
        raise ValueError(f"{task['id']} {arm}: compilation failed, not a qualifying red")
    run = bounded_command([str(judge / "probe"), "--nocapture"], exam, judge / "run", 30, environment)
    attempt["oracle"] = run
    failure_log = (judge / "run.stderr").read_text(encoding="utf-8")
    passed_log = (judge / "run.stdout").read_text(encoding="utf-8")
    attempt["behavior_assertion_failed"] = run["exit_code"] not in [0, None] and "ORACLE_" in failure_log
    attempt["oracle_pass"] = oracle_completed(run["exit_code"], passed_log)
    write_json(root / "result.json", attempt)
    return attempt


def oracle_completed(code, stdout):
    return code == 0 and "test external_oracle ... ok" in stdout and "1 passed; 0 failed" in stdout


def qualify(args):
    context = prepare_campaign(args)
    _, out, tasks, _, _, _, prereg_digest = context
    results = []
    for task in tasks:
        row = {"task_id": task["id"], "family": task["family"], "attempts": {}, "qualified": False}
        results.append(row)
        try:
            for arm in ["base", "reference", "decoy"]:
                row["attempts"][arm] = run_arm(args, context, task, arm)
        except (SafetyStop, ValueError, OSError, subprocess.SubprocessError):
            write_json(out / "qualification.partial.json", results)
            raise
        attempts = row["attempts"]
        row["qualified"] = (attempts["base"]["behavior_assertion_failed"]
                            and attempts["reference"]["oracle_pass"]
                            and attempts["decoy"]["behavior_assertion_failed"])
        row["admission"] = "scored_pool" if row["qualified"] else "investigation_pool"
    if digest(out / "preregistration.json") != prereg_digest:
        raise SafetyStop("preregistration mutated during execution")
    prereg = json.loads((out / "preregistration.json").read_text())
    for name, expected in prereg["inputs"].items():
        if digest(HERE / name) != expected:
            raise SafetyStop(f"input changed during campaign: {name}")
    report = {"preregistration_sha256": prereg_digest, "results": results,
              "qualified": sum(row["qualified"] for row in results), "total": len(tasks),
              "paid_calls": 0, "capability_improvement": "unverified"}
    write_json(out / "qualification.json", report)
    print(json.dumps({key: report[key] for key in ["qualified", "total", "paid_calls", "capability_improvement"]}))
    return 0 if report["qualified"] >= 10 else 1


def control_oracle(control, expected_files):
    return (control["files"] == expected_files and control["files_match"] is True
            and control["application_close"] == "clean"
            and "tool/result" in control["durable_event_kinds"]
            and control["observed"]["status"] == "success")


def checked_attempt(root, task, arm, recorded):
    attempt = json.loads((root / "result.json").read_text(encoding="utf-8"))
    expected_revision = task["base_revision"] if arm == "base" else task["reference_fix_revision"]
    if attempt != recorded or attempt["revision"] != expected_revision:
        raise ValueError("attempt metadata differs from frozen campaign report")
    expected_files = set(task["sources"]) | {"TASK.md"}
    if set(attempt["exam_files"]) != expected_files:
        raise ValueError("exam source roster changed")
    for name, expected in attempt["exam_files"].items():
        if digest(root / "exam" / name) != expected:
            raise ValueError("exam source changed after execution")
    for label, stem in [("compile", "compile"), ("oracle", "run")]:
        for suffix in ["stdout", "stderr"]:
            if digest(root / "judge" / f"{stem}.{suffix}") != attempt[label][f"{suffix}_sha256"]:
                raise ValueError("raw evidence digest mismatch")
    if attempt["compile"]["exit_code"] != 0:
        raise ValueError("compile failure cannot count as a behavioral red")
    if digest(root / "judge/probe.rs") != attempt["probe_sha256"]:
        raise ValueError("compiled probe changed after execution")
    stdout = (root / "judge/run.stdout").read_text(encoding="utf-8")
    stderr = (root / "judge/run.stderr").read_text(encoding="utf-8")
    code = attempt["oracle"]["exit_code"]
    return oracle_completed(code, stdout), code not in [0, None] and "ORACLE_" in stderr


def checked_manifest(root):
    frozen = json.loads((root / "preregistration.json").read_text(encoding="utf-8"))
    report = json.loads((root / "qualification.json").read_text(encoding="utf-8"))
    if digest(root / "preregistration.json") != report["preregistration_sha256"]:
        raise ValueError("preregistration differs from recorded digest")
    for name, expected in frozen["inputs"].items():
        if digest(root / "frozen-inputs" / name) != expected:
            raise ValueError("frozen evaluator source changed")
    tasks = json.loads((root / "frozen-inputs/tasks.json").read_text(encoding="utf-8"))
    validate_tasks(tasks)
    ids = [task["id"] for task in tasks]
    if ids != frozen["task_ids"] or [row["task_id"] for row in report["results"]] != ids:
        raise ValueError("campaign roster differs from complete frozen tasks")
    return frozen, tasks, report


def recompute_qualification(root):
    frozen, tasks, report = checked_manifest(root)
    qualified = 0
    rows = []
    for task, row in zip(tasks, report["results"]):
        task_id = task["id"]
        observations = {arm: checked_attempt(root / task_id / arm, task, arm, row["attempts"][arm])
                        for arm in ["base", "reference", "decoy"]}
        base, reference, decoy = [observations[arm] for arm in ["base", "reference", "decoy"]]
        admitted = base[1] and reference[0] and decoy[1]
        qualified += admitted
        rows.append({"task_id": task_id, "qualified": admitted})
    print(json.dumps({"qualified": qualified, "total": len(rows), "rows": rows}))
    return 0 if qualified >= frozen["rules"]["minimum_qualified"] else 1


def recompute_control(root):
    frozen = json.loads((root / "preregistration.json").read_text(encoding="utf-8"))
    summary = json.loads((root / "summary.json").read_text(encoding="utf-8"))
    if digest(root / "preregistration.json") != summary["preregistration_sha256"]:
        raise ValueError("control preregistration changed")
    if digest(root / "frozen-inputs/eval.py") != frozen["eval_sha256"]:
        raise ValueError("frozen control evaluator changed")
    records = json.loads((root / "records.json").read_text(encoding="utf-8"))
    summarize(records, ["application-control"])
    row = records[0]
    for suffix in ["stdout", "stderr"]:
        if digest(root / f"application-run.{suffix}") != row["execution"][f"{suffix}_sha256"]:
            raise ValueError("control raw evidence changed")
    control = json.loads((root / "application-control.json").read_text(encoding="utf-8"))
    row["oracle_pass"] = row["execution"]["exit_code"] == 0 and control_oracle(control, frozen["expected_files"])
    row["budget_ms"] = frozen["budget_ms"]
    row["wall_ms"] = row["execution"]["wall_ms"]
    result = summarize(records, ["application-control"])
    print(json.dumps(result))
    return 0 if result["successes"] == 1 else 1


def recompute(args):
    root = args.campaign.resolve()
    return recompute_qualification(root) if (root / "qualification.json").is_file() else recompute_control(root)


def dry_run(args):
    out = args.out.resolve()
    repo = args.repo.resolve()
    if repo == out or repo in out.parents or out in [Path.home(), Path.home() / ".clat"] or Path.home() / ".clat" in out.parents:
        raise ValueError("dry-run output must be outside the repository and real state root")
    out.mkdir(mode=0o700)
    fixture = repo / "tests/fixtures/agent-scenarios/write-file-permission-control"
    expected_files = [{"path": str(p.relative_to(fixture / "expected")),
                       "bytes": p.stat().st_size, "sha256": digest(p)}
                      for p in sorted((fixture / "expected").rglob("*")) if p.is_file()]
    frozen = {"evidence": "offline-scripted-control", "paid_calls": 0,
              "harness_sha256": digest(args.harness.resolve()), "eval_sha256": digest(HERE / "eval.py"),
              "fixture_sha256": {str(p.relative_to(fixture)): digest(p) for p in sorted(fixture.rglob("*")) if p.is_file()},
              "budget_ms": 30000, "retry": "none", "expected_files": expected_files}
    write_json(out / "preregistration.json", frozen)
    frozen_digest = digest(out / "preregistration.json")
    (out / "frozen-inputs").mkdir(mode=0o700)
    (out / "frozen-inputs/eval.py").write_bytes((HERE / "eval.py").read_bytes())
    (out / "home").mkdir(mode=0o700)
    (out / "tmp").mkdir(mode=0o700)
    environment = {"PATH": os.defpath, "HOME": str(out / "home"), "TMPDIR": str(out / "tmp"),
                   "CLAT_AGENT_EVAL_EXPORT_DIR": str(out)}
    execution = bounded_command([str(args.harness.resolve()), "--exact",
                                "agent_eval::campaign::offline_campaign_exports_real_application_control", "--nocapture"],
                               out, out / "application-run", 30, environment)
    control_path = out / "application-control.json"
    control = json.loads(control_path.read_text(encoding="utf-8")) if control_path.is_file() else None
    passed = (execution["exit_code"] == 0 and control is not None
              and control_oracle(control, expected_files))
    row = record("application-control", "completed" if execution["exit_code"] == 0 else "failed",
                 passed, [], execution["wall_ms"], 30000)
    row["execution"] = execution
    if digest(out / "preregistration.json") != frozen_digest:
        raise SafetyStop("dry-run preregistration mutated")
    write_json(out / "records.json", [row])
    summary = summarize([row], ["application-control"])
    summary["preregistration_sha256"] = frozen_digest
    write_json(out / "summary.json", summary)
    print(json.dumps(summary))
    return 0 if row["success"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    qualification = commands.add_parser("qualify")
    qualification.add_argument("--repo", type=Path, required=True)
    qualification.add_argument("--dependencies", type=Path, required=True)
    qualification.add_argument("--out", type=Path, required=True)
    dry = commands.add_parser("dry-run")
    dry.add_argument("--repo", type=Path, required=True)
    dry.add_argument("--harness", type=Path, required=True)
    dry.add_argument("--out", type=Path, required=True)
    recomputation = commands.add_parser("recompute")
    recomputation.add_argument("--campaign", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("this first microtask verifier is Unix-only; do not silently weaken cleanup")
    try:
        return {"qualify": qualify, "dry-run": dry_run, "recompute": recompute}[args.command](args)
    except (SafetyStop, ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        print(f"STOP: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
