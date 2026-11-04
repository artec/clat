//! Background, read-only repository browsing through the core Git service.
use super::*;
use clat_core::client_ports::git_read::GitReadQuery;
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct GitState {
    page: GitPage,
    request: String,
    loading: bool,
    failed: bool,
    repository: String,
    project_name: String,
    rows: Vec<GitRow>,
    files: Vec<Value>,
    detail: String,
    detail_back: GitPage,
    selection: usize,
    offset: usize,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum GitPage {
    #[default]
    Repositories,
    Categories,
    Files,
    History,
    Detail,
}

struct GitRow {
    label: String,
    id: String,
    staged: bool,
}

impl App {
    pub(super) fn open_workbench_git(&mut self) {
        let view = self.workbench.as_mut().expect("workbench");
        view.view = View::Git;
        view.git = GitState::default();
        view.git.project_name = self
            .project
            .root()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.git_query(GitReadQuery::Repositories, json!({}));
    }

    fn git_query(&mut self, query: GitReadQuery, params: Value) {
        let request = uuid::Uuid::new_v4().to_string();
        let identity = self.native_selection_identity();
        let session = self.session_id.clone();
        let project = self.project.root().to_path_buf();
        let sender = self.event_sender.clone();
        let native = self.native_plugin_context();
        let local = self
            .application
            .as_ref()
            .and_then(|app| app.git_read_client().ok());
        let state = &mut self.workbench.as_mut().expect("workbench").git;
        state.request.clone_from(&request);
        state.loading = true;
        state.failed = false;
        let Some(sender) = sender else {
            state.loading = false;
            state.failed = true;
            return;
        };
        thread::spawn(move || {
            let result = if let Some((client, _, _)) = native {
                client.call(query.rpc_method(), &params)
            } else if identity.is_some() {
                Err(String::new())
            } else if let Some(client) = local {
                client.query(query, params)
            } else {
                Err(String::new())
            };
            let _ = sender.send(UiEvent::GitRead {
                request,
                identity,
                session,
                project,
                result,
            });
        });
    }

    pub(in crate::tui) fn workbench_git_reply(
        &mut self,
        request: &str,
        identity: Option<(u64, u64)>,
        session: Option<SessionId>,
        project: &std::path::Path,
        result: Result<Value, String>,
    ) {
        if self.native_selection_identity() != identity
            || self.session_id != session
            || self.project.root() != project
        {
            return;
        }
        let Some(view) = self
            .workbench
            .as_mut()
            .filter(|view| matches!(view.view, View::Git))
        else {
            return;
        };
        let state = &mut view.git;
        if state.request != request {
            return;
        }
        state.loading = false;
        match result {
            Ok(value) => state.apply_reply(value, self.locale.text("tui.git_diff_unavailable")),
            Err(_) => state.failed = true,
        }
    }

    pub(super) fn handle_workbench_git_key(&mut self, key: KeyEvent) {
        let state = &mut self.workbench.as_mut().expect("workbench").git;
        if key.code == KeyCode::Esc {
            state.request.clear();
            state.loading = false;
            state.failed = false;
            match state.page {
                GitPage::Repositories => self.workbench.as_mut().unwrap().view = View::Menu,
                GitPage::Categories => {
                    state.page = GitPage::Repositories;
                    state.selection = 0;
                    self.git_query(GitReadQuery::Repositories, json!({}));
                }
                GitPage::Files | GitPage::History => {
                    state.page = GitPage::Categories;
                    state.selection = 0;
                }
                GitPage::Detail => {
                    state.page = state.detail_back;
                    state.offset = 0;
                }
            }
            return;
        }
        if state.page == GitPage::Detail {
            match key.code {
                KeyCode::Up => state.offset = state.offset.saturating_sub(1),
                KeyCode::Down => state.offset = state.offset.saturating_add(1),
                KeyCode::PageUp => state.offset = state.offset.saturating_sub(10),
                KeyCode::PageDown => state.offset = state.offset.saturating_add(10),
                _ => {}
            }
            return;
        }
        if state.loading || state.failed {
            return;
        }
        match key.code {
            KeyCode::Up => state.selection = state.selection.saturating_sub(1),
            KeyCode::Down => {
                state.selection = (state.selection + 1).min(state.len().saturating_sub(1))
            }
            KeyCode::Enter => self.git_select(),
            _ => {}
        }
    }

    fn git_select(&mut self) {
        let state = &mut self.workbench.as_mut().expect("workbench").git;
        match state.page {
            GitPage::Repositories => {
                let Some(row) = state.rows.get(state.selection) else {
                    return;
                };
                state.repository.clone_from(&row.id);
                state.page = GitPage::Categories;
                state.selection = 0;
                let params = json!({"repositoryId":state.repository});
                self.git_query(GitReadQuery::Snapshot, params);
            }
            GitPage::Categories if state.selection < 2 => {
                let staged = state.selection == 0;
                state.rows = files_for_side(&state.files, staged);
                state.page = GitPage::Files;
                state.selection = 0;
            }
            GitPage::Categories => {
                state.page = GitPage::History;
                state.selection = 0;
                let params = json!({"repositoryId":state.repository, "scope":"current"});
                self.git_query(GitReadQuery::History, params);
            }
            GitPage::Files => {
                let Some(row) = state.rows.get(state.selection) else {
                    return;
                };
                let params =
                    json!({"repositoryId":state.repository,"path":row.id,"staged":row.staged});
                state.page = GitPage::Detail;
                state.detail_back = GitPage::Files;
                state.offset = 0;
                self.git_query(GitReadQuery::Diff, params);
            }
            GitPage::History => {
                if let Some(row) = state.rows.get(state.selection) {
                    state.detail = format!("{}\n\n{}", row.label, row.id);
                    state.detail_back = GitPage::History;
                    state.page = GitPage::Detail;
                    state.offset = 0;
                }
            }
            GitPage::Detail => {}
        }
    }

    pub(super) fn git_lines(
        &self,
        view: &Workbench,
        area: Rect,
        visible: usize,
    ) -> Vec<Line<'static>> {
        let state = &view.git;
        if state.loading {
            return vec![Line::from(self.locale.text("tui.workbench_loading"))];
        }
        if state.failed {
            return vec![Line::from(self.locale.text("tui.workbench_unavailable"))];
        }
        if state.page == GitPage::Detail {
            let width = (area.width as usize * 84 / 100).saturating_sub(4).max(1);
            let lines = wrap_text(&state.detail, width);
            let start = state.offset.min(lines.len().saturating_sub(1));
            return lines
                .into_iter()
                .skip(start)
                .take(visible)
                .map(Line::from)
                .collect();
        }
        let labels: Vec<String> = if state.page == GitPage::Categories {
            ["tui.git_staged", "tui.git_unstaged", "tui.git_history"]
                .iter()
                .map(|key| self.locale.text(key).to_owned())
                .collect()
        } else {
            state.rows.iter().map(|row| row.label.clone()).collect()
        };
        if labels.is_empty() {
            return vec![Line::from(self.locale.text("tui.workbench_empty"))];
        }
        let start = state.selection.saturating_add(1).saturating_sub(visible);
        labels
            .into_iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(i, label)| {
                Line::from(format!(
                    "{} {label}",
                    if i == state.selection { "›" } else { " " }
                ))
            })
            .collect()
    }
}

impl GitState {
    fn len(&self) -> usize {
        if self.page == GitPage::Categories {
            3
        } else {
            self.rows.len()
        }
    }
    fn apply_reply(&mut self, value: Value, unavailable: &str) {
        match self.page {
            GitPage::Repositories => {
                self.rows = value["repositories"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| v["available"] == true)
                    .filter_map(|v| {
                        Some(GitRow {
                            id: v["id"].as_str()?.into(),
                            label: v["path"]
                                .as_str()
                                .filter(|path| !path.is_empty())
                                .unwrap_or(&self.project_name)
                                .into(),
                            staged: false,
                        })
                    })
                    .collect()
            }
            GitPage::Categories => {
                self.files = value["files"].as_array().cloned().unwrap_or_default()
            }
            GitPage::History => {
                self.rows = value["commits"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| {
                        Some(GitRow {
                            id: v["id"].as_str()?.into(),
                            label: format!(
                                "{} · {}",
                                v["date"].as_str().unwrap_or(""),
                                v["subject"].as_str().unwrap_or("")
                            ),
                            staged: false,
                        })
                    })
                    .collect()
            }
            GitPage::Detail => {
                self.detail = if value["binary"] == true || !value["unsupported"].is_null() {
                    unavailable.into()
                } else if let Some(patch) = value["patch"].as_str() {
                    patch.into()
                } else {
                    format!(
                        "{}\n{}\n\n{}\n{}",
                        value["leftLabel"].as_str().unwrap_or(""),
                        value["left"].as_str().unwrap_or(""),
                        value["rightLabel"].as_str().unwrap_or(""),
                        value["right"].as_str().unwrap_or("")
                    )
                };
            }
            GitPage::Files => {}
        }
    }
}

fn files_for_side(files: &[Value], staged: bool) -> Vec<GitRow> {
    files
        .iter()
        .filter(|file| {
            if staged {
                file["staged"] == true
            } else {
                file["unstaged"] == true || file["untracked"] == true
            }
        })
        .filter_map(|file| {
            let path = file["path"].as_str()?;
            Some(GitRow {
                label: format!("{} · {path}", file["status"].as_str().unwrap_or("")),
                id: path.into(),
                staged,
            })
        })
        .collect()
}

#[cfg(all(test, feature = "runtime-tests"))]
mod tests {
    use super::*;
    use crate::tui::snapshot_tests::Harness;

    #[test]
    fn git_old_repository_reply_cannot_replace_new_selection() {
        let mut harness = Harness::trusted("git-reply-scope", 80, 24);
        let app = &mut harness.app;
        let session = app.session_id.clone();
        let project = app.project.root().to_path_buf();
        let workbench = Workbench {
            view: View::Git,
            git: GitState {
                page: GitPage::Categories,
                repository: "second".into(),
                request: "new-request".into(),
                loading: true,
                ..GitState::default()
            },
            ..Workbench::default()
        };
        app.workbench = Some(workbench);
        app.workbench_git_reply(
            "old-request",
            None,
            session.clone(),
            &project,
            Ok(json!({"files":[{"path":"old"}]})),
        );
        assert!(app.workbench.as_ref().unwrap().git.loading);
        assert!(app.workbench.as_ref().unwrap().git.files.is_empty());
        app.workbench_git_reply(
            "new-request",
            None,
            session.clone(),
            &project.join("other"),
            Ok(json!({"files":[{"path":"wrong-project"}]})),
        );
        assert!(app.workbench.as_ref().unwrap().git.files.is_empty());
        app.workbench_git_reply(
            "new-request",
            None,
            session,
            &project,
            Ok(json!({"files":[{"path":"new"}]})),
        );
        assert_eq!(app.workbench.as_ref().unwrap().git.files[0]["path"], "new");
    }

    #[test]
    fn staged_and_unstaged_lists_preserve_shared_files() {
        let files = vec![
            json!({"path":"both", "staged":true, "unstaged":true}),
            json!({"path":"new", "untracked":true}),
        ];
        assert_eq!(files_for_side(&files, true).len(), 1);
        assert_eq!(files_for_side(&files, false).len(), 2);
    }
}
