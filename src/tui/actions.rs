use super::*;
use crate::dsh::backend::DshTask;
use crate::tui::model_editor::{
    EditorAction, ModelEditor, ModelPicker, PickerAction, ProfileSave, ProfileSummary,
};

impl App {
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
        Ok(())
    }

    /// Shift+Tab：循环思考档位并随模型配置持久化（INV-D）。生效于
    /// 下一次 run（`start_run` 每次重读 `model_state`）；标题栏即时
    /// 同步（`self.config` 原地更新，重绘即见）。保存失败整体回滚，
    /// 内存与库不出现半套配置。
    pub(super) fn cycle_thinking_level(&mut self) {
        let vendor = self.config.vendor();
        if thinking_levels(vendor).is_empty() {
            self.flash_status("thinking levels apply to DeepSeek and GLM models");
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
        let value = self.input.take();
        let value = value.trim().to_owned();
        // 附件只随对话消息走：slash 命令不携带、也不清空（留待下一
        // 条消息）；纯附件（空文本）允许提交。
        let is_command = value.starts_with('/');
        if value.is_empty() && (self.attachments.is_empty() || is_command) {
            return;
        }
        let attachments = if is_command {
            Vec::new()
        } else {
            std::mem::take(&mut self.attachments)
        };
        // 输入历史是进程内的（↑/↓ 召回）；跨重启的回忆来自会话的
        // transcript 投影（recent_inputs），命令输入永不落盘。
        self.input.remember(value.clone());

        // dsh 分流（§2.4 落点：首行判 dsh 态——命令与消息全部走宿主，
        // 本地 application/run 全家不激活）。附件判空用已取出的局部值
        //（审计 P2-3：此前的 `self.attachments` 检查永远落在 mem::take
        // 之后，恒为空——附件被静默吞掉且提示永不出现）。
        if self.dsh.is_some() || self.dsh_connect.is_some() {
            self.submit_dsh(value);
            // 提交后置位提示：submit_dsh 自己会 flash（sending…），警告
            // 必须是留在状态栏上的那条——附件没发出去，这比 sending 更
            // 重要。
            if !attachments.is_empty() {
                self.flash_status("attachments are not supported in clat dsh mode");
            }
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
            self.start_run(value.clone(), attachments);
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
        let value = self.input.take();
        let value = value.trim().to_owned();
        if value.is_empty() {
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
            self.input.remember(value.clone());
            self.submit_dsh(value);
            return;
        }
        self.input.remember(value.clone());
        let outcome = self
            .application
            .as_ref()
            .map(|application| application.steer(value.clone()));
        match outcome {
            Some(SteerOutcome::Queued) => {
                self.conversation.push_pending_steering(value);
                self.flash_status("steering queued — applies at the next model step");
            }
            // 回退普通提交——steering 不携带附件（M6：附件只能随空闲
            // 态的新消息走）。提交失败（前一个 run 收尾尚未完成）时把
            // 文本退回编辑框，用户重按 Enter 即可。
            _ => {
                let fallback = value.clone();
                if !self.start_run(value, Vec::new()) {
                    self.input.insert_str(&fallback);
                }
            }
        }
    }

    /// 启动一次 run；返回是否成功（失败已 flash 原因——steer 回退
    /// 路径据此把文本退还编辑框）。
    fn start_run(&mut self, prompt: String, attachments: Vec<std::path::PathBuf>) -> bool {
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
            attachments,
            asker: Some(Arc::new(ChannelUserAsker::new(sender.clone()))),
            prompt: prompt.clone(),
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
        // W1-13：纪元先于任何收尾可见性落地——完成消息将携带它。
        self.run_epoch += 1;
        let epoch = self.run_epoch;
        self.conversation.push_user(prompt);
        self.conversation_scroll_from_bottom = 0;
        self.run_handle = Some(handle);
        self.running = true;
        // 实时用量基线：流式 Usage 在其上累加，结束以 RunOutput 权威替换。
        self.run_usage_base = Some(self.session_usage.clone());
        self.run_usage_acc = Usage::default();
        // 路由桶基线同律（INV-C1）：run 期间只动本次 run 的路由桶。
        self.run_routes_base = Some(self.usage_routes.clone());
        self.run_route = None;
        self.flash_status("starting model…");

        // Completion is already post-persistence and post-scope-cleanup; this
        // tiny frontend bridge only multiplexes it into the terminal channel.
        thread::spawn(move || {
            if let Ok(result) = completed.recv() {
                let _ = sender.send(UiEvent::Worker(WorkerMessage::Done { epoch, result }));
            }
        });
        true
    }
}
