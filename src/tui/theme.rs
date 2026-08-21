//! 样式角色表（phase-1 P0-2）：前端全部视觉样式的单一事实来源。
//!
//! 组件不直接构造 `Color`/`Style`——除本文件外，`tui.rs` /
//! `markdown.rs` / `model_editor.rs` / `session_picker.rs` 不出现
//! `Color::`（`tests/architecture_boundaries.rs` 的门禁强制）。一义一
//! 角色；语义重叠的角色（`Dim` vs `Faint`、`Selected` 的双重用途）按
//! 现状保留并在各自条目注明统一/分离的后续计划。
//!
//! 真彩白名单（16 色 + 前景纪律的显式例外，新增必须在此登记并说明）：
//! - 品牌思考动画 shimmer 的两个端点（运行时在两端之间插值）；
//! - 静态 ASCII LOGO 复用 shimmer 低端色值（见 `Role::Logo`，不新增色）；
//! - 用户消息块与代码块的深色背景（浅色终端上的可读性底色）。

use ratatui::style::{Color, Modifier, Style};

/// 品牌 shimmer 低端（白名单成员）。
pub(crate) const BRAND_SHIMMER_LOW: Color = Color::Rgb(65, 118, 230);
/// 品牌 shimmer 高端（白名单成员）。
pub(crate) const BRAND_SHIMMER_HIGH: Color = Color::Rgb(211, 226, 255);

/// 用户消息块背景（真彩白名单成员）。
const USER_BG: Color = Color::Rgb(48, 50, 60);
/// 用户消息块前景（真彩白名单成员）。
const USER_FG: Color = Color::Rgb(233, 234, 239);
/// 代码块/行内代码背景（真彩白名单成员）。
const CODE_BG: Color = Color::Rgb(36, 39, 46);
const CODE_FG: Color = Color::Rgb(185, 214, 232);

pub(crate) enum Role {
    /// 纯粗体强调（对话框标题、头部标记、markdown 行内 bold）。
    Bold,
    /// 行内 italic。
    Italic,
    /// `Modifier::DIM` 退隐（弹窗提示行）。
    Dim,
    /// `fg=DarkGray` 退隐（计时、分隔线、滚动条轨道）。与 `Dim` 的
    /// 统一（DSH 用 SGR faint）是 P2。
    Faint,
    /// `REVERSED`——选区高亮与列表选中共用；语义分离是 P2。
    Selected,
    /// 用户消息整块底色。
    UserBlock,
    /// 用户消息 `❯ ` 前缀（叠加块背景）。
    UserMarker,
    /// 助手消息 `⏺ ` 前缀。
    AssistantMarker,
    ScrollTrack,
    ScrollThumb,
    /// markdown H1。
    HeadingPrimary,
    /// markdown H2 及以下。
    Heading,
    ListBullet,
    QuoteBar,
    QuoteText,
    /// 行内与块级代码同款。
    Code,
    Link,
    /// 思考动画 spinner 的最低亮度帧（品牌例外）。
    ThinkingGlyph,
    /// 静态 ASCII LOGO（欢迎页 + 退出告别）。复用白名单 shimmer 低端
    /// 色值——品牌色的静态呈现，不新增色。
    Logo,
    /// 成功态（工具卡 ✓、settled 卡头）。
    Success,
    /// 警示态（pending 卡头）。
    Warning,
    /// 错误态（错误输出、被拒卡头）。
    Error,
}

pub(crate) fn style(role: Role) -> Style {
    match role {
        Role::Bold => Style::default().add_modifier(Modifier::BOLD),
        Role::Italic => Style::default().add_modifier(Modifier::ITALIC),
        Role::Dim => Style::default().add_modifier(Modifier::DIM),
        Role::Faint => Style::default().fg(Color::DarkGray),
        Role::Selected => Style::default().add_modifier(Modifier::REVERSED),
        Role::UserBlock => Style::default().fg(USER_FG).bg(USER_BG),
        Role::UserMarker => Style::default()
            .fg(Color::Yellow)
            .bg(USER_BG)
            .add_modifier(Modifier::BOLD),
        Role::AssistantMarker => Style::default().fg(Color::Gray),
        Role::ScrollTrack => Style::default().fg(Color::DarkGray),
        Role::ScrollThumb => Style::default().fg(Color::Cyan),
        Role::HeadingPrimary => Style::default()
            .fg(Color::LightCyan)
            .add_modifier(Modifier::BOLD),
        Role::Heading => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        Role::ListBullet => Style::default().fg(Color::Cyan),
        Role::QuoteBar => Style::default().fg(Color::Cyan),
        Role::QuoteText => Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::ITALIC),
        Role::Code => Style::default().fg(CODE_FG).bg(CODE_BG),
        Role::Link => Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::UNDERLINED),
        Role::ThinkingGlyph => Style::default().fg(BRAND_SHIMMER_LOW),
        Role::Logo => Style::default().fg(BRAND_SHIMMER_LOW),
        Role::Success => Style::default().fg(Color::Green),
        Role::Warning => Style::default().fg(Color::Yellow),
        Role::Error => Style::default().fg(Color::Red),
    }
}

/// 两个 RGB 颜色之间的线性插值（品牌 shimmer 专用，`amount` ∈ 0..=1，
/// 不做钳制——与既有实现逐字节一致；非 RGB 输入返回低端色）。
pub(crate) fn blend(low: Color, high: Color, amount: f64) -> Color {
    fn channel(a: u8, b: u8, amount: f64) -> u8 {
        (a as f64 + (b as f64 - a as f64) * amount).round() as u8
    }
    match (low, high) {
        (Color::Rgb(fr, fg, fb), Color::Rgb(tr, tg, tb)) => Color::Rgb(
            channel(fr, tr, amount),
            channel(fg, tg, amount),
            channel(fb, tb, amount),
        ),
        _ => low,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_interpolates_between_the_brand_endpoints() {
        assert_eq!(
            blend(BRAND_SHIMMER_LOW, BRAND_SHIMMER_HIGH, 0.0),
            BRAND_SHIMMER_LOW
        );
        assert_eq!(
            blend(BRAND_SHIMMER_LOW, BRAND_SHIMMER_HIGH, 1.0),
            BRAND_SHIMMER_HIGH
        );
        let Color::Rgb(r, g, b) = blend(BRAND_SHIMMER_LOW, BRAND_SHIMMER_HIGH, 0.5) else {
            panic!("blend must stay RGB");
        };
        assert!(
            (65..=211).contains(&r) && (118..=226).contains(&g) && (230..=255).contains(&b),
            "midpoint between the endpoints: {r},{g},{b}"
        );
        // 非 RGB 退化输入返回低端色。
        assert_eq!(blend(Color::Cyan, Color::Red, 0.5), Color::Cyan);
    }
}
