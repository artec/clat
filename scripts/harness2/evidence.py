"""Read actual durable maintenance evidence without inferring purpose from prose."""
import json
from pathlib import Path


def verify_evidence(messages, evidence):
    calls={}
    delivered=set()
    for message in messages:
        for call in message.get("tool_calls",[]):
            function=call.get("function",{})
            if function.get("name")=="harness2_evidence":
                arguments=function.get("arguments",{})
                arguments=json.loads(arguments) if isinstance(arguments,str) else arguments
                calls[call["id"]]=arguments.get("id")
        if message.get("role")!="tool":
            continue
        id=calls.get(message.get("tool_call_id"))
        if id not in evidence:
            continue
        try:
            payload=json.loads(message["content"])
        except (ValueError,TypeError):
            raise ValueError("experiment evidence was not delivered as complete JSON") from None
        if payload!=dict(id=id,text=evidence[id]):
            raise ValueError("experiment evidence changed before the model request")
        delivered.add(id)
    return delivered


def maintenance_file(run_dir):
    root=Path(run_dir).resolve()
    result=json.loads((root / "result.json").read_text())
    evidence=Path(result["evidence_root"])
    if ".." in evidence.parts or not evidence.is_absolute():
        raise ValueError("evidence must belong to this execution")
    for part in [evidence,*evidence.parents]:
        if part.is_symlink() and part.parent.resolve().is_relative_to(root):
            raise ValueError("evidence cannot cross symlinks")
    evidence=evidence.resolve()
    if evidence==root or not evidence.is_relative_to(root):
        raise ValueError("evidence escapes execution ownership")
    return evidence / "maintenance-views.json"


def trajectory(run_dir):
    run_dir = Path(run_dir)
    result = json.loads((run_dir / "result.json").read_text())
    lines = []
    for stage in result.get("stages", []):
        lines += ["[ACTION] " + stage["stage"], "[OBSERVATION] " + stage.get("text", "")]
    if result.get('status')=='failed':
        lines += ['[INCOMPLETE EXECUTION; NOT A DELIVERABLE]',json.dumps(dict(error=result.get('error'),failure=result.get('failure'),completed_stages=result.get('completed_stages',[])),ensure_ascii=False)]
    # Purpose is established by the durable carrier, never editable prompt wording.
    if 'evidence_root' not in result:
        return '\n'.join(lines+['[Durable replacement evidence unavailable]'])
    path=maintenance_file(run_dir)
    if path.exists():
        for view in json.loads(path.read_text())["views"]:
            lines += ["[ACTION] checkpoint source and retained context",json.dumps(view["before"],ensure_ascii=False),
                      "[OBSERVATION] durable replacement",json.dumps(view["after"],ensure_ascii=False)]
    return "\n".join(lines)
