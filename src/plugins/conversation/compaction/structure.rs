//! Validate the requested checkpoint shape without fixing wording or language.
pub(crate) fn valid(text: &str) -> bool {
    let mut sections = 0;
    let mut body = false;
    let mut fence = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some((marker, length)) = fence_marker(line) {
            match fence {
                Some((kind, minimum))
                    if marker == kind && length >= minimum && line[length..].trim().is_empty() =>
                {
                    fence = None
                }
                None => fence = Some((marker, length)),
                _ => {}
            }
            continue;
        }
        if fence.is_none() && line.starts_with("## ") {
            if (sections > 0 && !body) || line[3..].trim().is_empty() {
                return false;
            }
            sections += 1;
            body = false;
        } else if !line.is_empty() {
            if sections == 0 {
                return false;
            }
            body = true;
        }
    }
    sections == 8 && body && fence.is_none()
}

fn fence_marker(line: &str) -> Option<(u8, usize)> {
    let marker = *line.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = line.bytes().take_while(|b| *b == marker).count();
    (length >= 3).then_some((marker, length))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoint_structure_is_language_independent_and_ignores_fenced_headings() {
        let valid_text = (0..8)
            .map(|i| format!("## 節 {i}\n(none)\n"))
            .collect::<String>();
        assert!(valid(&valid_text));
        assert!(valid(&valid_text.replace('\n', "\r\n")));
        assert!(!valid(&valid_text.replace("## 節 3\n(none)", "## 節 3")));
        assert!(!valid(&format!("```\n{valid_text}```")));
        assert!(!valid(&format!("{valid_text}\n## ninth\nbody")));
        assert!(!valid(r#"{"complete":"but not a checkpoint"}"#));
        let fenced = valid_text.replacen("(none)", "````rust\n```\n## not a section\n````", 1);
        assert!(valid(&fenced));
        assert!(!valid(&valid_text.replacen("(none)", "```\n```", 1)));
    }
}
