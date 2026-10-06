use super::*;
use ratatui::{Terminal, backend::TestBackend, style::Color};
use serde_json::json;

struct Fixture {
    app: App,
    terminal: Terminal<TestBackend>,
    root: PathBuf,
    project: PathBuf,
}
impl Fixture {
    fn new(width: u16, height: u16, view: View) -> Self {
        let (root, project) = crate::test_support::roots("plg7-style");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let client = crate::host::HostClient::fixture_for_frontend_tests(&root);
        let mut app = App::open_native(Project::new(&project), client.clone()).unwrap();
        app.set_native_online_for_snapshot(true);
        app.test_freeze_tick = true;
        app.default_status = "<HOST-ROOT>".into();
        app.status = "<HOST-ROOT>".into();
        let mut dialog = PluginDialog::new();
        dialog.view = view;
        dialog.status = "Host ready".into();
        let row = json!({"id":"dev.example.greeter","name":"Greeter 插件","version":"1.0.0",
            "runtime":"wasm-component","enabled":true,"health":"healthy","publisher":"example",
            "summary":"A small local plugin."});
        dialog.installed = vec![row.clone()];
        let mut market_row = row;
        market_row.as_object_mut().unwrap().remove("enabled");
        dialog.market = vec![market_row];
        dialog.review = Some(client.plugin_review_fixture_for_tests(
            json!({"action":"install","review":{
            "packages":[{"id":"dev.example.greeter","runtime":"wasm-component",
                "capabilities":{"tools":true},"config_schema":{"type":"object"}}]}}),
        ));
        dialog.remove = Some(("dev.example.greeter".into(), true));
        dialog
            .config
            .insert_str("{\"id\":{\"key\":\"private-style-marker\"}}");
        app.plugins.dialog = Some(dialog);
        Self {
            app,
            terminal: Terminal::new(TestBackend::new(width, height)).unwrap(),
            root,
            project,
        }
    }
    fn draw(&mut self) {
        self.terminal.draw(|frame| self.app.draw(frame)).unwrap();
    }
    fn snapshot(&mut self, name: &str) {
        self.draw();
        let first = self.projection();
        self.draw();
        assert_eq!(first, self.projection(), "stable visual projection");
        crate::tui::snapshot_tests::check_or_refresh(name, &first);
    }
    fn projection(&self) -> String {
        let text = crate::tui::snapshot_tests::render_projection(
            self.terminal.backend().buffer(),
            (0, 0),
            0,
        );
        crate::tui::snapshot_tests::normalize_paths(&text, &self.project)
    }
    fn popup(&self) -> Rect {
        let buffer = self.terminal.backend().buffer();
        let (x, y) = (0..buffer.area.height)
            .find_map(|y| {
                (0..buffer.area.width).find_map(|x| {
                    (buffer[(x, y)].symbol() == "┌" && buffer[(x, y)].fg == Color::Yellow)
                        .then_some((x, y))
                })
            })
            .expect("yellow popup border");
        let bottom = (y + 1..buffer.area.height)
            .find(|row| buffer[(x, *row)].symbol() == "└")
            .unwrap();
        let right = (x + 1..buffer.area.width)
            .find(|column| buffer[(*column, y)].symbol() == "┐")
            .unwrap();
        Rect::new(x, y, right - x + 1, bottom - y + 1)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.close_plugin_dialog();
        crate::test_support::cleanup_tree(&self.root);
    }
}

#[test]
fn plg7_installed_snapshot_uses_family_shell_and_short_height() {
    let mut fixture = Fixture::new(80, 24, View::Installed);
    fixture.draw();
    let area = fixture.popup();
    assert!(
        area.height < popup_height_cap(fixture.terminal.backend().buffer().area),
        "one package must shrink the popup"
    );
    let buffer = fixture.terminal.backend().buffer();
    let navigation = (area.y..area.bottom())
        .find_map(|y| {
            (area.x..area.right()).find_map(|x| {
                (x + 2 < area.right()
                    && buffer[(x, y)].symbol() == "T"
                    && buffer[(x + 1, y)].symbol() == "a"
                    && buffer[(x + 2, y)].symbol() == "b")
                    .then_some((x, y))
            })
        })
        .expect("Tab navigation footnote");
    assert_eq!(
        buffer[navigation].fg,
        Color::DarkGray,
        "navigation footnote uses Faint"
    );
    fixture.snapshot("plugin-installed-short");
}

#[test]
fn plg7_five_view_snapshots_share_the_popup_family() {
    for (view, name) in [
        (View::Market, "plugin-market-short"),
        (View::Review, "plugin-review"),
        (View::Config, "plugin-config"),
        (View::Remove, "plugin-remove"),
    ] {
        let mut fixture = Fixture::new(80, 24, view);
        fixture.draw();
        fixture.popup();
        assert!(!fixture.projection().contains("private-style-marker"));
        fixture.snapshot(name);
    }
}

#[test]
fn plg7_review_reaches_last_line_before_consent_at_narrow_heights() {
    for (width, height) in [(80, 24), (40, 18), (30, 12)] {
        let mut fixture = Fixture::new(width, height, View::Review);
        let ticket = fixture.app.native_plugin_context().unwrap().0.plugin_review_fixture_for_tests(json!({"action":"install","review":{
            "packages":[{"id":"first","capabilities":{"tools":true}},{"id":"last-capability-marker","capabilities":{"tools":true}}]}}));
        fixture.app.plugins.dialog.as_mut().unwrap().review = Some(ticket);
        fixture.draw();
        assert!(!fixture.app.plugins.dialog.as_ref().unwrap().can_commit());
        let total = fixture.app.plugins.dialog.as_ref().unwrap().total;
        for _ in 0..total {
            fixture
                .app
                .plugin_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
            fixture.draw();
        }
        let dialog = fixture.app.plugins.dialog.as_ref().unwrap();
        assert_eq!(
            dialog.reviewed, dialog.total,
            "all visible capability lines are reachable"
        );
        fixture
            .app
            .plugin_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(fixture.app.plugins.dialog.as_ref().unwrap().can_commit());
    }
}

#[test]
fn plg7_horizontal_guards_clear_wide_glyph_origins() {
    let mut fixture = Fixture::new(80, 24, View::Installed);
    fixture.draw();
    let area = fixture.popup();
    let background = "界".repeat(40);
    fixture
        .terminal
        .draw(|frame| {
            let rows = (0..24)
                .map(|_| Line::from(background.clone()))
                .collect::<Vec<_>>();
            frame.render_widget(
                Paragraph::new(rows),
                Rect::new(area.x - 1, 0, 80 - area.x + 1, 24),
            );
        })
        .unwrap();
    fixture
        .terminal
        .draw(|frame| {
            let rows = (0..24)
                .map(|_| Line::from(background.clone()))
                .collect::<Vec<_>>();
            frame.render_widget(
                Paragraph::new(rows),
                Rect::new(area.x - 1, 0, 80 - area.x + 1, 24),
            );
            fixture.app.draw_plugin_dialog(frame);
        })
        .unwrap();
    let buffer = fixture.terminal.backend().buffer();
    for y in area.y..area.bottom() {
        assert_eq!(
            buffer[(area.x - 1, y)].symbol(),
            " ",
            "wide-glyph origin in left guard must be cleared"
        );
        assert_eq!(
            buffer[(area.right(), y)].symbol(),
            " ",
            "right guard must be cleared"
        );
    }
}

#[test]
fn plg7_short_review_resize_still_clears_consent_when_body_height_is_unchanged() {
    let mut fixture = Fixture::new(80, 50, View::Review);
    fixture.draw();
    let dialog = fixture.app.plugins.dialog.as_mut().unwrap();
    assert_eq!(dialog.reviewed, dialog.total);
    dialog.accepted = true;
    let page = dialog.page;
    fixture.terminal = Terminal::new(TestBackend::new(80, 60)).unwrap();
    fixture.draw();
    let dialog = fixture.app.plugins.dialog.as_ref().unwrap();
    assert_eq!(
        dialog.page, page,
        "short content keeps the same body height"
    );
    assert!(
        !dialog.accepted,
        "physical resize must retain PLG-6 consent reset semantics"
    );
}
