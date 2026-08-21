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
            WorkerMessage::Done(result) => {
                self.finish_run(result);
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
            RunEvent::SteeringApplied { .. } => {
                // 转录用户块与 pending 区升级都由会话模型负责
                //（apply_run_event 已处理）；徽标计数派生自 pending 区，
                // 无独立状态可回收。
            }
            _ => {}
        }
    }

    fn finish_run(&mut self, result: crate::ApplicationRunResult) {
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
            // 未经 claim 的插话不落盘（S4）；显式告知而不是静默吞掉。
            self.flash_status(format!(
                "{discarded} steering discarded — run ended before it applied"
            ));
        }
        self.conversation_scroll_from_bottom = 0;
    }
}
