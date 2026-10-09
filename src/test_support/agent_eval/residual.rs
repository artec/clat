//! Independent OS witness for the offline campaign's ordinary Unix process group.
use std::process::{Command, Stdio};

fn group_is_gone(group: i32) -> Result<bool, String> {
    assert!(group > 1 && group != unsafe { libc::getpgrp() });
    if unsafe { libc::kill(-group, 0) } == 0 {
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(true)
    } else {
        Err(format!("process-group visibility unknown: {error}"))
    }
}

struct OwnedGroup(i32);

impl Drop for OwnedGroup {
    fn drop(&mut self) {
        if self.0 > 1 && self.0 != unsafe { libc::getpgrp() } {
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
}

#[test]
fn live_process_group_cannot_be_reported_as_clean() {
    use std::os::unix::process::CommandExt;
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exec sleep 30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let _emergency = OwnedGroup(child.id() as i32);
    let result = group_is_gone(child.id() as i32);
    child.kill().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "child reap deadline");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        !result.unwrap(),
        "a live child must fail the residual oracle"
    );
}

use super::*;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Witness {
    group: Option<OwnedGroup>,
    pids: Vec<i32>,
    scratch: Option<PathBuf>,
    steps: usize,
}

struct ProbeScript {
    project: PathBuf,
    witness: Mutex<Witness>,
}

impl ProbeScript {
    fn observe(&self, state: &mut Witness) -> Result<(), crate::ModelError> {
        let canary = state
            .scratch
            .as_ref()
            .ok_or_else(|| crate::ModelError::request("no captured scratch"))?
            .join("eval-canary");
        if std::fs::read(canary).ok().as_deref() != Some(b"canary") {
            return Err(crate::ModelError::request("scratch canary was not written"));
        }
        for name in ["owner.pid", "child.pid"] {
            let pid = std::fs::read_to_string(self.project.join(name))
                .map_err(|e| crate::ModelError::request(e.to_string()))?
                .trim()
                .parse::<i32>()
                .map_err(|e| crate::ModelError::request(e.to_string()))?;
            let group = unsafe { libc::getpgid(pid) };
            if pid <= 1 || group <= 1 || group == unsafe { libc::getpgrp() } {
                return Err(crate::ModelError::request("invalid owned process witness"));
            }
            if let Some(owned) = &state.group {
                if owned.0 != group {
                    return Err(crate::ModelError::request("child escaped the owned group"));
                }
            } else {
                state.group = Some(OwnedGroup(group));
            }
            state.pids.push(pid);
        }
        Ok(())
    }
}

impl TestModelScript for ProbeScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        _events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, crate::ModelError> {
        let mut state = self.witness.lock().unwrap();
        state.steps += 1;
        match state.steps {
            1 => {
                let scratch = request
                    .instructions
                    .unwrap_or_default()
                    .split_once("use the private scratch directory ")
                    .and_then(|(_, tail)| tail.split_once(". It is removed"))
                    .map(|(path, _)| PathBuf::from(path))
                    .ok_or_else(|| crate::ModelError::request("missing run scratch guidance"))?;
                if !scratch.is_dir() {
                    return Err(crate::ModelError::request("scratch was not created"));
                }
                let canary = scratch.join("eval-canary");
                let quoted_canary =
                    format!("'{}'", canary.to_string_lossy().replace('\'', "'\\''"));
                state.scratch = Some(scratch);
                let mut response = crate::test_support::response("", FinishReason::ToolCalls);
                response.tool_calls.push(ToolCall {
                    id: "owned-command".into(),
                    name: "exec_command".into(),
                    arguments: serde_json::json!({
                        "cmd": format!("printf canary > {quoted_canary}; printf $$ > owner.pid; trap 'kill \"$kid\" 2>/dev/null; wait \"$kid\"; exit 0' TERM; sleep 30 & kid=$!; printf $kid > child.pid; wait"),
                        "yield_time_ms": 1000,
                        "sandbox": "auto"
                    }),
                });
                Ok(response)
            }
            2 => {
                self.observe(&mut state)?;
                Ok(crate::test_support::response(
                    "probe complete",
                    FinishReason::Completed,
                ))
            }
            _ => Err(crate::ModelError::request("unexpected probe request")),
        }
    }
}

fn wait_for_cleanup(witness: &Witness) -> Result<(), String> {
    let group = witness
        .group
        .as_ref()
        .ok_or("no live process witness captured")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let mut pids_gone = true;
        for pid in &witness.pids {
            if unsafe { libc::kill(*pid, 0) } == 0 {
                pids_gone = false;
            } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                return Err("PID visibility unknown".into());
            }
        }
        if pids_gone && group_is_gone(group.0)? {
            break;
        }
        if Instant::now() >= deadline {
            return Err("owned PID/process group remains after Application close".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let scratch = witness.scratch.as_ref().ok_or("no scratch witness")?;
    match std::fs::symlink_metadata(scratch) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("scratch visibility unknown: {error}")),
        Ok(_) => return Err("captured scratch/canary remains after run".into()),
    }
    Ok(())
}

#[test]
fn application_teardown_reaps_owned_group_and_removes_scratch() {
    let (storage, project) = roots("agent-eval-residual");
    let _temp = TempTree(storage.parent().unwrap().to_owned());
    std::fs::create_dir_all(&project).unwrap();
    let script = Arc::new(ProbeScript {
        project: project.clone(),
        witness: Mutex::new(Witness::default()),
    });
    let mut app = BootstrapApplication::open(Project::new(&project), storage)
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Scripted(script.clone()),
        }))
        .unwrap();
    configure_test_model(&app);
    let (completion, receiver) = mpsc::channel();
    let attempt = app.start_run(ApplicationRunRequest {
        message: crate::message::PendingMessage::text("Run the frozen offline resource probe"),
        approver: Arc::new(AllowAllApprover),
        asker: None,
        events: Box::new(Vec::<RunEvent>::new()),
        completion,
    });
    let outcome = attempt.map_err(|e| e.to_string()).and_then(|handle| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !handle.is_finished() {
            handle.cancel();
            return Err("probe run deadline".into());
        }
        handle.join().map_err(|e| e.to_string())?;
        receiver
            .recv_timeout(Duration::from_secs(1))
            .map_err(|e| e.to_string())
    });
    let closed = app.close();
    assert!(closed.is_ok(), "Application close: {closed:?}");
    assert!(outcome.unwrap().is_ok(), "probe run failed");
    let witness = script.witness.lock().unwrap();
    assert_eq!(witness.steps, 2);
    assert_eq!(witness.pids.len(), 2);
    wait_for_cleanup(&witness).unwrap();
}
