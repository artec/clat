//! Candidate fidelity checks run in shadow only. No production admission or writes.
use super::*;
mod contract;
mod corpus;
mod evidence;
mod live;
mod replay;
mod saved;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Source {
    role: String,
    text: String,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Packet {
    sources: Vec<Source>,
    candidate: String,
}
struct Case {
    id: String,
    packet: Packet,
    expected_supported: bool,
    origin: String,
}

// A deliberately narrow lexical candidate, evaluated for false positives too.
fn reference_risks(packet: &Packet) -> Vec<String> {
    let pattern = regex::Regex::new(r"\b[A-Z][A-Z0-9]*(?:[-_][A-Z0-9]+)+\b").unwrap();
    let source = packet
        .sources
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let anchors = pattern
        .find_iter(&source)
        .map(|m| m.as_str())
        .filter(|s| s.bytes().any(|c| c.is_ascii_digit()))
        .collect::<std::collections::BTreeSet<_>>();
    let digits = regex::Regex::new(r"\d+").unwrap();
    pattern
        .find_iter(&packet.candidate)
        .map(|m| m.as_str())
        .filter(|value| {
            !anchors.contains(value)
                && value.bytes().any(|c| c.is_ascii_digit())
                && anchors
                    .iter()
                    .any(|old| digits.replace_all(old, "#") == digits.replace_all(value, "#"))
        })
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[test]
fn fidelity_lexical_reference_candidate_exposes_both_detection_and_false_rejection() {
    let rows = corpus::cases();
    let find = |name: &str| rows.iter().find(|r| r.id == name).unwrap();
    assert!(!reference_risks(&find("invented-reference").packet).is_empty());
    assert!(reference_risks(&find("explicit-correction").packet).is_empty());
    assert!(
        !reference_risks(&find("unconfirmed-technical-proposal").packet).is_empty(),
        "a valid inference demonstrates why this must not become a hard gate"
    );
    assert!(
        reference_risks(&find("critical-omission").packet).is_empty(),
        "lexical consistency cannot detect omitted facts"
    );
}
