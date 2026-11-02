"""Frozen synthetic roster and independent answer checker. Never sent to editors."""
import json

FAMILIES = ("requirements", "tools", "images")


def training():
    return {f"{family}-train":dict(family=family,trial=10,control=False) for family in FAMILIES}


def heldout():
    return {f"{family}-held-{trial}-r{repeat}":dict(family=family,trial=trial,control=False)
        for family in FAMILIES for trial in (11,12) for repeat in range(3)}


def legacy():
    return {f"{family}-legacy-{trial}{'-control' if control else ''}":dict(family=family,trial=trial,control=control)
        for family in FAMILIES for trial,control in ((0,False),(1,False),(2,False),(0,True))}


def expected(case):
    n = case["trial"]
    result = dict(format="jsonl", path="artifacts/星河/export-v2.jsonl", command="clat exec -- '核对账单'",
        error="E_RATE_429: retry-after=17", owner="林岚", revision="r9",pending="remote_acceptance",
        database="unchanged", ticket=f"ORION-{700+n}",head="HEAD_31",tail="TAIL_97",steering="do_not_deploy",steer_tag="STEER_52")
    if n >= 10:
        result.update(path=f"artifacts/合成{n}/out-{n+4}-final.jsonl",owner=("周禾","许澄","Mira")[n%3],revision=f"r{n+11}",
            error=f"E_RATE_429: retry-after={n+17}",head=f"HEAD_{n+31}",tail=f"TAIL_{n+97}")
    if case["family"] == "tools":
        # Independent sum of the eight declared synthetic records, not the model total.
        result.update(tool_total=sum(17+id*3+n*10 for id in range(8)),unconfirmed_count=2)
    if case["family"] == "images":
        result["latest_color"]="green"
    return result


def check_object(text, facts):
    try:
        actual=json.loads(text) if isinstance(text,str) else text
    except (ValueError,TypeError):
        return dict(passed=False,error="invalid JSON artifact")
    if not isinstance(actual,dict):
        return dict(passed=False,error="object required")
    wrong={key:dict(expected=value,actual=actual.get(key)) for key,value in facts.items()
           if actual.get(key)!=value or type(actual.get(key)) is not type(value)}
    extra=sorted(set(actual)-set(facts))
    return dict(passed=not wrong and not extra,wrong=wrong,extra=extra)


def check_recall(text, case, stage):
    required=expected(case)
    if stage=="recall-pre-tools":
        for key in ("head","tail","steer_tag","tool_total","unconfirmed_count"):
            if key in required:
                required[key]=None
    return check_object(text,required)


def evaluate(result, case):
    facts=expected(case)
    recalls=[]
    for stage in result.get("stages",[]):
        if stage.get("stage","").startswith("recall-"):
            recalls.append(dict(stage=stage["stage"],**check_recall(stage.get("text"),case,stage["stage"])))
    artifact=check_object(result.get("artifact"),facts)
    completed=result.get("status")=="passed" and len(recalls)==4
    return dict(passed=completed and artifact["passed"] and all(r["passed"] for r in recalls),
        lifecycle=completed,recalls=recalls,artifact=artifact,safety_failure=result.get("safety_failure",True))
