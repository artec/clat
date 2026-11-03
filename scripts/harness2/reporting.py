"""Read-only recomputation; runtime flags and model opinions remain separate."""
import json
import hashlib
from pathlib import Path
import uuid
from protocol import write_new, digest


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
    fact_improved=set()
    regressed=[]
    for key,pair in pairs.items():
        if set(pair)!={"base","candidate"}:
            continue
        if pair["base"]["passed"] and not pair["candidate"]["passed"]:
            regressed.append(key)
        if not pair["base"]["passed"] and pair["candidate"]["passed"]:
            improved.add(pair["candidate"]["family"])
            base=pair['base']
            if base.get('control') is False and pair['candidate'].get('control') is False and base.get('lifecycle') is True and base.get('safety_failure') is False and base.get('structure_verified') is True and base.get('facts_passed') is False and pair['candidate'].get('facts_passed') is True:
                fact_improved.add(pair['candidate']['family'])
    eligible=complete and roster_complete and len(candidates)==30 and all(row["passed"] and row["safety_failure"] is False for row in candidates) and len(fact_improved)>=2 and not regressed and semantics
    return dict(verdict="ELIGIBLE_FOR_REVIEW" if eligible else "KEEP_BASE",improved_domains=sorted(improved),fact_improved_domains=sorted(fact_improved),regressions=regressed)


def changed_candidate(root):
    try:
        frozen=root/'frozen-candidate'
        identity=json.loads((frozen/'identity.json').read_text())
        guidance=(frozen/'summary_guidance.md').read_text()
        baseline=json.loads((root/'workers/baseline-export/result.json').read_text())['guidance']
        return identity==dict(guidance=digest(guidance),baseline=digest(baseline)) and guidance!=baseline
    except (ValueError,KeyError,TypeError,OSError):
        return False


def report(root):
    root=Path(root)
    from assessment import recompute, semantic_complete, semantic_inventory, check_run
    from cases import training
    protocol=json.loads((root/'protocol.json').read_text()) if (root/'protocol.json').exists() else None
    rows=recompute(root)['rows'] if protocol else []
    complete=bool(rows) and all((Path(row['output'])/'complete.json').exists() for row in rows)
    unified=[]
    discovered=[]
    if protocol:
        for id,case in protocol['training'].items():
            output=root/'workers/unified-training'/id
            try:
                unified.append(dict(split='unified-training',id=id,arm='candidate',control=False,output=str(output),**check_run(output,case)))
            except (ValueError,KeyError,TypeError,OSError):
                unified.append(dict(split='unified-training',id=id,arm='candidate',control=False,output=str(output),passed=False))
        from cohort import discovery
        for item in discovery(root,protocol):
            row=dict(split=item['split'],id=item['id'],arm=item['arm'],family=item['case']['family'],control=False,output=str(item['output']))
            try:
                row.update(check_run(item['output'],item['case']))
            except (ValueError,KeyError,TypeError,OSError):
                row.update(passed=False,status='not_executed' if not item['output'].exists() else 'unverified')
            discovered.append(row)
        complete=complete and all((Path(row['output'])/'complete.json').exists() for row in discovered+unified)
    training_ok=len(unified)==3 and all(row['passed'] for row in unified)
    sources=[p for p in [root/'semantic-review.json',*root.glob('semantic-review-collection-*.json')] if p.exists()]
    semantic=max(sources,key=lambda p:p.stat().st_mtime) if sources else root/'semantic-review.json'
    semantic_ok=False
    audits=[]
    if semantic.exists():
        audits=json.loads(semantic.read_text())['rows']
        extra=root/'discovery-semantic-review.json'
        if extra.exists():
            audits+=json.loads(extra.read_text())['rows']
        semantic_ok=semantic_complete(root,rows+unified+discovered,audits)
    ledger=[]
    for path in sorted(root.rglob("request-*-end.json")):
        result=json.loads(path.read_text())
        ledger.append(dict(path=str(path.relative_to(root)),usage=result.get("usage"),cost=result.get("cost"),elapsed_ms=result.get("elapsed_ms"),stage=result.get("stage"),outcome=result.get("outcome",{}).get("finish")))
    for path in sorted(root.rglob("request-*-start.json")):
        end=path.with_name(path.name.replace("-start","-end"))
        if not end.exists():
            start=json.loads(path.read_text())
            ledger.append(dict(path=str(path.relative_to(root)),usage=None,cost=None,elapsed_ms=None,stage=start.get("stage"),outcome="incomplete observation"))
    changed=changed_candidate(root)
    value=dict(**acceptance(rows,complete and training_ok and changed,semantic_ok),complete=complete,semantic_opinions_complete=semantic_ok,candidate_changed=changed,
        checker_rows=rows,unified_training=unified,discovery_training=discovered,semantic_source=str(semantic) if semantic.exists() else None,semantic_inventory=semantic_inventory(rows+unified+discovered,audits),requests=ledger,**usage_summary(ledger),cost=None,
        reporter_version=7,admission_policy='fact-improvement-v3',reporter_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        limits=["finite synthetic experiment only","no independent human acceptance","no remote CI or native UI acceptance","upstream ledger USD defaults are not billing evidence"])
    output=root / ("report-"+str(uuid.uuid4())+".json")
    write_new(output,value)
    from report_index import index
    index(output.with_suffix('.md'),value)
    print(value["verdict"],output,flush=True)
    return value
