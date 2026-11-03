use super::*;

// These labels refer only to the stated critical defect / reviewed valid scope,
// not to a claim of exhaustive semantic correctness of the whole campaign.
pub(super) fn cases() -> Vec<Case> {
    let specs = [
        ("saved-invented-reference", "tools-candidate-0", 13, false),
        (
            "saved-missing-reference",
            "requirements-candidate-2",
            13,
            false,
        ),
        ("saved-tools-valid", "tools-candidate-2", 15, true),
        ("saved-images-valid", "images-candidate-1", 17, true),
        ("saved-user-correction", "requirements-candidate-1", 2, true),
        (
            "saved-complementary-parts",
            "requirements-candidate-1",
            16,
            true,
        ),
    ];
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("AGENTS.md").exists())
        .unwrap();
    specs
        .into_iter()
        .map(|(id, case, n, expected_supported)| {
            let root = repo
                .join("output/verification/plugin-phase9/campaign-v6")
                .join(case);
            let read = |suffix: &str| {
                serde_json::from_slice::<Value>(
                    &std::fs::read(root.join(format!("request-{n:04}-{suffix}.json"))).unwrap(),
                )
                .unwrap()
            };
            let wire = read("wire");
            let end = read("end");
            let messages = wire["request_body"]["messages"].as_array().unwrap();
            let mut sources = Vec::new();
            for (index, message) in messages.iter().enumerate() {
                let content = &message["content"];
                let text = content.as_str().map(str::to_owned).unwrap_or_else(|| {
                    content
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|part| {
                            assert_eq!(
                                part["type"], "text",
                                "text-only audit cannot silently discard image evidence"
                            );
                            part["text"].as_str().unwrap()
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                });
                if index + 1 == messages.len()
                    && text.starts_with("You are summarizing an agent task for context compaction.")
                {
                    continue;
                }
                let calls = message
                    .get("tool_calls")
                    .map(|c| format!("\nTool call records: {c}"))
                    .unwrap_or_default();
                sources.push(Source {
                    role: message["role"].as_str().unwrap().into(),
                    text: format!("{text}{calls}"),
                });
            }
            if let Some(tools) = wire["request_body"].get("tools") {
                sources.push(Source {
                    role: "tool definitions (not execution evidence)".into(),
                    text: tools.to_string(),
                });
            }
            Case {
                id: id.into(),
                packet: Packet {
                    sources,
                    candidate: end["outcome"]["text"].as_str().unwrap().into(),
                },
                expected_supported,
                origin: format!("read-only v6/{case}/request-{n:04}; original files unchanged"),
            }
        })
        .collect()
}

#[test]
#[ignore = "requires local synthetic v6 evidence; no model requests"]
fn fidelity_saved_sources_preserve_the_known_counterexample() {
    if !local_evidence_armed() {
        return;
    }
    let cases = cases();
    let bad = &cases[0];
    let source = bad
        .packet
        .sources
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(source.contains("ORION-700") && !source.contains("ORION-002"));
    assert!(bad.packet.candidate.contains("ORION-002"));
    assert!(!reference_risks(&bad.packet).is_empty());
    assert_eq!(cases.len(), 6);
}

pub(super) struct StoredReplacement {
    pub session: String,
    pub revision: u64,
    pub nodes: Vec<crate::plugins::services::CompactionNode>,
    pub range: std::ops::Range<usize>,
    pub summary: String,
    pub after: Vec<(u64, crate::ModelItem)>,
}

pub(super) fn replacement(case: &str, request: usize) -> StoredReplacement {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("AGENTS.md").exists())
        .unwrap();
    let root = repo
        .join("output/verification/plugin-phase9/campaign-v6")
        .join(case);
    let raw: Value = serde_json::from_slice(
        &std::fs::read(root.join(format!("request-{request:04}-end.json"))).unwrap(),
    )
    .unwrap();
    let summary = raw["outcome"]["text"].as_str().unwrap().to_owned();
    let events = super::super::super::tests::load_events(&root.join("state"));
    let backend = crate::session::persistence::JsonlBackend::new(
        root.join("state/sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let headers = backend.list_headers().unwrap();
    assert_eq!(headers.len(), 1);
    let session = headers[0].id.to_string();
    let framed = crate::plugins::frame_checkpoint(&summary);
    let index = events
        .iter()
        .position(|event| {
            event.event_type == "compaction/summary"
                && event.data["summary"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|part| part["text"] == framed)
        })
        .unwrap();
    let selected = events[index].data["shadowedSeqs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(events[index - 1].event_type, "compaction/start");
    let revision = events[index - 2].seq;
    let project = |prefix: &[_]| {
        let surface = crate::session::surface::Surface::fold(prefix).unwrap();
        crate::session::adapter::surface_to_model_items_with_seq(prefix, &surface).unwrap()
    };
    let before = project(&events[..index]);
    let carrier = index + 1;
    assert_eq!(events[carrier].event_type, "user/message");
    assert!(events[carrier].surface_op.is_some());
    let after = project(&events[..=carrier]);
    assert!(
        after
            .iter()
            .any(|(_, item)| *item == crate::ModelItem::user_text(&framed)),
        "coverage must be inspected after the actual replacement carrier, not the log-only summary event"
    );
    let nodes = before
        .into_iter()
        .map(|(seq, item)| crate::plugins::services::CompactionNode {
            seq,
            item,
            checkpoint: crate::session::catalog::is_context_checkpoint(&events[seq as usize]),
        })
        .collect::<Vec<_>>();
    let start = nodes
        .iter()
        .position(|n| selected.contains(&n.seq))
        .unwrap();
    let end = nodes
        .iter()
        .rposition(|n| selected.contains(&n.seq))
        .unwrap()
        + 1;
    assert!(nodes[start..end].iter().all(|n| selected.contains(&n.seq)));
    StoredReplacement {
        session,
        revision,
        nodes,
        range: start..end,
        summary,
        after,
    }
}

#[test]
#[ignore = "requires local synthetic journal; validates effective-view coverage without models"]
fn fidelity_omission_probe_accounts_for_unchanged_retained_context() {
    if !local_evidence_armed() {
        return;
    }
    let stored = replacement("requirements-candidate-2", 13);
    assert!(!stored.summary.contains("ORION-702"));
    let plan = crate::plugins::CompactionPlan::new(&stored.nodes, stored.range.clone()).unwrap();
    let candidate = plan.candidate(&stored.summary).unwrap();
    let mut replacement = candidate.replacement_items();
    // The frozen journal predates source excerpts. Compare its retained nodes
    // with the new replacement, using the original frame for the old carrier.
    assert_eq!(
        crate::plugins::checkpoint_body(candidate.checkpoint()),
        Some(stored.summary.as_str())
    );
    replacement[stored.range.start] =
        crate::ModelItem::user_text(crate::plugins::frame_checkpoint(&stored.summary));
    let old_view = stored
        .after
        .iter()
        .map(|(_, item)| item.clone())
        .collect::<Vec<_>>();
    assert_eq!(replacement, old_view);
    let retained = stored
        .nodes
        .iter()
        .enumerate()
        .filter(|(index, _)| !stored.range.contains(index))
        .map(|(_, node)| node)
        .collect::<Vec<_>>();
    let original = retained
        .iter()
        .find(|node| match &node.item {
            crate::ModelItem::User { content } => content.iter().any(|part| {
                matches!(part, crate::model::ContentPart::Text(text)
                    if text.starts_with("合成项目星河。初案") && text.contains("ORION-702"))
            }),
            _ => false,
        })
        .unwrap();
    assert!(
        stored
            .after
            .contains(&(original.seq, original.item.clone()))
    );
    assert!(
        retained
            .iter()
            .all(|node| stored.after.contains(&(node.seq, node.item.clone()))),
        "all retained source nodes survive the replacement unchanged"
    );
}
