//! Lossless, UTF-8 safe splitting before summarization; no per-item prefix truncation.
use super::*;

pub(super) fn rendered_units(items: &[ModelItem], active: bool) -> Vec<String> {
    if active {
        items
            .iter()
            .map(|item| render_group_with_limit(&[std::slice::from_ref(item)], false))
            .collect()
    } else {
        split_turns(items)
            .iter()
            .map(|turn| render_group(&[*turn]))
            .collect()
    }
}

#[cfg(test)]
pub(super) fn full_units(items: &[ModelItem], budget: usize) -> Vec<String> {
    rendered_units(items, true)
        .iter()
        .flat_map(|text| text_units(text, budget))
        .collect()
}

pub(super) fn text_units(rendered: &str, budget: usize) -> Vec<String> {
    // Four bytes per ASCII token, one token per other scalar, plus framing headroom.
    let limit = budget.saturating_sub(32).max(1);
    let mut units = Vec::new();
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
    units
}

pub(super) fn groups(units: Vec<String>, budget: usize) -> Result<Vec<Vec<String>>, String> {
    let mut groups = Vec::new();
    let mut current = Vec::new();
    let mut current_tokens = 0usize;
    for unit in units {
        let tokens = estimate_tokens_str(&unit);
        if tokens > budget {
            return Err(format!(
                "one complete summary unit needs {tokens} tokens, above the per-request budget of {budget}"
            ));
        }
        if current_tokens + tokens > budget && !current.is_empty() {
            groups.push(std::mem::take(&mut current));
            current_tokens = 0;
        }
        current_tokens += tokens;
        current.push(unit);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    Ok(groups)
}
