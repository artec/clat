//! Actual registered native tool -> permission -> pruner -> next model request.
use super::*;
use crate::model::{ModelError, ModelEventSink, ModelItem, ModelRequest, ModelResponse};
use std::sync::atomic::{AtomicUsize, Ordering};

struct ArgumentScript {
    step: AtomicUsize,
    denied: bool,
}
impl TestModelScript for ArgumentScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        _: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let step = self.step.fetch_add(1, Ordering::SeqCst);
        let result = request.items.iter().rev().find_map(|item| match item {
            ModelItem::ToolResult(out) => Some(out),
            _ => None,
        });
        let next = match step {
            0 => Some((
                "read_file",
                json!({"path":"edit.txt","start_line":400,"end_line":120}),
            )),
            1 => {
                let out = result.unwrap();
                assert!(out.is_error);
                assert_eq!(
                    out.output["argument_recovery"]["code"],
                    "invalid_line_range"
                );
                assert_eq!(out.output["argument_recovery"]["operation_started"], false);
                assert!(out.output.get("clat_truncated").is_none());
                Some((
                    "read_file",
                    json!({"path":"edit.txt","start_line":1,"end_line":2}),
                ))
            }
            2 => {
                assert!(!result.unwrap().is_error);
                assert!(result.unwrap().output.to_string().contains("body"));
                Some((
                    "edit_file",
                    json!({"path":"edit.txt","edits":[
                        {"old_str":"header","new_str":"header"},
                        {"old_str":"body","new_str":"updated","extra":"SYNTHETIC_PRIVATE"}
                    ]}),
                ))
            }
            3 if self.denied => {
                assert!(result.unwrap().is_error);
                assert!(result.unwrap().output.get("argument_recovery").is_none());
                None
            }
            3 => {
                let out = result.unwrap();
                assert!(out.is_error);
                let hint = &out.output["argument_recovery"];
                assert_eq!(hint["issues"].as_array().unwrap().len(), 2);
                assert_eq!(hint["applied"], false);
                assert!(out.output.get("clat_truncated").is_none());
                assert!(out.output.to_string().chars().count() < 2048);
                Some((
                    "edit_file",
                    json!({"path":"edit.txt","old_str":"body","new_str":"updated"}),
                ))
            }
            4 => {
                assert!(!result.unwrap().is_error);
                assert_eq!(result.unwrap().output["replacements"], 1);
                None
            }
            _ => panic!("no automatic retry or extra model request"),
        };
        Ok(super::write_guard::response(
            next,
            "argument recovery complete",
        ))
    }
}

fn argument_journey(denied: bool) {
    let (storage, root) = roots("ag8-argument-loop");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("edit.txt"), "header\nbody\n").unwrap();
    let script = Arc::new(ArgumentScript {
        step: AtomicUsize::new(0),
        denied,
    });
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(script.clone()),
    );
    configure_test_model(&app);
    if denied {
        run_with_approver(
            &mut app,
            "recover arguments",
            Arc::new(
                |_: crate::PermissionRequest, _: &crate::model::CancelToken| {
                    crate::permission::PermissionDecision::Deny {
                        reason: "synthetic denial".into(),
                    }
                },
            ),
        )
        .unwrap();
    } else {
        run(&mut app, "recover arguments").unwrap();
    }
    app.close().unwrap();
    assert_eq!(
        script.step.load(Ordering::SeqCst),
        if denied { 4 } else { 5 }
    );
    assert_eq!(
        std::fs::read_to_string(root.join("edit.txt")).unwrap(),
        if denied {
            "header\nbody\n"
        } else {
            "header\nupdated\n"
        }
    );
    let events = load_events(&storage);
    let results: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool/result")
        .collect();
    let journal = results
        .iter()
        .map(|e| e.data.to_string())
        .collect::<String>();
    assert!(journal.contains("invalid_line_range"));
    assert_eq!(journal.contains("invalid_edit_arguments"), !denied);
    assert!(!journal.contains("SYNTHETIC_PRIVATE"));
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn ag8_application_argument_recovery_drives_corrected_calls() {
    argument_journey(false);
}
#[test]
fn ag8_application_argument_recovery_respects_permission_denial() {
    argument_journey(true);
}
