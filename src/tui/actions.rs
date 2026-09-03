use super::*;
use crate::dsh::backend::DshTask;
use crate::tui::model_editor::{
    EditorAction, ModelEditor, ModelPicker, PickerAction, ProfileSave, ProfileSummary,
};

impl App {
    /// Retain retry material until `SteeringApplied` confirms that the core
    /// crossed the durable claim point. The transcript's dim pending row is
    /// presentation only and cannot reconstruct image source paths.
    pub(super) fn remember_native_steering(
        &mut self,
        prompt: String,
        attachments: Vec<std::path::PathBuf>,
    ) {
        if let Some(position) = self
            .native_steering_claim_credits
            .iter()
            .position(|claimed| claimed == &prompt)
        {
            self.native_steering_claim_credits.remove(position);
            self.release_core_staged_attachment_paths(attachments);
            return;
        }
        self.pending_native_steering
            .push_back(super::NativeSteeringDraft {
                prompt,
                attachments,
            });
    }

    /// Restore one exact unclaimed draft without merging message boundaries.
    /// Additional drafts remain ordered for subsequent retries, so several
    /// individually valid eight-image messages never become one invalid one.
    pub(super) fn restore_next_recovered_steering(&mut self) -> bool {
        if !self.input.text().is_empty() || !self.attachments.is_empty() {
            return false;
        }
        let Some(draft) = self.recovered_native_steering.pop_front() else {
            return false;
        };
        if !draft.attachments.is_empty()
            && let Err(error) = self
                .attachments
                .add_paths(self.project.root(), draft.attachments.clone())
        {
            self.recovered_native_steering.push_front(draft);
            self.flash_status(format!(
                "steering recovery is waiting — source image is unavailable: {error}"
            ));
            return false;
        }
        self.input.insert_str(&draft.prompt);
        let remaining = self.recovered_native_steering.len();
        self.flash_status(if remaining == 0 {
            "unclaimed steering restored — edit or Enter to retry".into()
        } else {
            format!(
                "unclaimed steering restored — {remaining} more draft(s) remain queued for retry"
            )
        });
        true
    }

    /// 写入一条瞬时提示：显示 `STATUS_TTL` 后自动回落到常驻状态
    /// （当前目录）。
    pub(super) fn flash_status(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_until = Some(Instant::now() + STATUS_TTL);
    }

    /// draw 前调用：瞬时提示到期后回落为常驻状态。
    pub(super) fn expire_status(&mut self) {
        if status_expired(self.status_until, Instant::now()) {
            self.status = self.default_status.clone();
            self.status_until = None;
        }
    }

    /// 请求核心 Monitor 插件立即重新查询一次。用于配置变更与模型
    /// 运行结束（额度刚被消耗）。
    pub(super) fn refresh_balance_now(&mut self) {
        if let Some(application) = &self.application {
            application.refresh_monitor();
        }
    }

    /// 处理 /resume 选择器的动作：确认则切换会话，取消则关闭。
    pub(super) fn apply_resume_action(&mut self, action: ResumeAction) {
        match action {
            ResumeAction::Continue => {}
            ResumeAction::Cancel => {
                self.session_picker = None;
            }
            ResumeAction::OpenDsh(row) => {
                // dsh 七步切换（§2.6，§0-1 create 收养式）：②在
                // dsh_adopt_session 发出；③-⑦由 Created 回执驱动。
                self.session_picker = None;
                self.dsh_adopt_session(*row);
            }
            ResumeAction::Open(session_id) => {
                self.session_picker = None;
                match self.switch_session(session_id) {
                    Ok(()) => self.flash_status("conversation resumed"),
                    Err(error) => self.flash_status(format!("failed to resume: {error}")),
                }
            }
        }
    }

    /// 切换到指定会话（/resume 确认时）：workspace 选择 CAS → 冷恢
    /// 复目标会话（原始事件永不删除，随时可再次 resume）→ 重置视图。
    fn switch_session(&mut self, session_id: SessionId) -> Result<(), String> {
        let snapshot = self
            .application
            .as_mut()
            .ok_or_else(|| "project application is unavailable".to_owned())?
            .switch_session(session_id.clone())
            .map_err(|error| error.to_string())?;
        self.session_id = Some(session_id);
        self.session_title = snapshot.session_title;
        // 转录从回放重建（G2/G8）；输入历史随会话切换：恢复目标会话
        // 自己的历史（含内存中未持久化的导航状态一并重置）。
        self.conversation =
            crate::tui::conversation::ConversationModel::from_replay(&snapshot.replay);
        self.input = InputBuffer::new(snapshot.input_history);
        self.conversation_scroll_from_bottom = 0;
        // 用量指标归属会话（TUI-L04）：恢复目标会话的 journal 统计
        // （与挂载路径同源），Cache/Context 切换即有值；路由桶同源
        // 还原（INV-C1）。
        self.session_usage = snapshot.session_usage;
        self.usage_routes = snapshot.usage_routes;
        self.last_turn_usage = snapshot.last_request_usage;
        // Drafts are session-local UI state. Never carry an unsent image into
        // a resumed conversation where its identity would be misleading.
        self.clear_attachment_draft();
        let pending_paths = self
            .pending_native_steering
            .iter()
            .chain(self.recovered_native_steering.iter())
            .flat_map(|draft| draft.attachments.iter().cloned())
            .collect::<Vec<_>>();
        self.release_core_staged_attachment_paths(pending_paths);
        self.pending_native_steering.clear();
        self.native_steering_claim_credits.clear();
        self.recovered_native_steering.clear();
        Ok(())
    }

    /// Shift+Tab：循环思考档位并随模型配置持久化（INV-D）。生效于
    /// 下一次 run（`start_run` 每次重读 `model_state`）；标题栏即时
    /// 同步（`self.config` 原地更新，重绘即见）。保存失败整体回滚，
    /// 内存与库不出现半套配置。
    pub(super) fn cycle_thinking_level(&mut self) {
        let vendor = self.config.vendor();
        if thinking_levels(vendor).is_empty() {
            // TC-3：Tencent Hy 思考服务端常开（标题栏 Thinking · Server）
            // ——按键如实告知不可调；其它未知端点本就无档位。
            self.flash_status(if vendor == ModelVendor::Tencent {
                "thinking is always on for this model (server-side)"
            } else {
                "thinking levels apply to known reasoning vendors"
            });
            return;
        }
        // 当前生效档位：一等字段优先，其次解析 extra_body；手工编辑成
        // disabled 视为关闭，从 High 起步一键恢复。
        let current = effective_thinking_level(&self.config).unwrap_or(ThinkingLevel::High);
        let Some(next) = next_thinking_level(vendor, current) else {
            return;
        };
        let previous = self.config.clone();
        self.config.thinking_level = Some(next);
        let vendor = self.config.vendor();
        apply_thinking_level(&mut self.config.extra_body, vendor, next);
        let saved = match self.application.as_ref() {
            Some(application) => application
                .save_model_state(&self.config, &self.credentials)
                .map_err(|error| error.to_string()),
            // 未确权阶段没有项目应用：只改内存配置，确权后落盘。
            None => Ok(()),
        };
        match saved {
            Ok(()) => self.flash_status(format!("Thinking · {}", next.label())),
            Err(error) => {
                self.config = previous;
                self.flash_status(format!("failed to save thinking level: {error}"));
            }
        }
    }

    /// B9：控制面档案 → picker 摘要（含活动标记）。
    fn model_profile_summaries(&self) -> Vec<ProfileSummary> {
        let Some(application) = self.application.as_ref() else {
            return Vec::new();
        };
        let Ok(active) = application.active_model_profile() else {
            return Vec::new();
        };
        let Ok(profiles) = application.list_model_profiles() else {
            return Vec::new();
        };
        profiles
            .into_iter()
            .map(|profile| ProfileSummary {
                active: active.as_deref() == Some(profile.name.as_str()),
                name: profile.name,
                endpoint: profile.endpoint.clone(),
                model: profile.model.clone(),
            })
            .collect()
    }

    /// B9：按名取档案 (config, credentials)。
    fn load_model_profile(
        &self,
        name: &str,
    ) -> Option<(crate::ModelConfig, crate::model::ProviderCredentials)> {
        self.application
            .as_ref()?
            .load_model_profile(name)
            .ok()
            .flatten()
    }

    /// B9：删除回退后同步内存镜像（config/credentials/descriptors）。
    fn sync_model_state_from_application(&mut self) {
        let Some(application) = self.application.as_ref() else {
            return;
        };
        if let Ok((config, credentials)) = application.model_state() {
            self.config = config;
            self.credentials = credentials;
            self.provider_descriptors = application.provider_descriptors(&self.credentials);
        }
    }

    /// 处理二级选择器的动作。确认预设时：同端点且已存有密钥 → 直接
    /// 保存切换；跨厂商或缺密钥 → 转入编辑器补密钥（清空旧厂商密钥，
    /// 避免把一家厂商的 key 发给另一家）。
    pub(super) fn apply_picker_action(&mut self, action: PickerAction) {
        match action {
            PickerAction::Continue => {}
            PickerAction::Cancel => {
                self.picker = None;
                self.picker_return = None;
                self.flash_status("model selection cancelled");
            }
            PickerAction::SelectDshModel {
                provider,
                model,
                effort,
            } => {
                // dsh（§2.5）：Enter → selectModel（高亮行的循环档位随行
                // 提交）；model_label/effort 刷新在 TaskReply::Selected
                // 回执（修 D-1 只 flash 不刷新）。
                self.picker = None;
                self.picker_return = None;
                if let Some(dsh) = self.dsh.as_ref()
                    && let Some(session) = dsh.current_session().map(str::to_owned)
                {
                    dsh.send_task(DshTask::Select {
                        session,
                        provider,
                        model,
                        effort,
                    });
                    self.flash_status("selecting model…");
                } else {
                    self.flash_status("no active session");
                }
            }
            PickerAction::OpenProfileEditor { edit } => {
                // INV-U1：进入编辑器前拍下导航态，取消时原位重建。
                self.picker_return = self.picker.as_ref().map(ModelPicker::snapshot);
                self.picker = None;
                match edit {
                    None => {
                        // B9（INV-M4/M5）：空白保守模板——绝不携带当前
                        // 配置冒充新档案（旧「身份谎言」入口已除）。
                        self.editor = Some(ModelEditor::new_profile_template(
                            self.provider_descriptors.clone(),
                        ));
                        self.flash_status("no custom models yet — create the first one");
                    }
                    Some(name) => match self.load_model_profile(&name) {
                        Some((config, credentials)) => {
                            self.editor = Some(ModelEditor::for_profile(
                                &name,
                                &config,
                                credentials,
                                self.provider_descriptors.clone(),
                            ));
                            self.flash_status(format!("editing profile {name}"));
                        }
                        None => self.flash_status(format!("profile {name} not found")),
                    },
                }
            }
            PickerAction::SwitchProfile(name) => {
                let Some(application) = self.application.as_ref() else {
                    return;
                };
                match application.activate_model_profile(&name) {
                    Ok(Some((config, credentials))) => {
                        self.config = config;
                        self.credentials = credentials;
                        self.provider_descriptors =
                            application.provider_descriptors(&self.credentials);
                        self.refresh_balance_now();
                        self.picker = None;
                        self.picker_return = None;
                        self.flash_status(format!("switched to profile {name}"));
                    }
                    Ok(None) => self.flash_status(format!("profile {name} not found")),
                    Err(error) => self.flash_status(format!("switch failed: {error}")),
                }
            }
            PickerAction::DeleteProfile(name) => {
                let Some(application) = self.application.as_ref() else {
                    return;
                };
                match application.delete_model_profile_with_fallback(&name) {
                    Ok(()) => {
                        // 活动态可能已回退（首档案/出厂默认）——同步镜像。
                        self.sync_model_state_from_application();
                        self.picker = None;
                        self.picker_return = None;
                        self.flash_status(format!("profile {name} deleted"));
                    }
                    Err(error) => self.flash_status(format!("delete failed: {error}")),
                }
            }
            PickerAction::SelectPreset(preset) => {
                let mut config = self.config.clone();
                preset.apply(&mut config);
                // 换模型不携带旧档位：归位 None，新模型跟随预设默认
                // （与编辑器 cycle_preset 同一不变量）。
                config.thinking_level = None;
                let same_endpoint = self.config.endpoint.trim_end_matches('/')
                    == preset.endpoint.trim_end_matches('/');
                let key_present = self
                    .credentials
                    .value(0)
                    .is_some_and(|value| !value.trim().is_empty());
                if same_endpoint && key_present {
                    match self
                        .application
                        .as_ref()
                        .ok_or_else(|| "project application is unavailable".to_owned())
                        .and_then(|application| {
                            application
                                .save_model_state(&config, &self.credentials)
                                .map_err(|error| error.to_string())
                        }) {
                        Ok(()) => {
                            self.config = config;
                            // 端点或密钥可能已变化，触发立即重新查询。
                            self.refresh_balance_now();
                            self.picker = None;
                            self.picker_return = None;
                            self.flash_status(format!("model switched to {}", preset.name));
                        }
                        Err(error) => self.flash_status(format!("failed to save model: {error}")),
                    }
                } else if let Some(restored) = self
                    .application
                    .as_ref()
                    .and_then(|application| {
                        application.vendor_key(preset.protocol, preset.endpoint)
                    })
                    .filter(|credentials| {
                        credentials
                            .value(0)
                            .is_some_and(|value| !value.trim().is_empty())
                    })
                {
                    // INV-VK2（厂商 key 记忆库）：该厂商的 key 之前输入
                    // 过已被记忆（save_model_state 输入即记忆）——直接
                    // 回填落位，不再弹编辑器要 key（修复：切走再切回
                    // 反复要 key）。
                    match self
                        .application
                        .as_ref()
                        .ok_or_else(|| "project application is unavailable".to_owned())
                        .and_then(|application| {
                            application
                                .save_model_state(&config, &restored)
                                .map_err(|error| error.to_string())
                        }) {
                        Ok(()) => {
                            self.config = config;
                            self.credentials = restored;
                            self.refresh_balance_now();
                            self.picker = None;
                            self.picker_return = None;
                            self.flash_status(format!(
                                "model switched to {} (saved key restored)",
                                preset.name
                            ));
                        }
                        Err(error) => self.flash_status(format!("failed to save model: {error}")),
                    }
                } else {
                    // INV-U1：转编辑器补密钥前拍下导航态（二级列表 + 光
                    // 标行），取消时原位重建。
                    self.picker_return = self.picker.as_ref().map(ModelPicker::snapshot);
                    self.picker = None;
                    let mut editor = ModelEditor::new_with_descriptors(
                        &config,
                        self.credentials.clone(),
                        self.provider_descriptors.clone(),
                    );
                    editor.apply_preset_and_focus_key(preset);
                    self.editor = Some(editor);
                    self.flash_status(format!("enter the API key for {}", preset.vendor));
                }
            }
        }
    }

    pub(super) fn apply_editor_action(&mut self, action: EditorAction) {
        match action {
            EditorAction::Continue => {}
            EditorAction::Cancel => {
                self.editor = None;
                // INV-U1（原位返回）：从 picker 进入的编辑器取消后，
                // 以快照原位重建 picker（层级 + 光标行），选择链路不
                // 因一次取消整体消失；档案数据未变，摘要重查即新。
                if let Some(snapshot) = self.picker_return.take() {
                    let profiles = self.model_profile_summaries();
                    let mut picker = ModelPicker::new(&self.config, profiles);
                    picker.restore_snapshot(snapshot);
                    self.picker = Some(picker);
                } else {
                    self.flash_status("model configuration cancelled");
                }
            }
            EditorAction::SaveProfile(saved) => {
                let ProfileSave {
                    name,
                    original_name,
                    config,
                    credentials,
                } = *saved;
                let Some(application) = self.application.as_ref() else {
                    return;
                };
                // 改名 = 存新名 + 删旧名（INV-M3：旧档案数据不残留双份）。
                let renamed = original_name
                    .as_deref()
                    .is_some_and(|original| original != name);
                let saved = application.save_model_profile(&name, &config, &credentials);
                let result = match saved {
                    Err(error) => Err(error.to_string()),
                    Ok(()) => {
                        if renamed && let Some(original) = &original_name {
                            let _ = application.delete_model_profile(original);
                        }
                        // 新建/编辑的档案即刻激活（原子换装）。
                        application
                            .activate_model_profile(&name)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    }
                };
                match result {
                    Ok(()) => {
                        self.config = config;
                        self.credentials = credentials;
                        self.provider_descriptors =
                            application.provider_descriptors(&self.credentials);
                        self.refresh_balance_now();
                        self.editor = None;
                        self.picker_return = None;
                        self.flash_status(format!("profile {name} saved and activated"));
                    }
                    Err(error) => {
                        self.flash_status(format!("failed to save profile: {error}"));
                    }
                }
            }
            EditorAction::Save(saved) => {
                let (config, credentials) = *saved;
                match self
                    .application
                    .as_ref()
                    .ok_or_else(|| "project application is unavailable".to_owned())
                    .and_then(|application| {
                        application
                            .save_model_state(&config, &credentials)
                            .map_err(|error| error.to_string())
                    }) {
                    Ok(()) => {
                        self.config = config;
                        self.credentials = credentials;
                        if let Some(application) = &self.application {
                            self.provider_descriptors =
                                application.provider_descriptors(&self.credentials);
                        }
                        // 端点或密钥可能已变化，触发立即重新查询。
                        self.refresh_balance_now();
                        self.flash_status(format!(
                            "model saved: {} · {}",
                            self.config.protocol, self.config.model
                        ));
                        self.editor = None;
                        self.picker_return = None;
                    }
                    Err(error) => {
                        self.flash_status(format!("failed to save model: {error}"));
                    }
                }
            }
        }
    }

    pub(super) fn submit_input(&mut self) {
        if self.clipboard_image_pending {
            self.flash_status("wait for clipboard image preparation before sending");
            return;
        }
        let value = self.input.take();
        let value = value.trim().to_owned();
        if self.handle_attachment_command(&value) {
            return;
        }
        // 附件只随对话消息走：slash 命令不携带、也不清空（留待下一
        // 条消息）；纯附件（空文本）允许提交。
        let is_command = value.starts_with('/');
        if value.is_empty() && (self.attachments.is_empty() || is_command) {
            return;
        }
        // 输入历史是进程内的（↑/↓ 召回）；跨重启的回忆来自会话的
        // transcript 投影（recent_inputs），命令输入永不落盘。
        self.input.remember(value.clone());

        // dsh 分流（§2.4 落点：首行判 dsh 态——命令与消息全部走宿主，
        // 本地 application/run 全家不激活）。附件判空用已取出的局部值
        //（审计 P2-3：此前的 `self.attachments` 检查永远落在 mem::take
        // 之后，恒为空——附件被静默吞掉且提示永不出现）。
        if self.dsh.is_some() || self.dsh_connect.is_some() {
            if !is_command && !self.attachments.is_empty() {
                self.input.insert_str(&value);
                self.flash_status("attachments are not supported in clat dsh mode — draft kept");
                return;
            }
            self.submit_dsh(value);
            // 提交后置位提示：submit_dsh 自己会 flash（sending…），警告
            // 必须是留在状态栏上的那条——附件没发出去，这比 sending 更
            // 重要。
            return;
        }

        // 命令语义全部在 core 注册表（INV-C1）：这里只剩「分发 → 渲染」。
        // 附件剥离/输入历史等输入路由留在前端。
        if is_command {
            let outcome = match self.application.as_mut() {
                Some(application) => application.dispatch_command(&value),
                None => Err(CommandError::Failed {
                    message: "project application is unavailable".to_owned(),
                }),
            };
            match outcome {
                Ok(outcome) => self.render_command_outcome(outcome),
                Err(error) => self.flash_status(error.to_string()),
            }
        } else {
            if !self.attachments.is_empty() && !self.config.capabilities.accepts_image_input() {
                self.input.insert_str(&value);
                self.flash_status(
                    "this model cannot send images — switch to a verified vision model or remove the draft",
                );
                return;
            }
            let paths = self.attachments.paths();
            if !self.start_run(value.clone(), paths) {
                // Admission/startup failures preserve the complete structured
                // draft and text so Enter is a lossless retry.
                self.input.insert_str(&value);
            }
        }
    }

    /// 命令 outcome 的终端呈现：各 `Start*` 开对应弹窗（数据来自
    /// outcome，不再自查门面）、`SessionReset` 清视图状态、错误已在上
    /// 游 flash。纯渲染，无命令语义。
    fn render_command_outcome(&mut self, outcome: CommandOutcome) {
        match outcome {
            CommandOutcome::Status(message) => self.flash_status(message),
            CommandOutcome::ShowHelp { commands } => {
                self.help_commands = commands;
                self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Help));
            }
            CommandOutcome::ShowMcpStatus(view) => {
                self.mcp_view = Some(view);
                self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Mcp));
            }
            CommandOutcome::ShowContext(view) => {
                self.context_view = Some(view);
                self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Context));
            }
            CommandOutcome::ShowSkills(view) => {
                self.skills_view = Some(view);
                self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Skills));
            }
            CommandOutcome::StartModelSelection => {
                // Claude Code 风格：先选厂商（一级），再选该厂商的模型
                //（二级）；Custom 入口经档案三态（B9：零档案直进新建
                // 页、≥1 档案列表 + New…）。
                self.editor = None;
                self.picker_return = None;
                let profiles = self.model_profile_summaries();
                self.picker = Some(ModelPicker::new(&self.config, profiles));
                self.flash_status("select a model");
            }
            CommandOutcome::StartSessionSelection { sessions } => {
                let current = self.session_id.clone();
                self.session_picker = Some(SessionPicker::new(sessions, current));
            }
            CommandOutcome::StartPermissionModeSelection { current } => {
                self.permission_picker = Some(
                    crate::tui::permission_picker::PermissionPicker::new(current),
                );
            }
            CommandOutcome::StartTitleEdit { prefill } => {
                self.rename_dialog = Some(RenameDialog::new(&prefill));
            }
            CommandOutcome::StartCompaction(handle) => {
                // 状态经 CompactionUpdated 事件回流（启动时 "compacting…"，
                // 完成/失败时结果文本）；Esc 取消。
                self.compact_handle = Some(handle);
            }
            CommandOutcome::StartVisionProbe(_) => {
                // VP-1：探测异步执行，判定经 VisionProbeNotice 事件回流；
                // TUI 无需持有句柄（fire-and-forget + 状态栏通知）。
                self.flash_status("vision probe started — the verdict lands here");
            }
            CommandOutcome::StartGoalRun => {
                self.start_goal_run();
            }
            CommandOutcome::SessionReset => {
                // /new 成功后的前端视图清空：用量指标归属会话（TUI-L04），
                // 新会话从零累计；路由桶同清（INV-C1 随会话归属）。
                self.session_id = None;
                self.session_title = None;
                self.conversation = crate::tui::conversation::ConversationModel::new();
                self.conversation_scroll_from_bottom = 0;
                self.input = InputBuffer::new(Vec::new());
                self.session_usage = Usage::default();
                self.usage_routes.clear();
                self.last_turn_usage = None;
                self.run_usage_base = None;
                self.run_routes_base = None;
                self.run_route = None;
                self.run_usage_acc = Usage::default();
                self.clear_attachment_draft();
                let pending_paths = self
                    .pending_native_steering
                    .iter()
                    .chain(self.recovered_native_steering.iter())
                    .flat_map(|draft| draft.attachments.iter().cloned())
                    .collect::<Vec<_>>();
                self.release_core_staged_attachment_paths(pending_paths);
                self.pending_native_steering.clear();
                self.native_steering_claim_credits.clear();
                self.recovered_native_steering.clear();
                self.flash_status("new conversation");
            }
            CommandOutcome::QuitRequested => self.should_quit = true,
        }
    }

    /// 运行中提交 = 插话（DSH `steer()`）：消息入队，在下一次模型请求
    /// 边界并入；入队即刻在转录尾部出现 dim 的 pending 回显（INV-SV1），
    /// claim 时由 `SteeringApplied` 升级为正式用户块。run 已判定终态
    /// （NotRunning，W1-04 封口语义）时回退普通提交；回退提交若撞上
    /// 收尾窗口失败，文本退还编辑框——绝不丢用户输入。
    pub(super) fn steer_input(&mut self) {
        if self.run_start_pending {
            self.flash_status("attachment admission is already in progress");
            return;
        }
        if self.clipboard_image_pending {
            self.flash_status("wait for clipboard image preparation before sending");
            return;
        }
        let value = self.input.take();
        let value = value.trim().to_owned();
        if self.handle_attachment_command(&value) {
            return;
        }
        if value.is_empty() && self.attachments.is_empty() {
            return;
        }
        if value.starts_with('/') {
            // slash 命令只作用于空闲态；退还输入，避免用户丢字。
            self.input.insert_str(&value);
            self.flash_status("slash commands run when idle — steering sends plain text");
            return;
        }
        // dsh 分流：running 态 Enter = steer（session.prompt mode:"steer"），
        // 回显与队列校正见 dsh_events（§2.3）。
        if self.dsh.is_some() || self.dsh_connect.is_some() {
            if !self.attachments.is_empty() {
                self.input.insert_str(&value);
                self.flash_status("attachments are not supported in clat dsh mode — draft kept");
                return;
            }
            self.input.remember(value.clone());
            self.submit_dsh(value);
            return;
        }
        if !self.attachments.is_empty() && !self.config.capabilities.accepts_image_input() {
            self.input.insert_str(&value);
            self.flash_status(
                "this model cannot send images — switch to a verified vision model or remove the draft",
            );
            return;
        }
        self.input.remember(value.clone());
        let paths = self.attachments.paths();
        if !paths.is_empty() {
            // `steer` admits staged images before pushing the pending message.
            // That can decode/normalize up to eight inputs, so use the same
            // sole-owner handoff as initial attachment admission instead of
            // blocking crossterm's render/input loop.
            if !self.start_image_steering_admission(value.clone(), paths) {
                self.input.insert_str(&value);
            }
            return;
        }
        let draft_count = paths.len();
        let outcome = self.application.as_ref().map(|application| {
            application.steer(crate::message::PendingMessage::from_front_end(
                value.clone(),
                None,
                paths.clone(),
            ))
        });
        match outcome {
            Some(SteerOutcome::Queued { .. }) => {
                let pending = if value.is_empty() {
                    format!("[{draft_count} image(s)]")
                } else if draft_count == 0 {
                    value.clone()
                } else {
                    format!("{value}\n[{draft_count} image(s)]")
                };
                self.conversation.push_pending_steering(pending);
                self.remember_native_steering(value, paths);
                self.attachments.clear();
                self.flash_status("steering queued — applies at the next model step");
            }
            Some(SteerOutcome::Refused { reason, .. }) => {
                self.input.insert_str(&value);
                self.flash_status(format!("steering refused: {reason}"));
            }
            // Run already sealed: fall back to an ordinary submission with
            // the exact same image draft. Success transfers ownership;
            // startup failure preserves both text and ordered attachments.
            _ => {
                let fallback = value.clone();
                if !self.start_run(value, paths) {
                    self.input.insert_str(&fallback);
                }
            }
        }
    }

    /// Run image-steering admission off the terminal thread. The active run
    /// intentionally remains live: only the `TrustedProjectApplication`
    /// facade moves briefly, and ordinary run events continue through the
    /// frontend-local channel. No draft is cleared until the worker returns a
    /// durable `Queued` outcome.
    fn start_image_steering_admission(
        &mut self,
        prompt: String,
        attachments: Vec<std::path::PathBuf>,
    ) -> bool {
        let sender = self
            .event_sender
            .clone()
            .expect("event channel is installed by run()");
        let Some(application) = self.application.take() else {
            self.flash_status("project application is unavailable");
            return false;
        };
        let (handoff, received) = mpsc::sync_channel::<TrustedProjectApplication>(0);
        let worker_sender = sender.clone();
        let spawn = thread::Builder::new()
            .name("clat-steering-admission".into())
            .spawn(move || {
                let Ok(application) = received.recv() else {
                    return;
                };
                let outcome = application.steer(crate::message::PendingMessage::from_front_end(
                    prompt.clone(),
                    None,
                    attachments,
                ));
                let message = UiEvent::Worker(WorkerMessage::SteeringAdmissionFinished(Box::new(
                    SteeringAdmissionFinished {
                        application,
                        prompt,
                        outcome,
                    },
                )));
                if let Err(error) = worker_sender.send(message)
                    && let UiEvent::Worker(WorkerMessage::SteeringAdmissionFinished(finished)) =
                        error.0
                {
                    // The terminal disappeared while it was the only holder
                    // of the facade. Close explicitly so an active run cannot
                    // outlive the UI process through a detached owner.
                    let _ = finished.application.close();
                }
            });
        if let Err(error) = spawn {
            self.application = Some(application);
            self.flash_status(format!(
                "failed to start steering admission worker: {error}"
            ));
            return false;
        }
        if let Err(error) = handoff.send(application) {
            self.application = Some(error.0);
            self.flash_status("steering admission worker terminated before handoff");
            return false;
        }
        self.run_start_pending = true;
        self.steering_admission_pending = true;
        self.flash_status("preparing steering images…");
        true
    }

    /// 把一次普通 run 的完整 pre-commit admission 交给有界 worker。
    ///
    /// 返回 true 只表示 handoff 已建立；附件在 RunStartFinished 成功时
    /// 才清空，异步失败则由该消息处理器恢复原文本。线程创建/移交失败
    /// 会同步返回 false，调用方据此立即恢复文本。
    pub(super) fn start_run(
        &mut self,
        prompt: String,
        attachments: Vec<std::path::PathBuf>,
    ) -> bool {
        if !self.config.is_configured() {
            self.flash_status("model is not configured — run /model first");
            return false;
        }
        if attachments.is_empty() {
            return self.start_text_run(prompt);
        }
        if self.run_start_pending {
            self.flash_status("run startup is already preparing attachments");
            return false;
        }
        let sender = self
            .event_sender
            .clone()
            .expect("event channel is installed by run()");
        let Some(application) = self.application.take() else {
            self.flash_status("project application is unavailable");
            return false;
        };
        // Spawn before moving the application. If the OS refuses the thread,
        // the frontend still owns the exact live application and can retry.
        let (handoff, received) = mpsc::sync_channel::<TrustedProjectApplication>(0);
        let worker_sender = sender.clone();
        let spawn = thread::Builder::new()
            .name("clat-run-admission".into())
            .spawn(move || {
                let Ok(mut application) = received.recv() else {
                    return;
                };
                let (completion, completed) = mpsc::channel();
                let gate = RunStartGate::closed();
                let request = ApplicationRunRequest {
                    message: crate::message::PendingMessage::from_front_end(
                        prompt.clone(),
                        None,
                        attachments,
                    ),
                    asker: Some(Arc::new(ChannelUserAsker::new(worker_sender.clone()))),
                    approver: Arc::new(ChannelApprover::new(worker_sender.clone())),
                    events: Box::new(DeferredChannelEventSink::new(
                        worker_sender.clone(),
                        gate.clone(),
                    )),
                    completion,
                };
                let outcome = application.start_run(request).map_or_else(
                    |error| Err(error.to_string()),
                    |handle| {
                        Ok(PreparedTuiRun {
                            handle,
                            completed,
                            gate,
                        })
                    },
                );
                let message = UiEvent::Worker(WorkerMessage::RunStartFinished(Box::new(
                    RunStartFinished {
                        application,
                        prompt,
                        outcome,
                    },
                )));
                if let Err(error) = worker_sender.send(message) {
                    // The terminal went away during admission. Never leave a
                    // core run blocked on the first-event gate, and close the
                    // recovered application explicitly after cancellation.
                    if let UiEvent::Worker(WorkerMessage::RunStartFinished(finished)) = error.0 {
                        let RunStartFinished {
                            application,
                            outcome,
                            ..
                        } = *finished;
                        if let Ok(started) = outcome {
                            started.handle.cancel();
                            started.gate.open();
                            let _ = started.handle.join();
                        }
                        let _ = application.close();
                    }
                }
            });
        if let Err(error) = spawn {
            self.application = Some(application);
            self.flash_status(format!("failed to start admission worker: {error}"));
            return false;
        }
        if let Err(error) = handoff.send(application) {
            self.application = Some(error.0);
            self.flash_status("admission worker terminated before handoff");
            return false;
        }
        self.run_start_pending = true;
        self.steering_admission_pending = false;
        self.flash_status("preparing attachments…");
        true
    }

    /// Text-only startup has no image decode/normalization work and retains
    /// the established synchronous handoff. This keeps the application
    /// continuously available for the overwhelmingly common fast path.
    fn start_text_run(&mut self, prompt: String) -> bool {
        let sender = self
            .event_sender
            .clone()
            .expect("event channel is installed by run()");
        let (completion, completed) = mpsc::channel();
        let request = ApplicationRunRequest {
            message: crate::message::PendingMessage::text(prompt.clone()),
            asker: Some(Arc::new(ChannelUserAsker::new(sender.clone()))),
            approver: Arc::new(ChannelApprover::new(sender.clone())),
            events: Box::new(ChannelEventSink(sender.clone())),
            completion,
        };
        let handle = match self
            .application
            .as_mut()
            .ok_or_else(|| "project application is unavailable".to_owned())
            .and_then(|application| {
                application
                    .start_run(request)
                    .map_err(|error| error.to_string())
            }) {
            Ok(handle) => handle,
            Err(error) => {
                self.flash_status(format!("failed to start run: {error}"));
                return false;
            }
        };
        self.run_epoch += 1;
        let epoch = self.run_epoch;
        self.conversation.push_user(prompt);
        self.conversation_scroll_from_bottom = 0;
        self.run_handle = Some(handle);
        self.running = true;
        self.run_usage_base = Some(self.session_usage.clone());
        self.run_usage_acc = Usage::default();
        self.run_routes_base = Some(self.usage_routes.clone());
        self.run_route = None;
        self.flash_status("starting model…");
        self.restore_next_recovered_steering();
        thread::spawn(move || {
            if let Ok(result) = completed.recv() {
                let _ = sender.send(UiEvent::Worker(WorkerMessage::Done { epoch, result }));
            }
        });
        true
    }

    fn start_goal_run(&mut self) -> bool {
        if !self.config.is_configured() {
            self.flash_status("model is not configured — run /model first");
            return false;
        }
        let sender = self
            .event_sender
            .clone()
            .expect("event channel is installed by run()");
        let (completion, completed) = mpsc::channel();
        let request = ApplicationRunRequest {
            message: crate::message::PendingMessage::text(String::new()),
            asker: Some(Arc::new(ChannelUserAsker::new(sender.clone()))),
            approver: Arc::new(ChannelApprover::new(sender.clone())),
            events: Box::new(ChannelEventSink(sender.clone())),
            completion,
        };
        let (handle, prompt) = match self
            .application
            .as_mut()
            .ok_or_else(|| "project application is unavailable".to_owned())
            .and_then(|application| {
                application
                    .start_goal_run(request)
                    .map_err(|error| error.to_string())
            }) {
            Ok(started) => started,
            Err(error) => {
                self.flash_status(format!("failed to start goal: {error}"));
                return false;
            }
        };
        self.run_epoch += 1;
        let epoch = self.run_epoch;
        self.conversation.push_user(prompt);
        self.conversation_scroll_from_bottom = 0;
        self.run_handle = Some(handle);
        self.running = true;
        self.run_usage_base = Some(self.session_usage.clone());
        self.run_usage_acc = Usage::default();
        self.run_routes_base = Some(self.usage_routes.clone());
        self.run_route = None;
        self.flash_status("starting bounded goal…");
        thread::spawn(move || {
            if let Ok(result) = completed.recv() {
                let _ = sender.send(UiEvent::Worker(WorkerMessage::Done { epoch, result }));
            }
        });
        true
    }
}
