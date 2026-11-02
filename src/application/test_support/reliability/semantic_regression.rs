//! Frozen failed synthetic selection, replayed through the real compactor.
use super::*;
#[test]
#[ignore = "paid GLM regression from authorized synthetic campaign; explicitly arm CLAT_PHASE9_LIVE"]
fn live_glm_checkpoint_retains_global_facts() {
    if std::env::var("CLAT_PHASE9_LIVE").as_deref() != Ok("1") {
        eprintln!("phase9 semantic regression NOT_ARMED");
        return;
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("AGENTS.md").exists())
        .unwrap();
    let source = repo.join("output/verification/plugin-phase9/campaign-v4/tools-candidate-0");
    let events = super::super::tests::load_events(&source.join("state"));
    let index = events
        .windows(2)
        .position(|p| {
            p[0].event_type == "compaction/start"
                && p[0].data["turn"] == 6
                && p[1].event_type == "compaction/summary"
        })
        .unwrap();
    let ids = events[index + 1].data["shadowedSeqs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    let surface = crate::session::surface::Surface::fold(&events[..index]).unwrap();
    let mut nodes =
        crate::session::adapter::surface_to_model_items_with_seq(&events[..index], &surface)
            .unwrap()
            .into_iter()
            .filter(|(seq, _)| ids.contains(seq))
            .map(|(seq, item)| crate::plugins::services::CompactionNode { seq, item })
            .collect::<Vec<_>>();
    assert!(!nodes.iter().any(|n| {
        crate::model::model_item_image_parts(&n.item)
            .next()
            .is_some()
    }));
    nodes.push(crate::plugins::services::CompactionNode {
        seq: events[index].seq + 1,
        item: crate::ModelItem::user_text("preserved tail outside the frozen selection"),
    });
    let out = PathBuf::from(std::env::var_os("CLAT_PHASE9_OUTPUT").unwrap());
    std::fs::create_dir(&out).unwrap();
    let project = Project::new(out.join("project"));
    std::fs::create_dir(project.root()).unwrap();
    let storage = out.join("state");
    std::fs::create_dir(&storage).unwrap();
    exercise::policy(&storage);
    let capture = Arc::new(model::Capture::new(out.clone()));
    capture.stage("frozen-semantic-regression");
    let state = Arc::new(tools::State::default());
    let mut app = exercise::mount(&project, &storage, &capture, &state).unwrap();
    exercise::configure(&app, 24000);
    app.tools = Arc::new(crate::tool::ToolRegistry::new());
    let (config, credentials) = app.model_state().unwrap();
    let mut captured = std::fs::read_dir(&source)
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-start.json")
        })
        .collect::<Vec<_>>();
    captured.sort();
    let snapshot = captured
        .iter()
        .map(|p| serde_json::from_slice::<Value>(&std::fs::read(p).unwrap()).unwrap())
        .find(|r| r["summary"] == true && r["stage"] == "recall-active")
        .unwrap();
    assert!(snapshot["tools"].as_array().unwrap().is_empty());
    let instructions = snapshot["instructions"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let definitions = Vec::new();
    let result =
        app.compactor
            .as_ref()
            .unwrap()
            .compact(crate::plugins::services::CompactionRequest {
                config: &config,
                credentials: &credentials,
                nodes: &nodes,
                measured_input: None,
                protected_boundaries: vec![],
                active_checkpoints: None,
                protected_head: None,
                spend_ledger: None,
                instructions,
                tool_definitions: definitions,
                force: true,
                cancel: crate::CancelToken::new(),
            });
    let summary = result.summary.unwrap_or_default();
    let facts = [
        "jsonl",
        "artifacts/星河/export-v2.jsonl",
        "clat exec -- '核对账单'",
        "E_RATE_429: retry-after=17",
        "林岚",
        "r9",
        "unchanged",
        "ORION-700",
    ];
    let missing = facts
        .iter()
        .filter(|f| !summary.contains(**f))
        .collect::<Vec<_>>();
    write_new(
        &out.join("result.json"),
        &json!({"missing":missing,"summary":summary,"degraded":result.degraded.as_ref().map(|r|r.raw.clone()),"source":"frozen candidate0 turn6 selection","eight_sections":summary.lines().filter(|l|l.starts_with("## ")).count()==8}),
    );
    app.close().unwrap();
    assert!(missing.is_empty(), "global task facts missing: {missing:?}");
    assert_eq!(summary.lines().filter(|l| l.starts_with("## ")).count(), 8);
}
