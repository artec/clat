"""Frozen synthetic roster and independent answer checker. Never sent to editors."""
import json

FAMILIES = ("requirements", "tools", "images")
CHECKER_VERSION = 4
RECALL_STAGES = ("recall-pre-tools", "recall-active", "recall-manual", "recall-cold")


def training():
    return {f"{family}-train":dict(family=family,trial=20,control=False) for family in FAMILIES}


def heldout():
    return {f"{family}-held-{trial}-r{repeat}":dict(family=family,trial=trial,control=False)
        for family in FAMILIES for trial in (21,22) for repeat in range(3)}


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
           if key not in actual or actual.get(key)!=value or type(actual.get(key)) is not type(value)}
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
    stages=[row["stage"] for row in recalls]
    completed=lifecycle(result,case) and len(stages)==len(RECALL_STAGES) and set(stages)==set(RECALL_STAGES)
    observations=[s['image_color_correct'] for s in result.get('stages',[]) if 'image_color_correct' in s]
    images_ok=case['family']!='images' or (len(observations)==1 and observations[0] is True)
    facts_ok=artifact['passed'] and all(r['passed'] for r in recalls) and images_ok
    return dict(passed=completed and facts_ok and result.get('safety_failure') is False,
        lifecycle=completed,facts_passed=facts_ok,image_observation_verified=images_ok,recalls=recalls,artifact=artifact,safety_failure=result.get("safety_failure"),checker_version=CHECKER_VERSION)


def lifecycle(result, case):
    cancelled=[s for s in result.get('stages',[]) if s.get('stage')=='cancel-steer']
    checkpoints=case['control'] or all(type(result.get(k)) is int and result[k]>=1 for k in ('pre_checkpoints','active_checkpoints','manual_checkpoints'))
    return (checkpoints and result.get('replay_equal') is True and result.get('checkpoint_replay_equal') is True
        and result.get('reads')==list(range(8)) and type(result.get('publications')) is int and result['publications']==1
        and len(cancelled)==1 and all(cancelled[0].get(k) is True for k in ('observed_provider_event','steering_admitted','cancelled')))
