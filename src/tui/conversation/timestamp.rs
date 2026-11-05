//! Local, presentation-only timestamps for user turns.
use clat_core::client_ports::presentation::{LocaleContext, UiMessage};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::tui::theme;

fn label(time_ms: i64, now: &jiff::Zoned, locale: LocaleContext) -> Option<String> {
    if time_ms <= 0 || time_ms > now.timestamp().as_millisecond() {
        return None;
    }
    let then = jiff::Timestamp::from_millisecond(time_ms)
        .ok()?
        .to_zoned(now.time_zone().clone());
    let days = then.date().until(now.date()).ok()?.get_days();
    if days == 0 {
        return Some(format!("{:02}:{:02}", then.hour(), then.minute()));
    }
    let (key, count) = if days < 7 {
        ("tui.time_days_ago", days)
    } else if days < 30 {
        ("tui.time_weeks_ago", days / 7)
    } else if days < 365 {
        ("tui.time_months_ago", days / 30)
    } else {
        ("tui.time_years_ago", days / 365)
    };
    Some(locale.render(&UiMessage::new(key).arg("count", count)))
}

pub(super) fn append(
    lines: &mut Vec<Line<'static>>,
    width: usize,
    time_ms: Option<i64>,
    now: &jiff::Zoned,
    locale: LocaleContext,
) {
    let Some(text) = time_ms.and_then(|time| label(time, now, locale)) else {
        return;
    };
    let label_width = UnicodeWidthStr::width(text.as_str());
    if label_width > width {
        return;
    }
    let Some(first) = lines.first_mut() else {
        return;
    };
    let padding = first
        .spans
        .last()
        .map(|span| span.content.len())
        .unwrap_or(0);
    if padding > label_width + 1 {
        first.spans.last_mut().unwrap().content = " ".repeat(padding - label_width - 1).into();
        first.spans.push(Span::styled(
            format!("{text} "),
            theme::style(theme::Role::Faint),
        ));
    } else {
        lines.insert(
            0,
            Line::from(Span::styled(
                format!("{}{text}", " ".repeat(width - label_width)),
                theme::style(theme::Role::Faint),
            )),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clat_core::client_ports::presentation::Locale;

    #[test]
    fn timestamps_follow_local_calendar_and_do_not_invent_missing_dates() {
        let now: jiff::Zoned = "2026-11-05T00:30:00+08:00[Asia/Shanghai]".parse().unwrap();
        let time = |s: &str| s.parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let locale = LocaleContext::new(Locale::English);
        assert_eq!(
            label(time("2026-11-04T16:05:00Z"), &now, locale).as_deref(),
            Some("00:05")
        );
        assert_eq!(
            label(time("2026-11-04T15:59:00Z"), &now, locale).as_deref(),
            Some("1 day ago")
        );
        assert_eq!(
            label(time("2026-10-29T04:00:00Z"), &now, locale).as_deref(),
            Some("1 week ago")
        );
        assert_eq!(
            label(time("2026-09-01T04:00:00Z"), &now, locale).as_deref(),
            Some("2 months ago")
        );
        assert!(label(0, &now, locale).is_none());
        assert!(label(now.timestamp().as_millisecond() + 1, &now, locale).is_none());
    }

    #[test]
    fn replay_keeps_message_timestamp_and_missing_time_stays_missing() {
        use crate::session::replay::ReplayEvent;
        use crate::tui::conversation::ConversationModel;

        let message = |time_ms| ReplayEvent::UserMessage {
            seq: 1,
            turn: 1,
            time_ms,
            text: "hello".into(),
            content_blocks: vec![],
            client_message_id: None,
            receipt: None,
        };
        let model = ConversationModel::from_replay(&[message(1234), message(0)]);
        assert_eq!(model.items[0].1.replay_time_ms, Some(1234));
        assert_eq!(model.items[1].1.replay_time_ms, None);
        let mut live = ConversationModel::new();
        live.push_user("hello".into());
        assert!(live.items[0].1.replay_time_ms.is_some());
    }

    #[test]
    fn timestamp_keeps_long_message_readable() {
        let now: jiff::Zoned = "2026-11-05T12:30:00+08:00[Asia/Shanghai]".parse().unwrap();
        let mut lines = super::super::render_user_block("a full-width message", 22);
        let original: String = lines
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect();
        append(
            &mut lines,
            22,
            Some(now.timestamp().as_millisecond()),
            &now,
            LocaleContext::default(),
        );
        let result: String = lines
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect();
        assert!(result.contains(original.trim_end()));
        assert!(result.contains("12:30"));
    }
}
