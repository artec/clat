from pathlib import Path
import json
import sys
import unittest

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from cases import expected, check_object, check_recall, heldout, legacy, training
from review import admit
from reporting import acceptance


class Judging(unittest.TestCase):
    def test_pre_tool_recall_requires_explicit_unknowns_not_invented_evidence(self):
        case=dict(family="tools",trial=10,control=False)
        facts=expected(case)
        for key in ("head","tail","steer_tag","tool_total","unconfirmed_count"):
            facts[key]=None
        self.assertTrue(check_recall(facts,case,"recall-pre-tools")["passed"])
        invented=dict(facts,head="HEAD_41")
        self.assertFalse(check_recall(invented,case,"recall-pre-tools")["passed"])
        self.assertFalse(check_recall(dict(facts,latest_color="blue"),case,"recall-pre-tools")["passed"])

    def test_report_keeps_partial_and_interrupted_usage_unknown(self):
        import tempfile
        from reporting import report
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            (root/"request-1-end.json").write_text(json.dumps(dict(usage=dict(input=7,output=3,cached=None),elapsed_ms=20)))
            (root/"request-2-end.json").write_text(json.dumps(dict(usage=dict(input=5,output=2,cached=0,reasoning=1),elapsed_ms=30)))
            (root/"request-3-start.json").write_text(json.dumps(dict(stage="interrupted")))
            result=report(root)
            self.assertEqual(result["usage_fields"]["cached"],dict(known_sum=0,observed_requests=1,unknown_requests=2))
            self.assertEqual(result["usage_fields"]["reasoning"],dict(known_sum=1,observed_requests=1,unknown_requests=2))
            self.assertEqual(result["usage_fields"]["input"],dict(known_sum=12,observed_requests=2,unknown_requests=1))
            self.assertIsNone(result["token_totals"]["input"])
            self.assertEqual(result["requests_with_unknown_usage"],1)
            self.assertEqual(result["known_request_elapsed_ms"],50)
            self.assertEqual(result["requests_with_unknown_elapsed"],1)
            self.assertEqual(result["verdict"],"KEEP_BASE")

    def test_full_evidence_intake_rejects_pressure_pruning_and_silent_replacement(self):
        from evidence import verify_evidence
        text="source "*10000
        messages=[dict(role="assistant",tool_calls=[dict(id="c",function=dict(name="harness2_evidence",arguments=json.dumps(dict(id="source"))))]),dict(role="tool",tool_call_id="c",content=json.dumps(dict(id="source",text=text)))]
        verify_evidence(messages,dict(source=text))
        for changed in (json.dumps(dict(id="source",text=text[:4096]+"[... pruned ...]"+text[-1024:])),messages[-1]["content"][:4096]+"\n[... pruned ...]\n"+messages[-1]["content"][-1024:]):
            messages[-1]["content"]=changed
            with self.assertRaises(ValueError):
                verify_evidence(messages,dict(source=text))
    def test_durable_trajectory_does_not_depend_on_editable_summary_prefix(self):
        import tempfile
        from evidence import trajectory
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            evidence=root/"observations"
            evidence.mkdir()
            (root/"result.json").write_text(json.dumps(dict(stages=[],evidence_root=str(evidence))))
            (evidence/"maintenance-views.json").write_text(json.dumps({"views":[{"before":[{"text":"source FALCON-17"}],"after":[{"text":"novel instruction prefix; FALCON-71"}]}]}))
            text=trajectory(root)
            self.assertIn("FALCON-17",text)
            self.assertIn("FALCON-71",text)

    def test_roster_and_independent_arithmetic_and_exact_values(self):
        self.assertEqual((len(training()),len(heldout()),len(legacy())),(3,18,12))
        facts=expected(dict(family="tools",trial=11,control=False))
        self.assertEqual(facts["tool_total"],1100)
        self.assertTrue(check_object(facts,facts)["passed"])
        for field,value in (("ticket","ORION-117"),("tool_total","1100"),("pending","accepted")):
            altered=dict(facts,**{field:value})
            self.assertFalse(check_object(altered,facts)["passed"])
        self.assertFalse(check_object(dict(facts,extra=True),facts)["passed"])

    def test_audit_cannot_invent_quotes_accept_invalid_sources_or_authorize(self):
        packet=dict(sources=[dict(text="FALCON-17 is current")],candidate="FALCON-71 is approved")
        audit=dict(verdict="unsupported",reason="identifier changed",findings=[dict(kind="change",source_index=0,source_quote="FALCON-17",candidate_quote="FALCON-71",explanation="digits changed")])
        self.assertEqual(admit(json.dumps(audit),packet),audit)
        for field,value in (("source_index",1),("source_quote","FALCON-71"),("candidate_quote","FALCON-17")):
            changed=json.loads(json.dumps(audit))
            changed["findings"][0][field]=value
            with self.assertRaises(ValueError):
                admit(json.dumps(changed),packet)
        with self.assertRaises(ValueError):
            admit(json.dumps(dict(verdict="supported",reason="yes",findings=audit["findings"])),packet)
        self.assertEqual(acceptance([],False,True)["verdict"],"KEEP_BASE")

    def test_candidate_selection_cannot_override_missing_or_regressing_evidence(self):
        rows=[]
        for i in range(30):
            for arm in ("base","candidate"):
                rows.append(dict(split="heldout" if i<18 else "legacy",id=str(i),arm=arm,family=("requirements","tools","images")[i%3],control=False,passed=arm=="candidate",facts_passed=arm=="candidate",lifecycle=True,structure_verified=True,safety_failure=False))
        self.assertEqual(acceptance(rows,True,True)["verdict"],"ELIGIBLE_FOR_REVIEW")
        self.assertEqual(acceptance(rows,False,True)["verdict"],"KEEP_BASE")
        self.assertEqual(acceptance(rows,True,False)["verdict"],"KEEP_BASE")
        duplicated=[dict(row) for row in rows]
        duplicated[-2:]=[dict(row) for row in rows[:2]]
        self.assertEqual(acceptance(duplicated,True,True)["verdict"],"KEEP_BASE")
        rows[0]["passed"]=True
        rows[1]["passed"]=False
        self.assertEqual(acceptance(rows,True,True)["verdict"],"KEEP_BASE")
