//! Lossless, UTF-8 safe splitting before summarization; no per-item prefix truncation.
use super::*;

pub(super) fn full_units(items: &[ModelItem], budget: usize) -> Vec<String> {
    // Four bytes per ASCII token, one token per other scalar, plus framing headroom.
    let limit = budget.saturating_sub(32).max(1);
    let mut units = Vec::new();
    for item in items {
        let rendered = render_group_with_limit(&[std::slice::from_ref(item)], false);
        let mut start = 0;
        let mut cost = 0;
        for (index, ch) in rendered.char_indices() {
            let next = if ch.is_ascii() { 1 } else { 4 };
            if cost + next > limit.saturating_mul(4) && index > start {
                units.push(rendered[start..index].to_owned());
                start = index;
                cost = 0;
            }
            cost += next;
        }
        if start < rendered.len() {
            units.push(rendered[start..].to_owned());
        }
    }
    units
}
