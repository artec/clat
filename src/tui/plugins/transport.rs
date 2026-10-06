use super::*;

impl App {
    pub(super) fn plugin_request(
        &mut self,
    ) -> Option<(crate::host::HostClient, SyncSender<UiEvent>, u64, u64, u64)> {
        self.plugins.jobs.retain(|job| !job.is_finished());
        let context = self.native_plugin_context();
        let dialog = self.plugins.dialog.as_mut()?;
        let Some((client, epoch, selection)) = context else {
            dialog.status = "Host offline · close and /reconnect".into();
            return None;
        };
        let ui = self.event_sender.clone()?;
        self.plugins.request_serial = self.plugins.request_serial.wrapping_add(1);
        dialog.request = self.plugins.request_serial;
        dialog.pending = true;
        Some((client, ui, epoch, selection, dialog.request))
    }
    pub(super) fn request_plugin_list(&mut self, market: bool) {
        if market && let Some(dialog) = self.plugins.dialog.as_mut() {
            // A failed generation switch must never leave old-generation rows
            // actionable under the new heading.
            dialog.market.clear();
            dialog.status = "Loading verified market…".into();
        }
        let generation = self
            .plugins
            .dialog
            .as_ref()
            .map(|d| d.market_generation)
            .unwrap_or(1);
        let Some((client, ui, epoch, selection, request)) = self.plugin_request() else {
            return;
        };
        let job = thread::spawn(move || {
            let reply = if market {
                PluginReply::Market(client.plugin_market(generation))
            } else {
                PluginReply::List(client.plugin_list())
            };
            let _ = ui.send(UiEvent::Native(native::NativeEvent::Plugin(
                epoch, selection, request, reply,
            )));
        });
        self.plugins.jobs.push(job);
    }
    pub(super) fn request_plugin_review(&mut self, action: &str) {
        let Some(row) = self.plugins.dialog.as_ref().and_then(|d| d.selected()) else {
            return;
        };
        let id = label(&row, "id");
        let generation = row["manifest_version"].as_u64().unwrap_or(1) as u32;
        let Some((client, ui, epoch, selection, request)) = self.plugin_request() else {
            return;
        };
        let action = action.to_owned();
        self.plugins.dialog.as_mut().unwrap().status =
            "Reading verified permissions and configuration schema…".into();
        let job = thread::spawn(move || {
            let reply = PluginReply::Review(client.plugin_review(&id, &action, generation));
            // SendError owns the ticket and drops/cancels it if the UI exited.
            let _ = ui.send(UiEvent::Native(native::NativeEvent::Plugin(
                epoch, selection, request, reply,
            )));
        });
        self.plugins.jobs.push(job);
    }
    pub(super) fn commit_plugin_review(&mut self) {
        if !self.plugins.dialog.as_ref().is_some_and(|d| d.can_commit()) {
            return;
        }
        let Some((_, ui, epoch, selection, request)) = self.plugin_request() else {
            return;
        };
        let dialog = self.plugins.dialog.as_mut().unwrap();
        let mut ticket = dialog.review.take().unwrap();
        let accepted = dialog.accepted;
        let configs = if dialog.config.text().is_empty() {
            "{}".into()
        } else {
            dialog.config.take()
        };
        dialog.clear_review();
        dialog.view = View::Installed;
        dialog.status = "Activating once; no automatic retry…".into();
        let job = thread::spawn(move || {
            let reply = PluginReply::Mutation(ticket.commit(accepted, &configs));
            let _ = ui.send(UiEvent::Native(native::NativeEvent::Plugin(
                epoch, selection, request, reply,
            )));
        });
        self.plugins.jobs.push(job);
    }
    pub(super) fn remove_plugin(&mut self) {
        let Some((id, uninstall)) = self.plugins.dialog.as_ref().and_then(|d| d.remove.clone())
        else {
            return;
        };
        let Some((client, ui, epoch, selection, request)) = self.plugin_request() else {
            return;
        };
        let dialog = self.plugins.dialog.as_mut().unwrap();
        dialog.remove = None;
        dialog.view = View::Installed;
        let job = thread::spawn(move || {
            let reply = PluginReply::Mutation(client.plugin_remove(&id, uninstall));
            let _ = ui.send(UiEvent::Native(native::NativeEvent::Plugin(
                epoch, selection, request, reply,
            )));
        });
        self.plugins.jobs.push(job);
    }
    pub(in crate::tui) fn plugin_loaded(
        &mut self,
        epoch: u64,
        selection: u64,
        request: u64,
        reply: PluginReply,
    ) {
        if self.native_selection_identity() != Some((epoch, selection))
            || !self
                .plugins
                .dialog
                .as_ref()
                .is_some_and(|d| d.request == request)
        {
            return;
        }
        let dialog = self.plugins.dialog.as_mut().unwrap();
        dialog.pending = false;
        match reply {
            PluginReply::List(result) => set_rows(dialog, result, false),
            PluginReply::Market(result) => set_rows(dialog, result, true),
            PluginReply::Review(result) => match result {
                Ok(ticket) => {
                    dialog.clear_review();
                    dialog.view = View::Review;
                    dialog.expires = Some(
                        Instant::now()
                            + Duration::from_secs(
                                ticket.review["expires_in_seconds"].as_u64().unwrap_or(0),
                            ),
                    );
                    dialog.review = Some(ticket);
                    dialog.status =
                        "Read all permissions, Space to approve, Enter to activate".into();
                }
                Err(error) => dialog.status = error,
            },
            PluginReply::Mutation(result) => {
                dialog.status = match result {
                    Ok(value)
                        if value["runtime_warnings"]
                            .as_array()
                            .is_some_and(|warnings| !warnings.is_empty()) =>
                    {
                        "Operation completed with runtime warnings; inspect /mcp".into()
                    }
                    Ok(_) => "Host operation completed; refreshing…".into(),
                    Err(e) => e,
                };
                dialog.view = View::Installed;
                self.refresh_native();
                self.request_plugin_list(false);
            }
        }
    }
}

fn set_rows(dialog: &mut PluginDialog, result: Result<Value, String>, market: bool) {
    match result {
        Ok(value) => {
            let rows = value[if market { "packages" } else { "installed" }]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if market {
                dialog.market = rows
            } else {
                dialog.installed = rows
            }
            dialog.selected = 0;
            if dialog.status.starts_with("Loading") {
                dialog.status = "Select a plugin · Tab switches installed/market".into();
            }
        }
        Err(error) => dialog.status = error,
    }
}

impl App {
    pub(in crate::tui) fn finish_plugin_jobs(&mut self) {
        self.close_plugin_dialog();
        for job in std::mem::take(&mut self.plugins.jobs) {
            let _ = job.join();
        }
        crate::host::HostClient::finish_plugin_review_cleanup();
    }
}
