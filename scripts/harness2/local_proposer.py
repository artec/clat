"""Bridge an explicitly labelled local author to the pinned upstream Proposer."""
import time
from pathlib import Path
from local_author import Exchange
from surface import FILE, read_tree


def make_proposer(worker, root, *, seed):
    from harness2.core.proposer import Proposer, ProposerError, Reply
    from harness2.core.spec import ChannelSpec

    class LocalProposer(Proposer):
        def __init__(self):
            super().__init__(ChannelSpec('clat','codex-local'))
            self.exchange=Exchange(root,dict(binary=worker.binary_hash,settings=worker.identity))

        def call(self, mission, context_dir, label, *, harvest=None, system='', **kwargs):
            if worker.safety_blocked:
                raise ProposerError('execution safety failed or unknown; author stopped')
            context=Path(context_dir)
            guidance=read_tree(context/'harness')[FILE] if (context/'harness').exists() else worker.baseline
            try:
                directory=self.exchange.request(context,mission,label,guidance,system=system,seed=seed)
                print('AUTHOR_REQUEST',directory,flush=True)
                response=self.exchange.response(directory)
                while response is None:
                    time.sleep(1)
                    if worker.safety_blocked:
                        raise ProposerError('execution safety failed or unknown; author stopped')
                    response=self.exchange.response(directory)
                workspace=self.workspace(directory,response['guidance']) if harvest else None
            except (ValueError,OSError) as error:
                raise ProposerError(str(error),attempts=1) from None
            reply=Reply(label=label,text=response['notes'],ok=True,attempts=1,
                        channel='codex-local',workspace=workspace,audit=directory)
            self._calls.append(reply)
            return reply

        def workspace(self, directory, guidance):
            workspace=directory/'edited'
            if workspace.exists() or workspace.is_symlink():
                if read_tree(workspace)!={FILE:guidance}:
                    raise ValueError('local author harvested guidance changed')
            else:
                workspace.mkdir(mode=0o700)
                path=workspace/FILE
                with path.open('x',encoding='utf-8') as file:
                    file.write(guidance)
            return workspace

    return LocalProposer()
