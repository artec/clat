"""Finite Codex-authored/GLM-executed research round; no DeepSeek requests."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
from bootstrap import PIN, prepare
from campaign import train, evaluation, freeze_json
from cases import FAMILIES, CHECKER_VERSION, legacy
from protocol import Worker, digest
from surface import FILE, validate_tree


def roster():
    training={f'{family}-train':dict(family=family,trial=30,control=False) for family in FAMILIES}
    heldout={f'{family}-held-{trial}-r{repeat}':dict(family=family,trial=trial,control=False)
             for family in FAMILIES for trial in (31,32) for repeat in range(3)}
    return dict(training=training,heldout=heldout,legacy=legacy())


def record(seed):
    return dict(protocol_version=3,checker_version=CHECKER_VERSION,review_protocol=2,
        discovery_prefix='clat-long-clat-agg1-s42',method=PIN,seed=42,**roster(),
        planned_scenarios=75,executor='glm-5.3-flash',improver='codex-local',reviewer='codex-local',
        execution_endpoint='https://open.bigmodel.cn/api/coding/paas/v4',improvement_endpoint=None,
        window=24000,output_limit=4096,control_window=1000000,thinking='high',
        request_parameters=dict(stream=True,max_tokens=4096,parallel_tool_calls=True,
            thinking=dict(type='enabled',clear_thinking=False),reasoning_effort='high',temperature=None),
        author_seed_sha256=hashlib.sha256(seed.encode()).hexdigest(),
        author_model_identity='current Codex conversation; exact backend identity not asserted',
        policy='75 finite executions; preserve all failures; no held-out tuning; KEEP_BASE pending complete acceptance and independent owner review')


def initialize(worker, root, seed):
    from run import initialize as common
    validate_tree({FILE:seed})
    freeze_json(root/'author-seed.json',dict(guidance=seed,author='codex-local'))
    return common(worker,root,record=record(seed))


def export_reviews(root, protocol):
    from cohort import discovery
    from review import packets_for_run
    rows=discovery(root,protocol)
    rows += [dict(split='unified-training',id=id,arm='candidate',output=root/'workers/unified-training'/id) for id in protocol['training']]
    rows += [dict(split=split,id=id,arm=arm,output=root/'workers'/split/id/arm)
             for split in ('heldout','legacy') for id in protocol[split] for arm in ('base','candidate')]
    packets=[]
    for row in rows:
        if (row['output']/'complete.json').exists():
            for packet in packets_for_run(row['output']):
                packets.append(dict(case={key:row[key] for key in ('split','id','arm')},
                    packet=digest(packet),input=packet))
    freeze_json(root/'codex-review-packets.json',dict(author='codex-local',rows=packets,
        policy='exact quote validation and scope review required; export is not semantic acceptance'))


def execute_phase(worker, root, phase, protocol, seed):
    from run import probes
    if phase in ('probe','campaign'):
        if not probes(worker,root,families=('glm','glm-tool','glm-image')):
            raise RuntimeError('GLM route probe failed; no fallback')
    elif json.loads((root/'probe-results.json').read_text()).get('passed') is not True:
        raise RuntimeError('GLM route probes have not passed')
    if phase in ('train','campaign'):
        from local_proposer import make_proposer
        from harness2.core.spec import ChannelSpec
        author=make_proposer(worker,root/'author-requests',seed=seed)
        train(worker,root,roster=protocol['training'],proposer=author,channel=ChannelSpec('clat','codex-local'))
    if phase in ('evaluate','campaign'):
        evaluation(worker,root,rosters=tuple((name,protocol[name]) for name in ('heldout','legacy')))
        export_reviews(root,protocol)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase',choices=('prepare','probe','train','evaluate','campaign','export-review'))
    parser.add_argument('--source',type=Path,required=True)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--seed-guidance',type=Path,required=True)
    args=parser.parse_args()
    if not args.output.is_absolute():
        parser.error('output must be absolute')
    if args.phase not in ('prepare','export-review') and os.environ.get('CLAT_HARNESS2_ARM')!='1':
        print('NOT_ARMED: no experiment material read and no model constructed')
        return 2
    from local_author import regular
    seed=regular(args.seed_guidance)
    validate_tree({FILE:seed})
    args.output.mkdir(parents=True,exist_ok=True,mode=0o700)
    sys.path.insert(0,str(prepare(args.source.resolve(),args.output/'upstream')))
    worker=Worker(args.binary)
    protocol=initialize(worker,args.output,seed)
    if args.phase=='prepare':
        return 0
    if args.phase=='export-review':
        export_reviews(args.output,protocol)
        return 0
    try:
        execute_phase(worker,args.output,args.phase,protocol,seed)
    except (RuntimeError,ValueError,OSError) as error:
        freeze_json(args.output/'stopped.json',dict(phase=args.phase,error=str(error),verdict='KEEP_BASE'))
        raise
    return 0


if __name__=='__main__':
    raise SystemExit(main())
