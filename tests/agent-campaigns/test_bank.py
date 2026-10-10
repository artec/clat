"""Bank admission is derived from retained observations, never a qualified flag."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('bank', Path(__file__).with_name('bank.py'))
bank = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bank)


class BankContracts(unittest.TestCase):
    def fixture(self, root):
        (root / 'cases.json').write_text(json.dumps({'cases': [
            {'name': 'normal', 'actual': 2, 'expected': 2},
            {'name': 'boundary', 'actual': 4, 'expected': 4}]}))
        return {'kind': 'external-cases', 'compile_exit': 0, 'run_exit': 0,
                'cases_path': 'cases.json'}

    def test_external_observations_override_self_report(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            (root / 'cases.json').write_text(json.dumps({'passed': True, 'cases': [
                {'name': 'boundary', 'actual': 0, 'expected': 4, 'passed': True}]}))
            self.assertEqual(bank.observe(root, receipt), (False, True))

    def test_compile_failure_cannot_be_behavioral_red(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            receipt['compile_exit'] = 1
            with self.assertRaises(ValueError):
                bank.observe(root, receipt)

    def test_missing_compile_receipt_cannot_be_zero(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            receipt['compile_exit'] = False
            with self.assertRaises(ValueError):
                bank.observe(root, receipt)

    def test_empty_or_duplicate_cases_are_invalid(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            for cases in [[], [{'name': 'x', 'actual': 1, 'expected': 1}] * 2]:
                (root / 'cases.json').write_text(json.dumps({'cases': cases}))
                with self.assertRaises(ValueError):
                    bank.observe(root, receipt)

    def test_timeout_cannot_qualify_as_red(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            receipt['run_exit'] = None
            with self.assertRaises(ValueError):
                bank.observe(root, receipt)

    def test_timeout_and_signal_are_not_normal_behavioral_reds(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            external = self.fixture(root)
            (root / 'cases.json').write_text(json.dumps({'cases': [
                {'name': 'boundary', 'actual': 0, 'expected': 4}]}))
            (root / 'out').write_text('')
            (root / 'err').write_text('ORACLE_boundary panic')
            historical = {'kind': 'rust-test', 'compile_exit': 0,
                          'stdout_path': 'out', 'stderr_path': 'err'}
            for receipt in [external, historical]:
                for code in [124, 125, -9]:
                    with self.subTest(kind=receipt['kind'], code=code):
                        receipt['run_exit'] = code
                        with self.assertRaises(ValueError):
                            bank.observe(root, receipt)

    def test_paths_cannot_escape_or_follow_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'ok').write_text('data')
            (root / 'link').symlink_to(root / 'ok')
            for name in ['../outside', '/absolute', 'link', 'a\\b']:
                with self.assertRaises(ValueError):
                    bank.local_file(root, name)

    def test_file_hash_drift_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'file').write_text('before')
            inventory = {'file': bank.sha256(root / 'file')}
            (root / 'file').write_text('after')
            with self.assertRaises(ValueError):
                bank.check_files(root, inventory)

    def test_test_output_without_oracle_completion_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'out').write_text('running 1 test\n')
            (root / 'err').write_text('')
            receipt = {'kind': 'rust-test', 'compile_exit': 0, 'run_exit': 0,
                       'stdout_path': 'out', 'stderr_path': 'err'}
            self.assertEqual(bank.observe(root, receipt), (False, False))

    def test_a_panic_label_without_completed_test_is_not_behavioral_red(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'out').write_text('running 1 test\n')
            (root / 'err').write_text('panicked at ORACLE_boundary')
            receipt = {'kind': 'rust-test', 'compile_exit': 0, 'run_exit': 101,
                       'stdout_path': 'out', 'stderr_path': 'err'}
            self.assertEqual(bank.observe(root, receipt), (False, False))

    def test_three_arms_without_extra_attack_are_not_admitted(self):
        observations = {'base': (False, True), 'reference': (True, False),
                        'decoy': (False, True)}
        self.assertFalse(bank.admitted(observations))
        observations['attack'] = (False, True)
        self.assertTrue(bank.admitted(observations))

    def test_duplicate_roster_or_wrong_split_is_rejected(self):
        tasks = [{'id': f'D{i}', 'split': 'discovery'} for i in range(10)]
        tasks += [{'id': f'H{i}', 'split': 'holdout'} for i in range(20)]
        bank.check_roster(tasks)
        changed = copy.deepcopy(tasks)
        changed[-1]['id'] = changed[0]['id']
        with self.assertRaises(ValueError):
            bank.check_roster(changed)
        changed = copy.deepcopy(tasks)
        changed[-1]['split'] = 'discovery'
        with self.assertRaises(ValueError):
            bank.check_roster(changed)

    def complete_bank(self, root):
        (root / 'green.json').write_text(json.dumps({'cases': [
            {'name': 'normal', 'actual': 2, 'expected': 2}]}))
        (root / 'red.json').write_text(json.dumps({'cases': [
            {'name': 'boundary', 'actual': 0, 'expected': 4}]}))
        green = {'kind': 'external-cases', 'compile_exit': 0,
                 'run_exit': 0, 'cases_path': 'green.json'}
        red = {**green, 'cases_path': 'red.json'}
        tasks = [{'id': f'T{i}', 'split': 'discovery' if i < 10 else 'holdout',
                  'receipts': {'base': red, 'reference': green, 'decoy': red, 'attack': red}}
                 for i in range(30)]
        manifest = {'version': 'test-only', 'tasks': tasks, 'artifacts': {
            name: bank.sha256(root / name) for name in ['green.json', 'red.json']}}
        (root / 'manifest.json').write_text(json.dumps(manifest))
        compact = {'manifest_sha256': bank.sha256(root / 'manifest.json'),
                   'qualified': 30, 'tasks': [{'id': task['id'], 'split': task['split']}
                                             for task in tasks]}
        (root / 'compact.json').write_text(json.dumps(compact))
        return manifest, compact

    def test_public_digest_binds_private_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, _ = self.complete_bank(root)
            self.assertEqual(bank.verify(root, root / 'compact.json')['qualified'], 30)
            manifest['tasks'][0]['receipts']['base']['run_exit'] = 101
            (root / 'manifest.json').write_text(json.dumps(manifest))
            with self.assertRaises(ValueError):
                bank.verify(root, root / 'compact.json')

    def test_known_qualified_flag_cannot_hide_a_green_decoy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, compact = self.complete_bank(root)
            manifest['tasks'][0]['qualified'] = True
            manifest['tasks'][0]['receipts']['decoy'] = manifest['tasks'][0]['receipts']['reference']
            (root / 'manifest.json').write_text(json.dumps(manifest))
            compact['manifest_sha256'] = bank.sha256(root / 'manifest.json')
            (root / 'compact.json').write_text(json.dumps(compact))
            with self.assertRaises(ValueError):
                bank.verify(root, root / 'compact.json')

    def test_unfrozen_observation_file_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, compact = self.complete_bank(root)
            del manifest['artifacts']['red.json']
            (root / 'manifest.json').write_text(json.dumps(manifest))
            compact['manifest_sha256'] = bank.sha256(root / 'manifest.json')
            (root / 'compact.json').write_text(json.dumps(compact))
            with self.assertRaises(ValueError):
                bank.verify(root, root / 'compact.json')

    def test_required_regression_discrimination_cannot_be_ignored(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, compact = self.complete_bank(root)
            task = manifest['tasks'][0]
            task['receipts']['test_reference'] = task['receipts']['base']
            task['required_observations'] = {'test_reference': [True, False]}
            (root / 'manifest.json').write_text(json.dumps(manifest))
            compact['manifest_sha256'] = bank.sha256(root / 'manifest.json')
            (root / 'compact.json').write_text(json.dumps(compact))
            with self.assertRaises(ValueError):
                bank.verify(root, root / 'compact.json')

    def test_boolean_is_not_an_integer_observation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            (root / 'cases.json').write_text(json.dumps({'cases': [
                {'name': 'numeric', 'actual': True, 'expected': 1}]}))
            self.assertEqual(bank.observe(root, receipt), (False, True))

    def test_regression_tests_must_discriminate_three_implementations(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            arms = {}
            for arm, code in [('base', 101), ('reference', 0), ('decoy', 101)]:
                text = ('running 1 test\ntest result: ok. 1 passed;' if code == 0
                        else 'running 1 test\npanicked at ORACLE_state\ntest result: FAILED. 0 passed;')
                arms[arm] = {'compile': {'exit_code': 0},
                             'process': {'exit_code': code, 'stdout': text, 'stderr': ''}}
            path = root / 'tests.json'
            path.write_text(json.dumps({'arms': arms}))
            receipt = {'kind': 'regression-tests', 'cases_path': 'tests.json'}
            self.assertEqual(bank.observe(root, receipt), (True, False))
            arms['base']['process'] = arms['reference']['process']
            path.write_text(json.dumps({'arms': arms}))
            self.assertEqual(bank.observe(root, receipt), (False, True))
            arms['reference']['compile']['exit_code'] = 1
            path.write_text(json.dumps({'arms': arms}))
            with self.assertRaises(ValueError):
                bank.observe(root, receipt)
if __name__ == '__main__':
    unittest.main()
