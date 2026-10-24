use super::*;

impl App {
    /// 主循环收到一条 worker 消息：流事件、权限请求或运行结束。
    pub(super) fn handle_worker_message(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::Event(event) => self.handle_run_event(event),
            WorkerMessage::HistoryReadCompleted(token, result) => {
                self.history_read_completed(token, result)
            }
            WorkerMessage::PermissionRequest {
                request,
                decision_tx,
            } => {
                self.phases.finish();
                let escalations = self
                    .application
                    .as_ref()
                    .map(|application| {
                        escalation_targets(application.permission_mode(), request.effect)
                    })
                    .unwrap_or_default();
                self.pending_permission = Some(PendingPermission {
                    request,
                    decision_tx,
                    argument_scroll: 0,
                    argument_page_size: 1,
                    argument_line_count: 0,
                    reviewed_through: 0,
                    reviewed_to_end: false,
                    escalations,
                });
                // AFK 提醒：run 正卡在等你批准——不响铃它可能静默卡到
                // 天荒地老。
                self.notify();
                self.flash_message(UiMessage::new("tui.approval_wait"));
            }
            WorkerMessage::AskUserRequest {
                question,
                answer_tx,
            } => {
                self.phases.finish();
                // 无选项时直接进入自定义输入模式（无可选内容）。
                let custom = question.options.is_empty().then(String::new);
                self.pending_ask_user = Some(PendingAskUser {
                    question,
                    answer_tx,
                    selection: 0,
                    custom,
                });
                // AFK 提醒：同权限框——run 在等你的回答。
                self.notify();
                self.flash_message(UiMessage::new("tui.question_wait"));
            }
            WorkerMessage::ClipboardPastePrepared(result) => self.apply_clipboard_result(result),
            WorkerMessage::RunStartFinished(finished) => self.restore_started_run(*finished),
            WorkerMessage::SteeringAdmissionFinished(finished) => {
                self.restore_steering_admission(*finished)
            }
            WorkerMessage::PromptSuggestionFinished { request, outcome } => {
                let valid = outcome.as_ref().map_or(true, |suggestion| {
                    self.session_id.as_ref() == Some(&suggestion.session_id)
                });
                self.finish_suggestion(request, valid, outcome.map(|suggestion| suggestion.text));
            }
            WorkerMessage::Done { epoch, result } => {
                self.finish_run(epoch, result);
            }
        }
    }

    fn apply_clipboard_result(
        &mut self,
        result: Result<crate::tui::attachments::PreparedClipboardPaste, String>,
    ) {
        self.clipboard_image_pending = false;
        match result {
            Ok(crate::tui::attachments::PreparedClipboardPaste::Image(path)) => {
                let root = self.project.root().to_path_buf();
                match self.attachments.add_paths(&root, [path.clone()]) {
                    Ok(_) => self.flash_status(
                        UiMessage::new("tui.clipboard_attached")
                            .arg("value1", (self.attachments.len()).to_string()),
                    ),
                    Err(error) => {
                        self.release_core_staged_attachment_paths([path]);
                        self.flash_message(
                            UiMessage::new("tui.message_clipboard_image_staging_failed_value")
                                .arg_message("error", error.message()),
                        );
                    }
                }
            }
            Ok(crate::tui::attachments::PreparedClipboardPaste::Text(text)) => {
                // Ctrl+V 系统剪贴板文本与 bracketed paste 同一保真契约
                //（PASTE-1）：段落换行归一为 `\n` 后入草稿。
                self.input.insert_paste(&text);
                self.flash_message(UiMessage::new("tui.clipboard_text"));
            }
            Ok(crate::tui::attachments::PreparedClipboardPaste::Empty) => {
                self.flash_message(UiMessage::new("tui.clipboard_empty"));
            }
            Err(error) => self.flash_status(error),
        }
    }

    fn restore_started_run(&mut self, finished: RunStartFinished) {
        let RunStartFinished {
            application,
            prompt,
            outcome,
        } = finished;
        self.application = Some(application);
        self.release_core_staged_attachment_paths(std::iter::empty());
        self.run_start_pending = false;
        self.steering_admission_pending = false;
        let exit_after_start = std::mem::take(&mut self.quit_after_run_start);
        match outcome {
            Ok(started) => {
                // W1-13：纪元与全部本地 run 状态必须先于 gate
                // 开放；这样首个 RunStarted 不可能被当成空闲态
                // 或累加到上一 run 的用量基线。
                self.run_epoch += 1;
                let epoch = self.run_epoch;
                self.cancel_requested = false;
                self.conversation.push_user(prompt);
                self.conversation_scroll_from_bottom = 0;
                self.run_handle = Some(started.handle);
                self.running = true;
                // CONT-1（C1）：新 run 开工即清上一 run 的常驻终态——旧
                // 终态不得把新 run 的执行期误标成已完成/已失败。
                self.terminal_status = None;
                self.terminal_presentation = None;
                if exit_after_start && let Some(handle) = &self.run_handle {
                    handle.cancel();
                }
                self.run_usage_base = Some(self.session_usage.clone());
                self.run_usage_acc = Usage::default();
                self.run_routes_base = Some(self.usage_routes.clone());
                self.run_route = None;
                self.clear_attachment_draft();
                self.flash_message(UiMessage::new("tui.run_starting"));
                self.restore_next_recovered_steering();

                let sender = self
                    .event_sender
                    .clone()
                    .expect("event channel is installed by run()");
                let completed = started.completed;
                // State is now coherent. Release the run worker's
                // first event, then bridge its post-persistence result.
                started.gate.open();
                thread::spawn(move || {
                    if let Ok(result) = completed.recv() {
                        let _ = sender.send(UiEvent::Worker(WorkerMessage::Done { epoch, result }));
                    }
                });
            }
            Err(error) => {
                // The composer was never mutated during handoff.
                // Restore only text; image IDs/order and staged files
                // remain byte-for-byte the same for a lossless retry.
                self.input.insert_str(&prompt);
                self.flash_message(
                    UiMessage::new("tui.message_failed_to_start_run_value")
                        .arg("error", (error).to_string()),
                );
            }
        }
        if exit_after_start {
            self.should_quit = true;
        }
    }

    fn restore_steering_admission(&mut self, finished: SteeringAdmissionFinished) {
        let SteeringAdmissionFinished {
            application,
            prompt,
            outcome,
        } = finished;
        self.application = Some(application);
        self.release_core_staged_attachment_paths(std::iter::empty());
        self.run_start_pending = false;
        self.steering_admission_pending = false;
        let exit_after_admission = std::mem::take(&mut self.quit_after_run_start);
        let draft_count = self.attachments.len();
        match outcome {
            SteerOutcome::Queued { .. } => {
                let pending = if prompt.is_empty() {
                    format!("[{draft_count} image(s)]")
                } else {
                    format!("{prompt}\n[{draft_count} image(s)]")
                };
                self.conversation.push_pending_steering(pending);
                self.remember_native_steering(prompt, self.attachments.paths());
                self.attachments.clear();
                self.flash_message(UiMessage::new("tui.steering_pending"));
            }
            SteerOutcome::Refused { reason, .. } => {
                self.input.insert_str(&prompt);
                self.flash_message(
                    UiMessage::new("tui.message_steering_refused_value")
                        .arg("reason", (reason).to_string()),
                );
            }
            // The run sealed while decode/admission was in flight.
            // Restore the sole application owner first, then take the
            // same ordinary-submit fallback as the synchronous path.
            SteerOutcome::NotRunning { .. } => {
                if exit_after_admission {
                    self.input.insert_str(&prompt);
                } else {
                    let fallback = prompt.clone();
                    let paths = self.attachments.paths();
                    if !self.start_run(prompt, paths) {
                        self.input.insert_str(&fallback);
                    }
                }
            }
        }
        if exit_after_admission {
            // Unlike initial admission, an existing run may still be
            // live. Cancel it before the normal close path so the
            // application cannot retain a detached model worker.
            if let Some(handle) = &self.run_handle {
                handle.cancel();
            }
            self.should_quit = true;
        }
    }

    pub(super) fn handle_run_event(&mut self, event: RunEvent) {
        // 转录装配的唯一 live 入口（G8）：状态行闪烁等呈现逻辑之外，
        // 事件先喂会话模型。
        self.conversation.apply_run_event(&event);
        // CF1（返工 2026-10-13）：取消请求标志的生命周期绑定真实 run
        // 事件。attached 模式没有本地 Done/settle 路径——终态与下一
        // 轮启动都由事件流驱动，复位必须发生在同样的流上，否则第一
        // 轮的 Cancelling… 会污染第二轮的 phase 行。
        match event {
            RunEvent::RunStarted { .. }
            | RunEvent::RunCancelled { .. }
            | RunEvent::RunCompleted { .. }
            | RunEvent::RunFailed { .. } => {
                self.cancel_requested = false;
                // CF7：新 run/终态同样解除取消在途门。
                if let Some(native) = self.native.as_mut() {
                    native.cancel_inflight = None;
                }
            }
            _ => {}
        }
        // 派生阶段（G6）：新模型步重开 Waiting；步内按事件推进、只进
        // 不退；未知事件落 `_ => {}` 保持现状、永不 panic。
        match &event {
            RunEvent::ModelRequested { .. } => self.phases.model_requested(),
            RunEvent::ModelStream {
                event: ModelEvent::ReasoningDelta { .. } | ModelEvent::ReasoningSummaryDelta { .. },
                ..
            } => self.phases.advance(Phase::Thinking),
            RunEvent::ModelStream {
                event: ModelEvent::TextDelta { .. } | ModelEvent::RefusalDelta { .. },
                ..
            } => self.phases.advance(Phase::Responding),
            RunEvent::ToolRequested { .. } => self.phases.advance(Phase::ExecutingTools),
            _ => {}
        }
        match event {
            RunEvent::ModelRequested {
                turn,
                provider,
                model,
            } => {
                // 本次 run 的入账路由（journal source 同口径，INV-C1）：
                // 以实际运行的 provider/model 为准，run 中途切配置不串桶。
                self.run_route = Some(crate::model::model_route_key(&provider, &model));
                self.flash_message(model_request_message(provider, model, turn.to_string()));
            }
            RunEvent::ModelStream {
                turn,
                event: ModelEvent::TextDelta { .. },
            }
            | RunEvent::ModelStream {
                turn,
                event: ModelEvent::RefusalDelta { .. },
            } => {
                // 流式追加与贴底滚动由模型负责；这里只管状态行。
                self.conversation_scroll_from_bottom = 0;
                self.flash_message(
                    UiMessage::new("tui.message_answering_turn_value")
                        .arg("turn", (turn).to_string()),
                );
            }
            RunEvent::ModelStream {
                event: ModelEvent::ReasoningDelta { .. },
                ..
            } => {}
            // 流式 usage（DeepSeek 经 stream_options.include_usage，GLM
            // 默认携带）只取最近一次：input+output 近似当前上下文水位，
            // 供状态栏 Context 段使用。多轮 run 每轮覆盖前一轮。同时
            // 在 run 基线上实时累计会话用量——Cache 段首跑中途即有值。
            RunEvent::ModelStream {
                event: ModelEvent::Usage(usage),
                ..
            } => {
                self.last_turn_usage = Some(usage.clone());
                self.run_usage_acc.add_assign(&usage);
                if let Some(base) = self.run_usage_base.clone() {
                    let mut live = base;
                    live.add_assign(&self.run_usage_acc);
                    self.session_usage = live;
                }
                // 路由桶同基线重建：本次 run 的累计全部记入本次 run 的
                // 路由（run 单路由；ModelRequested 先于任何 usage 到达）。
                if let Some(routes) = self.run_routes_base.clone() {
                    self.usage_routes = routes;
                    if let Some(route) = self.run_route.clone() {
                        self.usage_routes
                            .entry(route)
                            .or_default()
                            .add_assign(&self.run_usage_acc);
                    }
                }
            }
            RunEvent::ToolRequested { call } => {
                self.flash_message(tool_request_message(call.name, call.arguments))
            }
            RunEvent::PermissionDenied { tool, reason } => {
                self.flash_message(tool_denied_message(tool, reason))
            }
            RunEvent::ToolFinished { result } => {
                self.flash_message(tool_finished_message(result.tool_name, result.is_error))
            }
            RunEvent::SteeringApplied { message, .. } => {
                // 转录用户块与 pending 区升级都由会话模型负责
                //（apply_run_event 已处理）；徽标计数派生自 pending 区，
                // 无独立状态可回收。事件若抢在 admission ack 前到达，
                // 先记 credit，由稍后注册的本地草稿消费。
                let prompt = message.plain_text();
                if let Some(position) = self
                    .pending_native_steering
                    .iter()
                    .position(|draft| draft.prompt == prompt)
                {
                    if let Some(draft) = self.pending_native_steering.remove(position) {
                        self.release_core_staged_attachment_paths(draft.attachments);
                    }
                } else if self.steering_admission_pending {
                    self.native_steering_claim_credits.push_back(prompt);
                }
            }
            _ => {}
        }
    }

    fn finish_run(&mut self, epoch: u64, result: crate::ApplicationRunResult) {
        let dispatch_next =
            epoch == self.run_epoch && result.as_ref().is_ok_and(|done| !done.cancelled);
        self.settle_finished_run(epoch, result);
        if dispatch_next {
            self.dispatch_next_turn();
        }
    }

    /// CONT-1（C1/C2）：以真实终态设置常驻状态栏；存在可观察的未完成
    /// 待办时追加转录交代行（未完成数 + 未核验声明 + 续办提示），读不
    /// 到工作事实或全部完成时不编造。
    fn settle_run_terminal_status(&mut self, done: &crate::ApplicationRunDone) {
        let todos = self.application.as_ref().and_then(open_todo_summary);
        match (&todos, done.cancelled) {
            (Some((open, total, sample)), false) => {
                self.conversation.push_turn_end_message(UiMessage::new("tui.message_run_completed_value_of_value_todos_unfinished_task_completion_not_verified_value").arg("open",(open).to_string()).arg("total",(total).to_string()).arg("sample",(sample).to_string()));
                self.set_terminal_status(UiMessage::new("tui.message_completed_value_model_turns_value_todos_unfinished_send_a_message_to_continue").arg("value1",(done.turns).to_string()).arg("open",(open).to_string()));
            }
            (Some((open, _, _)), true) => {
                self.set_terminal_status(
                    UiMessage::new(
                        "tui.message_cancelled_value_model_turns_value_todos_unfinished",
                    )
                    .arg("value1", (done.turns).to_string())
                    .arg("open", (open).to_string()),
                );
            }
            (None, false) => {
                self.set_terminal_status(
                    UiMessage::new(
                        "tui.message_completed_value_model_turns_send_a_message_to_continue",
                    )
                    .arg("value1", (done.turns).to_string()),
                );
            }
            (None, true) => {
                self.set_terminal_status(
                    UiMessage::new("tui.message_cancelled_value_model_turns")
                        .arg("value1", (done.turns).to_string()),
                );
            }
        }
    }

    fn settle_finished_run(&mut self, epoch: u64, result: crate::ApplicationRunResult) {
        // W1-13：纪元失配 = 上一 run 的陈旧完成（新 run 已启动）。此时
        // 一切 `self` 上的收尾动作（take/join 新 run 的句柄、running 置
        // 假、用量基线对账、阶段收尾）都属于**新** run——一律不做。上一
        // run 的持久化在 completion 发出前已完成，转录已由事件流实时
        // 呈现；放弃的只有"权威用量覆盖"（TUI 本地近似值保留），代价
        // 远小于冻结 UI/串档。
        if epoch != self.run_epoch {
            return;
        }
        self.running = false;
        // CANCEL-1（C1）：run 终态后取消请求标志必须复位——否则下一
        // 个 run 的 phase 行会在第一帧错误地显示 Cancelling…。
        self.cancel_requested = false;
        if let Some(handle) = self.run_handle.take() {
            let _ = handle.join();
        }
        // 首 run 可能刚物化会话（Fresh→Session）：同步本地镜像，/resume
        // 的 current 标记与后续写路径立即正确。
        if let Some(application) = &self.application {
            self.session_id = application.current_session_id();
            // 会话右标题即时刷新：首条消息的 fallback 标题不产生事件
            //（投影派生），run 结束是它出现的第一个时机。
            self.session_title = application.session_title();
        }
        self.phases.finish();
        // run 刚消耗了额度：触发监控线程立即重新查询一次（计划外，
        // 不影响 5 分钟巡查周期）。
        self.refresh_balance_now();
        match result {
            Ok(done) => {
                // 会话用量以 run 结果权威覆盖：RunOutput.usage 是全 run
                // 总量，替换"基线 + 流式累计"的实时近似（不重复计）。
                match self.run_usage_base.take() {
                    Some(base) => {
                        self.session_usage = base;
                        self.session_usage.add_assign(&done.usage);
                    }
                    None => self.session_usage.add_assign(&done.usage),
                }
                // 路由桶权威覆盖（INV-C1：只动本次 run 的桶）。
                let run_route = self.run_route.take();
                match self.run_routes_base.take() {
                    Some(routes) => {
                        self.usage_routes = routes;
                        if let Some(route) = run_route {
                            self.usage_routes
                                .entry(route)
                                .or_default()
                                .add_assign(&done.usage);
                        }
                    }
                    None => {
                        if let Some(route) = run_route {
                            self.usage_routes
                                .entry(route)
                                .or_default()
                                .add_assign(&done.usage);
                        }
                    }
                }
                // 非流式 provider 兜底：本轮无任何 delta 时以最终输出
                // 回填 assistant（与 journal 的 settled 文本对拍一致）。
                self.conversation.settle_streamed_output(&done.output);
                // 终态通知进转录（G7）：与回放 TurnEnded 同源文本。
                if done.cancelled {
                    self.conversation
                        .push_turn_end_message(UiMessage::new("tui.turn_cancelled"));
                } else {
                    self.conversation
                        .push_turn_end_message(UiMessage::new("tui.turn_completed"));
                    // AFK 提醒：对话结束响铃。用户主动取消不响——人就在
                    // 键盘前（Esc 是他按的）。
                    self.notify();
                }
                // CONT-1（C1/C2）：run 真实终态常驻状态栏 + 未完工作诚实
                // 交代（RunCompleted ≠ 业务目标已核验完成）。
                self.settle_run_terminal_status(&done);
            }
            Err(failure) => {
                match self.run_usage_base.take() {
                    Some(base) => {
                        self.session_usage = base;
                        self.session_usage.add_assign(&failure.usage);
                    }
                    None => self.session_usage.add_assign(&failure.usage),
                }
                // 失败 run 的已产生用量同样入本次 run 的桶（usage 为零时
                // 无感知）。
                let run_route = self.run_route.take();
                match self.run_routes_base.take() {
                    Some(routes) => {
                        self.usage_routes = routes;
                        if let Some(route) = run_route {
                            self.usage_routes
                                .entry(route)
                                .or_default()
                                .add_assign(&failure.usage);
                        }
                    }
                    None => {
                        if let Some(route) = run_route {
                            self.usage_routes
                                .entry(route)
                                .or_default()
                                .add_assign(&failure.usage);
                        }
                    }
                }
                self.conversation.push_turn_end_message(
                    UiMessage::new("tui.turn_error").arg("detail", failure.error.to_string()),
                );
                // CONT-1（C1）：失败原因常驻状态栏（同文本），不随 4 秒
                // 提示消失；转录终态行与 RUNERR-1 词汇同源。
                self.set_terminal_status(
                    UiMessage::new("tui.run_failed_status")
                        .arg("value1", (failure.turns).to_string())
                        .arg("value2", (failure.error).to_string()),
                );
                // 失败也是"对话结束"——AFK 下同样需要知道。
                self.notify();
            }
        }
        let discarded = self.conversation.discard_pending_steering();
        self.retain_unclaimed_steering(discarded);
        self.conversation_scroll_from_bottom = 0;
    }
}

/// CONT-1（C2）：从 `workflow.details` 投影 JSON 读未完成待办，返回
/// `(未完成数, 总数, 有界样本)`。只报告可观察事实，不推断业务完成度；
/// 形状不符/读不到返回 `None`（无证据不宣称、不编造交代）。
/// standalone 与 attached 腿共用（attached 经 HostClient 取同一投影）。
pub(super) fn summarize_open_todos(details: &serde_json::Value) -> Option<(usize, usize, String)> {
    let todos = details.get("todos")?.as_array()?;
    let total = todos.len();
    let mut open = Vec::new();
    for todo in todos {
        if todo.get("status").and_then(serde_json::Value::as_str) == Some("completed") {
            continue;
        }
        open.push(
            todo.get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("untitled")
                .to_owned(),
        );
    }
    if open.is_empty() {
        return None;
    }
    let count = open.len();
    let mut sample = open.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if count > 3 {
        sample.push_str(&format!(" (+{} more)", count - 3));
    }
    Some((count, total, sample))
}

/// standalone 腿：直接读本地 Application 的 `workflow_details` 投影。
pub(super) fn open_todo_summary(
    application: &crate::TrustedProjectApplication,
) -> Option<(usize, usize, String)> {
    summarize_open_todos(&application.workflow_details().ok()?)
}

impl App {
    fn retain_unclaimed_steering(&mut self, discarded: usize) {
        if discarded > 0 {
            // 未经 claim 的插话不落盘（S4），但 frontend-owned 原始草稿
            // 必须继续可重试。逐条恢复，不合并独立消息的图片预算。
            self.recovered_native_steering
                .extend(self.pending_native_steering.drain(..));
            if !self.restore_next_recovered_steering() {
                self.flash_status(
                    UiMessage::new("tui.steering_retained")
                        .arg("discarded", (discarded).to_string()),
                );
            }
        }
    }
}

fn tool_request_message(name: String, arguments: serde_json::Value) -> UiMessage {
    UiMessage::new("tui.run_tool")
        .arg("value1", name)
        .arg("value2", arguments.to_string())
}
fn tool_denied_message(tool: String, reason: String) -> UiMessage {
    UiMessage::new("tui.run_denied")
        .arg("tool", tool)
        .arg("reason", reason)
}
fn tool_finished_message(tool: String, error: bool) -> UiMessage {
    UiMessage::new(if error {
        "tui.run_tool_error"
    } else {
        "tui.run_tool_success"
    })
    .arg("value1", tool)
}

fn model_request_message(provider: String, model: String, turn: String) -> UiMessage {
    UiMessage::new("tui.message_value_value_turn_value")
        .arg("provider", provider)
        .arg("model", model)
        .arg("turn", turn)
}
