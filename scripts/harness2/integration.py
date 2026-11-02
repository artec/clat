"""Credential-free integration with the actual pinned upstream method."""
import argparse
import json
from pathlib import Path
import sys
from bootstrap import prepare
from protocol import write_new


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--binary",type=Path)
    args=parser.parse_args()
    args.output.mkdir(parents=True)
    sys.path.insert(0,str(prepare(args.source.resolve(),args.output / "upstream")))
    from adapter import ClatAdapter,ClatProposer
    from harness2.core.schedule import run_domain
    from tests.test_contracts import GUIDANCE

    class FakeWorker:
        baseline=GUIDANCE
        identity={"method":"offline-scripted"}
        binary_hash="offline-scripted-no-executable"
        executions=0
        def run(self,output,role,**fields):
            output=Path(output)
            output.mkdir(parents=True)
            if role=="improve":
                return dict(status="completed",text="<notes>Ground identifiers in their source.</notes>\nNO_ENTRY",guidance=GUIDANCE+"\nPreserve the original source attribution.")
            self.executions+=1
            evidence=output / "observations"
            evidence.mkdir()
            write_new(evidence / "maintenance-views.json",dict(views=[]))
            result=dict(evidence_root=str(evidence),status="passed",artifact={"synthetic":True},safety_failure=False,stages=[dict(stage="continue",text="synthetic artifact")])
            write_new(output / "result.json",result)
            return result

    worker=FakeWorker()
    adapter=ClatAdapter(worker,args.output / "adapter",{"tools-train":dict(family="tools",trial=10,control=False)})
    proposer=ClatProposer(worker,args.output / "improvements")
    kwargs=dict(adapter=adapter,prop=proposer,runs_root=adapter.spec.runs_root,library_root=args.output / "library",modes=("agg",),seed=42)
    result=run_domain("tools",["tools-train"],1,**kwargs)
    assert result["failed"]==0,result
    assert worker.executions==4,worker.executions
    for path in adapter.spec.runs_root.rglob("summary.json"):
        record=json.loads(path.read_text())
        ledger=record["modes"]["agg"]["ledger"]
        assert (ledger["R"],ledger["P"])==(4,3),ledger
        assert not record["modes"]["agg"]["fell_back"],record
    count=worker.executions
    resumed=run_domain("tools",["tools-train"],1,**kwargs)
    assert worker.executions==count and resumed["tasks"][0]["status"]=="skipped"
    worker.identity={"method":"changed"}
    try:
        ClatAdapter(worker,adapter.root,adapter.roster)
    except ValueError:
        pass
    else:
        raise AssertionError("upstream resume must not bypass CLAT identity")
    if args.binary:
        from protocol import Worker
        from evidence import trajectory,maintenance_file
        real_output=args.output / "real-application"
        result=Worker(args.binary).run(real_output,"offline")
        assert result["replay_equal"] is True,result
        assert maintenance_file(real_output).is_file()
        assert "durable replacement" in trajectory(real_output)
    print("Pinned upstream integration: R=4, P=3; resume made no execution; identity drift rejected. No model calls.")
    return 0

if __name__=="__main__":
    raise SystemExit(main())
