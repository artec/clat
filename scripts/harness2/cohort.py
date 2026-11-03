"""The finite pinned-method rollout identities, separate from model instructions."""
from pathlib import Path
from protocol import digest

BENCH_NAME='clat-long'
METHOD_RUN_ID=BENCH_NAME+'-clat-agg1-s42'


def discovery(root, protocol):
    root=Path(root)
    prefix=protocol.get('discovery_prefix',METHOD_RUN_ID)
    return [dict(split='discovery',id=task+'/'+label,arm=label,case=case,
        output=root/'training/executions'/digest(prefix+'/'+task+'/'+label))
        for task,case in protocol['training'].items() for label in ('base1','inv1','dom1','agg1')]
