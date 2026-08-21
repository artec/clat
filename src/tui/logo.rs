//! CLAT ASCII LOGO——启动欢迎页与退出告别的单一视觉来源。
//!
//! 纯前端资产（宪法分层：呈现归 `tui_*`，不进核心）。字形为手工拼排
//! 的 ANSI Shadow 体（C/L/A/T 各 8 列直排拼接）；其中 T 的竖笔画从
//! 原版的右置改为横杠正下居中——右置的"顶横杠 + 右竖笔"读似数字 7
//!（2026-08-19 用户反馈）。歧义宽字符（╗ ═ ║ …）按 unicode-width
//! 默认口径（非 CJK）为窄体 1 列，与 ratatui 渲染一致，测试锁定每行
//! 等宽。

use crate::tui::theme::{self, Role};
use ratatui::text::{Line, Span};
use std::io::{self, IsTerminal, Write};

const LOGO_LINES: [&str; 6] = [
    " ██████╗██╗      █████╗ ███████╗",
    "██╔════╝██║     ██╔══██╗╚══██══╝",
    "██║     ██║     ███████║   ██║  ",
    "██║     ██║     ██╔══██║   ██║  ",
    "╚██████╗███████╗██║  ██║   ██║  ",
    " ╚═════╝╚══════╝╚═╝  ╚═╝   ╚═╝  ",
];

/// 空会话欢迎页（启动 / `/new` / `/clear` 后的会话区）：LOGO +
/// 版本行 + 起步提示。横向居中由调用方按最宽行处理。
pub(crate) fn welcome_lines() -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = LOGO_LINES
        .iter()
        .map(|text| Line::from(Span::styled((*text).to_owned(), theme::style(Role::Logo))))
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("v{} · cmd-line agent runtime", env!("CARGO_PKG_VERSION")),
        theme::style(Role::Dim),
    )));
    lines.push(Line::from(Span::styled(
        "type a message to begin · /help for keys",
        theme::style(Role::Faint),
    )));
    lines
}

/// 退出告别的 stdout 纯文本（ANSI 着色由 [`print_farewell`] 统一处理，
/// 文本本身可测）。
pub(crate) fn farewell_text() -> String {
    // 上、下各一个空行包夹 LOGO + 版本行：与恢复主屏后的提示符和
    // 之前的输出都隔出呼吸感（与 opencode 退出画面一致）。
    let mut out = String::from("\n");
    for line in LOGO_LINES {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&format!(
        "clat v{} — cmd-line agent runtime\n\n",
        env!("CARGO_PKG_VERSION")
    ));
    out
}

/// 终端恢复主屏后打印告别 LOGO。仅在 stdout 为 TTY 时输出（TUI 之外
/// 重定向的场景不污染管道）；失败静默——告别是纯装饰，不影响退出码。
pub(crate) fn print_farewell() {
    if !io::stdout().is_terminal() {
        return;
    }
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";
    let text = farewell_text();
    // 只裁最后一个换行再由 writeln 补回——版本行下的尾部空行必须
    // 保留，不能用 trim_end_matches 把它一并裁掉。
    let _ = writeln!(
        io::stdout(),
        "{dim}{}{reset}",
        text.strip_suffix('\n').unwrap_or(&text)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    /// LOGO 每行的字符数与显示列数（字模契约）。
    const LOGO_WIDTH: usize = 32;

    #[test]
    fn logo_lines_form_a_rectangle() {
        for line in LOGO_LINES {
            assert_eq!(
                line.chars().count(),
                LOGO_WIDTH,
                "glyph rows must have equal char counts"
            );
            assert_eq!(
                line.width(),
                LOGO_WIDTH,
                "display width must equal char count (ambiguous-width glyphs stay narrow)"
            );
        }
    }

    #[test]
    fn farewell_names_the_version() {
        let text = farewell_text();
        // 上、下空行 + 6 行 LOGO + 空行 + 版本行，与 LOGO_LINES 联动。
        assert!(
            text.starts_with('\n'),
            "farewell breathes: blank line first"
        );
        assert!(text.ends_with("\n\n"), "farewell breathes: blank line last");
        assert!(text.contains(concat!("clat v", env!("CARGO_PKG_VERSION"))));
        assert_eq!(text.lines().count(), LOGO_LINES.len() + 4);
    }

    #[test]
    fn welcome_pairs_logo_with_startup_hints() {
        let lines = welcome_lines();
        assert_eq!(lines.len(), LOGO_LINES.len() + 3);
        let logo: Vec<String> = lines
            .iter()
            .take(LOGO_LINES.len())
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert_eq!(logo, LOGO_LINES);
        let hint: String = lines[LOGO_LINES.len() + 2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(hint.contains("/help"), "welcome must point at /help");
    }
}
