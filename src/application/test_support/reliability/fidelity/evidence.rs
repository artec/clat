//! View-aware literal evidence, separate from model opinions and semantic validity.
use crate::model::{ContentPart, ModelItem};
use crate::plugins::CompactionCandidate;
use sha2::{Digest, Sha256};
use std::ops::Range;

#[derive(Clone, Debug)]
struct Fragment {
    binding: String,
    node: usize,
    part: usize,
    bytes: Range<usize>,
}
struct EvidenceView<'a, 'n> {
    candidate: &'a CompactionCandidate<'n>,
    binding: String,
}
impl<'a, 'n> EvidenceView<'a, 'n> {
    fn new(candidate: &'a CompactionCandidate<'n>, session: &str, revision: u64) -> Self {
        let nodes = candidate
            .source_nodes()
            .iter()
            .map(|node| (node.seq, &node.item))
            .collect::<Vec<_>>();
        let range = candidate.selected_range();
        let encoded = serde_json::to_vec(&(
            session,
            revision,
            nodes,
            range.start,
            range.end,
            candidate.checkpoint(),
        ))
        .unwrap();
        Self {
            candidate,
            binding: format!("{:x}", Sha256::digest(encoded)),
        }
    }
}
#[derive(Clone, Copy)]
enum StatementKind {
    LiteralAssertion,
    Proposal,
}
#[derive(Debug, PartialEq)]
enum ReferenceSupport {
    ExactLiteral,
    DifferentLiteral,
    UnverifiedProposal,
}
#[derive(Debug, PartialEq)]
struct Coverage {
    retained_nodes: Vec<usize>,
    checkpoint_occurrences: usize,
}

fn text_parts(item: &ModelItem) -> Vec<(usize, String)> {
    match item {
        ModelItem::User { content } | ModelItem::Assistant { content, .. } => content
            .iter()
            .enumerate()
            .filter_map(|(index, part)| match part {
                ContentPart::Text(text) => Some((index, text.clone())),
                ContentPart::Image { .. } => None,
            })
            .collect(),
        ModelItem::ToolResult(result) => vec![(0, result.output.to_string())],
        ModelItem::ToolCall(call) => vec![(0, call.arguments.to_string())],
        ModelItem::ProviderState(_) => vec![],
    }
}

fn quote(view: &EvidenceView<'_, '_>, source: &Fragment) -> Result<String, &'static str> {
    if source.binding != view.binding {
        return Err("evidence belongs to another context or candidate");
    }
    let candidate = view.candidate;
    let node = candidate
        .source_nodes()
        .get(source.node)
        .ok_or("unknown source node")?;
    let parts = text_parts(&node.item);
    let (_, text) = parts
        .iter()
        .find(|(index, _)| *index == source.part)
        .ok_or("source part is not textual evidence")?;
    if source.bytes.start >= source.bytes.end {
        return Err("empty evidence range");
    }
    text.get(source.bytes.clone())
        .map(str::to_owned)
        .ok_or("invalid source UTF-8 byte range")
}

fn compare_reference(
    view: &EvidenceView<'_, '_>,
    source: &Fragment,
    asserted: Range<usize>,
    kind: StatementKind,
) -> Result<ReferenceSupport, &'static str> {
    let source = quote(view, source)?;
    let candidate = view.candidate;
    if asserted.start >= asserted.end {
        return Err("empty candidate range");
    }
    let asserted = candidate
        .checkpoint()
        .get(asserted)
        .ok_or("invalid candidate UTF-8 byte range")?;
    if matches!(kind, StatementKind::Proposal) {
        return Ok(ReferenceSupport::UnverifiedProposal);
    }
    Ok(if source == asserted {
        ReferenceSupport::ExactLiteral
    } else {
        ReferenceSupport::DifferentLiteral
    })
}

fn coverage(view: &EvidenceView<'_, '_>, source: &Fragment) -> Result<Coverage, &'static str> {
    let literal = quote(view, source)?;
    let candidate = view.candidate;
    let selected = candidate.selected_range();
    let retained_nodes = candidate
        .source_nodes()
        .iter()
        .enumerate()
        .filter(|(index, node)| {
            !selected.contains(index)
                && text_parts(&node.item)
                    .iter()
                    .any(|(_, text)| text.contains(&literal))
        })
        .map(|(index, _)| index)
        .collect();
    Ok(Coverage {
        retained_nodes,
        checkpoint_occurrences: candidate.checkpoint().matches(&literal).count(),
    })
}

fn fragment(view: &EvidenceView<'_, '_>, node: usize, text: &str, needle: &str) -> Fragment {
    let start = text.find(needle).unwrap();
    Fragment {
        binding: view.binding.clone(),
        node,
        part: 0,
        bytes: start..start + needle.len(),
    }
}
fn assertion(candidate: &CompactionCandidate<'_>, needle: &str) -> Range<usize> {
    let start = candidate.checkpoint().find(needle).unwrap();
    start..start + needle.len()
}
fn nodes(texts: &[String]) -> Vec<crate::plugins::services::CompactionNode> {
    texts
        .iter()
        .enumerate()
        .map(|(index, text)| crate::plugins::services::CompactionNode {
            seq: index as u64,
            item: ModelItem::user_text(text),
        })
        .collect()
}

#[test]
fn fidelity_evidence_distinguishes_changed_reference_from_unverified_proposal() {
    let source = "当前 FALCON-17，UTF-8。".to_owned() + &"padding ".repeat(1000);
    let nodes = nodes(std::slice::from_ref(&source));
    let plan = crate::plugins::CompactionPlan::new(&nodes, 0..1).unwrap();
    let candidate = plan
        .candidate("FALCON-71 已确认；可考虑 UTF-16（未确认建议）")
        .unwrap();
    let view = EvidenceView::new(&candidate, "session-a", 9);
    let id = fragment(&view, 0, &source, "FALCON-17");
    assert_eq!(
        compare_reference(
            &view,
            &id,
            assertion(&candidate, "FALCON-71"),
            StatementKind::LiteralAssertion
        )
        .unwrap(),
        ReferenceSupport::DifferentLiteral
    );
    let encoding = fragment(&view, 0, &source, "UTF-8");
    assert_eq!(
        compare_reference(
            &view,
            &encoding,
            assertion(&candidate, "UTF-16"),
            StatementKind::Proposal
        )
        .unwrap(),
        ReferenceSupport::UnverifiedProposal
    );
    let corrected = plan.candidate("当前 FALCON-17").unwrap();
    let corrected_view = EvidenceView::new(&corrected, "session-a", 9);
    let corrected_id = fragment(&corrected_view, 0, &source, "FALCON-17");
    assert!(quote(&corrected_view, &id).is_err());
    assert_eq!(
        compare_reference(
            &corrected_view,
            &corrected_id,
            assertion(&corrected, "FALCON-17"),
            StatementKind::LiteralAssertion
        )
        .unwrap(),
        ReferenceSupport::ExactLiteral
    );
}

#[test]
fn fidelity_evidence_coverage_uses_retained_context_without_approving_semantics() {
    let texts = vec![
        "用户原始工单 FALCON-17".into(),
        "selected history ".repeat(1000),
        "tail".into(),
    ];
    let nodes = nodes(&texts);
    let plan = crate::plugins::CompactionPlan::new(&nodes, 1..2).unwrap();
    let candidate = plan.candidate("short checkpoint").unwrap();
    let view = EvidenceView::new(&candidate, "session-a", 9);
    let id = fragment(&view, 0, &texts[0], "FALCON-17");
    assert_eq!(
        coverage(&view, &id).unwrap(),
        Coverage {
            retained_nodes: vec![0],
            checkpoint_occurrences: 0
        }
    );
    let all = crate::plugins::CompactionPlan::new(&nodes, 0..2).unwrap();
    let missing = all.candidate("short checkpoint").unwrap();
    let missing_view = EvidenceView::new(&missing, "session-a", 9);
    let missing_id = fragment(&missing_view, 0, &texts[0], "FALCON-17");
    assert_eq!(
        coverage(&missing_view, &missing_id).unwrap(),
        Coverage {
            retained_nodes: vec![],
            checkpoint_occurrences: 0
        }
    );
    let ambiguous = all
        .candidate("FALCON-17 不再有效（未经来源支持的断言）")
        .unwrap();
    let ambiguous_view = EvidenceView::new(&ambiguous, "session-a", 9);
    let ambiguous_id = fragment(&ambiguous_view, 0, &texts[0], "FALCON-17");
    assert_eq!(
        coverage(&ambiguous_view, &ambiguous_id)
            .unwrap()
            .checkpoint_occurrences,
        1,
        "literal coverage is not semantic truth or authorization"
    );
}

#[test]
fn fidelity_evidence_rejects_cross_part_and_invalid_utf8_or_empty_citations() {
    let text = "路径🟩 FALCON-17".to_owned() + &" source ".repeat(1000);
    let nodes = nodes(std::slice::from_ref(&text));
    let plan = crate::plugins::CompactionPlan::new(&nodes, 0..1).unwrap();
    let candidate = plan.candidate("FALCON-17").unwrap();
    let view = EvidenceView::new(&candidate, "session-a", 9);
    let good = fragment(&view, 0, &text, "FALCON-17");
    assert!(
        quote(
            &view,
            &Fragment {
                node: 99,
                ..good.clone()
            }
        )
        .is_err()
    );
    assert!(
        quote(
            &view,
            &Fragment {
                part: 99,
                ..good.clone()
            }
        )
        .is_err()
    );
    assert!(
        quote(
            &view,
            &Fragment {
                bytes: 1..2,
                ..good.clone()
            }
        )
        .is_err()
    );
    assert!(
        quote(
            &view,
            &Fragment {
                bytes: 0..0,
                ..good.clone()
            }
        )
        .is_err()
    );
    assert!(compare_reference(&view, &good, 0..99999, StatementKind::LiteralAssertion).is_err());
}

#[test]
fn fidelity_evidence_cannot_reuse_citations_across_session_revision_or_selection() {
    let source = "FALCON-17 ".repeat(1000);
    let nodes = nodes(&[source.clone(), "tail".into()]);
    let plan = crate::plugins::CompactionPlan::new(&nodes, 0..1).unwrap();
    let candidate = plan.candidate("FALCON-17").unwrap();
    let view = EvidenceView::new(&candidate, "session-a", 9);
    let citation = fragment(&view, 0, &source, "FALCON-17");
    assert!(quote(&EvidenceView::new(&candidate, "session-b", 9), &citation).is_err());
    assert!(quote(&EvidenceView::new(&candidate, "session-a", 10), &citation).is_err());
    let revised_candidate = plan.candidate("FALCON-17 with a different claim").unwrap();
    assert!(
        quote(
            &EvidenceView::new(&revised_candidate, "session-a", 9),
            &citation
        )
        .is_err()
    );
    let other_plan = crate::plugins::CompactionPlan::new(&nodes, 0..2).unwrap();
    let other = other_plan.candidate("FALCON-17").unwrap();
    assert!(quote(&EvidenceView::new(&other, "session-a", 9), &citation).is_err());
}

#[test]
#[ignore = "requires local synthetic v6 evidence; no models or Host writes"]
fn fidelity_evidence_separates_real_fabrication_from_retained_original_coverage() {
    if !super::super::local_evidence_armed() {
        return;
    }
    let stored = super::saved::replacement("tools-candidate-0", 13);
    let plan = crate::plugins::CompactionPlan::new(&stored.nodes, stored.range.clone()).unwrap();
    let candidate = plan.candidate(&stored.summary).unwrap();
    let view = EvidenceView::new(&candidate, &stored.session, stored.revision);
    let (node, part, text) = stored
        .nodes
        .iter()
        .enumerate()
        .filter(|(index, _)| stored.range.contains(index))
        .flat_map(|(index, node)| {
            text_parts(&node.item)
                .into_iter()
                .map(move |(part, text)| (index, part, text))
        })
        .find(|(_, _, text)| text.contains("ORION-700"))
        .unwrap();
    let mut witness = fragment(&view, node, &text, "ORION-700");
    witness.part = part;
    assert_eq!(
        compare_reference(
            &view,
            &witness,
            assertion(&candidate, "ORION-002"),
            StatementKind::LiteralAssertion
        )
        .unwrap(),
        ReferenceSupport::DifferentLiteral
    );
    assert!(
        !coverage(&view, &witness).unwrap().retained_nodes.is_empty(),
        "keeping the true value elsewhere does not excuse a contradictory invented value in the checkpoint"
    );
    assert_eq!(
        candidate.replacement_items(),
        stored
            .after
            .iter()
            .map(|(_, item)| item.clone())
            .collect::<Vec<_>>()
    );
}
