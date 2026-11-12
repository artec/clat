//! Per-conversation session share selections for the terminal composer.

use super::{App, UiMessage};
use crate::SessionId;
use clat_core::client_ports::history::{HistoryReference, SessionShareSelection};
use clat_core::client_ports::host::HostSessionSharePreview;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct ShareDrafts(HashMap<Option<SessionId>, Vec<DraftShare>>);

#[derive(Clone, Debug)]
pub(super) struct DraftShare {
    pub id: String,
    pub preview: HostSessionSharePreview,
}

#[derive(Clone, Debug)]
pub(super) struct ShareRefresh {
    id: String,
    previous_version: String,
}

impl ShareDrafts {
    pub fn items(&self, owner: &Option<SessionId>) -> &[DraftShare] {
        self.0.get(owner).map_or(&[], Vec::as_slice)
    }

    pub fn add(&mut self, owner: Option<SessionId>, preview: HostSessionSharePreview) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.0.entry(owner).or_default().push(DraftShare {
            id: id.clone(),
            preview,
        });
        id
    }

    pub fn remove(&mut self, owner: &Option<SessionId>, index: usize) -> Option<DraftShare> {
        let items = self.0.get_mut(owner)?;
        (index < items.len()).then(|| items.remove(index))
    }

    fn refresh(
        &mut self,
        owner: &Option<SessionId>,
        request: ShareRefresh,
        preview: HostSessionSharePreview,
    ) {
        if let Some(item) = self.0.get_mut(owner).and_then(|items| {
            items.iter_mut().find(|item| {
                item.id == request.id && item.preview.selection.version == request.previous_version
            })
        }) {
            item.preview = preview;
        }
    }

    pub fn materialize(&mut self, owner: &Option<SessionId>) {
        if owner.is_some()
            && let Some(draft) = self.0.remove(&None)
        {
            self.0.entry(owner.clone()).or_default().extend(draft);
        }
    }

    /// Only remove identities acknowledged by this submission. Newer additions remain.
    pub fn committed(&mut self, owner: &Option<SessionId>, sent: &[String]) {
        let Some(items) = self.0.get_mut(owner) else {
            return;
        };
        items.retain(|item| !sent.contains(&item.id));
    }

    pub fn take_sent(&mut self, owner: &Option<SessionId>, sent: &[String]) -> Vec<DraftShare> {
        let Some(items) = self.0.get_mut(owner) else {
            return Vec::new();
        };
        let mut selected = Vec::new();
        items.retain(|item| {
            if sent.contains(&item.id) {
                selected.push(item.clone());
                false
            } else {
                true
            }
        });
        selected
    }

    pub fn restore(&mut self, owner: Option<SessionId>, mut shares: Vec<DraftShare>) {
        shares.append(self.0.entry(owner.clone()).or_default());
        self.0.insert(owner, shares);
    }
}

impl App {
    pub(super) fn session_share_submission(&self) -> (Vec<String>, Vec<SessionShareSelection>) {
        let items = self.share_drafts.items(&self.session_id);
        (
            items.iter().map(|item| item.id.clone()).collect(),
            items
                .iter()
                .map(|item| item.preview.selection.clone())
                .collect(),
        )
    }

    pub(super) fn session_share_rail(&self) -> Vec<ratatui::text::Line<'static>> {
        use ratatui::text::{Line, Span};
        let mut lines: Vec<_> = self
            .share_drafts
            .items(&self.session_id)
            .iter()
            .take(4)
            .enumerate()
            .map(|(index, item)| {
                let count = self.locale.render(
                    &UiMessage::new("input.share_count")
                        .arg("turns", item.preview.turns)
                        .arg("characters", item.preview.characters),
                );
                Line::from(Span::styled(
                    format!("[{}] {} · {}", index + 1, item.preview.title, count),
                    super::theme::style(super::theme::Role::Faint),
                ))
            })
            .collect();
        let more = self
            .share_drafts
            .items(&self.session_id)
            .len()
            .saturating_sub(4);
        if more > 0 {
            lines.push(Line::from(Span::styled(
                self.locale
                    .render(&UiMessage::new("input.share_more").arg("count", more)),
                super::theme::style(super::theme::Role::Faint),
            )));
        }
        if !lines.is_empty() {
            lines.push(Line::from(Span::styled(
                self.locale.text("input.share_rail_hint"),
                super::theme::style(super::theme::Role::Faint),
            )));
        }
        lines
    }

    pub(super) fn freeze_local_session_shares(
        &self,
        message: crate::message::PendingMessage,
        queued: bool,
    ) -> Result<(crate::message::PendingMessage, Vec<String>), String> {
        let items = self.share_drafts.items(&self.session_id);
        if queued || items.is_empty() {
            return Ok((message, Vec::new()));
        }
        let ids = items.iter().map(|item| item.id.clone()).collect();
        let selections = items
            .iter()
            .map(|item| item.preview.selection.clone())
            .collect::<Vec<_>>();
        let app = self.application.as_ref().ok_or_else(|| {
            self.locale
                .text("tui.project_application_is_unavailable")
                .to_owned()
        })?;
        let frozen = app
            .freeze_session_share_message(message, &selections, &crate::CancelToken::new())
            .map_err(|error| error.to_string())?;
        Ok((frozen, ids))
    }

    pub(super) fn handle_share_command(&mut self, text: &str) -> bool {
        let Some(rest) = text.strip_prefix("/share-session") else {
            return false;
        };
        if !rest.is_empty() && !rest.starts_with(' ') {
            return false;
        }
        let rest = rest.trim();
        if rest == "list" {
            self.show_share_list();
            return true;
        }
        if let Some(index) = rest
            .strip_prefix("remove ")
            .and_then(|v| v.parse::<usize>().ok())
        {
            if index > 0 {
                self.share_drafts.remove(&self.session_id, index - 1);
            }
            return true;
        }
        if let Some(index) = rest
            .strip_prefix("refresh ")
            .and_then(|value| value.parse::<usize>().ok())
        {
            if let Some(item) = index
                .checked_sub(1)
                .and_then(|index| self.share_drafts.items(&self.session_id).get(index))
                .cloned()
            {
                let previous_version = item.preview.selection.version;
                self.request_share_preview(
                    item.preview.selection.reference,
                    item.preview.selection.recent_turns,
                    Some(ShareRefresh {
                        id: item.id,
                        previous_version,
                    }),
                );
            } else {
                self.flash_message(UiMessage::new("history.invalid_query").arg("detail", ""));
            }
            return true;
        }
        let range = if rest.is_empty() {
            None
        } else if let Ok(n) = rest.parse::<usize>() {
            if n == 0 {
                self.flash_message(UiMessage::new("history.invalid_query").arg("detail", ""));
                return true;
            }
            Some(n)
        } else {
            self.flash_message(UiMessage::new("history.invalid_query").arg("detail", ""));
            return true;
        };
        self.share_picker = Some(range);
        if self.native.is_some() {
            self.open_native_sessions();
        } else if let Some(app) = &self.application {
            match app.list_sessions() {
                Ok(rows) => self.refresh_picker_sessions(rows),
                Err(error) => {
                    self.share_picker = None;
                    self.flash_message(error.presentation());
                }
            }
        } else {
            self.share_picker = None;
            self.flash_message(UiMessage::new("tui.project_application_is_unavailable"));
        }
        true
    }

    fn show_share_list(&mut self) {
        let text = self
            .share_drafts
            .items(&self.session_id)
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let count = self.locale.render(
                    &UiMessage::new("input.share_count")
                        .arg("turns", item.preview.turns)
                        .arg("characters", item.preview.characters),
                );
                format!("{}. {} · {}", i + 1, item.preview.title, count)
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.content_view = Some(super::ContentView::Remote {
            title: "/share-session",
            text: text.into(),
        });
        self.info_dialog = Some(super::InfoDialog::new(super::InfoDialogKind::Remote));
    }

    pub(super) fn preview_session_share(&mut self, source: SessionId, range: Option<usize>) {
        if self.session_id.as_ref() == Some(&source) {
            return;
        }
        let root = match self.project.root().canonicalize() {
            Ok(root) => root,
            Err(error) => {
                self.flash_status(error.to_string());
                return;
            }
        };
        let reference = HistoryReference {
            project: Some(root.to_string_lossy().into_owned()),
            session: source.as_str().into(),
            seq: None,
        };
        self.request_share_preview(reference, range, None);
    }

    fn request_share_preview(
        &mut self,
        reference: HistoryReference,
        range: Option<usize>,
        refresh: Option<ShareRefresh>,
    ) {
        let owner = self.session_id.clone();
        if let (Some(native), Some(ui)) = (&self.native, self.event_sender.clone()) {
            let client = native.client.clone();
            let (epoch, selection) = (native.epoch, native.selection);
            std::thread::spawn(move || {
                let result = client.session_share_preview(&reference, range, selection);
                let _ = ui.send(super::worker::UiEvent::Native(
                    super::native::NativeEvent::SharePreview(
                        epoch, selection, owner, refresh, result,
                    ),
                ));
            });
        } else if let Some(app) = &self.application {
            let result = app
                .history_reader()
                .share(&reference, range, &clat_core::CancelToken::new())
                .map(|share| HostSessionSharePreview {
                    selection: SessionShareSelection {
                        reference,
                        recent_turns: range,
                        version: share.version,
                    },
                    title: share.title,
                    turns: share.turns,
                    characters: share.characters,
                    last_time: share.last_time,
                    snapshot_time: share.snapshot_time,
                })
                .map_err(|error| error.to_string());
            self.finish_session_share_preview(owner, refresh, result);
        }
    }

    pub(super) fn finish_session_share_preview(
        &mut self,
        owner: Option<SessionId>,
        refresh: Option<ShareRefresh>,
        result: Result<HostSessionSharePreview, String>,
    ) {
        match result {
            Ok(preview) => {
                if let Some(refresh) = refresh {
                    self.share_drafts.refresh(&owner, refresh, preview);
                } else {
                    self.share_drafts.add(owner, preview);
                }
            }
            Err(error) => self.flash_status(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clat_core::client_ports::history::{HistoryReference, SessionShareSelection};

    fn preview(id: &str) -> HostSessionSharePreview {
        HostSessionSharePreview {
            selection: SessionShareSelection {
                reference: HistoryReference {
                    project: Some("/project".into()),
                    session: id.into(),
                    seq: None,
                },
                recent_turns: Some(2),
                version: "v1".into(),
            },
            title: id.into(),
            turns: 2,
            characters: 42,
            last_time: 1,
            snapshot_time: 2,
        }
    }

    #[test]
    fn share_drafts_are_isolated_by_destination_and_remove_only_selected_entry() {
        let mut drafts = ShareDrafts::default();
        let first = Some(SessionId::new("first"));
        let second = Some(SessionId::new("second"));
        drafts.add(first.clone(), preview("one"));
        drafts.add(first.clone(), preview("two"));
        drafts.add(second.clone(), preview("three"));
        assert_eq!(drafts.items(&first).len(), 2);
        assert_eq!(
            drafts
                .remove(&first, 0)
                .unwrap()
                .preview
                .selection
                .reference
                .session,
            "one"
        );
        assert_eq!(
            drafts.items(&first)[0].preview.selection.reference.session,
            "two"
        );
        assert_eq!(
            drafts.items(&second)[0].preview.selection.reference.session,
            "three"
        );
        assert!(drafts.remove(&second, 3).is_none());
    }

    #[test]
    fn share_drafts_materialization_moves_only_the_new_session_draft() {
        let mut drafts = ShareDrafts::default();
        drafts.add(None, preview("source"));
        let new = Some(SessionId::new("new"));
        drafts.materialize(&new);
        assert!(drafts.items(&None).is_empty());
        assert_eq!(drafts.items(&new).len(), 1);
    }

    #[test]
    fn share_drafts_receipt_does_not_remove_a_newer_or_different_draft() {
        let mut drafts = ShareDrafts::default();
        let owner = Some(SessionId::new("owner"));
        let sent = drafts.add(owner.clone(), preview("first"));
        drafts.add(owner.clone(), preview("later"));
        drafts.committed(&owner, std::slice::from_ref(&sent));
        assert_eq!(
            drafts.items(&owner)[0].preview.selection.reference.session,
            "later"
        );
        drafts.remove(&owner, 0);
        drafts.add(owner.clone(), preview("first"));
        drafts.committed(&owner, std::slice::from_ref(&sent));
        assert_eq!(
            drafts.items(&owner).len(),
            1,
            "a replacement with identical content is a new draft"
        );
        drafts.committed(&owner, std::slice::from_ref(&sent));
        assert_eq!(
            drafts.items(&owner)[0].preview.selection.reference.session,
            "first"
        );
    }

    #[test]
    fn share_drafts_receipt_removes_remaining_sent_entries_after_user_removal() {
        let mut drafts = ShareDrafts::default();
        let owner = Some(SessionId::new("owner"));
        let first = drafts.add(owner.clone(), preview("first"));
        let second = drafts.add(owner.clone(), preview("second"));
        drafts.remove(&owner, 0);
        drafts.add(owner.clone(), preview("new"));
        drafts.committed(&owner, &[first, second]);
        assert_eq!(drafts.items(&owner).len(), 1);
        assert_eq!(
            drafts.items(&owner)[0].preview.selection.reference.session,
            "new"
        );
    }

    #[test]
    fn share_preview_response_is_ignored_after_destination_or_generation_changes() {
        let root = std::env::temp_dir().join(format!("clat-share-picker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let client = clat_core::client_ports::host::HostClient::fixture_for_frontend_tests(&root);
        let mut app = App::open_native(crate::Project::new(&root), client).unwrap();
        let first = Some(SessionId::new("first"));
        let second = Some(SessionId::new("second"));
        app.session_id = second.clone();
        app.handle_native_event(super::super::native::NativeEvent::SharePreview(
            0,
            0,
            first.clone(),
            None,
            Ok(preview("source")),
        ));
        assert!(app.share_drafts.items(&first).is_empty());
        app.session_id = first.clone();
        app.handle_native_event(super::super::native::NativeEvent::SharePreview(
            0,
            1,
            first.clone(),
            None,
            Ok(preview("source")),
        ));
        assert!(app.share_drafts.items(&first).is_empty());
        app.handle_native_event(super::super::native::NativeEvent::SharePreview(
            0,
            0,
            first.clone(),
            None,
            Ok(preview("source")),
        ));
        assert_eq!(app.share_drafts.items(&first).len(), 1);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
