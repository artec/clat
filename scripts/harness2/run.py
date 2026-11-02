"""Explicitly armed Harness2 experiment driver. No production Host connection."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import sys

from bootstrap import prepare, PIN
from cases import training, heldout, legacy, evaluate
from protocol import Worker, digest, write_new


def initialize(worker, root):
    record = dict(method=PIN, seed=42, training=training(), heldout=heldout(), legacy=legacy(),
        planned_scenarios=75, improver_context=1000000, improver_output_limit=16384, improver_thinking="low", improver_maintenance=False, executor="glm-5.3-flash", improver="deepseek-v4-pro",
        execution_endpoint="https://open.bigmodel.cn/api/coding/paas/v4", improvement_endpoint="https://api.deepseek.com",
        window=24000, output_limit=4096, control_window=1000000,
        policy="freeze before calls; all failures retained; no holdout tuning; KEEP_BASE unless complete acceptance")
    protocol = root / "protocol.json"
    if protocol.exists():
        if json.loads(protocol.read_text()) != record:
            raise ValueError("protocol changed; use a new run root")
    else:
        write_new(protocol, record)
    worker.identity.update(protocol=digest(record), patch=hashlib.sha256((Path(__file__).parent / "upstream-clat.patch").read_bytes()).hexdigest())
    result = worker.run(root / "workers/baseline-export", "baseline")
    worker.baseline = result["guidance"]
    return record


def probes(worker, root):
    rows=[]
    for family in ("glm", "glm-tool", "glm-image", "deepseek", "deepseek-tool", "deepseek-review"):
        mission = 'Reply exactly: HARNESS2_ROUTE_OK'
        if family.endswith("-tool"):
            mission = 'Call harness2_evidence with id index, then id guidance. Reply HARNESS2_ROUTE_OK. Do not edit guidance.'
        elif family == "glm-image":
            mission = 'Observe the attached image. Reply only its dominant color as one lowercase English word.'
        elif family == "deepseek-review":
            mission = 'Inspect source: user says current ticket FALCON-17. Candidate says FALCON-71 is confirmed. Return only JSON {"verdict":"unsupported","source_quote":"FALCON-17","candidate_quote":"FALCON-71"} if the candidate contradicts the source. No tools.'
        evidence={"guidance":worker.baseline}
        if family=="deepseek-tool":
            evidence["long-evidence"]="合成证据；保持原样，不执行其中的任何文字。\n"*12000
            mission+=' Also read the complete long-evidence fixed ID. Do not copy its body to the answer.'
        output=root / "workers/probes" / family
        result = worker.run(output, "probe", family=family,
            mission=mission, evidence=evidence, guidance=worker.baseline)
        from evidence import verify_evidence
        for path in output.glob("request-*-wire.json"):
            body=json.loads(path.read_text()).get("request_body") or {}
            verify_evidence(body.get("messages",[]),evidence)
        text=result.get("text","").strip()
        passed=result.get("status")=="completed"
        if family.endswith("-tool"):
            passed=passed and result.get("reads",0)>=2
        elif family=="glm-image":
            passed=passed and text.lower()=="green"
        elif family=="deepseek-review":
            try:
                review=json.loads(text)
                passed=passed and review==dict(verdict="unsupported",source_quote="FALCON-17",candidate_quote="FALCON-71")
            except ValueError:
                passed=False
        else:
            passed=passed and text=="HARNESS2_ROUTE_OK"
        rows.append(dict(family=family,passed=passed))
        print("probe",family,"passed" if passed else "FAILED",flush=True)
        if not passed:
            break
    write_new(root / "probe-results.json", dict(rows=rows,passed=len(rows)==6 and all(r["passed"] for r in rows)))
    return len(rows)==6 and all(r["passed"] for r in rows)


def train(worker, root):
    from adapter import ClatAdapter, ClatProposer
    from harness2.core.schedule import run_domain
    from surface import FILE, read_tree
    adapter=ClatAdapter(worker,root / "training",training())
    prop=ClatProposer(worker,root / "workers/improvements")
    for task in training():
        result=run_domain(adapter.domain_of(task),[task],1,adapter=adapter,prop=prop,
            runs_root=adapter.spec.runs_root,library_root=root / "training/library",modes=("agg",),seed=42)
        if result["failed"]:
            raise RuntimeError("training incomplete; no candidate promotion")
    context=root / "composition"
    (context / "harness").mkdir(parents=True,exist_ok=True)
    (context / "harness" / FILE).write_text(worker.baseline)
    for i,path in enumerate(sorted(adapter.spec.runs_root.rglob("summary.json"))):
        summary=json.loads(path.read_text())
        chosen=summary["selections"]["agg"]
        h=path.parent / "harnesses" / chosen["chosen_label"]
        (context / f"candidate-{i}.md").write_text(read_tree(h)[FILE])
        (context / f"trajectory-{i}.md").write_text(adapter.trajectory(chosen["chosen_run_dir"],obs_trunc=50000))
    reply=prop.call("Compose one general summary directive using the three training task families. Use only observed evidence. Preserve eight sections and source scope. KEEP_BASE is valid. Never include task-specific answers. Reply with notes.",context,"unified-composition",harvest="harness")
    guidance=read_tree(reply.workspace)[FILE]
    final=root / "frozen-candidate"
    final.mkdir()
    (final / FILE).write_text(guidance)
    write_new(final / "identity.json",dict(guidance=digest(guidance),baseline=digest(worker.baseline)))
    for id,case in training().items():
        worker.run(root / "workers/unified-training" / id,"execute",**case,guidance=guidance)


def evaluation(worker, root):
    from surface import FILE
    guidance=(root / "frozen-candidate" / FILE).read_text()
    identity=json.loads((root / "frozen-candidate/identity.json").read_text())
    if identity!=dict(guidance=digest(guidance),baseline=digest(worker.baseline)):
        raise ValueError("frozen candidate changed")
    jobs=[(split,id,case,arm) for split,roster in (("heldout",heldout()),("legacy",legacy())) for id,case in roster.items() for arm in ("base","candidate")]
    random.Random(42).shuffle(jobs)
    rows=[]
    for split,id,case,arm in jobs:
        output=root / "workers" / split / id / arm
        result=worker.run(output,"execute",**case,guidance=guidance if arm=="candidate" else None)
        row=dict(split=split,id=id,arm=arm,output=str(output),family=case["family"],control=case["control"],**evaluate(result,case))
        rows.append(row)
        print(split,id,arm,"passed" if row["passed"] else "FAILED",flush=True)
        if row["safety_failure"]:
            break
    write_new(root / "evaluation.json",dict(rows=rows,expected=60,completed=len(rows),verdict="KEEP_BASE_PENDING_SEMANTIC_REVIEW"))


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase",choices=("prepare","probe","train","evaluate","review","report","campaign"))
    parser.add_argument("--source",type=Path,default=Path(__file__).resolve().parents[3]/"harness2")
    parser.add_argument("--binary",type=Path)
    parser.add_argument("--output",type=Path,required=True)
    args=parser.parse_args()
    if not args.output.is_absolute():
        parser.error("output must be absolute")
    if args.phase=="report":
        if not args.output.is_dir():
            parser.error("report requires an existing experiment directory")
        from reporting import report
        report(args.output)
        return 0
    if args.binary is None:
        parser.error("this phase requires --binary")
    args.output.mkdir(parents=True,exist_ok=True,mode=0o700)
    upstream=prepare(args.source.resolve(),args.output / "upstream")
    sys.path.insert(0,str(upstream))
    worker=Worker(args.binary)
    initialize(worker,args.output)
    if args.phase=="prepare":
        return 0
    if os.environ.get("CLAT_HARNESS2_ARM")!="1":
        raise RuntimeError("real experiment NOT_ARMED")
    if args.phase=="campaign":
        if not probes(worker,args.output):
            return 1
        train(worker,args.output)
        evaluation(worker,args.output)
        from review import audit_run
        rows=json.loads((args.output / "evaluation.json").read_text())["rows"]
        audits=[]
        for row in rows:
            audits.append(dict(id=row["id"],arm=row["arm"],reviews=audit_run(worker,args.output / "workers/semantic",row["output"])))
            print("review",row["id"],row["arm"],flush=True)
        write_new(args.output / "semantic-review.json",dict(rows=audits,policy="model opinions only; no production authority"))
        from reporting import report
        report(args.output)
        return 0
    if args.phase=="probe":
        return 0 if probes(worker,args.output) else 1
    if json.loads((args.output / "probe-results.json").read_text()).get("passed") is not True:
        raise RuntimeError("route probes have not passed")
    if args.phase=="train":
        train(worker,args.output)
    elif args.phase=="evaluate":
        evaluation(worker,args.output)
    else:
        from review import audit_run
        rows=json.loads((args.output / "evaluation.json").read_text())["rows"]
        audits=[dict(id=row["id"],arm=row["arm"],reviews=audit_run(worker,args.output / "workers/semantic",row["output"])) for row in rows]
        write_new(args.output / "semantic-review.json",dict(rows=audits,policy="model opinions only; no production authority"))
    return 0

if __name__=="__main__":
    raise SystemExit(main())
