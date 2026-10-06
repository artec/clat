use super::*;
mod content;
mod footer;
use content::browse_viewport;
pub(super) use content::plugin_lines;

impl App {
    pub(in crate::tui) fn draw_plugin_dialog(&mut self, frame: &mut Frame) {
        if self.pending_permission.is_some() || self.pending_ask_user.is_some() {
            self.close_plugin_dialog();
            return;
        }
        let Some(dialog) = self.plugins.dialog.as_mut() else {
            return;
        };
        dialog.expire();
        let terminal = frame.area();
        let width = popup_inner_width(84, terminal);
        let mut lines = plugin_lines(dialog, width);
        let footer_budget = popup_height_cap(terminal).saturating_sub(4) as usize;
        let bottom = footer::lines(dialog, width, lines.len(), footer_budget);
        let layout =
            content_dialog_layout(84, lines.len(), bottom.len().saturating_sub(1), terminal);
        let visible = layout.body.height as usize;
        if dialog.view == View::Review {
            let resized = self.plugins.review_terminal_area.replace(terminal) != Some(terminal);
            lines = review_viewport(dialog, lines, width, visible, resized);
        } else if matches!(dialog.view, View::Installed | View::Market) {
            lines = browse_viewport(dialog, width, visible);
        }
        clear_popup_with_guards(frame, layout.outer);
        frame.render_widget(popup_block(title(dialog.view)), layout.outer);
        frame.render_widget(Paragraph::new(lines), layout.body);
        // Rebuild after advancing the visible review count. Counter columns
        // have fixed width, so this cannot change the pagination geometry.
        let bottom = footer::lines(dialog, width, dialog.total, footer_budget);
        frame.render_widget(Paragraph::new(bottom), layout.footer);
    }
}

fn review_viewport(
    dialog: &mut PluginDialog,
    lines: Vec<Line<'static>>,
    width: usize,
    visible: usize,
    resized: bool,
) -> Vec<Line<'static>> {
    if resized || dialog.review_width != width || dialog.page != visible.max(1) {
        dialog.reviewed = 0;
        dialog.accepted = false;
        dialog.scroll = 0;
        dialog.review_width = width;
    }
    dialog.total = lines.len();
    dialog.page = visible.max(1);
    dialog.scroll = dialog.scroll.min(lines.len().saturating_sub(visible));
    if dialog.scroll <= dialog.reviewed {
        dialog.reviewed = dialog
            .reviewed
            .max((dialog.scroll + visible).min(lines.len()));
    }
    lines
        .into_iter()
        .skip(dialog.scroll)
        .take(visible)
        .collect()
}

fn title(view: View) -> &'static str {
    match view {
        View::Installed => "/plugin · installed",
        View::Market => "/plugin · market",
        View::Review => "/plugin · permissions",
        View::Config => "/plugin · configuration",
        View::Remove => "/plugin · confirmation",
    }
}

#[cfg(test)]
pub(super) fn browse_viewport_for_tests(
    dialog: &PluginDialog,
    width: usize,
    height: usize,
) -> String {
    browse_viewport(dialog, width, height)
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
        .collect()
}
