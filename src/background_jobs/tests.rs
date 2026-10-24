use super::*;
use crate::{
    Project, Tool,
    process::ProcessService,
    sandbox::{SandboxModeSource, SandboxService},
    session::{event::payloads, persistence::JsonlCompression, run_journal::NewSessionEvent},
};
use std::path::{Path, PathBuf};
struct Fixture {
    root: PathBuf,
    registry: Arc<JobRegistry>,
    source: Arc<ProcessService>,
    sessions: Arc<SessionService>,
    id: SessionId,
    cancel: CancelToken,
    generation: u64,
    helper: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("clat-job-test-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let project = Project::new(project.canonicalize().unwrap());
        let project_key = ProjectKey::from_cwd(&project.root().to_string_lossy());
        let sessions =
            Arc::new(SessionService::new(root.join("sessions"), JsonlCompression::Zstd).unwrap());
        let id = sessions.new_session(&project_key).unwrap().id;
        sessions
            .journal()
            .unwrap()
            .append_atomic_durable(&[NewSessionEvent::new(
                "user/message",
                payloads::user_message("jobs test"),
            )
            .append(vec![])])
            .unwrap();
        let source = Arc::new(ProcessService::new(
            project.clone(),
            Arc::new(SandboxService::for_project(project, SandboxModeSource::Classic).unwrap()),
        ));
        let cancel = CancelToken::new();
        let generation = source.bind_run(id.as_str(), cancel.clone()).unwrap();
        let registry = Arc::new(JobRegistry::new(
            sessions.clone(),
            source.clone(),
            project_key,
            Arc::new(JobCapacity::new(&root)),
        ));
        registry.bind(id.clone());
        let helper = root.join(if cfg!(windows) {
            "helper.exe"
        } else {
            "helper"
        });
        let code = root.join("helper.rs");
        std::fs::write(&code,r#"fn main(){use std::{env,fs,thread,time::Duration,io::{self,Write}};let a:Vec<_>=env::args().collect();fs::write(&a[1],b"executed").unwrap();println!("scratch={}",env::var("CLAT_JOB_SCRATCH").unwrap());println!("job-output");io::stdout().flush().unwrap();thread::sleep(Duration::from_millis(a[2].parse().unwrap()));let n:usize=a[4].parse().unwrap();for _ in 0..(n/1024){println!("{}","z".repeat(1024));}std::process::exit(a[3].parse().unwrap());}"#).unwrap();
        crate::process::compile_rust_test_helper(&code, &helper).unwrap();
        Self {
            root,
            registry,
            source,
            sessions,
            id,
            cancel,
            generation,
            helper,
        }
    }
    fn args(&self, delay: u64, exit: i32, bytes: usize) -> Value {
        json!({"label":"test job","argv":[self.helper.to_string_lossy(),self.marker().to_string_lossy(),delay.to_string(),exit.to_string(),bytes.to_string()],"timeout_ms":5000})
    }
    fn marker(&self) -> PathBuf {
        // The command may write inside its project, never the parent fixture
        // directory that also owns session facts and test executables.
        self.root.join("project").join("executed")
    }
    fn tool(&self, operation: tools::Operation) -> tools::JobTool {
        tools::JobTool {
            registry: self.registry.clone(),
            process: self.source.clone(),
            operation,
        }
    }
    fn start(&self, args: Value) -> JobView {
        let value = self
            .tool(tools::Operation::Start)
            .invoke(&args, &Project::new("."), &CancelToken::new())
            .unwrap();
        self.registry
            .entry(value["job"]["id"].as_str().unwrap())
            .unwrap()
            .view()
            .clone()
    }
    fn settled(&self, id: &str) -> JobView {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let row = self.registry.entry(id).unwrap();
            if row.finished.load(std::sync::atomic::Ordering::Acquire) {
                return row.view().clone();
            }
            assert!(Instant::now() < deadline, "job did not settle");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.registry.close();
        let _ = self.source.close();
        let _ = self.sessions.quiesce_active();
        crate::test_support::cleanup_tree(&self.root);
    }
}
#[test]
fn native_job_survives_origin_run_cancel_and_scratch_cleanup() {
    let f = Fixture::new();
    let job = f.start(f.args(400, 0, 0));
    assert_eq!(job.session, f.id.as_str());
    f.cancel.cancel();
    f.source.unbind_run(f.generation).unwrap();
    f.registry.unbind();
    let done = f.settled(&job.id);
    assert_eq!(done.state, "completed", "{done:?}");
    assert_eq!(done.observation.as_ref().unwrap()["exit_code"], 0);
    assert!(f.marker().exists());
    let output = f.registry.read_output(&job.id, Some(0)).unwrap();
    assert!(output.text.contains("job-output"));
    let scratch = output
        .text
        .lines()
        .find_map(|l| l.strip_prefix("scratch="))
        .unwrap();
    assert!(
        !Path::new(scratch).exists(),
        "job scratch is explicitly reclaimed after real termination"
    );
}
#[test]
fn native_job_reads_have_separate_agent_and_user_cursors_and_keep_exit() {
    let f = Fixture::new();
    let job = f.start(f.args(0, 7, 30000));
    let done = f.settled(&job.id);
    assert_eq!(done.state, "failed");
    assert_eq!(done.observation.as_ref().unwrap()["exit_code"], 7);
    let first = f.registry.read_output(&job.id, Some(0)).unwrap();
    let second = f.registry.read_output(&job.id, Some(0)).unwrap();
    assert_eq!(first.text, second.text);
    let agent = f.registry.agent_output(&job.id).unwrap();
    assert_eq!(agent.text, first.text);
    let agent2 = f.registry.agent_output(&job.id).unwrap();
    assert_ne!(agent2.next_offset, agent.next_offset);
    assert_eq!(
        f.registry.read_output(&job.id, Some(0)).unwrap().text,
        first.text
    );
}
#[test]
fn native_job_kill_request_is_not_terminal_and_host_close_stops_processes() {
    let f = Fixture::new();
    let job = f.start(f.args(5000, 0, 0));
    let requested = f.registry.kill(&job.id).unwrap();
    assert!(requested.finished_at.is_none());
    let done = f.settled(&job.id);
    assert_eq!(done.state, "killed");
    assert!(done.observation.unwrap()["cancelled"] == true);
    let job = f.start(f.args(5000, 0, 0));
    f.registry.close().unwrap();
    assert!(
        f.registry
            .entry(&job.id)
            .unwrap()
            .finished
            .load(std::sync::atomic::Ordering::Acquire)
    );
}
#[test]
fn native_job_frozen_start_cannot_move_to_a_different_session() {
    let f = Fixture::new();
    let args = f.args(0, 0, 0);
    let frozen = f
        .tool(tools::Operation::Start)
        .prepare_execution(&args)
        .unwrap();
    f.registry.bind(SessionId::new("other"));
    assert!(
        frozen
            .tool
            .invoke(&args, &Project::new("."), &CancelToken::new())
            .is_err()
    );
    assert!(!f.marker().exists());
    assert!(f.registry.list().is_empty());
}
#[test]
fn native_job_registration_failure_prevents_execution() {
    let f = Fixture::new();
    f.sessions
        .inject_persistence_faults(crate::session::persistence::FaultHooks {
            fail_batch_write: true,
            ..Default::default()
        });
    assert!(
        f.tool(tools::Operation::Start)
            .invoke(&f.args(0, 0, 0), &Project::new("."), &CancelToken::new())
            .is_err()
    );
    assert!(!f.marker().exists());
    assert!(f.registry.list().is_empty());
    f.sessions.inject_persistence_faults(Default::default());
}

#[test]
fn native_job_redaction_handles_split_markers_and_utf8_pages() {
    let mut line = output::RedactedLines::default();
    assert_eq!(line.push("secret=very", false).0, "");
    assert_eq!(
        line.push("-private-credential\n", false).0,
        "secret=[REDACTED]\n"
    );
    assert!(line.push(&"x".repeat(20000), false).1);
    assert_eq!(line.push("sensitive-tail\n", true).0, "");
    let mut ring = crate::process::ByteRing::new(20000);
    let source = "中文繁體測試".repeat(1000);
    ring.append(source.as_bytes());
    let mut cursor = 0;
    let mut result = String::new();
    loop {
        let (text, next, lost, more) = output::read_text(&ring, cursor);
        assert!(!lost);
        assert!(!text.contains('\u{fffd}'));
        result.push_str(&text);
        cursor = next;
        if !more {
            break;
        }
    }
    assert_eq!(result, source);
}
#[test]
fn native_job_notification_failure_keeps_actual_execution_result() {
    let f = Fixture::new();
    f.registry
        .set_notice(Arc::new(|_| panic!("notification failure probe")));
    let job = f.start(f.args(0, 0, 0));
    let done = f.settled(&job.id);
    assert_eq!(done.state, "completed");
    assert_eq!(done.observation.unwrap()["exit_code"], 0);
    assert!(done.notification_error.is_some());
}
#[test]
fn native_job_terminal_fact_targets_origin_without_switching_active_session() {
    let f = Fixture::new();
    let job = f.start(f.args(300, 0, 0));
    f.registry.unbind();
    f.sessions.quiesce_active().unwrap();
    let other = f.sessions.new_session(&f.registry.project).unwrap().id;
    f.sessions
        .journal()
        .unwrap()
        .append_atomic_durable(&[NewSessionEvent::new(
            "user/message",
            payloads::user_message("other session"),
        )
        .append(vec![])])
        .unwrap();
    assert_eq!(f.settled(&job.id).state, "completed");
    assert_eq!(f.sessions.active_id(), Some(other));
    let mut unit = projection::JobUnit::default();
    let reader = f.sessions.query_reader();
    use crate::session::projection::ProjectionUnit;
    reader
        .visit(&key(&f.registry.project, f.id.as_str()), &mut |e| {
            unit.fold(e)
        })
        .unwrap();
    let rows = unit.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "completed");
    let mut restored = projection::JobUnit::default();
    restored
        .restore(&crate::session::projection::CheckpointRow {
            ver: 1,
            seq: unit.as_of(),
            val: unit.snapshot(),
        })
        .unwrap();
    assert_eq!(restored.rows(), rows);
}
#[test]
fn native_job_timeout_and_output_overflow_preserve_observations() {
    let f = Fixture::new();
    let mut args = f.args(5000, 0, 0);
    args["timeout_ms"] = json!(100);
    let job = f.start(args);
    let done = f.settled(&job.id);
    assert_eq!(done.state, "failed");
    assert_eq!(done.observation.unwrap()["timed_out"], true);
    let job = f.start(f.args(0, 0, 900000));
    let done = f.settled(&job.id);
    assert_eq!(done.state, "completed");
    assert_eq!(done.observation.as_ref().unwrap()["exit_code"], 0);
    assert!(
        done.observation.as_ref().unwrap()["stdout_bytes"]
            .as_u64()
            .unwrap()
            > 800000
    );
    let output = f.registry.read_output(&job.id, Some(0)).unwrap();
    assert!(output.lost);
    assert!(output.more);
    assert!(output.text.len() <= 4096);
}
#[test]
fn native_job_error_after_spawn_never_claims_not_executed_or_replays() {
    let f = Fixture::new();
    f.source
        .fail_background_after_spawn
        .store(true, std::sync::atomic::Ordering::Release);
    let job = f.start(f.args(1000, 0, 0));
    assert_eq!(job.state, "interrupted");
    assert_eq!(job.observation.unwrap()["execution_outcome"], "unknown");
    assert!(f.marker().exists());
    assert!(!f.registry.has_active());
}
#[test]
fn native_job_restart_records_unknown_without_replaying_and_g7_is_scoped() {
    use crate::task::{JobObservation, JobQuery};
    let f = Fixture::new();
    let entry = f
        .registry
        .prepare_entry("never launched".into(), &f.id)
        .unwrap();
    f.registry
        .persist(&entry, crate::session::catalog::JobPhase::Registered)
        .unwrap();
    let original = entry.view().clone();
    drop(entry);
    f.registry.recover().unwrap();
    assert!(!f.marker().exists());
    let rows = f.registry.list();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "interrupted");
    assert!(f.registry.read_output(&rows[0].id, Some(0)).unwrap().lost);
    assert_eq!(
        f.registry.query(
            &original.reference,
            Instant::now() + Duration::from_secs(1),
            &CancelToken::new()
        ),
        JobObservation::Failed
    );
    let mut foreign = original.reference;
    foreign.scope = "other storage root".into();
    assert_eq!(
        f.registry.query(
            &foreign,
            Instant::now() + Duration::from_secs(1),
            &CancelToken::new()
        ),
        JobObservation::Unavailable
    );
}
#[test]
fn native_job_readable_projection_rejects_malformed_native_record() {
    use crate::session::{
        admission::admit_events_for_version, catalog::JobPhase, event::SessionEvent,
    };
    let f = Fixture::new();
    let entry = f.registry.prepare_entry("validate".into(), &f.id).unwrap();
    let view = entry.view().clone();
    let fact = record::event(&view, JobPhase::Registered);
    let mut event = SessionEvent::new(JobPhase::Registered.event_name(), 0, 1, fact.data);
    assert!(admit_events_for_version(&[event.clone()], 4).is_ok());
    event.data["native"]["version"] = json!(99);
    assert!(admit_events_for_version(&[event], 4).is_err());
}
struct JobModel {
    args: Value,
}
impl crate::Model for JobModel {
    fn provider(&self) -> &str {
        "test"
    }
    fn model_id(&self) -> &str {
        "job-workflow"
    }
    fn stream(
        &mut self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let complete = request
            .items
            .iter()
            .any(|i| matches!(i, crate::ModelItem::ToolResult(_)));
        let calls = if complete {
            vec![]
        } else {
            vec![crate::ToolCall {
                id: "job-call".into(),
                name: "job_start".into(),
                arguments: self.args.clone(),
            }]
        };
        for call in &calls {
            events.emit(crate::ModelEvent::ToolCallCompleted { call: call.clone() });
        }
        let reason = if complete {
            crate::FinishReason::Completed
        } else {
            crate::FinishReason::ToolCalls
        };
        events.emit(crate::ModelEvent::ResponseCompleted {
            finish_reason: reason.clone(),
        });
        Ok(crate::ModelResponse {
            text: if complete {
                "done".into()
            } else {
                String::new()
            },
            tool_calls: calls,
            finish_reason: reason,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}
struct Deny;
impl crate::PermissionPolicy for Deny {
    fn check(
        &self,
        _: &Project,
        _: &crate::ToolDefinition,
        _: &crate::ToolCall,
    ) -> crate::PermissionDecision {
        crate::PermissionDecision::Deny {
            reason: "denial test".into(),
        }
    }
}
#[test]
fn native_job_model_tool_model_denial_is_zero_execution_and_zero_registration() {
    for denied in [true, false] {
        let f = Fixture::new();
        let registry = crate::ToolRegistry::new();
        let owner = crate::plugin::PluginOwner::for_test(crate::plugin::PluginId::new("test.jobs"));
        let _lease = registry
            .register(owner, Arc::new(f.tool(tools::Operation::Start)))
            .unwrap();
        let policy: &dyn crate::PermissionPolicy = if denied { &Deny } else { &crate::AllowAll };
        let mut model = JobModel {
            args: f.args(0, 0, 0),
        };
        let mut events = Vec::new();
        let project = Project::new(&f.root);
        let output = crate::run::Run::new(&mut model, &registry, policy, &project)
            .execute_with_items(
                vec![crate::ModelItem::user_text("start")],
                crate::MessageContent::text("start"),
                None,
                &mut events,
            )
            .unwrap();
        assert_eq!(output.turns, 2);
        assert_eq!(
            events
                .iter()
                .any(|e| matches!(e, crate::RunEvent::PermissionDenied { .. })),
            denied
        );
        if denied {
            assert!(f.registry.list().is_empty());
            assert!(!f.marker().exists());
        } else {
            let job = f.registry.list().into_iter().next().unwrap();
            assert_eq!(f.settled(&job.id).state, "completed");
        }
    }
}
#[test]
fn native_job_monitor_panic_stops_owned_process_and_records_uncertainty() {
    let f = Fixture::new();
    f.registry
        .panic_next_monitor
        .store(true, std::sync::atomic::Ordering::Release);
    let job = f.start(f.args(5000, 0, 0));
    let done = f.settled(&job.id);
    assert_eq!(done.state, "interrupted");
    assert_eq!(done.observation.unwrap()["execution_outcome"], "unknown");
    assert!(done.cleanup_error.is_none());
    assert!(!f.registry.has_active());
}
#[test]
fn native_job_golden_catalog_admission_fold_checkpoint_and_live_replay() {
    use crate::session::{
        admission::admit_events_for_version,
        catalog::{JobPhase, event_spec},
        event::SessionEvent,
        projection::{CheckpointRow, ProjectionUnit},
    };
    let events: Vec<SessionEvent> =
        serde_json::from_str(include_str!("../../tests/fixtures/native-job-state.json")).unwrap();
    admit_events_for_version(&events, 4).unwrap();
    let mut projection = projection::JobUnit::default();
    for event in &events {
        assert!(event_spec(&event.event_type).unwrap().job_phase.is_some());
        projection.fold(event).unwrap();
    }
    let rows = projection.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "completed");
    let live = record::event(&rows[0], JobPhase::Settled);
    assert_eq!(live.data, events[1].data);
    let mut restored = projection::JobUnit::default();
    restored
        .restore(&CheckpointRow {
            ver: 1,
            seq: 1,
            val: projection.snapshot(),
        })
        .unwrap();
    assert_eq!(restored.rows(), rows);
}

#[test]
fn native_job_unsaved_terminal_stays_busy_without_rewriting_execution() {
    use crate::task::{JobObservation, JobQuery};
    let f = Fixture::new();
    let job = f.start(f.args(500, 0, 0));
    f.sessions
        .inject_persistence_faults(crate::session::persistence::FaultHooks {
            fail_batch_write: true,
            ..Default::default()
        });
    let done = f.settled(&job.id);
    assert_eq!(done.state, "completed");
    assert_eq!(done.observation.as_ref().unwrap()["exit_code"], 0);
    assert!(done.persistence_error.is_some());
    assert!(
        f.registry.has_active(),
        "an unsaved terminal fact blocks takeover"
    );
    assert!(matches!(
        f.registry.query(
            &done.reference,
            Instant::now() + Duration::from_secs(1),
            &CancelToken::new()
        ),
        JobObservation::Unavailable
    ));
    f.sessions.inject_persistence_faults(Default::default());
    let saved = f.registry.retry_settlement(&job.id).unwrap();
    assert_eq!(saved.state, "completed");
    assert!(saved.persistence_error.is_none());
    assert!(!f.registry.has_active());
    f.registry.retry_settlement(&job.id).unwrap();
    let mut settled = 0;
    f.sessions
        .query_reader()
        .visit(&key(&f.registry.project, f.id.as_str()), &mut |event| {
            if crate::session::catalog::event_spec(&event.event_type).and_then(|s| s.job_phase)
                == Some(crate::session::catalog::JobPhase::Settled)
            {
                settled += 1;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(settled, 1, "retry must not duplicate a committed fact");
}

#[test]
fn native_job_model_projection_keeps_exit_and_references_under_general_limit() {
    let f = Fixture::new();
    let job = f.start(f.args(0, 7, 30000));
    let mut done = f.settled(&job.id);
    done.observation.as_mut().unwrap()["diagnostic"] = json!("PRIVATE".repeat(20000));
    let single = model_view::job(&done);
    assert_eq!(single["id"], job.id);
    assert_eq!(single["observation"]["exit_code"], 7);
    assert_eq!(single["reference"]["result_ref"], done.reference.result_ref);
    let list = model_view::list(vec![done.clone(); 64]);
    assert!(list.to_string().chars().count() < 8192);
    assert_eq!(list["truncated"], true);
    assert!(!list["jobs"].as_array().unwrap().is_empty());
    let output = model_view::page(JobOutput {
        job: done,
        text: "\u{0}".repeat(100),
        next_offset: 100,
        lost: false,
        more: true,
    });
    assert!(output.to_string().chars().count() < 8192);
    assert_eq!(output["job"]["observation"]["exit_code"], 7);
    assert!(output["job"]["observation"].get("diagnostic").is_none());
}

#[test]
fn native_job_running_output_is_readable_before_exit() {
    let f = Fixture::new();
    let job = f.start(f.args(5000, 0, 0));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let output = f.registry.read_output(&job.id, Some(0)).unwrap();
        if output.text.contains("job-output") {
            assert!(output.job.finished_at.is_none());
            assert!(!output.lost);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "live output unavailable: {output:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn native_job_cleanup_failure_is_explicit_and_retry_reclaims_scratch() {
    let f = Fixture::new();
    let job = f.start(f.args(600, 0, 0));
    let entry = f.registry.entry(&job.id).unwrap();
    let (kernel, _) = entry.kernel.lock().unwrap().as_ref().unwrap().clone();
    let scratch = kernel.background_scratch_for_test();
    kernel
        .fail_next_cleanup
        .store(true, std::sync::atomic::Ordering::Release);
    let done = f.settled(&job.id);
    assert_eq!(done.state, "completed");
    assert!(done.cleanup_error.is_some());
    assert!(f.registry.has_active());
    assert!(scratch.exists());
    let retried = f.registry.retry_settlement(&job.id).unwrap();
    assert!(retried.cleanup_error.is_none());
    assert!(!f.registry.has_active());
    assert!(
        !scratch.exists(),
        "a second explicit close must retry the failed scratch cleanup"
    );
}
