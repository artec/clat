use super::*;

/// 权限对话框的参数字段摘要：对象时列出全部顶层键（截断到 10 个
/// 并标注剩余数），非对象（字符串/数字等）返回 None——摘要只在
/// 键存在时才有意义。危险目标（command/path/url 等）可能藏在长
/// JSON 深处，顶层键一览让批准前不可错过。
pub(super) fn top_level_argument_keys(arguments: &serde_json::Value) -> Option<String> {
    let map = arguments.as_object()?;
    if map.is_empty() {
        return None;
    }
    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let shown: Vec<&str> = keys.iter().take(10).copied().collect();
    let mut summary = shown.join(", ");
    if keys.len() > 10 {
        summary.push_str(&format!(" (+{} more)", keys.len() - 10));
    }
    Some(summary)
}

/// 权限对话框的写/执行工具专用预览。返回 None 表示该工具不适用
/// （回退完整 JSON）。预览行就是被审阅的参数：强制滚动与批准解锁
/// 逻辑对它一视同仁——渲染形式变了，审阅义务不变。
///
/// NWE-04：命令/路径/内容一律**逻辑行拆分 + wrap_text 换行**——
/// 未换行的超长命令尾部会被水平裁掉，而审阅计数只有 1 行，批准
/// 在用户没看到命令尾部时就解锁。控制字符（\n、\t 之外）转成
/// 可见的 ^X 记法，不可再藏。
/// 工具参数的结构化呈现（edit_file 迷你 diff / write_file 全文 /
/// run_command `$ cmd`）。权限对话框审阅与转录工具卡共用同一渲染器
/// （phase-1 P1-4：预览即卡片正文）。
pub(crate) fn tool_argument_lines(
    tool: &str,
    arguments: &serde_json::Value,
    width: usize,
) -> Option<Vec<Line<'static>>> {
    let object = arguments.as_object()?;
    let mut lines: Vec<Line<'static>> = Vec::new();
    // 控制字符可见化：\r、\0、ESC 等 shell 语义字符在预览里显形
    // 为 ^M、^@、^[，无法借零宽度隐身。
    fn visible(text: &str) -> String {
        text.chars()
            .map(|ch| match ch {
                '\t' => "    ".to_owned(),
                '\n' => ch.to_string(),
                '\x00'..='\x1f' => format!("^{}", (b'@' + ch as u8) as char),
                '\x7f' => "^?".to_owned(),
                _ => ch.to_string(),
            })
            .collect()
    }
    // 标题（edit/write 路径、$ 命令）也换行并可见化控制字符——
    // 路径和命令同样可能长于对话框宽度，藏尾部即藏目标。
    fn push_header(lines: &mut Vec<Line<'static>>, title: String, width: usize) {
        for logical in visible(&title).split('\n') {
            for wrapped in wrap_text(logical, width.saturating_sub(2)) {
                lines.push(Line::from(Span::styled(
                    format!("  {wrapped}"),
                    theme::style(theme::Role::Bold),
                )));
            }
        }
    }
    // 多行文本先按逻辑行拆分再换行：wrap_text 视 \n 为零宽字符，
    // 直接喂多行会把内容挤成一坨，审阅时无法分清结构。
    let push_wrapped = |lines: &mut Vec<Line<'static>>, prefix: &str, text: &str| {
        for logical in visible(text).split('\n') {
            for wrapped in wrap_text(logical, width.saturating_sub(prefix.len() + 2)) {
                lines.push(Line::from(format!("{prefix} {wrapped}")));
            }
        }
    };
    match tool {
        "edit_file" => {
            let path = object.get("path")?.as_str()?;
            let old_str = object.get("old_str")?.as_str()?;
            let new_str = object
                .get("new_str")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            push_header(&mut lines, format!("edit {path}"), width);
            lines.push(Line::from(Span::styled(
                "- old_str (must match the file exactly, once):",
                theme::style(theme::Role::Dim),
            )));
            push_wrapped(&mut lines, "-", old_str);
            lines.push(Line::from(Span::styled(
                "+ new_str:",
                theme::style(theme::Role::Dim),
            )));
            push_wrapped(&mut lines, "+", new_str);
        }
        "write_file" => {
            let path = object.get("path")?.as_str()?;
            let content = object.get("content")?.as_str()?;
            push_header(
                &mut lines,
                format!("write {path} ({} bytes)", content.len()),
                width,
            );
            for logical in visible(content).split('\n') {
                for wrapped in wrap_text(logical, width.saturating_sub(2)) {
                    lines.push(Line::from(format!("  {wrapped}")));
                }
            }
        }
        "run_command" => {
            let command = object.get("command")?.as_str()?;
            let timeout = object
                .get("timeout_seconds")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(120);
            let network = object
                .get("network")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let sandbox = object
                .get("sandbox")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("auto");
            push_header(&mut lines, format!("$ {command}"), width);
            let metadata = format!(
                "in project root · timeout {timeout}s · sandbox {sandbox} · network {}",
                if network { "requested" } else { "blocked" }
            );
            for wrapped in wrap_text(&metadata, width.saturating_sub(2)) {
                lines.push(Line::from(Span::styled(
                    format!("  {wrapped}"),
                    theme::style(theme::Role::Dim),
                )));
            }
        }
        _ => return None,
    }
    Some(lines)
}

/// 扩展“从首行起连续看过”的区间。新视口与既有区间相接/重叠时
/// 才前进；跳过中间行（例如直接 End）不能伪造完整审阅。
pub(super) fn advance_reviewed_through(reviewed_through: usize, start: usize, end: usize) -> usize {
    if start <= reviewed_through {
        reviewed_through.max(end)
    } else {
        reviewed_through
    }
}

pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut result = Vec::new();
    for source_line in text.split('\n') {
        if source_line.is_empty() {
            result.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_width = 0usize;
        for ch in source_line.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width > 0 && current_width + ch_width > width {
                result.push(std::mem::take(&mut current));
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }
        result.push(current);
    }
    result
}

/// 弹出窗与屏幕左右边缘的最小留白（列）。纯百分比布局在窄终端/
/// 分屏下会退化：94% 在 60 列下每侧仅 1 列、40 列下为 0，对话框
/// 直接贴住左右墙。所有弹出窗共用这一下限，宽度不够时收缩对话框
/// 并保持居中，而不是牺牲边距。
pub(crate) const POPUP_H_MARGIN: u16 = 4;

/// 弹窗上下不贴屏幕上下沿的最少行数（与 `POPUP_H_MARGIN` 对称的垂直
/// 边距；弹窗规范 2026-08-19：四边间距）。终端高度放不下时不硬挤
/// 没——按旧钳制退化（见 `centered_rect`）。
pub(crate) const POPUP_V_MARGIN: u16 = 2;

/// 垂直边距钳制生效所需的最低弹窗高度：更矮的终端保留旧行为。
const MIN_POPUP_HEIGHT: u16 = 6;

/// 钳制生效所需的最低对话框宽度：更窄的终端连"边距 + 可用宽度"
/// 都放不下，保留百分比行为，不把对话框挤没。
const MIN_POPUP_WIDTH: u16 = 16;

/// 弹出窗内容的水平内边距（列）。文字与边框字符之间留空，不贴框；
/// 手工换行/截断的宽度计算必须同步扣除 `2 × POPUP_TEXT_PADDING`。
pub(crate) const POPUP_TEXT_PADDING: u16 = 1;

/// 弹出窗统一的边框块：全边框 + 标题 + 1 列水平内边距 + Warning 黄
/// 边框/标题（弹窗规范 2026-08-19：所有弹窗同一样式——黄边框、背景
/// 压暗、四边间距；黄 = 需要注意/决策的模态语义，与主题 Role::Warning
/// 一致）。标题前后空一格由本构造器统一加（D-2 闪光点 a：全库 title
/// 一致风格），调用方传裸标题。
pub(crate) fn popup_block(title: &str) -> Block<'static> {
    let warning = theme::style(theme::Role::Warning);
    Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_style(warning.add_modifier(Modifier::BOLD))
        .border_style(warning)
        .padding(Padding::horizontal(POPUP_TEXT_PADDING))
}

/// 弹窗清屏垫边（wide-glyph guard，CP-3 收窄 2026-09-02）：Clear 范围
/// 左右各扩一列（钳制在终端区域内），上下不扩。跨在弹窗左边框起点上
/// 的宽字符（CJK/emoji 占 2 列，起点格在 Clear 范围之外）会让 ratatui
/// diff 的 `to_skip` 吞掉边框列的更新——上一帧的字形铺进边框列，本帧
/// │ 不再补发，左边线被吃掉（用户实测：仅左边线受损，弹窗内部不受影
/// 响；右侧因起点格在 Clear 范围内天然安全，扩列是末列宽字符换行类
/// 的廉价保险）。起点格被一并清掉后，diff 正常发出边框更新。
/// 历史实现曾在上下各扩一行——那是无事故依据的纯视觉对称（注释也未
/// 论证）：宽字形溢出与 `to_skip` 吞更新均为纯列向机制，字形不纵向
/// 跨行，上下守卫零防护价值、每侧白占一行底层 UI 的可见内容
///（2026-09-02 负责人裁定收窄，守卫几何测试锁形）。
pub(crate) fn clear_popup_with_guards(frame: &mut Frame, rect: Rect) {
    let area = frame.area();
    let left = rect.x.saturating_sub(1).max(area.x);
    let right = rect.right().saturating_add(1).min(area.right());
    if right <= left || rect.height == 0 {
        frame.render_widget(Clear, rect);
        return;
    }
    frame.render_widget(
        Clear,
        Rect {
            x: left,
            width: right - left,
            y: rect.y,
            height: rect.height,
        },
    );
}

/// 弹窗在给定终端内的最大可用高度（垂直边距感知）：放得下边距时为
/// `area.height - 2×POPUP_V_MARGIN`，终端过矮时退化为整屏高。这是
/// 弹窗高度的唯一预算来源——`centered_rect` 的钳制与所有弹窗内容方
/// 的分页/行数预算必须共用同一函数，否则预算与实际渲染高度错位
/// （真实回归：权限弹窗分页按 `area.height - 2` 旧预算计算，而
/// centered_rect 钳到 `area.height - 4`——End 翻到底时页底两行渲染
/// 在框外，永远看不到最后两行）。
pub(crate) fn popup_height_cap(area: Rect) -> u16 {
    let bounded = area.height.saturating_sub(2 * POPUP_V_MARGIN);
    if bounded >= MIN_POPUP_HEIGHT {
        bounded.min(area.height)
    } else {
        area.height
    }
}

/// 弹窗水平切分的实际宽度（列）：与 [`centered_rect`] 同一 Layout 与
/// `POPUP_H_MARGIN` 钳制，供"行数依赖内宽、而矩形高度又依赖行数"的
/// 内容驱动弹窗先行取宽（一致性由
/// `popup_width_matches_centered_rect` 锁定，不共代码路径是为了不动
/// 既有渲染的百分比取整行为）。
pub(crate) fn popup_width(percent_x: u16, area: Rect) -> u16 {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(area);
    let mut width = horizontal[1].width;
    let bounded = area.width.saturating_sub(2 * POPUP_H_MARGIN);
    if width > bounded && bounded >= MIN_POPUP_WIDTH {
        width = bounded;
    }
    width
}

/// 内容驱动弹窗的内宽（列）：弹窗宽减边框 2 列与内边距
/// 2×POPUP_TEXT_PADDING。内容折行宽度与实际渲染矩形必须同源，否则
/// 预算行数与真实可放行数错位（权限弹窗分页曾因此翻不到底）。
pub(crate) fn popup_inner_width(percent_x: u16, area: Rect) -> usize {
    popup_width(percent_x, area).saturating_sub(2 + 2 * POPUP_TEXT_PADDING) as usize
}

/// 内容驱动弹窗高度：内容行数 + 边框 2 行 + 空行 1 行 + 脚注 1 行，钳在
/// [`popup_height_cap`] 预算内。短内容得到小框（上下留出真实边距），
/// 长内容恰好贴满预算继续滚动（2026-08-19 第三轮反馈：/help 恒取满额
/// 高度，内容再少也是整屏框、边距形同虚设）。空行（2026-08-21 统一）
/// 与其余弹窗的"内容 → 空行 → 脚注"节奏一致，固定钉在脚注上方、
/// 不随内容滚动。
pub(crate) fn content_dialog_height(content_lines: usize, area: Rect) -> u16 {
    (content_lines as u16)
        .saturating_add(4)
        .min(popup_height_cap(area))
}

pub(crate) fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let height = height.min(popup_height_cap(area));
    let top = area.height.saturating_sub(height) / 2;
    let vertical = Rect::new(area.x, area.y + top, area.width, height.min(area.height));
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical);
    let mut rect = horizontal[1];
    let bounded = area.width.saturating_sub(2 * POPUP_H_MARGIN);
    if rect.width > bounded && bounded >= MIN_POPUP_WIDTH {
        rect.x = area.x + (area.width - bounded) / 2;
        rect.width = bounded;
    }
    rect
}

/// 权限参数内容在给定终端区域内实际可用的列数。与
/// `draw_permission_dialog` 共用同一矩形和边距计算，避免测试或
/// 预览再次把百分比误当成固定列数。边框 2 列与弹窗内边距
/// 2×POPUP_TEXT_PADDING 列必须先扣掉，预览行才不会贴框或右侧被裁。
pub(super) fn permission_argument_width(area: Rect) -> usize {
    centered_rect(84, 1, area)
        .width
        .saturating_sub(2) // 边框
        .saturating_sub(2 * POPUP_TEXT_PADDING) // 弹窗内边距
        .saturating_sub(4) as usize // 参数缩进/留白
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    /// CP-3（2026-09-02）判别：守卫几何——Clear 恰为 rect 左右各扩
    /// 1 列、纵向不扩。pre-fix（上下各扩 1 行的历史对称实现）本测试
    /// 的纵向腿红：框上/下紧邻行的底衬内容被清成空白。
    #[test]
    fn guard_clears_one_column_sideways_and_no_rows_vertically() {
        let mut terminal = Terminal::new(TestBackend::new(20, 8)).unwrap();
        terminal
            .draw(|frame| {
                // 铺底衬：全屏 'X' 模拟上一帧残留/底层 UI 内容。
                for y in 0..frame.area().height {
                    for x in 0..frame.area().width {
                        frame.buffer_mut()[(x, y)].set_char('X');
                    }
                }
                clear_popup_with_guards(frame, Rect::new(5, 2, 10, 4));
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let symbol = |x: u16, y: u16| buffer[(x, y)].symbol().to_owned();
        // 纵向腿：框上/下紧邻行（y=1、y=6）整行保留底衬——上下不扩。
        for y in [1_u16, 6] {
            for x in 4..=15_u16 {
                assert_eq!(symbol(x, y), "X", "row {y} must keep its backing content");
            }
        }
        // 横向腿：框行（y=2..5）内左右各扩 1 列（x=4、x=15）被清空，
        // 再外一圈（x=3、x=16）保留。
        for y in 2..6_u16 {
            assert_eq!(symbol(3, y), "X");
            assert_eq!(symbol(4, y), " ", "left guard column must be cleared");
            for x in 5..15_u16 {
                assert_eq!(symbol(x, y), " ");
            }
            assert_eq!(symbol(15, y), " ", "right guard column must be cleared");
            assert_eq!(symbol(16, y), "X");
        }
    }
}
