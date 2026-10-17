use super::*;
use crate::model::{ModelError, ModelEventSink, ModelItem, ModelRequest, ModelResponse};
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAM: &str = r#"
use std::io::Write;
fn main(){let mode=std::env::args().nth(1).unwrap();writeln!(std::fs::OpenOptions::new().create(true).append(true).open("starts").unwrap(),"{mode}").unwrap();
println!("test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s");
if mode=="wrapper" {println!("{}","diagnostic ".repeat(1500));} else {std::process::exit(101);}}
"#;
struct ConflictScript {
    program: String,
    step: AtomicUsize,
}
impl TestModelScript for ConflictScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        _: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let step = self.step.fetch_add(1, Ordering::SeqCst);
        let out = request.items.iter().rev().find_map(|i| match i {
            ModelItem::ToolResult(r) => Some(r),
            _ => None,
        });
        let next = match step {
            0 => Some(("run_command", json!({"argv":[self.program,"wrapper"]}))),
            1 => {
                let out = out.unwrap();
                assert!(!out.is_error);
                assert_eq!(out.output["exit_code"], 0);
                assert_eq!(
                    out.output["verification_guidance"]["status"],
                    "failure_observed"
                );
                assert_eq!(
                    out.output["verification_guidance"]["target_exit_code"],
                    Value::Null
                );
                assert!(out.output.get("clat_truncated").is_none());
                eprintln!(
                    "AG6_MODEL_GUIDANCE_CHARS {}",
                    out.output.to_string().chars().count()
                );
                Some((
                    "run_command",
                    json!({"argv":[self.program,"actual"],"verification":{"format":"rust-test-summary","minimum_executed":1}}),
                ))
            }
            2 => {
                let out = out.unwrap();
                assert_eq!(out.output["exit_code"], 101);
                assert_eq!(out.output["verification_receipt"]["status"], "failed");
                assert!(out.output.get("verification_guidance").is_none());
                None
            }
            _ => panic!("unexpected automatic retry or extra request"),
        };
        Ok(super::write_guard::response(
            next,
            "actual failure inspected",
        ))
    }
}
#[test]
fn ag6_application_conflict_guidance_reaches_model_and_drives_literal_verification() {
    let (storage, root) = roots("ag6-conflict-loop");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("probe.rs");
    std::fs::write(&source, PROGRAM).unwrap();
    let program = root.join(format!("probe{}", std::env::consts::EXE_SUFFIX));
    crate::process::compile_rust_test_helper(&source, &program).unwrap();
    let script = Arc::new(ConflictScript {
        program: program.to_string_lossy().into(),
        step: AtomicUsize::new(0),
    });
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(script.clone()),
    );
    configure_test_model(&app);
    assert_eq!(
        run(&mut app, "verify").unwrap().output,
        "actual failure inspected"
    );
    app.close().unwrap();
    assert_eq!(script.step.load(Ordering::SeqCst), 3);
    assert_eq!(
        std::fs::read_to_string(root.join("starts")).unwrap(),
        "wrapper\nactual\n"
    );
    let events = load_events(&storage);
    let results: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool/result")
        .collect();
    assert_eq!(results.len(), 2);
    let journal = results
        .iter()
        .map(|e| e.data.to_string())
        .collect::<String>();
    assert!(journal.contains("verification_guidance") && journal.contains("verification_receipt"));
    assert!(!journal.contains("diagnostic diagnostic"));
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
