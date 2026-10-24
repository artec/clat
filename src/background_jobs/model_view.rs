//! Bounded model projection; full observations remain in the Host read port.
use super::*;
pub(super) fn job(view: &JobView) -> Value {
    let mut result = json!({
        "id":view.id, "reference":view.reference, "session":view.session,
        "label":view.label, "state":view.state, "started_at":view.started_at,
        "finished_at":view.finished_at,
        "persistence_error":view.persistence_error.as_ref().map(|s|clip(s,160)),
        "cleanup_error":view.cleanup_error.as_ref().map(|s|clip(s,160)),
        "observation":view.observation,
    });
    if let Some(observation) = result["observation"].as_object_mut() {
        // Optional diagnostics never displace exit facts, verification receipts
        // or references. The user read port retains the complete observation.
        for field in ["diagnostic", "failure_context", "verification_guidance"] {
            observation.remove(field);
        }
    }
    result
}
pub(super) fn list(views: Vec<JobView>) -> Value {
    let total = views.len();
    let mut views = views;
    views.sort_by_key(|v| std::cmp::Reverse(v.started_at));
    let mut jobs = Vec::new();
    for view in views {
        let next = job(&view);
        if json!({"jobs":jobs,"next":next}).to_string().chars().count() > 6000 {
            break;
        }
        jobs.push(next);
    }
    json!({"total":total,"truncated":jobs.len()<total,"jobs":jobs,
        "next_action":"Use job_output with a known job ID to read its observation; the user jobs panel lists all retained jobs."})
}
pub(super) fn page(value: JobOutput) -> Value {
    let mut result = json!({"job":job(&value.job),"text":value.text,
        "next_offset":value.next_offset,"lost":value.lost,"more":value.more});
    // Escape expansion and noisy metadata share one budget. Reducing text here
    // would advance its cursor past unseen bytes, so the reader supplies a
    // smaller page instead; only optional label text may be clipped.
    if result.to_string().chars().count() > 7500 {
        result["job"]["label"] = json!(clip(&value.job.label, 40));
    }
    result
}
fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
