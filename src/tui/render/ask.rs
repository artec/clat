//! Question option presentation only; selected wire labels remain unchanged.
use super::*;

pub(super) fn option_lines(
    question: &crate::interaction::AskQuestion,
    selection: usize,
    width: usize,
    locale: LocaleContext,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, option) in question.options.iter().enumerate() {
        let selected = index == selection;
        let marker = if selected { "● " } else { "○ " };
        let mut spans = vec![Span::styled(
            marker,
            theme::style(if selected {
                theme::Role::Selected
            } else {
                theme::Role::Faint
            }),
        )];
        spans.push(if selected {
            Span::styled(
                question.option_text(index, locale),
                theme::style(theme::Role::Selected),
            )
        } else {
            Span::raw(question.option_text(index, locale))
        });
        lines.push(Line::from(spans));
        if let Some(description) = &option.description {
            for wrapped in wrap_text(description, width.saturating_sub(2)) {
                lines.push(Line::from(Span::styled(
                    format!("   {wrapped}"),
                    theme::style(theme::Role::Faint),
                )));
            }
        }
    }
    lines
}
