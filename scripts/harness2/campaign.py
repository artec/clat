"""Finite, resumable campaign phases; selection and product acceptance stay distinct."""
import json
from pathlib import Path
import random
from cases import training, heldout, legacy, evaluate
from protocol import digest, write_new
from surface import FILE, read_tree


def freeze_json(path, value):
    path=Path(path)
    if path.exists():
        if path.is_symlink() or json.loads(path.read_text())!=value:
            raise ValueError('frozen campaign record changed')
    else:
        write_new(path,value)


def freeze_text(path, text):
    if path.exists():
        if path.is_symlink() or path.read_text()!=text:
            raise ValueError('frozen composition input changed')
    else:
        with path.open('x',encoding='utf-8') as out:
            out.write(text)


def composition_context(adapter, root, baseline):
    context=root/'composition'
    (context/'harness').mkdir(parents=True,exist_ok=True)
    freeze_text(context/'harness'/FILE,baseline)
    sources=[]
    for i,path in enumerate(sorted(adapter.spec.runs_root.rglob('summary.json'))):
        summary=json.loads(path.read_text())
        selected=summary['selections']['agg']
        if selected.get('ok'):
            guide=read_tree(path.parent/'harnesses'/selected['chosen_label'])[FILE]
            trace=adapter.trajectory(selected['chosen_run_dir'],obs_trunc=50000)
            source=dict(domain=summary['domain'],kind='upstream_selected',label=selected['chosen_label'])
        else:
            guide=baseline
            trace=adapter.trajectory(summary['base'][0]['run_dir'],obs_trunc=50000)
            source=dict(domain=summary['domain'],kind='baseline_reference_selection_failed',error=selected.get('error'))
            prefix=summary['base'][0]['run_id'].rsplit('/',1)[0]
            for label in summary['modes']['agg']['finals']:
                trace+='\n[UNSUCCESSFUL FINAL; NOT A DELIVERABLE]\n'
                trace+=adapter.trajectory(adapter.root/'executions'/digest(prefix+'/'+label),obs_trunc=50000)
        freeze_text(context/f'candidate-{i}.md',guide)
        freeze_text(context/f'trajectory-{i}.md',trace)
        sources.append(source)
    if len(sources)!=3 or {s['domain'] for s in sources}!={'requirements','tools','images'}:
        raise ValueError('three frozen training domains required')
    freeze_json(context/'sources.json',dict(sources=sources,policy='failed selection is never promoted; baseline references are labelled'))
    return context


def train(worker, root, *, roster=None, proposer=None, channel=None):
    from adapter import ClatAdapter, ClatProposer
    from harness2.core.schedule import run_domain
    roster=training() if roster is None else roster
    adapter=ClatAdapter(worker,root/'training',roster,channel=channel)
    prop=ClatProposer(worker,root/'workers/improvements') if proposer is None else proposer
    require_safe_history(adapter.root)
    for task in roster:
        previous=list(adapter.spec.runs_root.rglob(task+'/summary.json'))
        if not previous:
            run_domain(adapter.domain_of(task),[task],1,adapter=adapter,prop=prop,
                runs_root=adapter.spec.runs_root,library_root=root/'training/library',modes=('agg',),seed=42)
        # Partial selections remain partial. Resume cannot silently rerun their rollouts.
    context=composition_context(adapter,root,worker.baseline)
    reply=prop.call('Compose one general summary directive using the three training task families. Use only observed evidence. Preserve eight sections and source scope. KEEP_BASE is valid. Never include task-specific answers. Explicit assistant observations made after actually receiving images are evidence too; distinguish them from guesses and offload placeholders. Reply with notes.',context,'unified-composition',harvest='harness')
    guidance=read_tree(reply.workspace)[FILE]
    final=root/'frozen-candidate'
    final.mkdir(exist_ok=True)
    freeze_text(final/FILE,guidance)
    freeze_json(final/'identity.json',dict(guidance=digest(guidance),baseline=digest(worker.baseline)))
    for id,case in roster.items():
        result=worker.run(root/'workers/unified-training'/id,'execute',**case,guidance=guidance)
        print('unified-training',id,result.get('status'),flush=True)
        if result.get('safety_failure') is not False:
            raise RuntimeError('unified training safety failed or unknown; no evaluation admission')


def require_safe_history(root):
    for output in (root/'executions').iterdir() if (root/'executions').exists() else ():
        if not output.is_dir():
            continue
        result=output/'result.json'
        complete=output/'complete.json'
        if not result.exists() or not complete.exists():
            raise RuntimeError('incomplete discovery history; safety cannot be verified')
        value=json.loads(result.read_text())
        if json.loads(complete.read_text()).get('result')!=digest(value) or value.get('safety_failure') is not False:
            raise RuntimeError('discovery safety failed or unknown; resume stopped')


def evaluation(worker, root, *, rosters=None):
    guidance=(root/'frozen-candidate'/FILE).read_text()
    identity=json.loads((root/'frozen-candidate/identity.json').read_text())
    if identity!=dict(guidance=digest(guidance),baseline=digest(worker.baseline)):
        raise ValueError('frozen candidate changed')
    rosters=(('heldout',heldout()),('legacy',legacy())) if rosters is None else rosters
    jobs=[(split,id,case,arm) for split,roster in rosters for id,case in roster.items() for arm in ('base','candidate')]
    random.Random(42).shuffle(jobs)
    rows=[]
    directory=root/'evaluation-rows'
    directory.mkdir(exist_ok=True)
    for index,(split,id,case,arm) in enumerate(jobs):
        output=root/'workers'/split/id/arm
        result=worker.run(output,'execute',**case,guidance=guidance if arm=='candidate' else None)
        row=dict(split=split,id=id,arm=arm,output=str(output),family=case['family'],control=case['control'],**evaluate(result,case))
        freeze_json(directory/f'{index:02}.json',row)
        rows.append(row)
        print(split,id,arm,'passed' if row['passed'] else 'FAILED',flush=True)
        if row['safety_failure'] is not False:
            break
    freeze_json(root/'evaluation.json',dict(rows=rows,expected=60,completed=len(rows),not_executed=[dict(split=s,id=i,arm=a) for s,i,_,a in jobs[len(rows):]],verdict='KEEP_BASE_PENDING_SEMANTIC_REVIEW'))
    if any(row['safety_failure'] is not False for row in rows):
        raise RuntimeError('evaluation safety failed or unknown; remaining model calls stopped')


def review_phase(worker, root):
    from review import audit_run
    from cohort import discovery
    protocol=json.loads((root/'protocol.json').read_text())
    cases=[row for row in discovery(root,protocol) if (row['output']/'complete.json').exists()]
    for split in ('heldout','legacy'):
        for id,roster in (heldout() if split=='heldout' else legacy()).items():
            for arm in ('base','candidate'):
                output=root/'workers'/split/id/arm
                if (output/'complete.json').exists():
                    cases.append(dict(split=split,id=id,arm=arm,output=output))
    for id in training():
        output=root/'workers/unified-training'/id
        if (output/'complete.json').exists():
            cases.append(dict(split='unified-training',id=id,arm='candidate',output=output))
    rows=[]
    for case in cases:
        rows.append(dict(split=case['split'],id=case['id'],arm=case['arm'],reviews=audit_run(worker,root/'workers/semantic',case['output'])))
        print('review',case['id'],case['arm'],flush=True)
    freeze_json(root/'semantic-review.json',dict(rows=rows,policy='model opinions only; no production authority'))
