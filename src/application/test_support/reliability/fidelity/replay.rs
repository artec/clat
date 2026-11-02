//! Rejudge captured responses with protocol variants, without making new requests.
use super::*;

#[test]
#[ignore = "requires local shadow output; no model or Host calls"]
fn fidelity_replay_captured_verdicts_without_modifying_original_results() {
    let Some(root) = local_evidence_root() else {
        return;
    };
    let manifest = read(&root.join("preregistration.json"));
    let mut rows = Vec::new();
    for case in manifest["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
        let directory = root.join(id);
        let input = read(&directory.join("packet.json"));
        let packet: Packet = serde_json::from_value(input["packet"].clone()).unwrap();
        use sha2::{Digest, Sha256};
        assert_eq!(
            case["packet_sha256"],
            format!("{:x}", Sha256::digest(serde_json::to_vec(&packet).unwrap())),
            "source packets must remain frozen"
        );
        assert_eq!(
            case["expected_supported"], input["expected_supported"],
            "original labels stay frozen; adjudications belong in a separate report"
        );
        let end = read(&directory.join("request-0000-end.json"));
        let mut response = crate::test_support::response(
            end["outcome"]["text"].as_str().unwrap_or_default(),
            crate::FinishReason::Completed,
        );
        response.finish_reason = match end["outcome"]["finish"].as_str() {
            Some("Completed") => crate::FinishReason::Completed,
            Some("MaxTokens") => crate::FinishReason::MaxTokens,
            _ => crate::FinishReason::Cancelled,
        };
        response.tool_calls =
            serde_json::from_value(end["outcome"]["tool_calls"].clone()).unwrap_or_default();
        let cancelled = end["cancelled"] == true;
        let strict = contract::admit(&response, &packet, cancelled);
        let wrapped = contract::admit_single_fence(&response, &packet, cancelled);
        rows.push(json!({"id":id,"label_scope":if id.starts_with("saved-"){"initial record label, requires adjudication"}else{"frozen synthetic specification"},"expected_supported":case["expected_supported"],"strict_supported":strict.as_ref().ok().map(|v|v.verdict==contract::Verdict::Supported),"strict_error":strict.as_ref().err(),"single_fence_supported":wrapped.as_ref().ok().map(|v|v.verdict==contract::Verdict::Supported),"single_fence_error":wrapped.as_ref().err(),"lexical_risks":reference_risks(&packet)}));
    }
    let synthetic = rows
        .iter()
        .filter(|r| r["label_scope"] == "frozen synthetic specification")
        .collect::<Vec<_>>();
    let mut metrics = Vec::new();
    for key in ["strict_supported", "single_fence_supported"] {
        metrics.push(json!({"protocol":key,"synthetic_count":synthetic.len(),"valid_blocked":synthetic.iter().filter(|r|r["expected_supported"]==true && r[key]!=true).count(),"bad_approved":synthetic.iter().filter(|r|r["expected_supported"]==false && r[key]==true).count(),"technical_failures":rows.iter().filter(|r|r[key].is_null()).count()}));
    }
    let report = root.join(format!("protocol-replay-{}.json", uuid::Uuid::new_v4()));
    write_new(
        &report,
        &json!({"rows":rows,"metrics":metrics,"note":"one JSON fence can reduce framing failures but cannot establish semantic truth; original labels/results remain unchanged; saved case adjudications separate"}),
    );
    eprintln!(
        "fidelity replay: {}",
        report.file_name().unwrap().to_string_lossy()
    );
    assert_eq!(rows.len(), 25);
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
