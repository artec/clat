//! Read-only reconstruction of failed synthetic requests; no Host or tool execution.
use super::*;
use crate::Tool;
use crate::model::{ModelOptions, ModelRequest};
use std::collections::BTreeMap;

const CASES: [(&str, &str, usize, bool); 8] = [
    ("cold-0", "tools-candidate-2", 18, false),
    ("cold-1", "tools-candidate-2", 18, false),
    ("cold-2", "tools-candidate-2", 18, false),
    ("max-original", "tools-candidate-1", 17, false),
    ("max-without-thinking", "tools-candidate-1", 17, true),
    ("conditional", "requirements-candidate-0", 15, false),
    ("unrevoked", "requirements-candidate-2", 16, false),
    ("image-identifier", "images-candidate-0", 14, false),
];

struct Frozen {
    snapshot: Value,
    items: Vec<crate::ModelItem>,
    definitions: Vec<crate::ToolDefinition>,
    source_hash: String,
}
fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("AGENTS.md").exists())
        .unwrap()
}
impl Frozen {
    fn load(case: &str, number: usize) -> Self {
        let root = repo()
            .join("output/verification/plugin-phase9/campaign-v5")
            .join(case);
        let bytes = std::fs::read(root.join(format!("request-{number:04}-start.json"))).unwrap();
        let snapshot: Value = serde_json::from_slice(&bytes).unwrap();
        let events = super::super::tests::load_events(&root.join("state"));
        let mut known = BTreeMap::new();
        for end in 1..=events.len() {
            let prefix = &events[..end];
            let surface = crate::session::surface::Surface::fold(prefix).unwrap();
            for (_, item) in
                crate::session::adapter::surface_to_model_items_with_seq(prefix, &surface).unwrap()
            {
                known.insert(format!("{item:?}"), item);
            }
        }
        let items = snapshot["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                let debug = v.as_str().unwrap();
                let item = known
                    .get(debug)
                    .cloned()
                    .or_else(|| text_item(debug))
                    .expect("frozen item must match original journal or an exact single-text item");
                assert_eq!(format!("{item:?}"), debug);
                assert!(
                    crate::model::model_item_image_parts(&item).next().is_none(),
                    "these frozen selections contain no local images"
                );
                item
            })
            .collect();
        let state = Arc::new(tools::State::default());
        let definitions = [
            tools::Resource(state.clone()).definition(),
            tools::Publish(state).definition(),
        ];
        let definitions = snapshot["tools"].as_array().unwrap().iter().map(|v| {
            let definition = definitions.iter().find(|d| d.name == v["name"]).unwrap().clone();
            assert_eq!(v, &json!({"name":definition.name,"description":definition.description,"schema":definition.input_schema,"effect":definition.effect.to_string(),"strict":definition.strict}));
            definition
        }).collect();
        Self {
            snapshot,
            items,
            definitions,
            source_hash: format!("{:x}", sha2::Sha256::digest(&bytes)),
        }
    }
    fn options(&self) -> ModelOptions {
        ModelOptions {
            output_limit: self.snapshot["output_limit"].as_u64().map(|v| v as u32),
            temperature: self.snapshot["temperature"].as_f64(),
            parallel_tool_calls: self.snapshot["parallel_tool_calls"].as_bool(),
            provider_options: self.snapshot["provider_options"].clone(),
            ..Default::default()
        }
    }
}
fn text_item(debug: &str) -> Option<crate::ModelItem> {
    let encoded = debug
        .strip_prefix("User { content: [Text(")?
        .strip_suffix(")] }")?;
    let text: String = serde_json::from_str(encoded).ok()?;
    Some(crate::ModelItem::user_text(text))
}

#[test]
#[ignore = "local synthetic evidence required; offline reconstruction with no model requests"]
fn diagnosis_freezes_original_requests_without_reinterpreting_debug_text() {
    if !local_evidence_armed() {
        return;
    }
    for (_, case, number, _) in CASES {
        let frozen = Frozen::load(case, number);
        assert_eq!(
            frozen.items.len(),
            frozen.snapshot["items"].as_array().unwrap().len()
        );
        assert_eq!(
            crate::model::estimate_request_tokens(
                frozen.snapshot["instructions"].as_str(),
                &frozen.items,
                &frozen.definitions
            ),
            frozen.snapshot["estimate"].as_u64().unwrap()
        );
    }
    assert!(text_item("User { content: [Text(\"🟩\\nline\")] }").is_some());
    assert!(text_item("User { content: [Text(\"x\"), Image(secret)] }").is_none());
}

#[test]
#[ignore = "paid bounded GLM diagnosis; explicitly arm CLAT_PHASE9_LIVE"]
fn live_glm_frozen_failure_diagnosis() {
    if std::env::var("CLAT_PHASE9_LIVE").as_deref() != Ok("1") {
        eprintln!("phase9 diagnosis NOT_ARMED");
        return;
    }
    let out =
        PathBuf::from(std::env::var_os("CLAT_PHASE9_OUTPUT").expect("new absolute output path"));
    assert!(out.is_absolute());
    std::fs::create_dir(&out).unwrap();
    write_new(
        &out.join("preregistration.json"),
        &json!({"cases":CASES,"model":"glm-5.3-flash","policy":"eight direct attempts; no tool execution, Host, network retries or retry-to-green; independent raw SSE parity"}),
    );
    let mut rows = Vec::new();
    for (name, case, number, disable_thinking) in CASES {
        rows.push(run_frozen(
            &out,
            name,
            case,
            number,
            disable_thinking,
            false,
        ));
    }
    write_new(
        &out.join("diagnosis.json"),
        &json!({"rows":rows,"acceptance":"diagnostic evidence, not full campaign acceptance"}),
    );
    assert!(
        rows.iter().all(|r| r["wire_parity"] == true),
        "raw / parsed parity failure requires investigation"
    );
}

fn run_frozen(
    out: &Path,
    name: &str,
    case: &str,
    number: usize,
    disable_thinking: bool,
    current_directive: bool,
) -> Value {
    let mut frozen = Frozen::load(case, number);
    if current_directive {
        assert_eq!(frozen.snapshot["summary"], true);
        let config = crate::ModelConfig {
            max_context_tokens: Some(24000),
            ..Default::default()
        };
        *frozen.items.last_mut().unwrap() =
            crate::ModelItem::user_text(crate::plugins::directive_for_test(&config));
    }
    let directory = out.join(name);
    std::fs::create_dir(&directory).unwrap();
    write_new(
        &directory.join("frozen.json"),
        &json!({"case":case,"request":number,"source_sha256":frozen.source_hash,"snapshot":frozen.snapshot,"neutral_items":frozen.items}),
    );
    let capture = Arc::new(model::Capture::new(directory.clone()));
    capture.stage(name);
    let mut config = crate::ModelConfig::default();
    crate::preset_by_id("glm-5.3-flash")
        .unwrap()
        .apply(&mut config);
    if frozen.snapshot["summary"] == true {
        config = crate::model::internal_text_config(&config);
    }
    if disable_thinking {
        config.extra_body["thinking"] = json!({"type":"disabled"});
    }
    let mut model = model::build(&config, &capture);
    let options = frozen.options();
    let cancel = crate::CancelToken::with_deadline(
        std::time::Instant::now() + std::time::Duration::from_secs(180),
    );
    let mut events = Vec::new();
    let result = model.stream(
        ModelRequest {
            instructions: frozen.snapshot["instructions"].as_str(),
            items: &frozen.items,
            tools: &frozen.definitions,
            options: &options,
            cancel: &cancel,
        },
        &mut events,
    );
    let wire: Value =
        serde_json::from_slice(&std::fs::read(directory.join("request-0000-wire.json")).unwrap())
            .unwrap();
    let parity = parity(&wire, result.as_ref().ok());
    let expected = corpus::roster()
        .into_iter()
        .find(|c| c.id() == case)
        .unwrap()
        .expected();
    let facts = result
        .as_ref()
        .ok()
        .filter(|_| frozen.snapshot["summary"] != true)
        .map(|r| recompute::judge(&r.text, &expected));
    let structure = result
        .as_ref()
        .ok()
        .is_some_and(|r| crate::plugins::valid_checkpoint_structure(&r.text));
    let row = json!({"name":name,"source":case,"request":number,"disable_thinking":disable_thinking,"current_directive":current_directive,"wire_parity":parity,"summary_structure":structure,"facts":facts,"error":result.as_ref().err().map(|e|e.to_string()),"note":"summary facts and conditional scope require separate semantic review"});
    write_new(&directory.join("result.json"), &row);
    row
}

#[test]
#[ignore = "paid three-class scope regression; explicitly arm CLAT_PHASE9_LIVE"]
fn live_glm_summary_scope_diagnosis() {
    if std::env::var("CLAT_PHASE9_LIVE").as_deref() != Ok("1") {
        eprintln!("phase9 summary scope NOT_ARMED");
        return;
    }
    let cases = [
        ("conditional", "requirements-candidate-0", 13),
        ("unrevoked", "requirements-candidate-2", 16),
        ("image-identifier", "images-candidate-0", 14),
    ];
    let out = PathBuf::from(std::env::var_os("CLAT_PHASE9_OUTPUT").unwrap());
    assert!(out.is_absolute());
    std::fs::create_dir(&out).unwrap();
    write_new(
        &out.join("preregistration.json"),
        &json!({"cases":cases,"policy":"three frozen requests with current production directive; original prefix and item contents retained; no tools executed"}),
    );
    let rows = cases
        .into_iter()
        .map(|(name, case, n)| run_frozen(&out, name, case, n, false, true))
        .collect::<Vec<_>>();
    write_new(
        &out.join("scope.json"),
        &json!({"rows":rows,"note":"semantic review required, no retry-to-green"}),
    );
    assert!(
        rows.iter()
            .all(|r| r["wire_parity"] == true && r["summary_structure"] == true)
    );
}

fn parity(wire: &Value, response: Option<&crate::ModelResponse>) -> bool {
    let Some(response) = response else {
        return false;
    };
    if wire["overflow"] != false {
        return false;
    }
    let raw = wire["consumed_sse"].as_str().unwrap().replace("\r\n", "\n");
    let mut text = String::new();
    let mut reasoning = String::new();
    for frame in raw.split("\n\n") {
        let data = frame
            .lines()
            .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            return false;
        };
        let delta = &value["choices"][0]["delta"];
        text.push_str(delta["content"].as_str().unwrap_or_default());
        reasoning.push_str(delta["reasoning_content"].as_str().unwrap_or_default());
    }
    let emitted = wire["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "text")
        .filter_map(|e| e["delta"].as_str())
        .collect::<String>();
    text == response.text
        && text == emitted
        && reasoning == response.reasoning.as_deref().unwrap_or_default()
}

use sha2::Digest;
