//! Frontend message identity, never a persistent journal index.
use super::*;

#[derive(Clone)]
pub(crate) struct BodyMatch {
    pub identity: u64,
    pub offset: usize,
    pub row: usize,
    pub preview: String,
}

impl ConversationModel {
    pub(crate) fn find_body(
        &mut self,
        query: &str,
        width: usize,
        visibility: ToolCardVisibility,
    ) -> Vec<BodyMatch> {
        self.ensure_rendered(width);
        let query = query.to_lowercase();
        let mut matches = Vec::new();
        let mut row = 0;
        for (item, cache) in &self.items {
            let rows = item_rows(item, cache, visibility);
            let text = match item {
                ConversationItem::User { text } | ConversationItem::Assistant { text, .. } => {
                    Some(text)
                }
                _ => None,
            };
            if let Some(text) = text.filter(|_| !query.is_empty()) {
                let (folded, source_offsets) = folded_source(text);
                let body_start = match item {
                    ConversationItem::Assistant { .. } => rows.as_slice().len().saturating_sub(
                        render_markdown(text, width.saturating_sub(2).max(1)).len(),
                    ),
                    _ => 0,
                };
                for (occurrence, (folded_offset, _)) in folded.match_indices(&query).enumerate() {
                    if matches.len() >= 2000 {
                        break;
                    }
                    let offset = source_offsets[folded_offset];
                    let line = occurrence_row(&rows.as_slice()[body_start..], &query, occurrence);
                    matches.push(BodyMatch {
                        identity: cache.identity,
                        offset,
                        row: row + body_start + line,
                        preview: text[offset..].chars().take(120).collect(),
                    });
                }
            }
            row += rows.as_slice().len() + usize::from(!rows.as_slice().is_empty());
        }
        matches
    }
}

fn folded_source(text: &str) -> (String, Vec<usize>) {
    let mut folded = String::new();
    let mut offsets = Vec::new();
    for (offset, character) in text.char_indices() {
        for lower in character.to_lowercase() {
            folded.push(lower);
            offsets.extend(std::iter::repeat_n(offset, lower.len_utf8()));
        }
    }
    (folded, offsets)
}

fn occurrence_row(lines: &[Line<'static>], query: &str, mut occurrence: usize) -> usize {
    for (row, line) in lines.iter().enumerate() {
        let count = line.to_string().to_lowercase().matches(query).count();
        if occurrence < count {
            return row;
        }
        occurrence -= count;
    }
    // Raw Markdown syntax or a match crossing a wrap boundary: anchor the body
    // and show the exact source occurrence in the find panel, never a Think row.
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn body_search_excludes_thinking_and_keeps_identity_after_prefix_insert() {
        let mut model = ConversationModel::new();
        model.push_user("用户 needle needle".into());
        model.push_item(ConversationItem::Assistant {
            text: "first needle\n\nsecond needle after İ".into(),
            reasoning: Some("needle in private thinking".into()),
            reasoning_running: false,
            tool_calls: Vec::new(),
            provider: String::new(),
            model: String::new(),
        });
        model.open_tool_card("test".into(), "needle tool".into(), Value::Null);
        let hits = model.find_body("needle", 40, ToolCardVisibility::Collapsed);
        assert_eq!(hits.len(), 4);
        assert!(hits[3].row > hits[2].row);
        assert_eq!(hits[3].preview, "needle after İ");
        let id = hits[0].identity;
        let mut prefix = ConversationModel::new();
        prefix.push_user("earlier needle".into());
        prefix.items.append(&mut model.items);
        model.items = prefix.items;
        let hits = model.find_body("needle", 15, ToolCardVisibility::Hidden);
        assert_eq!(hits.len(), 5);
        assert_eq!(hits[1].identity, id);
        assert_eq!(hits[2].identity, id);
        assert_ne!(hits[1].offset, hits[2].offset);
    }
}
