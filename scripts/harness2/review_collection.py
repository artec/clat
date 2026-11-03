"""Collect completed review attempts without creating a worker or calling a model."""
import hashlib
import json
import uuid
from pathlib import Path
from protocol import digest, write_new
from review import GUIDANCE, GUIDANCE_V1, packets_for_run, review_result


def saved_opinion(root, packet, identity, guidance=GUIDANCE_V1):
    output=(root/'workers/semantic'/digest(packet)).resolve()
    result=json.loads((output/'result.json').read_text())
    manifest=dict(role='review',output=str(output),mission=guidance+'\n'+json.dumps(packet,ensure_ascii=False))
    expected=digest(dict(binary=identity['binary'],settings=identity['settings'],manifest=manifest))
    marker=json.loads((output/'complete.json').read_text())
    if marker!=dict(identity=expected,result=digest(result)):
        raise ValueError('review result or frozen request identity changed')
    if json.loads((output/'worker.json').read_text()).get('exit_code')!=0:
        raise ValueError('review worker exit unverified')
    return review_result(result,packet)


def collect(root):
    root=Path(root).resolve()
    protocol=json.loads((root/'protocol.json').read_text())
    identity=json.loads((root/'training/execution-identity.json').read_text())
    version=protocol.get('review_protocol',1)
    if version not in (1,2):
        raise ValueError('unknown review protocol')
    guidance=GUIDANCE if version==2 else GUIDANCE_V1
    from cohort import discovery
    cases=[dict(split=r['split'],id=r['id'],arm=r['arm'],output=r['output']) for r in discovery(root,protocol)]
    cases += [dict(split='unified-training',id=id,arm='candidate',output=root/'workers/unified-training'/id) for id in protocol['training']]
    cases += [dict(split=split,id=id,arm=arm,output=root/'workers'/split/id/arm) for split in ('heldout','legacy') for id in protocol[split] for arm in ('base','candidate')]
    rows=[]
    for case in cases:
        packets=packets_for_run(case['output'])
        reviews=[]
        for packet in packets:
            try:
                reviews.append(saved_opinion(root,packet,identity,guidance))
            except (ValueError,KeyError,TypeError,OSError) as error:
                reviews.append(dict(packet=digest(packet),status='not_available',error=str(error)))
        rows.append(dict(split=case['split'],id=case['id'],arm=case['arm'],reviews=reviews))
    result=dict(rows=rows,policy='read-only collection; failed calls retained; no model requests or retries',
                collector_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest())
    write_new(root/('semantic-review-collection-'+str(uuid.uuid4())+'.json'),result)
    return result
