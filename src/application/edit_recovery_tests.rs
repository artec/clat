//! Native registration -> permission -> invocation -> pruning -> next request -> journal.
use super::*;
use crate::model::{ModelError, ModelEventSink, ModelItem, ModelRequest, ModelResponse};
use std::sync::atomic::{AtomicUsize, Ordering};

const ORIGINAL: &str =
    "header\nfn repeated_symbol(x: &str) {}\nseparator\nfn repeated_symbol(x: &str) {}\n";
struct EditScript {
    step: AtomicUsize,
    denied: bool,
}
impl TestModelScript for EditScript {
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
            0 => Some((
                "edit_file",
                json!({"path":"edit.txt","old_str":"fn repeated_symbol(x: &str) {}","new_str":"changed"}),
            )),
            1 if self.denied => {
                assert!(out.unwrap().is_error);
                assert!(out.unwrap().output.get("edit_recovery").is_none());
                None
            }
            1 => {
                let result = out.unwrap();
                assert!(result.is_error);
                let recovery = &result.output["edit_recovery"];
                assert_eq!(recovery["code"], "ambiguous");
                assert_eq!(recovery["applied"], false);
                assert!(result.output.get("clat_truncated").is_none());
                let hint = &recovery["candidates"][0];
                eprintln!(
                    "AG6_MODEL_EDIT_ERROR_CHARS {}",
                    result.output.to_string().chars().count()
                );
                Some((
                    "read_file",
                    json!({"path":"edit.txt","start_line":hint["start_line"],"end_line":hint["end_line"]}),
                ))
            }
            2 => {
                let result = out.unwrap();
                assert!(!result.is_error);
                assert!(result.output.to_string().contains("separator"));
                Some((
                    "edit_file",
                    json!({"path":"edit.txt","edits":[
                        {"old_str":"header\nfn repeated_symbol(x: &str) {}","new_str":"header\nfn first() {}"},
                        {"old_str":"separator\nfn repeated_symbol(x: &str) {}","new_str":"separator\nfn second() {}"}
                    ]}),
                ))
            }
            3 => {
                let result = out.unwrap();
                assert!(!result.is_error, "{result:?}");
                assert_eq!(result.output["replacements"], 2);
                None
            }
            _ => panic!("extra request after completion"),
        };
        Ok(super::write_guard::response(next, "edit sequence complete"))
    }
}

#[test]
fn ag6_application_edit_recovery_drives_scoped_read_and_atomic_batch() {
    edit_sequence(false);
}
#[test]
fn ag6_application_edit_permission_denial_has_no_read_hint_or_side_effect() {
    edit_sequence(true);
}
fn edit_sequence(denied: bool) {
    let (storage, root) = roots("ag6-edit-loop");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("edit.txt"), ORIGINAL).unwrap();
    let script = Arc::new(EditScript {
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
            "edit",
            Arc::new(
                |_: crate::PermissionRequest, _: &crate::model::CancelToken| {
                    crate::PermissionDecision::Deny {
                        reason: "not authorized".into(),
                    }
                },
            ),
        )
        .unwrap();
    } else {
        run(&mut app, "edit").unwrap();
    }
    app.close().unwrap();
    assert_eq!(
        script.step.load(Ordering::SeqCst),
        if denied { 2 } else { 4 }
    );
    assert_eq!(
        std::fs::read_to_string(root.join("edit.txt")).unwrap(),
        if denied {
            ORIGINAL
        } else {
            "header\nfn first() {}\nseparator\nfn second() {}\n"
        }
    );
    let events = load_events(&storage);
    let calls: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool/call")
        .collect();
    assert_eq!(calls.len(), if denied { 0 } else { 3 });
    if !denied {
        assert!(
            events
                .iter()
                .any(|e| e.event_type == "tool/result"
                    && e.data.to_string().contains("edit_recovery"))
        );
    }
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
