"""Read-only recomputation; runtime flags and model opinions remain separate."""
import json
import hashlib
from pathlib import Path
import uuid
from protocol import write_new


def usage_summary(ledger):
    fields={}
    for key in ("input","output","cached","reasoning"):
        known=[request["usage"][key] for request in ledger
               if isinstance(request.get("usage"),dict)
               and type(request["usage"].get(key)) is int and request["usage"][key]>=0]
        fields[key]=dict(known_sum=sum(known),observed_requests=len(known),unknown_requests=len(ledger)-len(known))
    elapsed=[request["elapsed_ms"] for request in ledger
             if type(request.get("elapsed_ms")) is int and request["elapsed_ms"]>=0]
    return dict(usage_fields=fields,
        token_totals={key:value["known_sum"] if not value["unknown_requests"] else None for key,value in fields.items()},
        observed_token_totals={key:value["known_sum"] if value["observed_requests"] else None for key,value in fields.items()},
        requests_with_unknown_usage=sum(not isinstance(request.get("usage"),dict) for request in ledger),
        known_request_elapsed_ms=sum(elapsed),requests_with_unknown_elapsed=len(ledger)-len(elapsed))


def acceptance(rows, complete, semantics):
    candidates=[row for row in rows if row["arm"]=="candidate"]
    pairs={}
    for row in rows:
        pairs.setdefault((row["split"],row["id"]),{})[row["arm"]]=row
    roster_complete=len(rows)==60 and len(pairs)==30 and all(set(pair)=={"base","candidate"} for pair in pairs.values())
    improved=set()
    regressed=[]
    for key,pair in pairs.items():
        if set(pair)!={"base","candidate"}:
            continue
        if pair["base"]["passed"] and not pair["candidate"]["passed"]:
            regressed.append(key)
        if not pair["base"]["passed"] and pair["candidate"]["passed"]:
            improved.add(pair["candidate"]["family"])
    eligible=complete and roster_complete and len(candidates)==30 and all(row["passed"] and row["safety_failure"] is False for row in candidates) and len(improved)>=2 and not regressed and semantics
    return dict(verdict="ELIGIBLE_FOR_REVIEW" if eligible else "KEEP_BASE",improved_domains=sorted(improved),regressions=regressed)


def report(root):
    root=Path(root)
    rows=[]
    evaluation=root / "evaluation.json"
    complete=False
    if evaluation.exists():
        result=json.loads(evaluation.read_text())
        rows=result["rows"]
        complete=result["completed"]==result["expected"]==60
    semantic=root / "semantic-review.json"
    semantic_ok=False
    if semantic.exists():
        audits=json.loads(semantic.read_text())["rows"]
        expected={(row["id"],row["arm"]):row for row in rows}
        observed={(row["id"],row["arm"]) for row in audits}
        semantic_ok=len(audits)==60 and len(expected)==60 and observed==set(expected)
        for row in audits:
            original=expected.get((row["id"],row["arm"]))
            semantic_ok=semantic_ok and original is not None and (original.get("control",False) or len(row["reviews"])>=3)
            semantic_ok=semantic_ok and all(a["status"]=="opinion" and a["audit"]["verdict"]=="supported" for a in row["reviews"])
    ledger=[]
    for path in sorted(root.rglob("request-*-end.json")):
        result=json.loads(path.read_text())
        ledger.append(dict(path=str(path.relative_to(root)),usage=result.get("usage"),cost=result.get("cost"),elapsed_ms=result.get("elapsed_ms"),stage=result.get("stage"),outcome=result.get("outcome",{}).get("finish")))
    for path in sorted(root.rglob("request-*-start.json")):
        end=path.with_name(path.name.replace("-start","-end"))
        if not end.exists():
            start=json.loads(path.read_text())
            ledger.append(dict(path=str(path.relative_to(root)),usage=None,cost=None,elapsed_ms=None,stage=start.get("stage"),outcome="incomplete observation"))
    value=dict(**acceptance(rows,complete,semantic_ok),complete=complete,semantic_opinions_complete=semantic_ok,
        requests=ledger,**usage_summary(ledger),cost=None,
        reporter_version=2,reporter_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        limits=["finite synthetic experiment only","no independent human acceptance","no remote CI or native UI acceptance","upstream ledger USD defaults are not billing evidence"])
    output=root / ("report-"+str(uuid.uuid4())+".json")
    write_new(output,value)
    print(value["verdict"],output,flush=True)
    return value
