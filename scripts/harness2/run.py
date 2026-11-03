"""Explicitly armed Harness2 experiment driver. No production Host connection."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys

from bootstrap import prepare, PIN
from cases import training, heldout, legacy, CHECKER_VERSION
from protocol import Worker, digest, write_new
from campaign import train, evaluation, review_phase, freeze_json
from cohort import METHOD_RUN_ID


def initialize(worker, root, *, record=None):
    record = record if record is not None else dict(protocol_version=2, checker_version=CHECKER_VERSION, review_protocol=2, discovery_prefix=METHOD_RUN_ID, method=PIN, seed=42, training=training(), heldout=heldout(), legacy=legacy(),
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
    tasks = worker.run(root / "workers/corpus-freeze", "corpus")
    freeze_json(root/'task-inputs.json',tasks)
    worker.identity.update(task_inputs=digest(tasks))
    result = worker.run(root / "workers/baseline-export", "baseline")
    worker.baseline = result["guidance"]
    return record


def probes(worker, root, *, families=None):
    families=families or ("glm", "glm-tool", "glm-image", "deepseek", "deepseek-tool", "deepseek-review")
    rows=[]
    for family in families:
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
        delivered=set()
        for path in output.glob("request-*-wire.json"):
            body=json.loads(path.read_text()).get("request_body") or {}
            delivered.update(verify_evidence(body.get("messages",[]),evidence))
        text=result.get("text","").strip()
        passed=result.get("status")=="completed"
        if family.endswith("-tool"):
            required={'guidance','long-evidence'} if family=='deepseek-tool' else {'guidance'}
            passed=passed and result.get("reads",0)>=2 and required<=delivered
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
    passed=len(rows)==len(families) and all(r["passed"] for r in rows)
    freeze_json(root / "probe-results.json", dict(rows=rows,passed=passed))
    return passed


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase",choices=("prepare","probe","train","evaluate","review","collect-review","report","campaign"))
    parser.add_argument("--source",type=Path,default=Path(__file__).resolve().parents[3]/"harness2")
    parser.add_argument("--binary",type=Path)
    parser.add_argument("--output",type=Path,required=True)
    args=parser.parse_args()
    if not args.output.is_absolute():
        parser.error("output must be absolute")
    if args.phase in ('report','collect-review'):
        if not args.output.is_dir():
            parser.error("report requires an existing experiment directory")
        if args.phase=='collect-review':
            from review_collection import collect
            collect(args.output)
        else:
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
    try:
        if args.phase=="campaign":
            if not probes(worker,args.output):
                raise RuntimeError('route probes failed; no route fallback')
            train(worker,args.output)
            evaluation(worker,args.output)
            review_phase(worker,args.output)
        elif args.phase=="probe":
            if not probes(worker,args.output):
                raise RuntimeError('route probes failed; no route fallback')
        else:
            if json.loads((args.output/'probe-results.json').read_text()).get('passed') is not True:
                raise RuntimeError('route probes have not passed')
            if args.phase=='train':
                train(worker,args.output)
            elif args.phase=='evaluate':
                evaluation(worker,args.output)
            else:
                review_phase(worker,args.output)
    except (RuntimeError,ValueError,OSError) as error:
        freeze_json(args.output/'stopped.json',dict(phase=args.phase,error=str(error),status='stopped',verdict='KEEP_BASE'))
        from reporting import report
        report(args.output)
        return 1
    if args.phase=='campaign':
        from reporting import report
        report(args.output)
    return 0

if __name__=="__main__":
    raise SystemExit(main())
