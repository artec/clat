"""Read-only admission for orphan results; never manufactures a successful exit."""
import hashlib
import json
from pathlib import Path
from protocol import digest, write_new
from evidence import maintenance_file


def terminal_proof(output):
    output=Path(output).resolve()
    result=json.loads((output/'result.json').read_text())
    audit=result.get('terminal_audit',{})
    required=('verified','root_lease_reacquired','projection_replay_verified')
    if result.get('safety_failure') is not False or any(audit.get(k) is not True for k in required):
        raise ValueError('interrupted execution has no verified terminal safety')
    evidence=maintenance_file(output)
    if audit.get('maintenance_sha256')!=hashlib.sha256(evidence.read_bytes()).hexdigest():
        raise ValueError('terminal maintenance evidence changed')
    starts=sorted(evidence.parent.glob('request-*-start.json'))
    if not starts or audit.get('requests_ended')!=len(starts):
        raise ValueError('terminal request coverage unavailable')
    files={}
    for path in [output/'result.json',evidence]+starts+[p.with_name(p.name.replace('-start','-end')) for p in starts]:
        if path.is_symlink() or not path.is_file():
            raise ValueError('incomplete or linked terminal evidence')
        files[str(path.relative_to(output))]=hashlib.sha256(path.read_bytes()).hexdigest()
    return dict(result=digest(result),files=files,exit_code=None,
                status='interrupted_after_terminal_audit',safety_failure=False)


def record_interruption(output):
    output=Path(output)
    if (output/'complete.json').exists() or (output/'worker.json').exists():
        raise ValueError('completed worker must use its original receipt')
    value=terminal_proof(output)
    value['policy']='no rerun; terminal safety verified; process exit not observed; cannot pass'
    write_new(output/'interruption.json',value)
    return value


def interrupted_result(output):
    output=Path(output)
    record=json.loads((output/'interruption.json').read_text())
    expected=terminal_proof(output)
    if any(record.get(k)!=v for k,v in expected.items()):
        raise ValueError('interruption evidence changed')
    return dict(passed=False,lifecycle=False,facts_passed=False,safety_failure=False,
                structure_verified=False,status=record['status'],worker_exit_verified=False,
                result_sha256=record['result'])
