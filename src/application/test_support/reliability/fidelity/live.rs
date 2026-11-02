use super::*;
use crate::model::{ModelOptions, ModelRequest};
use sha2::{Digest, Sha256};

#[test]
#[ignore = "paid shadow fidelity experiment; arm CLAT_PHASE9_LIVE explicitly"]
fn live_glm_fidelity_shadow_evaluation() {
    if std::env::var("CLAT_PHASE9_LIVE").as_deref() != Ok("1") {
        eprintln!("fidelity shadow NOT_ARMED");
        return;
    }
    let out = PathBuf::from(std::env::var_os("CLAT_PHASE9_OUTPUT").unwrap());
    assert!(out.is_absolute());
    std::fs::create_dir(&out).unwrap();
    let mut cases = corpus::cases();
    cases.extend(saved::cases());
    let manifest=cases.iter().map(|c|json!({"id":c.id,"expected_supported":c.expected_supported,"origin":c.origin,"packet_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&c.packet).unwrap()))})).collect::<Vec<_>>();
    write_new(
        &out.join("preregistration.json"),
        &json!({"cases":manifest,"policy":"one direct audit per fixed case; no Host, tools, production gate, or retry-to-green; labelled expectations not sent to model"}),
    );
    let mut rows = Vec::new();
    for case in cases {
        rows.push(evaluate(&out, &case));
    }
    let supported = rows
        .iter()
        .filter(|r| r["expected_supported"] == true)
        .count();
    let false_reject = rows
        .iter()
        .filter(|r| r["expected_supported"] == true && r["audit_supported"] == false)
        .count();
    let missed = rows
        .iter()
        .filter(|r| r["expected_supported"] == false && r["audit_supported"] == true)
        .count();
    let technical = rows
        .iter()
        .filter(|r| r["audit_supported"].is_null())
        .count();
    let legitimate_blocked = rows
        .iter()
        .filter(|r| r["expected_supported"] == true && r["audit_supported"] != true)
        .count();
    write_new(
        &out.join("evaluation.json"),
        &json!({"rows":rows,"supported_cases":supported,"unsupported_cases":rows.len()-supported,"false_rejections":false_reject,"legitimate_blocked_including_technical":legitimate_blocked,"missed_bad_cases":missed,"technical_failures":technical,"verdict":"shadow evidence only; do not enable a production gate from a single batch"}),
    );
    assert_eq!(
        technical, 0,
        "all invalid or incomplete audits stay visible; never count as accepted"
    );
}

fn evaluate(out: &Path, case: &Case) -> Value {
    let directory = out.join(&case.id);
    std::fs::create_dir(&directory).unwrap();
    write_new(
        &directory.join("packet.json"),
        &json!({"packet":case.packet,"expected_supported":case.expected_supported,"origin":case.origin}),
    );
    let capture = Arc::new(model::Capture::new(directory.clone()));
    capture.stage("fidelity-audit");
    let mut config = crate::ModelConfig::default();
    crate::preset_by_id("glm-5.3-flash")
        .unwrap()
        .apply(&mut config);
    config = crate::model::internal_text_config(&config);
    let items = [crate::ModelItem::user_text(
        serde_json::to_string(&case.packet).unwrap(),
    )];
    let options = ModelOptions {
        output_limit: Some(2048),
        ..Default::default()
    };
    let estimated = crate::model::estimate_request_tokens(Some(contract::GUIDANCE), &items, &[]);
    if estimated + 2048 + 512 > 24000 {
        return json!({"id":case.id,"expected_supported":case.expected_supported,"audit_supported":null,"error":"full audit does not fit declared 24k trial capacity"});
    }
    let cancel = crate::CancelToken::with_deadline(
        std::time::Instant::now() + std::time::Duration::from_secs(180),
    );
    let mut model = model::build(&config, &capture);
    let response = model.stream(
        ModelRequest {
            instructions: Some(contract::GUIDANCE),
            items: &items,
            tools: &[],
            options: &options,
            cancel: &cancel,
        },
        &mut Vec::new(),
    );
    let verdict = response
        .as_ref()
        .map_err(|e| e.to_string())
        .and_then(|r| contract::admit(r, &case.packet, cancel.is_cancelled()));
    let supported = verdict
        .as_ref()
        .ok()
        .map(|r| r.verdict == contract::Verdict::Supported);
    let lexical = reference_risks(&case.packet);
    let row = json!({"id":case.id,"expected_supported":case.expected_supported,"audit_supported":supported,"audit":verdict.as_ref().ok(),"technical_error":verdict.as_ref().err(),"lexical_risks":lexical,"lexical_would_reject":!lexical.is_empty(),"estimate":estimated});
    write_new(&directory.join("result.json"), &row);
    row
}
