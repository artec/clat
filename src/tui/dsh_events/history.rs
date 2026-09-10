use super::*;

impl App {
    pub(super) fn dsh_load_history(
        &mut self,
        session: String,
        events: Vec<SessionEvent>,
        first_seq: Option<u64>,
        has_more: bool,
    ) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        if dsh.current_session.as_deref() != Some(session.as_str()) {
            return;
        }
        let was_open = dsh.ws_open;
        let mut staged = None;
        let initial_load = dsh.history_loading;
        if initial_load {
            dsh.history_loading = false;
            staged = Some(std::mem::take(&mut dsh.staged_events));
            dsh.transcript.load_history(&mut self.conversation, &events);
        } else if self.conversation.is_empty() {
            // 空视图的补拉（Subscribed 基线锚定后触发）：无从判隙，
            // 整页重放最稳。
            dsh.transcript.load_history(&mut self.conversation, &events);
        } else {
            let mut unseen = Vec::new();
            for event in &events {
                if dsh.transcript.gap_before(event).is_some() {
                    unseen.push(event.clone());
                }
            }
            for event in &unseen {
                dsh.transcript.apply(&mut self.conversation, event);
            }
        }
        if initial_load {
            dsh.history_before_seq = first_seq;
            self.conversation_has_more = has_more;
            self.conversation_history_loading = false;
            self.conversation_history_windowed = has_more;
        }
        // 投影 fold 幂等（latest-wins）：整页重放与间隙补齐统一走全量。
        self.dsh_fold_session_projections(&events);
        if let Some(staged) = staged {
            // 暂存的 live 帧在整页之上补放（已入页的会被 seq 判重跳过）。
            for event in staged {
                self.dsh_reduce_session_event(&session, event);
            }
        }
        if !was_open {
            self.dsh_open_downlinks();
            self.flash_status("ready");
        }
    }

    pub(super) fn dsh_prepend_history(
        &mut self,
        session: String,
        requested_before_seq: u64,
        events: Vec<SessionEvent>,
        first_seq: Option<u64>,
        has_more: bool,
    ) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        if dsh.current_session.as_deref() != Some(session.as_str())
            || dsh.history_before_seq != Some(requested_before_seq)
            || !self.conversation_history_loading
        {
            return;
        }
        let replay = dsh.transcript.older_history_replay(&events);
        dsh.history_before_seq = first_seq.or(dsh.history_before_seq);
        self.prepend_conversation_page(&replay, has_more);
    }

    pub(in crate::tui) fn dsh_load_older_history(&mut self) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        let Some(session) = dsh.current_session.clone() else {
            self.conversation_has_more = false;
            return;
        };
        let Some(before_seq) = dsh.history_before_seq else {
            self.conversation_has_more = false;
            return;
        };
        self.conversation_history_loading = true;
        dsh.send_task(DshTask::OlderHistory {
            session,
            before_seq,
        });
    }

    /// 装载失败的中止（审计 P2-1）：解除暂存态，已暂存的 live 帧走
    /// 完整归约补放——历史页缺席但近期活动可见，loading 不悬挂。
    pub(super) fn dsh_abort_history_loading(&mut self) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        self.conversation_history_loading = false;
        if !dsh.history_loading {
            return;
        }
        dsh.history_loading = false;
        let staged = std::mem::take(&mut dsh.staged_events);
        if let Some(session) = dsh.current_session.clone() {
            for event in staged {
                self.dsh_reduce_session_event(&session, event);
            }
        }
    }

    /// 会话级投影 fold（latest-wins）：title / preset / contextWindow /
    /// usage——装载与间隙补齐共用（§2.6 步骤 4 的装载侧）。
    pub(super) fn dsh_fold_session_projections(&mut self, events: &[SessionEvent]) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        for event in events {
            match event.event_type.as_str() {
                "session/title" => {
                    if let Some(title) = event.data.get("title").and_then(Value::as_str) {
                        self.session_title = Some(title.to_owned());
                    }
                }
                "sandbox/mode" => {
                    if let Some(mode) = event.data.get("mode").and_then(Value::as_str) {
                        dsh.preset = Some(mode.to_owned());
                    }
                }
                "request/context" => {
                    dsh.apply_request_context(event);
                }
                "request/header" => {
                    dsh.apply_request_header(event);
                }
                "assistant/message" => {
                    if let Some(usage) = event.data.get("usage") {
                        dsh.usage.observe(usage);
                    }
                }
                _ => {}
            }
        }
    }

    /// 会话切换（§2.6 七步的 ③④⑤⑥⑦ 骨干；② 由调用方先发 Create
    /// 收养任务、Created 回执进入此处；①在途弹框关闭且不 respond）。
    pub(super) fn dsh_switch_session(&mut self, session: String) {
        // 拍板 A：切换即记住（下次启动优先回它；写失败 fail-soft，
        // 下次回落列表头）。restore/收养//new 全部经此，单点写入。
        let memory_path = self.dsh_memory_path.clone();
        crate::dsh::last_session::remember_last_session_at(&memory_path, &session);
        // 先落目标会话：Typert mux 的 follow 目标从 current_session 取。
        if let Some(dsh) = self.dsh.as_mut() {
            dsh.current_session = Some(session.clone());
        }
        // DV-10 收尾（TUI 时序修复，负责人 dogfood 病历 2026-09-07）：
        // Typert 世代的 History = page + controller.follow——mux 必须
        // **先于** History 到位。旧编排沿 Legacy 的"历史装载完成后开
        // WS"，Typert 下 History 永远先于 AdoptMux 进 worker，controller
        // 恒缺席（状态栏 "typert history needs the mux controller
        // (AdoptMux missing)"）。Legacy 双 WS 是宿主级、与次序无关，
        // 维持原编排；同一任务的 FIFO 队列保证 AdoptMux → History 序
        //（判别腿 `typert_restore_adopts_the_mux_before_requesting_
        // history`）。失败降级：开下行失败时 History 仍发出，worker 侧
        // 如实报错（与修复前同一可见形态，不静默）。
        if self
            .dsh
            .as_ref()
            .is_some_and(|dsh| dsh.era == crate::dsh::client::DshEra::Typert)
        {
            self.dsh_open_downlinks();
        }
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        dsh.session_tail = session
            .chars()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        dsh.transcript = DshTranscript::new();
        dsh.history_before_seq = None;
        dsh.unknown_events = 0;
        dsh.preset = None;
        dsh.context_window = None;
        // 档位随目标会话重投影（历史 request/header fold 恢复）。
        dsh.current_effort = None;
        dsh.usage = DshUsageAcc::new();
        dsh.pending_approval = None;
        dsh.pending_question = None;
        self.pending_permission = None;
        self.pending_ask_user = None;
        self.session_title = None;
        self.running = false;
        self.phases.finish();
        // 视图立即接管为空白（审计 P1-1）：旧会话内容绝不显示在新会话
        // 标题下——此前残留内容会把整页装载误判成间隙补齐而全数跳过
        // （fresh transcript 的 gap_before 恒 None），形成跨会话串线。
        self.conversation = ConversationModel::new();
        self.conversation_scroll_from_bottom = 0;
        self.conversation_has_more = false;
        self.conversation_history_loading = true;
        self.conversation_history_windowed = false;
        // 装载阶段显式化：整页回执前 live 帧入暂存区，回执后统一补放。
        dsh.history_loading = true;
        dsh.staged_events.clear();
        let session_for_names = session.clone();
        dsh.send_task(DshTask::History { session });
        // 名字目录 prime（每连接一次）：标签从裸 id 升级为展示名
        //（2026-08-23 负责人 dogfood：`deepseek-official ·
        // deepseek-v4-flash` 应显示 `DeepSeek · <模型 Name>`，与 web 端
        // 同源）。目录是宿主全局的，切换会话不重复拉。
        if !dsh.names_primed {
            dsh.names_primed = true;
            dsh.send_task(DshTask::ModelNames {
                session: session_for_names,
            });
        }
        self.flash_status("loading history…");
    }

    // ---- WS 帧归约 ----
}
