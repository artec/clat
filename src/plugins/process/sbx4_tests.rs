use super::prepared::enforced_workspace;
use super::*;

pub(super) fn root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "clat-process-plugin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn sbx4_fixture() -> (
    std::path::PathBuf,
    Arc<ProcessService>,
    Arc<RwLock<PermissionMode>>,
    u64,
) {
    let root = root();
    let mode = Arc::new(RwLock::new(PermissionMode::ProjectWrite));
    let sandbox = Arc::new(
        SandboxService::new(root.clone(), SandboxModeSource::Shared(Arc::clone(&mode))).unwrap(),
    );
    let service = Arc::new(ProcessService::new(Project::new(&root), sandbox));
    let generation = service.bind_run("sbx4", CancelToken::new()).unwrap();
    (root, service, mode, generation)
}

fn sbx4_decision(
    tool: &dyn Tool,
    args: &Value,
    mode: Arc<RwLock<PermissionMode>>,
    project: &Project,
) -> crate::PermissionDecision {
    use crate::permission::{ModePolicy, PermissionPolicy};
    let prepared = tool.prepare_execution(args);
    ModePolicy::new(mode).check_prepared(
        project,
        &tool.definition(),
        &crate::ToolCall {
            id: "sbx4".into(),
            name: tool.definition().name,
            arguments: args.clone(),
        },
        prepared.as_ref(),
    )
}

#[test]
fn sbx4_execute_requires_real_provider_and_network_requests_still_ask() {
    let (root, service, mode, generation) = sbx4_fixture();
    let tool = ExecCommandTool {
        service: Arc::clone(&service),
    };
    let project = Project::new(&root);
    let decision = sbx4_decision(
        &tool,
        &json!({"cmd":"echo sbx4"}),
        Arc::clone(&mode),
        &project,
    );
    assert_eq!(
        matches!(decision, crate::PermissionDecision::Allow),
        cfg!(any(target_os = "macos", windows))
    );
    for args in [
        json!({"cmd":"echo sbx4","network":true}),
        json!({"cmd":"echo sbx4","sandbox":"off"}),
    ] {
        assert!(matches!(
            sbx4_decision(&tool, &args, Arc::clone(&mode), &project),
            crate::PermissionDecision::Ask { .. }
        ));
    }
    service.unbind_run(generation).unwrap();
    service.close().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sbx4_prepared_command_cannot_replan_after_mode_or_run_change() {
    let (root, service, mode, generation) = sbx4_fixture();
    let tool = ExecCommandTool {
        service: Arc::clone(&service),
    };
    let args = json!({"cmd":"echo forbidden"});
    let prepared = tool.prepare_execution(&args).unwrap();
    *mode.write().unwrap() = PermissionMode::FullAccess;
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&root), &CancelToken::new())
            .unwrap_err()
            .to_string()
            .contains("mode changed")
    );
    *mode.write().unwrap() = PermissionMode::ProjectWrite;
    let prepared = tool.prepare_execution(&args).unwrap();
    service.unbind_run(generation).unwrap();
    let next = service.bind_run("new-run", CancelToken::new()).unwrap();
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&root), &CancelToken::new())
            .unwrap_err()
            .to_string()
            .contains("run ended")
    );
    service.unbind_run(next).unwrap();
    service.close().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sbx4_classic_readonly_fullaccess_destructive_and_unprepared_stay_unchanged() {
    use crate::permission::{PermissionPolicy, SafeByDefault};
    let (root, service, mode, generation) = sbx4_fixture();
    let project = Project::new(&root);
    let tool = ExecCommandTool {
        service: Arc::clone(&service),
    };
    let args = json!({"cmd":"echo sbx4"});
    let prepared = tool.prepare_execution(&args).unwrap();
    let mut definition = tool.definition();
    let call = crate::ToolCall {
        id: "classic".into(),
        name: definition.name.clone(),
        arguments: args.clone(),
    };
    assert!(matches!(
        SafeByDefault.check_prepared(&project, &definition, &call, Some(&prepared)),
        crate::PermissionDecision::Ask { .. }
    ));
    *mode.write().unwrap() = PermissionMode::ReadOnly;
    assert!(matches!(
        sbx4_decision(&tool, &args, Arc::clone(&mode), &project),
        crate::PermissionDecision::Ask { .. }
    ));
    *mode.write().unwrap() = PermissionMode::FullAccess;
    assert!(matches!(
        sbx4_decision(&tool, &args, Arc::clone(&mode), &project),
        crate::PermissionDecision::Allow
    ));
    *mode.write().unwrap() = PermissionMode::ProjectWrite;
    let policy = crate::permission::ModePolicy::new(mode);
    definition.effect = ToolEffect::Destructive;
    assert!(matches!(
        policy.check_prepared(&project, &definition, &call, Some(&prepared)),
        crate::PermissionDecision::Ask { .. }
    ));
    definition.effect = ToolEffect::Execute;
    assert!(matches!(
        policy.check_prepared(&project, &definition, &call, None),
        crate::PermissionDecision::Ask { .. }
    ));
    service.unbind_run(generation).unwrap();
    service.close().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sbx4_mode_guard_covers_admitted_operation_and_plan_is_single_use() {
    let (root, service, mode, generation) = sbx4_fixture();
    service.sandbox_for_test_mode_guard(&mode);
    let tool = ExecCommandTool {
        service: Arc::clone(&service),
    };
    let args = json!({"cmd":"echo bounded","yield_time_ms":250});
    let prepared = tool.prepare_execution(&args).unwrap();
    assert!(
        prepared
            .tool
            .invoke(
                &json!({"cmd":"echo changed"}),
                &Project::new(&root),
                &CancelToken::new()
            )
            .unwrap_err()
            .to_string()
            .contains("arguments changed")
    );
    prepared
        .tool
        .invoke(&args, &Project::new(&root), &CancelToken::new())
        .unwrap();
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&root), &CancelToken::new())
            .unwrap_err()
            .to_string()
            .contains("already consumed")
    );
    service.unbind_run(generation).unwrap();
    service.close().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sbx4_facts_discriminate_provider_mode_and_enforcement() {
    use crate::sandbox::{SandboxFacts, SandboxLevel};
    let mut facts = SandboxFacts {
        provider: "seatbelt".into(),
        mode: SandboxLevel::WorkspaceWrite,
        enforcement: "full".into(),
        policy_digest: None,
        fallback_reason: None,
    };
    assert!(enforced_workspace(&facts));
    facts.provider = "none".into();
    assert!(!enforced_workspace(&facts));
    facts.provider = "windows-acl".into();
    facts.enforcement = "partial".into();
    assert!(enforced_workspace(&facts));
    facts.mode = SandboxLevel::ReadOnly;
    assert!(!enforced_workspace(&facts));
    facts.mode = SandboxLevel::WorkspaceWrite;
    facts.enforcement = "unusable".into();
    assert!(!enforced_workspace(&facts));
}

#[test]
fn sbx4_stdin_inherits_same_run_enforcement_and_cannot_cross_runs() {
    let (root, service, mode, generation) = sbx4_fixture();
    let project = Project::new(&root);
    let id = service
        .start(ProcessStart {
            command: "echo done".into(),
            workdir: None,
            tty: false,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    let tool = WriteStdinTool {
        service: Arc::clone(&service),
    };
    let args = json!({"session_id":id});
    assert_eq!(
        matches!(
            sbx4_decision(&tool, &args, Arc::clone(&mode), &project),
            crate::PermissionDecision::Allow
        ),
        cfg!(any(target_os = "macos", windows))
    );
    let prepared = tool.prepare_execution(&args).unwrap();
    service.unbind_run(generation).unwrap();
    let next = service.bind_run("replacement", CancelToken::new()).unwrap();
    assert!(
        prepared
            .tool
            .invoke(&args, &project, &CancelToken::new())
            .unwrap_err()
            .to_string()
            .contains("owner or permission")
    );
    service.unbind_run(next).unwrap();
    service.close().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
