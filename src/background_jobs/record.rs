use super::*;
use crate::session::{catalog::JobPhase, event::SessionEvent, run_journal::NewSessionEvent};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRecord {
    scope: String,
    version: u32,
    project: String,
    session: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    observation: Option<Value>,
}
pub(crate) fn validate(event: &SessionEvent) -> Result<(), String> {
    if event.data["owner"] != OWNER {
        return Ok(());
    }
    let native: NativeRecord =
        serde_json::from_value(event.data["native"].clone()).map_err(|e| e.to_string())?;
    if event.ignorable == Some(true)
        || native.scope.len() != 64
        || !native.scope.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("invalid native job record authority".into());
    }
    if native.version != 1
        || event.data["kind"] != KIND
        || native.project.is_empty()
        || native.session.is_empty()
        || native.session.len() > 1024
    {
        return Err("invalid native job identity".into());
    }
    if uuid::Uuid::parse_str(event.data["jobId"].as_str().unwrap_or("")).is_err()
        || serde_json::to_vec(&event.data)
            .map_err(|e| e.to_string())?
            .len()
            > 32 * 1024
    {
        return Err("native job record exceeds its identity or size bound".into());
    }
    let phase = crate::session::catalog::event_spec(&event.event_type).and_then(|s| s.job_phase);
    if event.data["startedAt"].as_i64().is_none() {
        return Err("native startedAt must be an integer".into());
    }
    match phase {
        Some(JobPhase::Registered)
            if native.state.is_none()
                && native.observation.is_none()
                && event.data.get("finishedAt").is_none() => {}
        Some(JobPhase::Settled)
            if native.state.is_some() && event.data["finishedAt"].as_i64().is_some() => {}
        _ => return Err("native job phase and observation disagree".into()),
    }
    if native.state.as_deref() == Some("completed")
        && native.observation.as_ref().is_none_or(|o| {
            o["exit_code"] != 0
                || o["timed_out"] == true
                || o["cancelled"] == true
                || o["terminated"] == true
        })
    {
        return Err("completed native job lacks a successful process exit".into());
    }
    if let Some(state) = native.state.as_deref() {
        if !matches!(state, "completed" | "failed" | "killed" | "interrupted") {
            return Err("invalid native job state".into());
        }
        let status = match state {
            "completed" => "completed",
            "killed" => "killed",
            _ => "failed",
        };
        let cause = match state {
            "interrupted" => "teardown",
            "killed" => "kill",
            _ => "producer",
        };
        if event.data["status"] != status
            || event.data["cause"] != cause
            || event.data["producerReported"] != (state != "interrupted")
        {
            return Err("native and interoperable job outcomes disagree".into());
        }
    }
    Ok(())
}
pub(super) fn event(view: &JobView, phase: JobPhase) -> NewSessionEvent {
    let native = NativeRecord {
        scope: view.reference.scope.clone(),
        version: 1,
        project: view.project.clone(),
        session: view.session.clone(),
        state: view.finished_at.map(|_| view.state.clone()),
        observation: view.observation.clone(),
    };
    let mut data = json!({"jobId":view.id,"owner":OWNER,"kind":KIND,"label":view.label,"startedAt":view.started_at,"native":native});
    if phase == JobPhase::Settled {
        data["finishedAt"] = json!(view.finished_at.unwrap_or_else(now));
        data["status"] = json!(match view.state.as_str() {
            "completed" => "completed",
            "killed" => "killed",
            _ => "failed",
        });
        data["cause"] = json!(match view.state.as_str() {
            "interrupted" => "teardown",
            "killed" => "kill",
            _ => "producer",
        });
        data["producerReported"] = json!(view.state != "interrupted");
    }
    NewSessionEvent::new(phase.event_name(), data)
}
pub(super) fn view(event: &SessionEvent, phase: JobPhase) -> Option<JobView> {
    if event.data["owner"] != OWNER || event.ignorable == Some(true) {
        return None;
    }
    let native: NativeRecord = serde_json::from_value(event.data["native"].clone()).ok()?;
    let id = event.data["jobId"].as_str()?.to_owned();
    Some(JobView {
        reference: crate::task::JobReference {
            provider: "clat-native".into(),
            scope: native.scope.clone(),
            id: id.clone(),
            result_ref: format!("clat-job:{}:{id}", native.scope),
            last_observation: native.state.clone().unwrap_or_else(|| "registered".into()),
        },
        id: id.clone(),
        project: native.project,
        session: native.session,
        label: event.data["label"].as_str()?.into(),
        state: native.state.unwrap_or_else(|| "running".into()),
        started_at: event.data["startedAt"].as_i64()?,
        finished_at: event.data.get("finishedAt").and_then(Value::as_i64),
        observation: native.observation,
        persistence_error: None,
        cleanup_error: None,
        notification_id: (phase == JobPhase::Settled).then(|| format!("job:{id}:settled")),
        notification_error: None,
    })
}
