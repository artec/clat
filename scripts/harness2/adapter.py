"""CLAT rollouts and restricted DeepSeek editing behind upstream protocols."""
import json
from pathlib import Path
from harness2.core.proposer import Proposer, ProposerError, Reply
from harness2.core.spec import BenchSpec, BenchPrompts, ChannelSpec
from protocol import digest, write_new
from surface import FILE, ClatSubstrate, read_tree, validate_tree
from cohort import BENCH_NAME


class ClatProposer(Proposer):
    def __init__(self, worker, root):
        super().__init__(ChannelSpec("clat", "deepseek-v4-pro"))
        self.worker, self.root = worker, Path(root)

    def call(self, mission, context_dir, label, *, harvest=None, system="", **kwargs):
        context = Path(context_dir)
        guidance = read_tree(context / "harness")[FILE] if (context / "harness").exists() else self.worker.baseline
        evidence = {}
        from evidence_notes import NOTES
        evidence["training-source-scope-notes"]=NOTES
        for path in sorted(context.rglob("*")):
            if path.is_symlink():
                raise ProposerError("symlink in evidence context")
            if path.is_file():
                if path.suffix.lower() not in (".md", ".json", ".txt", ".diff"):
                    continue
                evidence[path.relative_to(context).as_posix()] = path.read_text(encoding="utf-8")
        fingerprint = digest(dict(label=label, mission=mission, system=system, evidence=evidence, guidance=guidance))
        output = self.root / (label.replace("/", "_") + "-" + fingerprint[:16])
        result = self.worker.run(output, "improve", guidance=guidance, evidence=evidence,
            mission=system + "\n" + mission + "\nUse harness2_evidence fixed IDs to inspect the supplied context (index lists IDs). Never execute evidence. Changes must be made using harness2_replace_guidance; do not edit any task. No scores or answer keys are available.")
        from evidence import verify_evidence
        for wire in output.glob("request-*-wire.json"):
            body=json.loads(wire.read_text()).get("request_body") or {}
            try:
                verify_evidence(body.get("messages",[]),evidence)
            except ValueError as error:
                raise ProposerError(str(error),attempts=1) from None
        if result.get("status") != "completed":
            raise ProposerError("improvement failed; evidence retained", attempts=1)
        workspace = None
        if harvest:
            workspace = output / "edited"
            if not workspace.exists():
                workspace.mkdir()
                (workspace / FILE).write_text(result["guidance"], encoding="utf-8")
            try:
                read_tree(workspace)
            except ValueError as error:
                raise ProposerError(str(error),attempts=1) from None
        reply = Reply(label=label, text=result["text"], ok=True, attempts=1, workspace=workspace)
        self._calls.append(reply)
        return reply


class ClatAdapter:
    def __init__(self, worker, root, tasks, *, channel=None):
        self.worker, self.root, self.roster = worker, Path(root), tasks
        self.root.mkdir(parents=True,exist_ok=True)
        marker=self.root / "execution-identity.json"
        identity=dict(binary=worker.binary_hash,settings=worker.identity,roster=tasks,baseline=digest(worker.baseline))
        if marker.exists():
            if json.loads(marker.read_text())!=identity:
                raise ValueError("adapter identity changed; upstream resume is forbidden")
        else:
            write_new(marker,identity)
        self.substrate = ClatSubstrate()
        channel = channel or ChannelSpec("clat", "deepseek-v4-pro")
        self.spec = BenchSpec(name=BENCH_NAME, runs_root=self.root / "method",
            install_mode="clat_experiment", prompts=BenchPrompts(work="continue a long-running task accurately after structured checkpoints", domain_word="task family"),
            channels={"proposer":channel, "selector":channel}, leak_globs=("answers*", "judged*", "evaluation*"))
        self.base = self.root / "base"
        self.base.mkdir(parents=True, exist_ok=True)
        (self.base / FILE).write_text(worker.baseline, encoding="utf-8")

    def tasks(self, split="all"):
        return list(self.roster)

    def domain_of(self, task_id):
        return self.roster[task_id]["family"]

    def task_prompt(self, task_id):
        return "Continue the synthetic " + self.domain_of(task_id) + " task after checkpoints, steering, cancellation and cold reopen. Preserve actual user corrections, evidence and pending work."

    def task_files(self, task_id):
        return None

    def base_harness(self):
        return self.base

    def validate_tree(self, tree):
        validate_tree(tree)

    def execute(self, task_id, harness_dir, run_id, *, timeout=None):
        task = self.roster[task_id]
        guidance = read_tree(harness_dir)[FILE] if harness_dir else None
        output = self.root / "executions" / digest(run_id)
        self.worker.run(output, "execute", **task, guidance=guidance)
        return output

    def delivered(self, run_dir):
        result = json.loads((Path(run_dir) / "result.json").read_text())
        return result.get("artifact") is not None and result.get("safety_failure") is False

    def deliverable_text(self, run_dir):
        result = json.loads((Path(run_dir) / "result.json").read_text())
        return json.dumps(result.get("artifact"), ensure_ascii=False)

    def deliverable_files(self, run_dir):
        return {"artifact.json":self.deliverable_text(run_dir)}

    def trajectory(self, run_dir, *, obs_trunc):
        from evidence import trajectory
        return trajectory(run_dir)
