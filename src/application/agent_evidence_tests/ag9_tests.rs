//! Behavioral probes use public tool names/JSON so the same tests run on baseline.
use super::*;
use std::sync::Mutex;

#[derive(Clone, Copy)]
enum Case {
    #[cfg(unix)]
    Screen,
    Report,
    Contract,
}
struct EvidenceScript {
    case: Case,
    program: String,
    state: Mutex<(usize, Option<Value>, usize)>,
}
impl TestModelScript for EvidenceScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        _: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if request.tools.is_empty() {
            return Ok(super::super::write_guard::response(None, "Evidence review"));
        }
        let mut state = self.state.lock().unwrap();
        let last = request.items.iter().rev().find_map(|i| match i {
            ModelItem::ToolResult(r) => Some(r),
            _ => None,
        });
        if let Some(r) = last {
            assert!(!r.is_error, "production tool failed: {}", r.output);
            eprintln!(
                "AG9_MODEL_VIEW tool={} chars={}",
                r.tool_name,
                r.output.to_string().chars().count()
            );
            assert!(
                r.output.get("clat_truncated").is_none(),
                "evidence lost to pruning"
            );
        }
        let out = last.map(|r| &r.output);
        let next = match self.case {
            #[cfg(unix)]
            Case::Screen => self.screen(&mut state, out),
            Case::Report => self.report(&mut state, out),
            Case::Contract => self.contract(&mut state, out),
        };
        Ok(super::super::write_guard::response(
            next,
            "observed evidence; no unsupported pass",
        ))
    }
}
impl EvidenceScript {
    #[cfg(unix)]
    fn screen(
        &self,
        state: &mut (usize, Option<Value>, usize),
        out: Option<&Value>,
    ) -> Option<(&'static str, Value)> {
        let next = match state.0 {
            0 => Some((
                "exec_command",
                json!({"argv":[self.program,"screen"],"tty":true,"yield_time_ms":1000}),
            )),
            1 => {
                assert_eq!(out.unwrap()["running"], true);
                state.1 = Some(out.unwrap()["session_id"].clone());
                // A yielded process is not evidence of a painted screen. Wait
                // for the child's actual ready marker without restarting it.
                if !out.unwrap()["pty"]
                    .as_str()
                    .unwrap_or("")
                    .contains("AG9_SCREEN_READY")
                {
                    state.2 += 1;
                    assert!(state.2 <= 10, "child never produced its ready marker");
                    return Some((
                        "write_stdin",
                        json!({"session_id":state.1,"yield_time_ms":1000}),
                    ));
                }
                Some((
                    "terminal_snapshot",
                    json!({"session_id":state.1,"row_count":2}),
                ))
            }
            2 => {
                let o = out.unwrap();
                assert_eq!(o["projection_complete"], true);
                assert_eq!(o["lines"], json!(["private-draft", "updated"]));
                assert_eq!(o["application_state_verified"], false);
                Some((
                    "write_stdin",
                    json!({"session_id":state.1,"chars":"go\n","yield_time_ms":1000}),
                ))
            }
            _ => {
                assert_eq!(out.unwrap()["exit_code"], 0);
                None
            }
        };
        state.0 += 1;
        next
    }
    fn report(
        &self,
        state: &mut (usize, Option<Value>, usize),
        out: Option<&Value>,
    ) -> Option<(&'static str, Value)> {
        if state.0 == 0 {
            state.0 += 1;
            return Some((
                "exec_command",
                json!({"argv":[self.program,"json"],"yield_time_ms":1000,
                "verification":{"format":"playwright-json","minimum_executed":1}}),
            ));
        }
        let o = out.unwrap();
        if o["running"] == true {
            return Some((
                "write_stdin",
                json!({"session_id":o["session_id"],"yield_time_ms":1000}),
            ));
        }
        let r = &o["verification_receipt"];
        assert_eq!(o["exit_code"], 0);
        assert_eq!(r["status"], "inconclusive");
        assert_eq!(r["expected_failures"], 1);
        assert_eq!(r["passed"], 1);
        assert_eq!(r["failed"], 0);
        state.0 += 1;
        None
    }
    fn contract(
        &self,
        state: &mut (usize, Option<Value>, usize),
        out: Option<&Value>,
    ) -> Option<(&'static str, Value)> {
        if state.0 > 0 {
            let o = out.unwrap();
            assert_eq!(o["status"], if state.0 == 1 { "failed" } else { "passed" });
            assert_eq!(o["source_sha256"].as_str().unwrap().len(), 64);
            assert!(!o.to_string().contains("private-expectation"));
            if state.0 == 1 {
                for check in o["checks"].as_array().unwrap() {
                    assert_eq!(check["status"], "failed");
                }
            } else {
                return None;
            }
        }
        let path = if state.0 == 0 {
            "bad.json"
        } else {
            "good.json"
        };
        state.0 += 1;
        Some((
            "verify_observation",
            json!({"path":path,"checks":[
                {"id":"truth","op":"is_true","pointer":"/truth"},
                {"id":"sequence","op":"equals","pointer":"/steps","expected":["draft","history","draft"]},
                {"id":"open-first","op":"all_before","pointer":"/trace","before":"opened","after":"tool"},
                {"id":"private","op":"equals","pointer":"/secret","expected":"private-expectation"}
            ]}),
        ))
    }
}

fn exercise(case: Case, name: &str) {
    let (storage, root) = roots(name);
    std::fs::create_dir_all(&root).unwrap();
    let program = compile_probe(&root);
    let script = Arc::new(EvidenceScript {
        case,
        program: program.to_string_lossy().into_owned(),
        state: Mutex::new((0, None, 0)),
    });
    observation_files(&root);
    let project = Project::new(&root);
    let mut app = mount(&project, &storage, TestBehavior::Scripted(script));
    configure_test_model(&app);
    let done = run(&mut app, "verify declared evidence with controls").unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("starts")).unwrap_or_default(),
        match case {
            #[cfg(unix)]
            Case::Screen => "screen\n",
            Case::Report => "json\n",
            Case::Contract => "",
        },
        "no duplicate command execution or automatic rerun"
    );
    assert_eq!(done.output, "observed evidence; no unsupported pass");
    let live = app.snapshot().unwrap();
    let id = live.session_id.unwrap();
    app.close().unwrap();
    let events = load_events(&storage);
    let journal = events
        .iter()
        .map(|e| e.data.to_string())
        .collect::<String>();
    assert!(!journal.contains("private-draft") && !journal.contains("private-expectation"));
    let mut reopened = crate::test_support::reopen_after_close(|| {
        mount_result(&project, &storage, TestBehavior::Success)
    });
    let replay = reopened.switch_session(id).unwrap();
    assert_eq!(format!("{:?}", live.replay), format!("{:?}", replay.replay));
    match case {
        #[cfg(unix)]
        Case::Screen => assert!(journal.contains("screen_text_omitted_from_journal")),
        Case::Contract => {
            assert!(journal.contains("contract_omitted") && journal.contains("order_violated"))
        }
        Case::Report => {
            assert!(journal.contains("expected_failures") && journal.contains("inconclusive"))
        }
    }
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
fn observation_files(root: &std::path::Path) {
    for (name, data) in [
        (
            "bad.json",
            json!({"truth":false,"steps":["draft","draft"],"trace":["tool","opened","tool"],"secret":"wrong"}),
        ),
        (
            "good.json",
            json!({"truth":true,"steps":["draft","history","draft"],"trace":["opened","opened","tool","tool"],"secret":"private-expectation"}),
        ),
        (
            "report.json",
            json!({"errors":[],"suites":[{"specs":[{"tests":[
                {"expectedStatus":"passed","status":"expected","results":[{"status":"passed"}]},
                {"expectedStatus":"failed","status":"expected","results":[{"status":"failed"}]}
            ]}]}]}),
        ),
    ] {
        std::fs::write(root.join(name), data.to_string()).unwrap();
    }
}
#[cfg(unix)]
#[test]
fn ag9_application_terminal_evidence_reaches_model_and_redacts_replay() {
    exercise(Case::Screen, "ag9-screen-loop");
}
#[test]
fn ag9_application_expected_failure_cannot_become_business_pass() {
    exercise(Case::Report, "ag9-report-loop");
}
#[test]
fn ag9_application_contract_detects_same_wrong_and_end_only_evidence() {
    exercise(Case::Contract, "ag9-contract-loop");
}

#[test]
fn ag9_application_json_permission_denial_starts_no_process() {
    denial("playwright-json", "ag9-json-deny");
}
