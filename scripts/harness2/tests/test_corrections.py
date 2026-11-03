from pathlib import Path
import json
import sys
import unittest

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from cases import check_object, evaluate, expected


class Corrections(unittest.TestCase):
    def test_read_only_review_collection_binds_frozen_request_and_keeps_payment_failure(self):
        import tempfile
        from protocol import digest
        from review import GUIDANCE_V1 as GUIDANCE
        from review_collection import saved_opinion
        packet=dict(session='s',revision=8,sources=[dict(text='fact')],candidate='fact')
        identity=dict(binary='frozen',settings=dict(driver='frozen-driver'))
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory).resolve();output=root/'workers/semantic'/digest(packet)
            output.mkdir(parents=True)
            result=dict(status='failed',error='402 Insufficient Balance',usage=None,
                        text=json.dumps(dict(verdict='supported',reason='unsupported claim',findings=[])))
            manifest=dict(role='review',output=str(output),mission=GUIDANCE+'\n'+json.dumps(packet,ensure_ascii=False))
            execution=digest(dict(binary=identity['binary'],settings=identity['settings'],manifest=manifest))
            (output/'result.json').write_text(json.dumps(result))
            (output/'complete.json').write_text(json.dumps(dict(identity=execution,result=digest(result))))
            (output/'worker.json').write_text('{"exit_code":0}')
            row=saved_opinion(root,packet,identity)
            self.assertEqual(row['status'],'technical_failure')
            self.assertEqual(row['error'],'402 Insufficient Balance')
            self.assertIsNone(row['usage'])
            with self.assertRaises(ValueError):
                saved_opinion(root,packet,dict(identity,settings=dict(driver='changed')))
            (output/'result.json').write_text('{}')
            with self.assertRaises(ValueError):saved_opinion(root,packet,identity)

    def test_unsupported_opinion_can_complete_review_coverage_without_adoption(self):
        from review import packet_coverage,complete_coverage
        from protocol import digest
        packet=dict(session='s',revision=4,sources=[dict(text='FALCON-17')],candidate='FALCON-71')
        audit=dict(verdict='unsupported',reason='changed identifier',findings=[dict(kind='change',source_index=0,source_quote='FALCON-17',candidate_quote='FALCON-71',explanation='different ticket')])
        reviews=[dict(packet=digest(packet),status='opinion',audit=audit)]
        self.assertTrue(packet_coverage(reviews,[packet]))
        self.assertFalse(complete_coverage(reviews,[packet]))
        self.assertFalse(packet_coverage(reviews*2,[packet]))

    def test_unknown_execution_safety_blocks_all_following_model_roles(self):
        import tempfile
        from unittest.mock import patch
        from protocol import Worker
        for safety in (None, True):
            with tempfile.TemporaryDirectory() as directory:
                root=Path(directory)
                binary=root/'worker'
                binary.write_bytes(b'frozen')
                worker=Worker(binary,isolated=False)
                with patch.object(worker,'_run',return_value=dict(status='failed',safety_failure=safety)) as call:
                    with self.assertRaises(RuntimeError):
                        worker.run(root/'first','execute')
                    for role in ('execute','improve','review','probe'):
                        with self.assertRaises(RuntimeError):
                            worker.run(root/role,role)
                    self.assertEqual(call.call_count,1)
                    self.assertFalse((root/'review').exists())
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            binary=root/'worker'
            binary.write_bytes(b'frozen')
            worker=Worker(binary,isolated=False)
            with patch.object(worker,'_run',return_value=dict(status='failed',safety_failure=False)) as call:
                worker.run(root/'first','execute')
                worker.run(root/'second','review')
                self.assertEqual(call.call_count,2)

    def test_missing_execution_result_also_blocks_later_model_calls(self):
        import tempfile
        from unittest.mock import patch
        from protocol import Worker
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);binary=root/'worker';binary.write_bytes(b'frozen')
            worker=Worker(binary,isolated=False)
            with patch.object(worker,'_run',side_effect=RuntimeError('worker lost')) as call:
                with self.assertRaises(RuntimeError):worker.run(root/'first','execute')
                with self.assertRaises(RuntimeError):worker.run(root/'second','review')
                self.assertEqual(call.call_count,1)

    def test_missing_is_not_explicit_null(self):
        self.assertTrue(check_object({'evidence':None},{'evidence':None})['passed'])
        self.assertFalse(check_object({},{'evidence':None})['passed'])

    def test_resuming_discovery_requires_safe_complete_original_results(self):
        import tempfile
        from campaign import require_safe_history
        from protocol import digest
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            require_safe_history(root)
            output=root/'executions/attempt'
            output.mkdir(parents=True)
            with self.assertRaises(RuntimeError):
                require_safe_history(root)
            for safety in (None,True,False):
                result=dict(status='failed',safety_failure=safety)
                (output/'result.json').write_text(json.dumps(result))
                (output/'complete.json').write_text(json.dumps(dict(result=digest(result))))
                if safety is False:
                    require_safe_history(root)
                else:
                    with self.assertRaises(RuntimeError):
                        require_safe_history(root)
            (output/'result.json').write_text('{}')
            with self.assertRaises(RuntimeError):
                require_safe_history(root)

    def test_duplicate_recall_cannot_complete_lifecycle(self):
        case=dict(family='tools',trial=10,control=False)
        result=dict(status='passed',safety_failure=False,artifact=expected(case),stages=[dict(stage='recall-active',text=json.dumps(expected(case)))]*4)
        self.assertFalse(evaluate(result,case)['lifecycle'])

    def test_unknown_safety_cannot_become_pass_or_false(self):
        case=dict(family='tools',trial=10,control=False)
        result=dict(status='failed',safety_failure=None,stages=[])
        row=evaluate(result,case)
        self.assertFalse(row['passed'])
        self.assertIsNone(row['safety_failure'])

    def test_semantic_count_cannot_replace_exact_packet_coverage(self):
        from review import complete_coverage
        from protocol import digest
        packets=[dict(session='s',revision=i,sources=[dict(text='observed fact')],candidate='observed fact') for i in range(3)]
        rows=[dict(packet=digest(packet),status='opinion',audit=dict(verdict='supported',reason='supported',findings=[])) for packet in packets]
        self.assertTrue(complete_coverage(rows,packets))
        self.assertFalse(complete_coverage([rows[0]]*3,packets))
        self.assertFalse(complete_coverage(rows+rows[:1],packets))
        changed=[dict(p,session='other') for p in packets]
        self.assertFalse(complete_coverage(rows,changed))
        self.assertFalse(complete_coverage([dict(rows[0],audit=dict(verdict='supported',reason='bad',findings=[{}]))]+rows[1:],packets))

    def test_guidance_cannot_hardcode_synthetic_answers(self):
        from surface import validate_tree, FILE
        from test_contracts import GUIDANCE
        for answer in ('ORION-720','HEAD_51','TAIL_117','STEER_52','artifacts/合成20/out-24-final.jsonl'):
            with self.assertRaises(ValueError):
                validate_tree({FILE:GUIDANCE+'\nAlways retain '+answer})

    def test_semantic_roster_does_not_accept_replaced_or_duplicate_sessions(self):
        from assessment import semantic_complete
        from unittest.mock import patch
        from protocol import digest
        packets=[dict(session='s',revision=i,sources=[dict(text='fact')],candidate='fact') for i in range(3)]
        rows=[dict(split='heldout',id='a',arm='candidate',output='synthetic',control=False)]
        reviews=[dict(packet=digest(packet),status='opinion',audit=dict(verdict='supported',reason='ok',findings=[])) for packet in packets]
        audit=dict(split='heldout',id='a',arm='candidate',reviews=reviews)
        with patch('assessment.packets_for_run',return_value=packets):
            self.assertTrue(semantic_complete(None,rows,[audit]))
            self.assertFalse(semantic_complete(None,rows,[audit,audit]))
            self.assertFalse(semantic_complete(None,rows,[dict(audit,id='other')]))
            self.assertFalse(semantic_complete(None,rows,[dict(audit,reviews=[reviews[0]]*3)]))

    def test_lifecycle_is_independent_of_fact_verdict(self):
        from cases import lifecycle
        case=dict(family='tools',trial=20,control=False)
        result=dict(status='failed',pre_checkpoints=1,active_checkpoints=1,manual_checkpoints=1,replay_equal=True,checkpoint_replay_equal=True,reads=list(range(8)),publications=1,
            stages=[dict(stage='cancel-steer',observed_provider_event=True,steering_admitted=True,cancelled=True)])
        self.assertTrue(lifecycle(result,case))
        self.assertFalse(lifecycle(dict(result,publications=2),case))
        self.assertFalse(lifecycle(dict(result,checkpoint_replay_equal=False),case))

    def test_failed_exploration_reviews_existing_checkpoints_without_claiming_completion(self):
        from assessment import semantic_complete
        from unittest.mock import patch
        row=dict(split='discovery',id='tools/inv1',arm='inv1',output='synthetic',
                 control=False,passed=False,lifecycle=False)
        audit=dict(split='discovery',id=row['id'],arm='inv1',reviews=[])
        with patch('assessment.packets_for_run',return_value=[]):
            self.assertTrue(semantic_complete(None,[row],[audit]))
            self.assertFalse(semantic_complete(None,[row],[]))
            for split in ('heldout','legacy','unified-training'):
                self.assertFalse(semantic_complete(None,[dict(row,split=split)],
                    [dict(audit,split=split)]))
        self.assertFalse(row['passed'])
        self.assertFalse(row['lifecycle'])

    def test_composition_labels_failed_selection_without_replacing_it(self):
        import tempfile
        from types import SimpleNamespace
        from campaign import composition_context
        from test_contracts import GUIDANCE
        from surface import FILE
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            method=root/'method'
            for family in ('requirements','tools','images'):
                task=method/family
                (task/'harnesses/agg1').mkdir(parents=True)
                (task/'harnesses/agg1'/FILE).write_text(GUIDANCE)
                record=dict(domain=family,base=[dict(run_dir='base',run_id='run/'+family+'/base1')],modes=dict(agg=dict(finals=['agg1'])),selections=dict(agg=dict(ok=family!='tools',chosen_label='agg1',chosen_run_dir='chosen',error='no artifact' if family=='tools' else '')))
                (task/'summary.json').write_text(json.dumps(record))
            calls=[]
            def trace(path,**kwargs):
                calls.append(str(path))
                return 'synthetic observed evidence'
            adapter=SimpleNamespace(spec=SimpleNamespace(runs_root=method),root=root,trajectory=trace)
            context=composition_context(adapter,root,GUIDANCE)
            sources=json.loads((context/'sources.json').read_text())['sources']
            self.assertEqual(next(s for s in sources if s['domain']=='tools')['kind'],'baseline_reference_selection_failed')
            self.assertIn('UNSUCCESSFUL FINAL',(context/'trajectory-2.md').read_text())
            composition_context(adapter,root,GUIDANCE)
            self.assertEqual(len(calls),8)
            with self.assertRaises(ValueError):
                composition_context(adapter,root,GUIDANCE+'changed')

    def test_running_worker_cannot_use_changed_driver(self):
        import tempfile
        from unittest.mock import patch
        from protocol import Worker
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            binary=root/'binary'
            binary.write_bytes(b'synthetic')
            with patch('protocol.driver_hash',return_value='v1'):
                worker=Worker(binary,isolated=False)
            with patch('protocol.driver_hash',return_value='v2'):
                with self.assertRaises(RuntimeError):
                    worker.run(root/'run','baseline')
            self.assertFalse((root/'run').exists())

    def test_changed_export_cannot_reuse_original_result_marker(self):
        import tempfile
        import hashlib
        from protocol import digest
        from assessment import check_run
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            evidence=root/'observations'
            evidence.mkdir()
            file=evidence/'maintenance-views.json'
            file.write_text(json.dumps(dict(views=[])))
            result=dict(status='failed',evidence_root=str(evidence),terminal_audit=dict(maintenance_sha256=hashlib.sha256(file.read_bytes()).hexdigest()))
            (root/'result.json').write_text(json.dumps(result))
            (root/'complete.json').write_text(json.dumps(dict(result=digest(result))))
            (root/'worker.json').write_text(json.dumps(dict(exit_code=0)))
            case=dict(family='tools',trial=20,control=True)
            check_run(root,case)
            file.write_text(json.dumps(dict(views=[dict(structure_valid=True)])))
            with self.assertRaises(ValueError):
                check_run(root,case)

    def test_probe_evidence_requires_actual_delivered_id_not_read_count(self):
        from evidence import verify_evidence
        messages=[dict(role='assistant',tool_calls=[dict(id='c',function=dict(name='harness2_evidence',arguments=json.dumps(dict(id='guidance'))))]),dict(role='tool',tool_call_id='c',content=json.dumps(dict(id='guidance',text='source')))]
        self.assertEqual(verify_evidence(messages,dict(guidance='source',long_evidence='other')),{'guidance'})

    def test_report_cannot_adopt_when_review_counts_hide_missing_packets(self):
        import tempfile
        from unittest.mock import patch
        from protocol import digest
        from reporting import report
        from cases import training,heldout,legacy
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            protocol=dict(training=training(),heldout=heldout(),legacy=legacy())
            (root/'protocol.json').write_text(json.dumps(protocol))
            base=root/'workers/baseline-export'
            base.mkdir(parents=True)
            (base/'result.json').write_text(json.dumps(dict(guidance='baseline directive')))
            final=root/'frozen-candidate'
            final.mkdir()
            (final/'summary_guidance.md').write_text('candidate directive')
            identity=dict(guidance=digest('candidate directive'),baseline=digest('baseline directive'))
            (final/'identity.json').write_text(json.dumps(identity))
            rows=[]
            for split,roster in (('heldout',heldout()),('legacy',legacy())):
                for id,case in roster.items():
                    for arm in ('base','candidate'):
                        output=root/'workers'/split/id/arm
                        output.mkdir(parents=True)
                        (output/'complete.json').write_text('{}')
                        rows.append(dict(split=split,id=id,arm=arm,output=str(output),family=case['family'],control=case['control'],passed=arm=='candidate',facts_passed=arm=='candidate',lifecycle=True,structure_verified=True,safety_failure=False))
            unified=[dict(split='unified-training',id=id,arm='candidate',output=str(root/'workers/unified-training'/id),control=False) for id in training()]
            from cohort import discovery
            discovered=[dict(split=item['split'],id=item['id'],arm=item['arm'],output=str(item['output']),control=False) for item in discovery(root,protocol)]
            for row in unified+discovered:
                output=Path(row['output'])
                output.mkdir(parents=True)
                (output/'complete.json').write_text('{}')
            def packets(output):
                return [dict(session=str(output),revision=i,sources=[dict(text='source fact')],candidate='source fact') for i in range(3)]
            audits=[dict(split=row['split'],id=row['id'],arm=row['arm'],reviews=[dict(packet=digest(p),status='opinion',audit=dict(verdict='supported',reason='source supports',findings=[])) for p in packets(row['output'])]) for row in rows+unified+discovered]
            file=root/'semantic-review.json'
            file.write_text(json.dumps(dict(rows=audits)))
            with patch('assessment.recompute',return_value=dict(rows=rows)),patch('assessment.check_run',return_value=dict(passed=True)),patch('assessment.packets_for_run',side_effect=packets):
                self.assertEqual(report(root)['verdict'],'ELIGIBLE_FOR_REVIEW')
                (final/'summary_guidance.md').write_text('baseline directive')
                (final/'identity.json').write_text(json.dumps(dict(guidance=digest('baseline directive'),baseline=digest('baseline directive'))))
                self.assertEqual(report(root)['verdict'],'KEEP_BASE')
                (final/'summary_guidance.md').write_text('candidate directive')
                (final/'identity.json').write_text(json.dumps(identity))
                (final/'summary_guidance.md').write_text('tampered directive')
                self.assertEqual(report(root)['verdict'],'KEEP_BASE')
                (final/'summary_guidance.md').write_text('candidate directive')
                audits[0]['reviews']=[audits[0]['reviews'][0]]*3
                file.write_text(json.dumps(dict(rows=audits)))
                self.assertEqual(report(root)['verdict'],'KEEP_BASE')

    def test_cancellation_timing_alone_cannot_qualify_as_summary_improvement(self):
        from reporting import acceptance
        from cases import heldout,legacy
        rows=[]
        for split,roster in (('heldout',heldout()),('legacy',legacy())):
            for id,case in roster.items():
                for arm in ('base','candidate'):
                    rows.append(dict(split=split,id=id,arm=arm,family=case['family'],control=False,passed=arm=='candidate',facts_passed=True,lifecycle=arm=='candidate',structure_verified=True,safety_failure=False))
        self.assertEqual(acceptance(rows,True,True)['verdict'],'KEEP_BASE')
        for row in rows:
            if row['arm']=='base':
                row['facts_passed']=False
        self.assertEqual(acceptance(rows,True,True)['verdict'],'KEEP_BASE')
        for row in rows:
            row['lifecycle']=True
        self.assertEqual(acceptance(rows,True,True)['verdict'],'ELIGIBLE_FOR_REVIEW')

    def test_failed_legacy_result_without_evidence_root_cannot_crash_review(self):
        import tempfile
        from review import audit_run
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            (root/'result.json').write_text(json.dumps(dict(status='failed',error='synthetic incomplete result')))
            self.assertEqual(audit_run(None,root/'reviews',root),[dict(status='not_available')])

    def test_wrong_initial_image_observation_cannot_hide_behind_correct_final_json(self):
        case=dict(family='images',trial=20,control=False)
        facts=expected(case)
        pre=dict(facts,head=None,tail=None,steer_tag=None)
        stages=[dict(stage='recall-pre-tools',text=json.dumps(pre))]+[dict(stage=name,text=json.dumps(facts)) for name in ('recall-active','recall-manual','recall-cold')]
        stages+=[dict(stage='cancel-steer',observed_provider_event=True,steering_admitted=True,cancelled=True),dict(stage='evidence',image_color_correct=False)]
        result=dict(status='failed',artifact=facts,safety_failure=False,pre_checkpoints=1,active_checkpoints=1,manual_checkpoints=1,replay_equal=True,checkpoint_replay_equal=True,reads=list(range(8)),publications=1,stages=stages)
        self.assertFalse(evaluate(result,case)['passed'])

    def test_transport_failure_is_not_cancelled_without_actual_request_evidence(self):
        import tempfile
        from assessment import cancellation_observed
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            path=root/'request-0001-end.json'
            path.write_text(json.dumps(dict(stage='cancel-steer',cancelled=False,outcome=dict(error_kind='Transport'))))
            self.assertFalse(cancellation_observed(root))
            path.write_text(json.dumps(dict(stage='cancel-steer',cancelled=True,outcome=dict(finish='Cancelled'))))
            self.assertTrue(cancellation_observed(root))

    def test_correct_artifact_does_not_authorize_publication_before_requested_stage(self):
        import tempfile
        from assessment import publication_authorized
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            path=root/'request-0001-end.json'
            frame=dict(stage='active-tools',outcome=dict(tool_calls=[dict(name='phase9_publish')]))
            path.write_text(json.dumps(frame))
            self.assertFalse(publication_authorized(root))
            frame['stage']='publish'
            path.write_text(json.dumps(frame))
            self.assertTrue(publication_authorized(root))

    def test_original_text_controls_cannot_supply_summary_benefit_domains(self):
        from reporting import acceptance
        from cases import heldout,legacy
        rows=[]
        for split,roster in (('heldout',heldout()),('legacy',legacy())):
            for id,case in roster.items():
                for arm in ('base','candidate'):
                    passed=arm=='candidate' or not case['control']
                    rows.append(dict(split=split,id=id,arm=arm,family=case['family'],control=case['control'],passed=passed,facts_passed=passed,lifecycle=True,structure_verified=True,safety_failure=False))
        self.assertEqual(acceptance(rows,True,True)['verdict'],'KEEP_BASE')

    def test_orphan_terminal_proof_never_claims_success_and_rejects_incomplete_evidence(self):
        import tempfile,hashlib
        from continuation import record_interruption,interrupted_result
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);evidence=root/'observations';evidence.mkdir()
            view=evidence/'maintenance-views.json';view.write_text('{"views":[]}')
            start=evidence/'request-0000-start.json';start.write_text('{}')
            end=evidence/'request-0000-end.json';end.write_text('{}')
            result=dict(status='failed',safety_failure=False,evidence_root=str(evidence),terminal_audit=dict(verified=True,root_lease_reacquired=True,projection_replay_verified=True,requests_ended=1,maintenance_sha256=hashlib.sha256(view.read_bytes()).hexdigest()))
            (root/'result.json').write_text(json.dumps(result))
            record_interruption(root)
            row=interrupted_result(root)
            from assessment import check_run
            self.assertEqual(check_run(root,dict(control=False)),row)
            self.assertFalse(row['passed']);self.assertFalse(row['lifecycle'])
            self.assertFalse(row['worker_exit_verified']);self.assertIs(row['safety_failure'],False)
            end.unlink()
            with self.assertRaises((ValueError,OSError)):
                interrupted_result(root)
            end.write_text('{}')
            result['safety_failure']=None
            (root/'result.json').write_text(json.dumps(result))
            with self.assertRaises(ValueError):
                interrupted_result(root)
