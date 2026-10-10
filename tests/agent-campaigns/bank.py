#!/usr/bin/env python3
"""Verify a frozen offline bank against its public compact digest. No model I/O."""
import argparse
import hashlib
import json
import re
from pathlib import Path, PurePosixPath


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def local_file(root, name):
    path = PurePosixPath(name)
    if path.is_absolute() or not path.parts or '..' in path.parts or '\\' in name:
        raise ValueError('artifact path escapes bank')
    current = root
    for part in path.parts:
        current = current / part
        if current.is_symlink():
            raise ValueError('bank artifact must not be a symlink')
    if not current.is_file():
        raise ValueError('missing bank artifact: ' + name)
    return current


def read_json(path):
    def reject_constant(value):
        raise ValueError('non-finite JSON value: ' + value)

    return json.loads(path.read_text(encoding='utf-8'), parse_constant=reject_constant)


def check_files(root, inventory):
    if not inventory:
        raise ValueError('empty artifact inventory')
    for name, expected in inventory.items():
        if sha256(local_file(root, name)) != expected:
            raise ValueError('bank artifact changed: ' + name)


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False)


def observe_regression(root, receipt):
    arms = read_json(local_file(root, receipt['cases_path']))['arms']
    outcomes = []
    for arm in ['base', 'reference', 'decoy']:
        row = arms[arm]
        if type(row['compile']['exit_code']) is not int or row['compile']['exit_code'] != 0:
            raise ValueError('regression compile failure is not behavioral evidence')
        process = row['process']
        code = process['exit_code']
        text = process['stdout'] + process['stderr']
        count = re.search(r'running ([0-9]+) tests?', text)
        if type(code) is not int or code not in [0, 101] or count is None:
            raise ValueError('missing regression execution evidence')
        if int(count.group(1)) == 0:
            outcomes.append('empty')
        elif code == 0 and 'test result: ok.' in text:
            outcomes.append('green')
        elif code == 101 and 'panicked at' in text and 'test result: FAILED.' in text:
            outcomes.append('red')
        else:
            raise ValueError('regression did not finish normally')
    matches = outcomes == ['red', 'green', 'red']
    return matches, not matches


def observe(root, receipt):
    if receipt['kind'] == 'regression-tests':
        return observe_regression(root, receipt)
    if type(receipt['compile_exit']) is not int or receipt['compile_exit'] != 0:
        raise ValueError('compile failure is not a behavioral red')
    code = receipt['run_exit']
    if type(code) is not int:
        raise ValueError('missing/timeout execution is not qualification')
    if receipt['kind'] == 'rust-test':
        if code not in [0, 101]:
            raise ValueError('abnormal Rust-test termination is not behavioral qualification')
        stdout = local_file(root, receipt['stdout_path']).read_text(encoding='utf-8')
        stderr = local_file(root, receipt['stderr_path']).read_text(encoding='utf-8')
        green = (code == 0 and 'test external_oracle ... ok' in stdout
                 and '1 passed; 0 failed' in stdout)
        red = (code == 101 and 'ORACLE_' in stderr and 'panicked at' in stderr
               and 'test external_oracle ... FAILED' in stdout
               and 'test result: FAILED. 0 passed; 1 failed' in stdout)
        return green, red
    if receipt['kind'] != 'external-cases':
        raise ValueError('unknown observation format')
    if code != 0:
        raise ValueError('abnormal candidate termination is not a completed case observation')
    cases = read_json(local_file(root, receipt['cases_path']))['cases']
    names = [case['name'] for case in cases]
    if not cases or len(set(names)) != len(names):
        raise ValueError('empty or duplicated oracle cases')
    matches = [canonical(case['actual']) == canonical(case['expected']) for case in cases]
    return code == 0 and all(matches), not all(matches)


def admitted(observations):
    return (observations.get('base') == (False, True)
            and observations.get('reference') == (True, False)
            and observations.get('decoy') == (False, True)
            and observations.get('attack') == (False, True))


def check_roster(tasks):
    ids = [task['id'] for task in tasks]
    if len(set(ids)) != len(ids):
        raise ValueError('duplicate bank task')
    splits = [task['split'] for task in tasks]
    if splits.count('discovery') != 10 or splits.count('holdout') < 20:
        raise ValueError('bank requires exactly 10 discovery and at least 20 holdout tasks')
    if any(split not in ['discovery', 'holdout'] for split in splits):
        raise ValueError('unknown bank split')


def verify(root, compact):
    public = read_json(compact)
    manifest_path = local_file(root, 'manifest.json')
    if sha256(manifest_path) != public['manifest_sha256']:
        raise ValueError('private bank differs from public frozen identity')
    manifest = read_json(manifest_path)
    check_roster(manifest['tasks'])
    check_files(root, manifest['artifacts'])
    public_rows = public['tasks']
    if [row['id'] for row in public_rows] != [row['id'] for row in manifest['tasks']]:
        raise ValueError('public/private roster differs')
    results = []
    for task, card in zip(manifest['tasks'], public_rows):
        if task['split'] != card['split']:
            raise ValueError('public/private split differs')
        observations = {}
        for arm, receipt in task['receipts'].items():
            for key in ['stdout_path', 'stderr_path', 'cases_path']:
                if key in receipt and receipt[key] not in manifest['artifacts']:
                    raise ValueError('observation omitted from frozen inventory')
            observations[arm] = observe(root, receipt)
        for arm, expected in task.get('required_observations', {}).items():
            if observations.get(arm) != tuple(expected):
                raise ValueError('required observation failed: ' + task['id'] + '/' + arm)
        results.append({'id': task['id'], 'qualified': admitted(observations)})
    qualified = sum(row['qualified'] for row in results)
    if qualified != len(results) or public['qualified'] != qualified:
        raise ValueError('unqualified task or compact count mismatch')
    return {'version': manifest['version'], 'qualified': qualified, 'tasks': results,
            'paid_calls': 0, 'evidence': 'trusted-offline-qualification-only',
            'capability_improvement': 'unverified'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bank', type=Path, required=True)
    parser.add_argument('--compact', type=Path, required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(verify(args.bank.resolve(), args.compact), ensure_ascii=False))
    except (ValueError, KeyError, TypeError, OSError) as error:
        parser.exit(1, 'BANK STOP: ' + str(error) + '\n')


if __name__ == '__main__':
    main()
