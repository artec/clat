"""Local authorship must not masquerade as paid API calls or bypass identity."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from test_contracts import GUIDANCE
from surface import FILE


class LocalAuthorContracts(unittest.TestCase):
    def test_unarmed_live_entry_reads_no_material_and_creates_no_directory(self):
        import os, subprocess
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);output=root/'must-not-exist'
            env=dict(os.environ);env.pop('CLAT_HARNESS2_ARM',None)
            result=subprocess.run([sys.executable,str(Path(__file__).resolve().parents[1]/'local_campaign.py'),
                'campaign','--source',str(root/'missing-source'),'--binary',str(root/'missing-worker'),
                '--output',str(output),'--seed-guidance',str(root/'missing-guide')],
                capture_output=True,text=True,env=env)
            self.assertEqual(result.returncode,2,result.stderr)
            self.assertIn('NOT_ARMED',result.stdout)
            self.assertFalse(output.exists())

    def test_new_roster_is_finite_and_cannot_reuse_old_holdout(self):
        from local_campaign import roster,record
        from cases import heldout
        rows=roster()
        self.assertEqual({case['trial'] for case in rows['training'].values()},{30})
        self.assertEqual({case['trial'] for case in rows['heldout'].values()},{31,32})
        self.assertEqual(len(rows['heldout']),18)
        self.assertFalse(set(rows['heldout']) & set(heldout()))
        protocol=record(GUIDANCE)
        self.assertEqual(protocol['planned_scenarios'],12+3+2*(18+12))
        self.assertIsNone(protocol['improvement_endpoint'])
        self.assertEqual(protocol['improver'],'codex-local')

    def exchange(self, root):
        from local_author import Exchange
        return Exchange(root/'author', dict(binary='frozen', protocol='round2'))

    def context(self, root):
        context=root/'context'
        (context/'harness').mkdir(parents=True)
        (context/'harness'/FILE).write_text(GUIDANCE)
        (context/'trajectory.md').write_text('Observed synthetic evidence only.')
        return context

    def respond(self, directory, **changes):
        value=dict(request=directory.name, author='codex-local', notes='NO_ENTRY', guidance=GUIDANCE)
        value.update(changes)
        (directory/'response.json').write_text(json.dumps(value))

    def test_response_is_bound_to_complete_context_and_execution_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);context=self.context(root);exchange=self.exchange(root)
            request=exchange.request(context,'propose','domain/inv1',GUIDANCE)
            self.assertIsNone(exchange.response(request))
            self.respond(request)
            self.assertEqual(exchange.response(request)['guidance'],GUIDANCE)
            (context/'trajectory.md').write_text('Different evidence.')
            self.assertNotEqual(exchange.request(context,'propose','domain/inv1',GUIDANCE),request)
            self.assertNotEqual(exchange.request(context,'different mission','domain/inv1',GUIDANCE),request)
            from local_author import Exchange
            other=Exchange(root/'author',dict(binary='changed',protocol='round2'))
            self.assertNotEqual(other.request(context,'propose','domain/inv1',GUIDANCE),request)

    def test_wrong_identity_or_answer_injection_is_retained_and_rejected(self):
        for change in (dict(request='stale'),dict(author='deepseek'),dict(guidance=GUIDANCE+'\nORION-730'),dict(path='../escape')):
            with tempfile.TemporaryDirectory() as directory:
                root=Path(directory);exchange=self.exchange(root)
                request=exchange.request(self.context(root),'propose','inv1',GUIDANCE)
                self.respond(request,**change)
                with self.assertRaises(ValueError):exchange.response(request)
                self.assertTrue((request/'rejected.json').is_file())
                self.assertTrue((request/'response.json').is_file())
                self.assertFalse((request/'edited').exists())
                (request/'response.json').write_text('{}')
                with self.assertRaises(ValueError):exchange.response(request)

    def test_answers_holdout_symlink_and_executable_are_not_author_inputs(self):
        for forbidden in ('answers.json','evaluation.json','heldout/trace.md','judged-result.txt'):
            with tempfile.TemporaryDirectory() as directory:
                root=Path(directory);context=self.context(root)
                path=context/forbidden;path.parent.mkdir(exist_ok=True);path.write_text('secret answer')
                with self.assertRaises(ValueError):self.exchange(root).request(context,'propose','inv1',GUIDANCE)
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);context=self.context(root)
            (context/'alias.md').symlink_to(context/'trajectory.md')
            with self.assertRaises(ValueError):self.exchange(root).request(context,'propose','inv1',GUIDANCE)
            (context/'alias.md').unlink()
            (context/'trajectory.md').chmod(0o700)
            with self.assertRaises(ValueError):self.exchange(root).request(context,'propose','inv1',GUIDANCE)

    def test_tampered_request_and_symlink_response_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);exchange=self.exchange(root)
            request=exchange.request(self.context(root),'propose','inv1',GUIDANCE)
            target=root/'outside.json';target.write_text('{}')
            (request/'response.json').symlink_to(target)
            with self.assertRaises(ValueError):exchange.response(request)
            self.assertEqual(target.read_text(),'{}')
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);exchange=self.exchange(root)
            request=exchange.request(self.context(root),'propose','inv1',GUIDANCE)
            (request/'request.json').write_text('{}')
            self.respond(request)
            with self.assertRaises(ValueError):exchange.response(request)

    def test_accepted_opinion_cannot_be_replaced_on_resume(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);exchange=self.exchange(root)
            request=exchange.request(self.context(root),'propose','inv1',GUIDANCE)
            self.respond(request)
            exchange.response(request)
            self.respond(request,notes='A different retrospective opinion.')
            with self.assertRaises(ValueError):exchange.response(request)


if __name__=='__main__':
    unittest.main()
