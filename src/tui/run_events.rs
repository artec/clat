use super::*;

impl App {
    /// 主循环收到一条 worker 消息：流事件、权限请求或运行结束。
    pub(super) fn handle_worker_message(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::Event(event) => self.handle_run_event(event),
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
                self.flash_status("permission required — review arguments, then allow or deny");
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
                self.flash_status("the model asks a question — answer or Esc to decline");
            }
            WorkerMessage::ClipboardImagePrepared(result) => {
                self.clipboard_image_pending = false;
                match result {
                    Ok(path) => {
                        let root = self.project.root().to_path_buf();
                        match self.attachments.add_paths(&root, [path.clone()]) {
                            Ok(_) => self.flash_status(format!(
                                "clipboard image attached ({}) — Enter sends · Esc drops draft",
                                self.attachments.len()
                            )),
                            Err(error) => {
                                self.release_core_staged_attachment_paths([path]);
                                self.flash_status(format!(
                                    "clipboard image staging failed: {error}"
                                ));
                            }
                        }
                    }
                    Err(error) => self.flash_status(error),
                }
            }
            WorkerMessage::RunStartFinished(finished) => {
                let RunStartFinished {
                    application,
                    prompt,
                    outcome,
                } = *finished;
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
                        self.conversation.push_user(prompt);
                        self.conversation_scroll_from_bottom = 0;
                        self.run_handle = Some(started.handle);
                        self.running = true;
                        if exit_after_start && let Some(handle) = &self.run_handle {
                            handle.cancel();
                        }
                        self.run_usage_base = Some(self.session_usage.clone());
                        self.run_usage_acc = Usage::default();
                        self.run_routes_base = Some(self.usage_routes.clone());
                        self.run_route = None;
                        self.clear_attachment_draft();
                        self.flash_status("starting model…");
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
                                let _ = sender
                                    .send(UiEvent::Worker(WorkerMessage::Done { epoch, result }));
                            }
                        });
                    }
                    Err(error) => {
                        // The composer was never mutated during handoff.
                        // Restore only text; image IDs/order and staged files
                        // remain byte-for-byte the same for a lossless retry.
                        self.input.insert_str(&prompt);
                        self.flash_status(format!("failed to start run: {error}"));
                    }
                }
                if exit_after_start {
                    self.should_quit = true;
                }
            }
            WorkerMessage::SteeringAdmissionFinished(finished) => {
                let SteeringAdmissionFinished {
                    application,
                    prompt,
                    outcome,
                } = *finished;
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
                        self.flash_status("steering queued — applies at the next model step");
                    }
                    SteerOutcome::Refused { reason, .. } => {
                        self.input.insert_str(&prompt);
                        self.flash_status(format!("steering refused: {reason}"));
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
            WorkerMessage::Done { epoch, result } => {
                self.finish_run(epoch, result);
            }
        }
    }

    fn handle_run_event(&mut self, event: RunEvent) {
        // 转录装配的唯一 live 入口（G8）：状态行闪烁等呈现逻辑之外，
        // 事件先喂会话模型。
        self.conversation.apply_run_event(&event);
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
                self.flash_status(format!("{provider}/{model} · turn {turn}"));
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
                self.flash_status(format!("answering · turn {turn}"));
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
                self.flash_status(format!("tool → {} {}", call.name, call.arguments));
            }
            RunEvent::PermissionDenied { tool, reason } => {
                self.flash_status(format!("permission ✗ {tool} — {reason}"));
            }
            RunEvent::ToolFinished { result } => {
                if result.is_error {
                    self.flash_status(format!("tool ✗ {}", result.tool_name));
                } else {
                    self.flash_status(format!("tool ✓ {}", result.tool_name));
                }
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
                    self.conversation.push_turn_end("cancelled".into());
                    self.flash_status(format!("cancelled · {} model turns", done.turns));
                } else {
                    self.conversation.push_turn_end("completed".into());
                    self.flash_status(format!("completed · {} model turns", done.turns));
                    // AFK 提醒：对话结束响铃。用户主动取消不响——人就在
                    // 键盘前（Esc 是他按的）。
                    self.notify();
                }
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
                self.conversation
                    .push_turn_end(format!("error: {}", failure.error));
                self.flash_status(format!(
                    "run failed after {} model turns: {}",
                    failure.turns, failure.error
                ));
                // 失败也是"对话结束"——AFK 下同样需要知道。
                self.notify();
            }
        }
        let discarded = self.conversation.discard_pending_steering();
        if discarded > 0 {
            // 未经 claim 的插话不落盘（S4），但 frontend-owned 原始草稿
            // 必须继续可重试。逐条恢复，不合并独立消息的图片预算。
            self.recovered_native_steering
                .extend(self.pending_native_steering.drain(..));
            if !self.restore_next_recovered_steering() {
                self.flash_status(format!(
                    "{discarded} unclaimed steering draft(s) retained for retry"
                ));
            }
        }
        self.conversation_scroll_from_bottom = 0;
    }
}
