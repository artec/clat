"""Explicit local Codex authorship. No API, shell or model worker is invoked here."""
import json
from pathlib import Path
import stat
from protocol import digest, write_new
from surface import FILE, validate_tree


def regular(path):
    mode=path.lstat().st_mode
    if not stat.S_ISREG(mode) or mode & 0o111:
        raise ValueError('author input must be a non-executable regular file')
    return path.read_text(encoding='utf-8')


def context_evidence(context):
    context=Path(context)
    if context.is_symlink():
        raise ValueError('symlink in author context')
    evidence={}
    for path in sorted(context.rglob('*')):
        parts=path.relative_to(context).parts
        if any(part.lower().startswith(('answers','judged','evaluation','heldout')) for part in parts):
            raise ValueError('answers and held-out material are not author inputs')
        if path.is_symlink():
            raise ValueError('symlink in author context')
        if path.is_file():
            text=regular(path)
            if path.suffix.lower() in ('.md','.json','.txt','.diff'):
                evidence[path.relative_to(context).as_posix()]=text
    return evidence


class Exchange:
    def __init__(self, root, identity):
        self.root,self.identity=Path(root),identity

    def request(self, context, mission, label, guidance, *, system='', seed=None):
        validate_tree({FILE:guidance})
        if seed is not None:
            validate_tree({FILE:seed})
        body=dict(version=1,author='codex-local',identity=self.identity,label=label,
                  mission=mission,system=system,guidance=guidance,
                  evidence=context_evidence(context),seed_guidance=seed)
        directory=self.root/digest(body)
        if self.root.is_symlink() or directory.is_symlink():
            raise ValueError('symlink in author output')
        directory.mkdir(parents=True,exist_ok=True,mode=0o700)
        path=directory/'request.json'
        if path.exists() or path.is_symlink():
            if json.loads(regular(path))!=body:
                raise ValueError('local author request changed')
        else:
            write_new(path,body)
        return directory

    def response(self, directory):
        directory=Path(directory)
        if (directory/'rejected.json').exists():
            raise ValueError('rejected author response retained; cannot resubmit this identity')
        try:
            request=json.loads(regular(directory/'request.json'))
            if directory.is_symlink() or digest(request)!=directory.name or request['identity']!=self.identity:
                raise ValueError('local author request identity changed')
            path=directory/'response.json'
            if not path.exists() and not path.is_symlink():
                return None
            value=json.loads(regular(path))
            required={'request','author','notes','guidance'}
            if set(value)!=required or value['request']!=directory.name or value['author']!='codex-local':
                raise ValueError('local author response identity or fields invalid')
            if not isinstance(value['notes'],str) or not value['notes'].strip():
                raise ValueError('local author notes required')
            validate_tree({FILE:value['guidance']})
            marker=directory/'accepted.json'
            receipt=dict(request=directory.name,response=digest(value),author='codex-local')
            if marker.exists() or marker.is_symlink():
                if json.loads(regular(marker))!=receipt:
                    raise ValueError('accepted local author response changed')
            else:
                write_new(marker,receipt)
            return value
        except (ValueError,KeyError,TypeError,OSError) as error:
            write_new(directory/'rejected.json',dict(status='rejected',error=str(error)))
            raise ValueError(str(error)) from None
