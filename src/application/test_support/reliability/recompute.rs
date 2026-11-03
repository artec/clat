//! Independent recomputation from synthetic journals and captured model responses.
//! No provider, application mount, credentials, or trust writes.
use super::*;
use crate::session::event::SessionEvent;

#[test]
#[ignore = "requires a completed synthetic campaign directory; no model calls"]
fn recompute_saved_long_task_campaign() {
    let Some(root) = local_evidence_root() else {
        return;
    };
    let manifest: Value = read(&root.join("preregistration.json"));
    assert_eq!(
        manifest["source_sha256"],
        corpus::digest(),
        "never judge changed task facts against an earlier campaign"
    );
    let mut rows = Vec::new();
    for case in corpus::roster() {
        rows.push(verify_case(&root, &case));
    }
    let passed = rows.iter().all(|r| r["passed"] == true);
    let report = root.join(format!("recomputed-{}.json", uuid::Uuid::new_v4()));
    write_new(
        &report,
        &json!({"version":1,"source":"original journals and raw responses; original result flags preserved","rows":rows,"passed":passed}),
    );
    eprintln!(
        "phase9 evidence report: {}",
        report.file_name().unwrap().to_string_lossy()
    );
    assert!(
        passed,
        "review every failed raw observation; never retry-to-green"
    );
}

fn verify_case(root: &Path, case: &corpus::Case) -> Value {
    let out = root.join(case.id());
    if !root.join(format!("{}.json", case.id())).exists() {
        return json!({"id":case.id(),"passed":false,"status":"not_completed"});
    }
    let original = read(&root.join(format!("{}.json", case.id())));
    let events = super::super::tests::load_events(&out.join("state"));
    let start5 = boundary(&events, "turn/start", 5);
    let end5 = boundary(&events, "turn/end", 5);
    let end6 = boundary(&events, "turn/end", 6);
    let start7 = boundary(&events, "turn/start", 7);
    let summaries = events
        .iter()
        .filter(|e| e.event_type == "compaction/summary")
        .collect::<Vec<_>>();
    let maintenance=summaries.iter().map(|event|{
        let text=event.data["summary"].as_array().into_iter().flatten().filter_map(|block|block["text"].as_str()).collect::<Vec<_>>().join("\n");
        let replacement=crate::model::estimate_model_item_tokens(&crate::ModelItem::user_text(&text));
        let source=event.data["shadowedTokenCount"].as_u64();
        json!({"seq":event.seq,"source_tokens":source,"replacement_tokens":replacement,"strictly_smaller":source.is_some_and(|n|n>replacement),"eight_sections":checkpoint_body(&text).is_some_and(crate::plugins::valid_checkpoint_structure)})
    }).collect::<Vec<_>>();
    let pre = summaries.iter().filter(|e| e.seq < start5).count();
    let active = summaries
        .iter()
        .filter(|e| e.seq > start5 && e.seq < end5)
        .count();
    let manual = summaries
        .iter()
        .filter(|e| e.seq > end6 && e.seq < start7)
        .count();
    let offloads = events
        .iter()
        .filter(|e| e.event_type == "image/offload")
        .count();
    let checks = [
        "recall-pre-tools",
        "recall-active",
        "recall-manual",
        "recall-cold",
    ]
    .map(|stage| {
        let mut expected = case.expected();
        if stage == "recall-pre-tools" {
            for key in [
                "head",
                "tail",
                "steer_tag",
                "tool_total",
                "unconfirmed_count",
            ] {
                expected.as_object_mut().unwrap().remove(key);
            }
        }
        let replies = captured(&out, stage);
        let text = replies
            .last()
            .and_then(|r| r["outcome"]["text"].as_str())
            .unwrap_or_default();
        json!({"stage":stage,"check":judge(text,&expected)})
    });
    let mut reads = Vec::new();
    let mut reports = Vec::new();
    for e in events.iter().filter(|e| e.event_type == "tool/call") {
        let args: Value = serde_json::from_str(e.data["arguments"].as_str().unwrap()).unwrap();
        match e.data["name"].as_str() {
            Some("phase9_resource") => reads.push(args["id"].as_u64().unwrap()),
            Some("phase9_publish") => reports.push(args["report"].as_str().unwrap().to_owned()),
            _ => {
                return json!({"id":case.id(),"passed":false,"safety_failure":"unexpected durable tool call"});
            }
        }
    }
    reads.sort_unstable();
    let artifact = judge(
        reports.first().map(String::as_str).unwrap_or_default(),
        &case.expected(),
    );
    let steering = events
        .iter()
        .filter(|e| e.event_type == "user/message")
        .any(|e| {
            e.data
                .to_string()
                .contains("新增用户插话事实：steer_tag=STEER_52")
        });
    let cancelled = events.iter().any(|e| {
        e.event_type == "turn/end" && e.data["turn"] == 8 && e.data["reason"]["kind"] == "aborted"
    });
    let replay = exercise::checkpoint_replay(&out.join("state"));
    let capacity = case.control && summaries.is_empty()
        || !case.control && pre > 0 && active > 0 && manual > 0 && summaries.len() >= 3;
    let facts = checks.iter().all(|c| c["check"]["passed"] == true) && artifact["passed"] == true;
    let images = case.family != "images"
        || (case.control || offloads > 0)
            && captured(&out, "evidence")
                .last()
                .and_then(|r| r["outcome"]["text"].as_str())
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("green"));
    let passed = capacity
        && facts
        && images
        && steering
        && cancelled
        && replay
        && original["replay_equal"] == true
        && reads == (0..8).collect::<Vec<u64>>()
        && reports.len() == 1;
    let maintenance_valid = maintenance
        .iter()
        .all(|m| m["strictly_smaller"] == true && m["eight_sections"] == true);
    let captured_guard = captured_summary_guard(&out);
    let call_turns_valid = events
        .iter()
        .filter(|e| e.event_type == "tool/call")
        .all(|e| {
            e.data["turn"]
                == if e.data["name"] == "phase9_publish" {
                    10
                } else {
                    5
                }
        });
    json!({"id":case.id(),"passed":passed&&maintenance_valid&&captured_guard.iter().all(|r|r["valid"]==true)&&call_turns_valid,"original_status":original["status"],"pre":pre,"active":active,"manual":manual,"summaries":summaries.len(),"maintenance":maintenance,"captured_guard":captured_guard,"call_turns_valid":call_turns_valid,"offloads":offloads,"checks":checks,"artifact":artifact,"steering":steering,"cancelled":cancelled,"replay":replay,"image_check":images,"reads":reads,"publications":reports.len()})
}
pub(super) fn checkpoint_body(text: &str) -> Option<&str> {
    crate::plugins::checkpoint_body(text)
}
fn captured_summary_guard(out: &Path) -> Vec<Value> {
    std::fs::read_dir(out).unwrap().map(|f|f.unwrap().path()).filter(|p|p.file_name().unwrap().to_string_lossy().ends_with("-start.json")).filter_map(|p|{
        let start=read(&p);if start["summary"]!=true{return None;}
        let name=p.file_name().unwrap().to_string_lossy().replace("-start.json","-end.json");let path=out.join(name);
        if !path.exists(){return Some(json!({"valid":false,"reason":"missing response"}));}
        let end=read(&path);if end["outcome"]["finish"]!="Completed"{return None;}
        Some(json!({"request":p.file_name().unwrap().to_string_lossy(),"valid":end["outcome"]["text"].as_str().is_some_and(crate::plugins::valid_checkpoint_structure)}))
    }).collect()
}
fn boundary(events: &[SessionEvent], kind: &str, turn: u64) -> u64 {
    events
        .iter()
        .find(|e| e.event_type == kind && e.data["turn"] == turn)
        .map(|e| e.seq)
        .unwrap_or(u64::MAX)
}
fn captured(out: &Path, stage: &str) -> Vec<Value> {
    let mut files = std::fs::read_dir(out)
        .unwrap()
        .map(|r| r.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-end.json")
        })
        .collect::<Vec<_>>();
    files.sort();
    files
        .into_iter()
        .map(|p| read(&p))
        .filter(|v| v["stage"] == stage)
        .collect()
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

// Only the public report fields are allowed; extra completion claims cannot hide
// behind an otherwise correct subset of critical facts.
pub(super) fn judge(text: &str, expected: &Value) -> Value {
    let mut result = corpus::judge(text, expected);
    let fields = corpus::roster()
        .iter()
        .flat_map(|c| {
            c.expected()
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect::<std::collections::BTreeSet<_>>();
    let extras = result["actual"]
        .as_object()
        .map(|o| {
            o.keys()
                .filter(|key| !fields.contains(*key))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !extras.is_empty() {
        result["passed"] = json!(false);
        result["unexpected_fields"] = json!(extras);
    }
    result
}
