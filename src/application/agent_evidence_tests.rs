//! Native production catalog -> permission -> processes -> pruner -> next request -> journal.
use super::*;
use crate::model::{ModelError, ModelEventSink, ModelItem, ModelRequest, ModelResponse};
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAM: &str = r#"
use std::io::{self,Write};
fn main(){
 let mode=std::env::args().nth(1).unwrap();
 writeln!(std::fs::OpenOptions::new().create(true).append(true).open("starts").unwrap(),"{mode}").unwrap();
 if mode=="job" {let mut s=String::new();io::stdin().read_line(&mut s).unwrap();assert_eq!(s,"go\n");println!("  1 failed\n  2 passed (0.1s)");std::process::exit(1);}
 for _ in 0..200 {println!("{}","bounded diagnostic ".repeat(5));}
 println!("  1 passed (0.1s)");
}
"#;
struct RecoveryScript {
    program: String,
    step: AtomicUsize,
}
impl TestModelScript for RecoveryScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        _: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let n = self.step.fetch_add(1, Ordering::SeqCst);
        let out = request.items.iter().rev().find_map(|i| match i {
            ModelItem::ToolResult(r) => Some(r),
            _ => None,
        });
        let v = |mode: &str| json!({"argv":[self.program,mode],"verification":{"format":"playwright-summary","minimum_executed":1}});
        let next = match n {
            0 => {
                let mut a = v("job");
                a["stdin"] = json!("pipe");
                a["yield_time_ms"] = json!(250);
                Some(("exec_command", a))
            }
            1 => {
                assert_eq!(out.unwrap().output["running"], true);
                Some(("write_stdin", json!({"session_id":999999})))
            }
            2 => {
                let r = out.unwrap();
                assert!(r.is_error, "bad ID must still be refused");
                let ids = r.output["process_recovery"]["sessions"]
                    .as_array()
                    .expect("model must see recovery");
                assert_eq!(ids.len(), 1);
                assert_eq!(ids[0]["running"], true);
                assert!(!r.output.to_string().contains("clat_truncated"));
                Some((
                    "write_stdin",
                    json!({"session_id":ids[0]["session_id"],"chars":"go\n","yield_time_ms":1000}),
                ))
            }
            3 => {
                let r = out.unwrap();
                assert_eq!(r.output["verification_receipt"]["status"], "failed");
                assert_eq!(r.output["verification_receipt"]["passed"], 2);
                Some(("run_command", v("inspect")))
            }
            4 => {
                let r = out.unwrap();
                assert_eq!(r.output["verification_receipt"]["status"], "passed");
                assert_eq!(r.output["verification_receipt"]["passed"], 1);
                assert!(r.output.to_string().len() < crate::plugins::THRESHOLD_CHARS);
                assert!(!r.output.to_string().contains("clat_truncated"));
                None
            }
            _ => panic!("unexpected extra model request"),
        };
        Ok(super::write_guard::response(
            next,
            "recovered existing process; inspected failed verification",
        ))
    }
}

#[test]
fn ag5_evidence_application_recovers_correct_process_and_consumes_bounded_receipts() {
    let (storage, root) = roots("ag5-evidence-loop");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("probe.rs");
    std::fs::write(&source, PROGRAM).unwrap();
    let program = root.join(format!("probe{}", std::env::consts::EXE_SUFFIX));
    crate::process::compile_rust_test_helper(&source, &program).unwrap();
    let script = Arc::new(RecoveryScript {
        program: program.to_string_lossy().into_owned(),
        step: AtomicUsize::new(0),
    });
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(script.clone()),
    );
    configure_test_model(&app);
    let done = run(
        &mut app,
        "verify then inspect failures without restarting the running job",
    )
    .unwrap();
    app.close().unwrap();
    assert_eq!(
        done.output,
        "recovered existing process; inspected failed verification"
    );
    assert_eq!(script.step.load(Ordering::SeqCst), 5);
    assert_eq!(
        std::fs::read_to_string(root.join("starts")).unwrap(),
        "job\ninspect\n"
    );
    let events = load_events(&storage);
    let results: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool/result")
        .collect();
    assert_eq!(results.len(), 4);
    assert_eq!(results[1].data["message"]["isError"], true);
    let journal = results
        .iter()
        .map(|e| e.data.to_string())
        .collect::<String>();
    assert!(journal.contains("process_recovery") && journal.contains("playwright-summary"));
    assert!(journal.contains("output_omitted_from_journal"));
    assert!(!journal.contains("bounded diagnostic"));
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

struct DeniedScript {
    program: String,
}
impl TestModelScript for DeniedScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        _: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if let Some(ModelItem::ToolResult(r)) = request.items.last() {
            assert!(r.is_error);
            return Ok(super::write_guard::response(None, "permission refused"));
        }
        Ok(super::write_guard::response(
            Some((
                "run_command",
                json!({"argv":[self.program,"inspect"],
            "verification":{"format":"playwright-summary","minimum_executed":1}}),
            )),
            "",
        ))
    }
}
#[test]
fn ag5_evidence_one_shot_permission_denial_precedes_invocation() {
    let (storage, root) = roots("ag5-evidence-deny");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("probe.rs");
    std::fs::write(&source, PROGRAM).unwrap();
    let program = root.join(format!("probe{}", std::env::consts::EXE_SUFFIX));
    crate::process::compile_rust_test_helper(&source, &program).unwrap();
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(Arc::new(DeniedScript {
            program: program.to_string_lossy().into_owned(),
        })),
    );
    configure_test_model(&app);
    let done = run_with_approver(
        &mut app,
        "verify",
        Arc::new(
            |_: crate::PermissionRequest, _: &crate::model::CancelToken| {
                crate::PermissionDecision::Deny {
                    reason: "not authorized".into(),
                }
            },
        ),
    )
    .unwrap();
    app.close().unwrap();
    assert_eq!(done.output, "permission refused");
    assert!(!root.join("starts").exists());
    assert!(
        !load_events(&storage)
            .iter()
            .any(|e| e.event_type == "tool/call")
    );
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
