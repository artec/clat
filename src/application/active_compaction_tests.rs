//! Single user turn → real file tools → internal summary → next request → cold replay.
use super::*;
use crate::model::{
    FinishReason, ModelError, ModelEvent, ModelEventSink, ModelItem, ModelRequest, ModelResponse,
};
use crate::session::persistence::FaultHooks;
use crate::test_support::TestModelScript;

#[derive(Clone, Copy)]
enum SummaryMode {
    Normal,
    Failure,
    FailureThenCapacity,
    ImageTurn,
    Cancel,
    Persistence,
    Steering,
    /// CP-C1 红①：摘要返回前并发追加一条 log-only 事件（模拟后台
    /// `job/*` 在摘要网络阶段落地）——旧 CAS/全量比较下 turn 死。
    ConcurrentAppend,
    /// CP-C1 红②：摘要返回前并发改写**跨度内**一个旧 tool/result
    ///（模拟并发 prune）——跨度锚定应拒绝提交且降级不抛。
    SpanRewrite,
    /// CP-C4 红①/红②：压力场景下摘要请求到达即 panic——清组必须先把
    /// 压力泄到阈值之下，零付费摘要调用。
    SweepPressure,
}
struct LongTask {
    seeding: std::sync::atomic::AtomicBool,
    goal_input: std::sync::atomic::AtomicBool,
    main: AtomicUsize,
    summaries: AtomicUsize,
    summary_entered: std::sync::atomic::AtomicBool,
    summary_released: std::sync::atomic::AtomicBool,
    mode: SummaryMode,
    snapshots: Mutex<Vec<Vec<ModelItem>>>,
    sessions: Mutex<Option<Arc<crate::session::use_cases::SessionService>>>,
    /// 项目根（每轮写入两个新观测文件用）。
    root: Mutex<Option<std::path::PathBuf>>,
    /// SpanRewrite 只改写一次的闸。
    span_rewritten: std::sync::atomic::AtomicBool,
}
impl LongTask {
    fn new(mode: SummaryMode) -> Self {
        Self {
            seeding: std::sync::atomic::AtomicBool::new(false),
            goal_input: std::sync::atomic::AtomicBool::new(false),
            main: AtomicUsize::new(0),
            summaries: AtomicUsize::new(0),
            summary_entered: std::sync::atomic::AtomicBool::new(false),
            summary_released: std::sync::atomic::AtomicBool::new(false),
            mode,
            snapshots: Mutex::new(Vec::new()),
            sessions: Mutex::new(None),
            root: Mutex::new(None),
            span_rewritten: std::sync::atomic::AtomicBool::new(false),
        }
    }
}
impl TestModelScript for LongTask {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if self.seeding.load(Ordering::SeqCst) {
            return Ok(crate::test_support::response(
                "seed",
                FinishReason::Completed,
            ));
        }
        if request.items.last().is_some_and(|item|matches!(item, ModelItem::User {content} if matches!(content.first(),Some(crate::model::ContentPart::Text(text)) if text.contains("context compaction")))) {
            assert_eq!(request.options.request_tail, None, "summarization only consumes carried history");
            assert!(request.tools.is_empty(),
                "the internal summary must never inherit the main agent's published tools");
            self.summaries.fetch_add(1, Ordering::SeqCst);
            if matches!(self.mode, SummaryMode::ImageTurn | SummaryMode::FailureThenCapacity) {
                assert_eq!(request.items.iter().flat_map(crate::model::model_item_image_parts).count(), 0, "the just-admitted images belong to the main request, not the summary");
            }
            assert!(
                self.main.load(Ordering::SeqCst) >= 2,
                "must compact inside a running user turn"
            );
            match self.mode {
                SummaryMode::Failure | SummaryMode::FailureThenCapacity => {
                    return Err(ModelError::request("injected summary refusal"));
                }
                SummaryMode::Cancel => {
                    self.summary_entered.store(true, Ordering::SeqCst);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    while !request.cancel.is_cancelled() {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "run cancellation did not reach summary"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                }
                SummaryMode::Steering if self.summaries.load(Ordering::SeqCst) == 1 => {
                    self.summary_entered.store(true, Ordering::SeqCst);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    while !self.summary_released.load(Ordering::SeqCst)
                        && !request.cancel.is_cancelled()
                    {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "steering did not release summary"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                }
                SummaryMode::Steering => {}
                SummaryMode::Persistence => self
                    .sessions
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .inject_persistence_faults(FaultHooks {
                        fail_batch_write: true,
                        ..Default::default()
                    }),
                SummaryMode::ConcurrentAppend => {
                    // CP-C1 红①：摘要窗口内并发追加——一条 log-only 事件
                    // （判别 committed_seq CAS 门）+ 一条跨度外的 surface
                    // user message（判别全量 surface 相等门）。两者修前
                    // 分别撞 CAS 与全量比较杀 turn；跨度锚定下都容忍。
                    let sessions = self.sessions.lock().unwrap().as_ref().unwrap().clone();
                    sessions
                        .journal()
                        .expect("journal")
                        .append_atomic_durable(&[crate::session::run_journal::NewSessionEvent::new(
                            "user/message",
                            json!({
                                "id": uuid::Uuid::new_v4().to_string(),
                                "role": "user",
                                "content": [{
                                    "type": "text",
                                    "text": "background note appended during the summary window",
                                }],
                                "source": { "kind": "user" },
                            }),
                        )
                        .append(Vec::new())])
                        .expect("surface append lands outside the selected span");
                    sessions
                        .journal()
                        .expect("journal")
                        .append_atomic_durable(&[crate::session::run_journal::NewSessionEvent::new(
                            "job/registered",
                            json!({
                                "jobId":"concurrent-probe",
                                "label":"probe",
                                "owner":"test",
                                "kind":"process",
                                "command":["sleep","0"],
                                "cwd":"/tmp",
                                "startedAt":0,
                            }),
                        )
                        .log_only()])
                        .expect("concurrent log-only append lands inside the summary window");
                }
                SummaryMode::SpanRewrite if self.span_rewritten.swap(true, Ordering::SeqCst) => {}
                SummaryMode::SpanRewrite => {
                    // CP-C1 红②：摘要窗口内并发改写跨度内最旧的
                    // tool/result（模拟并发 prune replace）。只改写一次——
                    // 重复改写同内容会被跨度锚定视为未变而放行。
                    let sessions = self.sessions.lock().unwrap().as_ref().unwrap().clone();
                    let target = sessions
                        .maintenance_events()
                        .into_iter()
                        .filter(|event| event.event_type == "tool/result")
                        .min_by_key(|event| event.seq)
                        .expect("a durable tool result to rewrite");
                    // 与 prune replace 同款：克隆原事件，只改 content——
                    // replace 校验只允许结果内容变化。
                    let mut data = target.data;
                    *data
                        .pointer_mut("/message/content")
                        .expect("result content") = json!([{
                            "type": "text",
                            "text": "rewritten by a concurrent prune",
                        }]);
                    sessions
                        .journal()
                        .expect("journal")
                        .append_atomic_durable(&[crate::session::run_journal::NewSessionEvent::new(
                            "tool/result",
                            data,
                        )
                        .replace(target.seq, target.seq, vec![target.seq])])
                        .expect("in-span rewrite lands inside the summary window");
                }
                SummaryMode::SweepPressure => {
                    panic!("paid summary reached: the model-free sweep must absorb the pressure first");
                }
                SummaryMode::Normal | SummaryMode::ImageTurn => {}
            }
            let finish = if request.cancel.is_cancelled() {
                FinishReason::Cancelled
            } else {
                FinishReason::Completed
            };
            return Ok(crate::test_support::response(
                &crate::test_support::checkpoint("Read observations preserved; continue remaining reads."),
                finish,
            ));
        }
        assert!(
            request
                .tools
                .iter()
                .any(|definition| definition.name == "read_file"),
            "normal agent requests keep the complete authorized file tool schema after compaction"
        );
        let tail = request
            .options
            .request_tail
            .as_deref()
            .expect("current runtime snapshot");
        assert_eq!(
            tail.matches("Current runtime context supplied by CLAT")
                .count(),
            1
        );
        let n = self.main.fetch_add(1, Ordering::SeqCst);
        // 每轮写入两个新观测文件再读回：EvidenceTracker 要求读工具产出
        // 新证据（同文件同参同出会 stall；read_file 的 Fresh 也不撤销待
        // 发纠正）。真实长任务是扫描大量不同文件，不是反复读同一份。
        if let Some(root) = self.root.lock().unwrap().clone() {
            for index in 0..2 {
                std::fs::write(
                    root.join(format!("observations-{n}-{index}.txt")),
                    format!("observation evidence {n}-{index}\n").repeat(600),
                )
                .unwrap();
            }
        }
        self.snapshots.lock().unwrap().push(request.items.to_vec());
        if matches!(self.mode, SummaryMode::FailureThenCapacity)
            && self.summaries.load(Ordering::SeqCst) > 0
        {
            return Err(ModelError::with_kind(
                crate::model::ModelErrorKind::ContextCapacity,
                "fixture rejected the current context",
            ));
        }
        if n >= 8 && matches!(self.mode, SummaryMode::ImageTurn) {
            return Ok(crate::test_support::response(
                "done",
                FinishReason::Completed,
            ));
        }
        let goal = self.goal_input.load(Ordering::SeqCst);
        if goal && n == 8 {
            return Ok(crate::test_support::response(
                "awaiting review",
                FinishReason::Completed,
            ));
        }
        if goal {
            assert!(request.items.iter().any(|item|matches!(item,ModelItem::User{content} if content.iter().any(|p|matches!(p,crate::model::ContentPart::Text(text) if text.contains("KEEP_USER_INTENT")&&text.contains("<round>1</round>"))))));
        } else {
            assert!(
                request
                    .items
                    .contains(&ModelItem::user_text("KEEP_USER_INTENT".to_owned()))
            );
        }
        if n > 0 {
            let newest = request
                .items
                .iter()
                .rev()
                .find(|item| matches!(item, ModelItem::ToolResult(_)));
            // 不变量（CP-C4 后口径）：最新交互组保留**真实内容**——
            // 本轮内的富对象，或跨 run 重放的完整字符串；清组占位符与
            // 剪枝残桩都不合格。
            let rich = matches!(newest, Some(ModelItem::ToolResult(result))
                if result.output.is_object()
                    || result
                        .output
                        .as_str()
                        .is_some_and(|text| text.contains("observation evidence")));
            assert!(rich, "keep rich live results rather than replay strings");
            if n < 8
                && matches!(newest, Some(ModelItem::ToolResult(result)) if result.tool_name == "read_file")
            {
                assert!(
                    matches!(newest, Some(ModelItem::ToolResult(result)) if result.call_id == format!("read-{}-1", n - 1)),
                    "retain the newest complete exchange"
                );
            }
        }
        let mut reply = crate::test_support::response("done", FinishReason::Completed);
        if n < 7 {
            reply.text.clear();
            reply.finish_reason = FinishReason::ToolCalls;
            reply.tool_calls = (0..2)
                .map(|index| crate::ToolCall {
                    id: format!("read-{n}-{index}"),
                    name: "read_file".into(),
                    arguments: json!({
                        "path": format!("observations-{n}-{index}.txt"),
                        "max_bytes": 16000
                    }),
                })
                .collect();
            for call in &reply.tool_calls {
                events.emit(ModelEvent::ToolCallCompleted { call: call.clone() });
            }
        } else if goal && !request.tools.iter().any(|tool| tool.name == "update_goal") {
            reply.text.clear();
            reply.finish_reason = FinishReason::ToolCalls;
            reply.tool_calls.push(crate::ToolCall {
                id: "describe-goal".into(),
                name: "describe_capability".into(),
                arguments: json!({"name": "update_goal"}),
            });
        } else if goal {
            reply.text.clear();
            reply.finish_reason = FinishReason::ToolCalls;
            reply.tool_calls.push(crate::ToolCall {
                id: "goal-review".into(),
                name: "update_goal".into(),
                arguments: json!({"operation":"complete","expected_revision":1,"summary":"ready for review"}),
            });
        } else {
            events.emit(ModelEvent::TextDelta {
                delta: reply.text.clone(),
            });
        }
        Ok(reply)
    }
}
fn fixture(
    mode: SummaryMode,
    budget: Option<u64>,
) -> (TrustedProjectApplication, Arc<LongTask>, PathBuf) {
    let (storage, root) = roots("active-context");
    std::fs::create_dir_all(&root).unwrap();
    let script = Arc::new(LongTask::new(mode));
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(script.clone()),
    );
    // Auxiliary title concurrency has its own gate; this script owns only main/summary steps.
    if let Some(worker) = app.title_worker.as_mut() {
        worker.shutdown().unwrap();
    }
    configure_test_model_with_budget(&app, u32::MAX);
    let fixed = app.context_snapshot().unwrap().total_estimate;
    // The failure case must exceed the frozen request limit, not merely the
    // earlier compaction trigger (which includes an extra safety reserve).
    // Leave less history room so a failed summary cannot still fit and allow
    // another main-model step after changes to the fixed tool catalog.
    let history_room = if matches!(mode, SummaryMode::Failure) {
        8000
    } else {
        9000
    };
    configure_test_model_with_budget(&app, u32::try_from((fixed + history_room) * 5 / 4).unwrap());
    let (mut config, credentials) = app.model_state().unwrap();
    config.run_token_budget = budget;
    app.save_model_state(&config, &credentials).unwrap();
    *script.sessions.lock().unwrap() = Some(Arc::clone(&app.sessions));
    *script.root.lock().unwrap() = Some(root.clone());
    (app, script, storage)
}

#[test]
fn committed_compaction_notifications_match_durable_replay() {
    let (mut app, _, storage) = fixture(SummaryMode::Normal, None);
    let (sender, receiver) = mpsc::channel();
    app.subscribe(sender);
    run(&mut app, "KEEP_USER_INTENT").unwrap();
    let session = app.current_session_id().unwrap();
    let recorded = receiver
        .try_iter()
        .filter_map(|event| match event {
            ApplicationEvent::CompactionRecorded {
                session,
                seq,
                summary,
                disclosure,
            } => Some((session, seq, summary, disclosure)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        recorded.len() >= 2,
        "each committed reduction must be reported"
    );
    app.close().unwrap();
    let replay = crate::session::replay::ReplayAdapter::fold(&load_events(&storage));
    for (owner, seq, summary, disclosure) in &recorded {
        assert_eq!(owner, &session);
        assert!(replay.iter().any(|event| matches!(event,
            crate::session::replay::ReplayEvent::Compaction { seq: replay_seq, summary_text, disclosure: Some(data), .. }
            if replay_seq == seq && summary_text == summary && data == disclosure
        )), "live disclosure must match its durable summary event");
    }
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_single_turn_parallel_tools_recompact_and_cold_replay() {
    let (mut app, script, storage) = fixture(SummaryMode::Normal, None);
    let result = run(&mut app, "KEEP_USER_INTENT").expect("long task completes");
    assert!(!result.cancelled);
    assert_eq!(script.main.load(Ordering::SeqCst), 8);
    assert!(
        script.summaries.load(Ordering::SeqCst) >= 2,
        "checkpoint must support later compaction"
    );
    let live = app.sessions.surface_nodes().unwrap();
    let snapshots = script.snapshots.lock().unwrap();
    assert!(snapshots.iter().any(|items| items.iter().any(|item| matches!(item, ModelItem::User {content} if content.iter().any(|part| matches!(part, crate::model::ContentPart::Text(text) if text.contains("<compacted-summary>")))))));
    drop(snapshots);
    let project = app.project.clone();
    app.close().unwrap();
    let events = load_events(&storage);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "turn/start")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "turn/end")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "tool/result")
            .count(),
        14,
        "no tool lost or replayed"
    );
    for family in events
        .windows(4)
        .filter(|family| family[0].event_type == "compaction/start")
    {
        assert_eq!(family[1].event_type, "compaction/summary");
        assert_eq!(family[2].event_type, "user/message");
        assert_eq!(family[3].event_type, "compaction/end");
        let seqs = family[2].source_event_seqs.as_ref().unwrap();
        assert_eq!(
            seqs.len(),
            seqs.iter().collect::<std::collections::HashSet<_>>().len()
        );
        assert!(
            !seqs.contains(
                &events
                    .iter()
                    .find(|event| event.event_type == "user/message")
                    .unwrap()
                    .seq
            )
        );
    }
    let mut reopened = crate::test_support::reopen_after_close(|| {
        mount_result(&project, &storage, TestBehavior::Success)
    });
    assert_eq!(
        reopened.sessions.surface_nodes().unwrap(),
        live,
        "cold replay preserves the committed context"
    );
    assert!(reopened.snapshot().unwrap().replay.iter().any(|event| matches!(event, crate::session::replay::ReplayEvent::UserMessage {text,..} if text == "KEEP_USER_INTENT")));
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_provider_capacity_failure_does_not_retry_or_replay_tools() {
    let (mut app, script, storage) = fixture(SummaryMode::Failure, None);
    assert!(
        run(&mut app, "KEEP_USER_INTENT").is_err(),
        "oversized request must not reach provider after failed summary"
    );
    let seen = script.main.load(Ordering::SeqCst);
    assert_eq!(
        script.summaries.load(Ordering::SeqCst),
        2,
        "only the counting-breaker retries run before the rejection (INV-CP2); \
         the capacity rejection itself must not add another summary attempt"
    );
    app.close().unwrap();
    let events = load_events(&storage);
    let tools = events
        .iter()
        .filter(|event| event.event_type == "tool/result")
        .count();
    assert!(tools > 0 && tools <= seen * 2, "no tool must be replayed");
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "compaction/summary"),
        "no inferred-window compaction is allowed after a provider rejection"
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn startup_pressure_is_relieved_model_free_and_no_summary_is_charged() {
    // CP-C4/CP-C2 映射：原名
    // startup_summary_failure_cannot_restart_through_active_maintenance_or_capacity_recovery。
    // 旧契约（粘性断路器：启动摘要一次失败后本 run 不再尝试）已被
    // INV-CP2 计数熔断取代；且 INV-CP5 清组在该场景把压力在付钱摘要
    // 之前泄掉——启动期根本不再尝试摘要，run 零摘要开销直接完成，
    // 清组以 compaction/prune 影子计价落 journal。失败-重启（计数
    // 熔断的 3 次上界与复位）语义由 cp1 计数熔断专属测试与 Failure
    // 测试（2 次重试）覆盖。
    let (mut app, script, storage) = fixture(SummaryMode::FailureThenCapacity, None);
    configure_test_model_with_budget(&app, u32::MAX);
    run(&mut app, "KEEP_USER_INTENT").expect("seed completed tool exchanges");
    assert_eq!(script.summaries.load(Ordering::SeqCst), 0);
    let history = app.context_snapshot().unwrap().total_estimate;
    configure_test_model_with_budget(&app, u32::try_from(history * 11 / 10).unwrap());

    run(&mut app, "KEEP_USER_INTENT").expect("sweep relieved the startup pressure model-free");
    assert_eq!(
        script.summaries.load(Ordering::SeqCst),
        0,
        "no summary attempt: the model-free sweep absorbed the pressure"
    );
    assert_eq!(script.main.load(Ordering::SeqCst), 9);
    app.close().unwrap();
    let events = load_events(&storage);
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "compaction/prune"),
        "durable sweep accounting"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "compaction/end" && event.data["error"].is_string())
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn two_images_cross_automatic_compaction_threshold_and_survive_either_outcome() {
    // CP-C4/CP-C2 映射：旧三腿循环含 (true,true) 失败变体与
    // "summaries>0 == attachments"（附件轮跨阈值即付钱摘要）断言——
    // INV-C5 清组把旧工具组压力在摘要之前泄掉，两轮都零摘要完成；
    // 图片在用户消息里、不经工具组清组，原样保留。失败路径覆盖由
    // 重塑的 startup 测试与 CP-C2 计数熔断测试承接。
    for attachments in [false, true] {
        let (mut app, script, storage) = fixture(SummaryMode::ImageTurn, None);
        let (mut config, credentials) = app.model_state().unwrap();
        config
            .capabilities
            .input_modalities
            .push(crate::model::Modality::Image);
        config.capabilities.image_input_verified = true;
        config.overrides.max_context_tokens = crate::Override::Set(u32::MAX);
        app.save_model_state(&config, &credentials).unwrap();
        assert_eq!(
            app.model_state().unwrap().0.max_context_tokens,
            Some(u32::MAX)
        );
        run(&mut app, "KEEP_USER_INTENT").expect("seed identical complete tool exchanges");
        assert_eq!(script.summaries.load(Ordering::SeqCst), 0);

        let dimensions = [(1806, 754), (1796, 842)];
        let image_tokens = dimensions
            .iter()
            .map(|&(w, h)| {
                crate::media::estimate_image_tokens_from_dimensions(Some((
                    u64::from(w),
                    u64::from(h),
                )))
            })
            .sum::<u64>();
        let input = app.context_snapshot().unwrap().input_estimate;
        config.overrides.max_context_tokens =
            crate::Override::Set(u32::try_from((input + image_tokens / 2) * 5 / 4).unwrap());
        app.save_model_state(&config, &credentials).unwrap();
        let effective = app.model_state().unwrap().0;
        assert!(effective.capabilities.accepts_image_input());
        let budget = crate::model::ContextBudget::resolve(&effective)
            .unwrap()
            .unwrap();
        assert!(input < budget.threshold && budget.threshold < input + image_tokens);
        let images = if attachments {
            compaction_images(storage.parent().unwrap(), dimensions)
        } else {
            Vec::new()
        };
        let result = run_with_attachments(&mut app, "KEEP_USER_INTENT", images);
        assert_eq!(result.unwrap().output, "done");
        assert_eq!(
            script.summaries.load(Ordering::SeqCst),
            0,
            "the model-free sweep absorbs the crossing before a paid summary"
        );
        assert_eq!(script.main.load(Ordering::SeqCst), 9);
        let expected_images = if attachments { 2 } else { 0 };
        assert_eq!(
            script
                .snapshots
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .iter()
                .flat_map(crate::model::model_item_image_parts)
                .count(),
            expected_images
        );
        assert_eq!(
            app.context_snapshot().unwrap().image_count,
            expected_images as u64
        );
        app.close().unwrap();
        let events = load_events(&storage);
        assert_eq!(
            events
                .iter()
                .filter(
                    |event| event.event_type == "compaction/end" && event.data["error"].is_string()
                )
                .count(),
            0,
            "no summary was attempted, so no compaction failure is recorded"
        );
        assert_eq!(
            events
                .iter()
                .any(|event| event.event_type == "compaction/prune"),
            attachments,
            "only the crossing (image) turn runs the model-free sweep"
        );
        assert!(
            !events
                .iter()
                .any(|event| event.event_type == "compaction/summary"),
            "no paid summary lands in either leg"
        );
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}

fn compaction_images(root: &std::path::Path, dimensions: [(u32, u32); 2]) -> Vec<PathBuf> {
    dimensions
        .into_iter()
        .enumerate()
        .map(|(i, (width, height))| {
            let path = root.join(format!("pressure-{i}.png"));
            std::fs::write(
                &path,
                crate::test_support::png_bytes(width, height, [i as u8, 180, 40]),
            )
            .unwrap();
            path
        })
        .collect()
}

#[test]
fn active_compaction_cancel_stops_without_another_model_or_tool_request() {
    let (mut app, script, storage) = fixture(SummaryMode::Cancel, None);
    let (completion, receiver) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("KEEP_USER_INTENT"),
            approver: allow_all_approver(),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !script.summary_entered.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "summary never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    handle.cancel();
    handle.join().unwrap();
    let result = receiver
        .recv_timeout(crate::test_support::HANG)
        .unwrap()
        .expect("normal cancellation");
    assert!(result.cancelled);
    assert!(script.summaries.load(Ordering::SeqCst) >= 1);
    let seen = script.main.load(Ordering::SeqCst);
    app.close().unwrap();
    let events = load_events(&storage);
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "step/start")
            .count(),
        seen
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "tool/result")
            .count(),
        seen * 2
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_persistence_failure_never_switches_surface_or_replays_tools() {
    let (mut app, script, storage) = fixture(SummaryMode::Persistence, None);
    let (sender, receiver) = mpsc::channel();
    app.subscribe(sender);
    let result = run(&mut app, "KEEP_USER_INTENT");
    assert!(
        !receiver
            .try_iter()
            .any(|event| matches!(event, ApplicationEvent::CompactionRecorded { .. })),
        "a failed durable write must not publish a success disclosure"
    );
    assert!(result.is_err());
    assert!(script.summaries.load(Ordering::SeqCst) >= 1);
    let nodes = app.sessions.surface_nodes().unwrap();
    assert!(!nodes.iter().any(|(_, item)| matches!(item, ModelItem::User {content} if content.iter().any(|part| matches!(part, crate::model::ContentPart::Text(text) if text.contains("<compacted-summary>"))))));
    app.sessions
        .inject_persistence_faults(FaultHooks::default());
    app.close().unwrap();
    let events = load_events(&storage);
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "tool/result")
            .count(),
        script.main.load(Ordering::SeqCst) * 2
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_summary_usage_exhausts_same_run_budget_before_next_request() {
    let (mut app, script, storage) = fixture(SummaryMode::Normal, Some(40));
    // Main requests alone would finish at 40; internal summaries must prevent
    // completion on that same cap instead of disappearing from the ledger.
    let result = run(&mut app, "KEEP_USER_INTENT");
    assert!(result.is_err());
    assert!(script.summaries.load(Ordering::SeqCst) > 0);
    app.close().unwrap();
    let events = load_events(&storage);
    let input: u64 = events
        .iter()
        .filter_map(|event| event.data["usage"]["inputTokens"].as_u64())
        .sum();
    let output: u64 = events
        .iter()
        .filter_map(|event| event.data["usage"]["outputTokens"].as_u64())
        .sum();
    assert!(input + output >= 40);
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_below_pressure_does_not_request_a_summary() {
    let (mut app, script, storage) = fixture(SummaryMode::Normal, None);
    configure_test_model_with_budget(&app, u32::MAX);
    run(&mut app, "KEEP_USER_INTENT").unwrap();
    assert_eq!(script.main.load(Ordering::SeqCst), 8);
    assert_eq!(script.summaries.load(Ordering::SeqCst), 0);
    app.close().unwrap();
    assert!(
        !load_events(&storage)
            .iter()
            .any(|event| event.event_type == "compaction/start")
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_steering_during_summary_survives_later_checkpoints() {
    let (mut app, script, storage) = fixture(SummaryMode::Steering, None);
    let (completion, receiver) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("KEEP_USER_INTENT"),
            approver: allow_all_approver(),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !script.summary_entered.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "summary never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(matches!(
        app.steer(crate::message::PendingMessage::text("KEEP_STEERING_INTENT")),
        SteerOutcome::Queued { .. }
    ));
    script.summary_released.store(true, Ordering::SeqCst);
    handle.join().unwrap();
    receiver
        .recv_timeout(crate::test_support::HANG)
        .unwrap()
        .expect("complete with steering");
    assert!(script.summaries.load(Ordering::SeqCst) > 1);
    assert!(
        script
            .snapshots
            .lock()
            .unwrap()
            .iter()
            .any(|items| items.contains(&ModelItem::user_text("KEEP_STEERING_INTENT")))
    );
    assert!(
        app.sessions
            .surface_nodes()
            .unwrap()
            .iter()
            .any(|(_, item)| *item == ModelItem::user_text("KEEP_STEERING_INTENT"))
    );
    app.close().unwrap();
    let events = load_events(&storage);
    let steering = events
        .iter()
        .find(|event| {
            event.event_type == "user/message"
                && event.data["content"]
                    .to_string()
                    .contains("KEEP_STEERING_INTENT")
        })
        .unwrap()
        .seq;
    assert!(
        !events
            .iter()
            .filter_map(|event| event.source_event_seqs.as_ref())
            .any(|seqs| seqs.contains(&steering))
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_goal_round_keeps_intent_and_waits_for_user_acceptance() {
    let (mut app, script, storage) = fixture(SummaryMode::Normal, None);
    script.goal_input.store(true, Ordering::SeqCst);
    app.goal_create(
        "KEEP_USER_INTENT",
        crate::goal::GoalAcceptance::User,
        crate::goal::GoalLimits {
            max_rounds: 1,
            max_tokens: 1_000_000,
            max_time_secs: 60,
            max_failures: 2,
        },
        true,
    )
    .unwrap();
    let (completion, receiver) = mpsc::channel();
    let (handle, _) = app
        .start_goal_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(""),
            approver: allow_all_approver(),
            asker: None,
            events: Box::new(Vec::<crate::RunEvent>::new()),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let result = receiver.try_recv().unwrap();
    let view = app.goal().unwrap().unwrap();
    let summaries = script.summaries.load(Ordering::SeqCst);
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
    assert!(result.is_ok(), "{result:?}");
    assert!(summaries >= 2);
    assert_eq!(view.state.rounds_started, 1);
    assert_eq!(
        view.state.phase,
        crate::goal::GoalPhase::Blocked,
        "a completion candidate waits for human acceptance"
    );
    assert!(!view.armed);
}

#[test]
fn active_compaction_automatic_target_never_switches_or_writes_foreground() {
    use super::super::target_run::{AutomaticSource, TargetRunRequest};
    let (mut app, script, storage) = fixture(SummaryMode::Normal, None);
    script.seeding.store(true, Ordering::SeqCst);
    run(&mut app, "KEEP_USER_INTENT").unwrap();
    let target = app.current_session_id().unwrap();
    app.new_session().unwrap();
    run(&mut app, "foreground must remain untouched").unwrap();
    let foreground = app.current_session_id().unwrap();
    let foreground_seq = app.committed_seq();
    app.control.add_trust(app.project.root()).unwrap();
    script.seeding.store(false, Ordering::SeqCst);
    let (mut config, credentials) = app.model_state().unwrap();
    config.run_token_budget = Some(1_000_000);
    let (completion, receiver) = mpsc::channel();
    let handle = app
        .start_target_run(TargetRunRequest {
            session: target.clone(),
            message: crate::message::PendingMessage::from_front_end(
                "KEEP_USER_INTENT",
                Some("phase9-target".into()),
                vec![],
            ),
            config,
            credentials,
            timeout: Duration::from_secs(15),
            web_budget: Arc::new(crate::web_tools::PaidCallBudget::new(0)),
            meter: Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX)),
            approver: allow_all_approver(),
            asker: None,
            source: AutomaticSource::Job {
                job_id: "offline-reliability".into(),
            },
            events: Box::new(Vec::<crate::RunEvent>::new()),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let result = receiver.try_recv().unwrap();
    assert_eq!(app.current_session_id(), Some(foreground));
    assert_eq!(app.committed_seq(), foreground_seq);
    let key = SessionKey {
        project: app.project_key(),
        id: target,
    };
    app.close().unwrap();
    let backend = crate::session::persistence::JsonlBackend::new(
        storage.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let events = backend.load(&key, false).unwrap().events;
    crate::test_support::cleanup_tree(storage.parent().unwrap());
    assert!(result.is_ok(), "{result:?}");
    assert!(
        events
            .iter()
            .filter(|e| e.event_type == "compaction/summary")
            .count()
            >= 2
    );
    assert_eq!(script.main.load(Ordering::SeqCst), 8);
}

// ── CP-1 判别测试（2026-11-20 立项，docs/todo/cp1-compaction-stability）──

/// INV-CP1 红①：摘要窗口内的 log-only 追加（`job/*` 类）不杀 turn——
/// 压缩照常落地，追加事件照常存活。修前（committed_seq CAS + 全量
/// surface 相等）此场景 Err("session changed during context maintenance")
/// 上抛，当前 turn 死。
#[test]
fn cp1_concurrent_log_only_append_survives_and_compaction_lands() {
    let (mut app, script, storage) = fixture(SummaryMode::ConcurrentAppend, None);
    let (sender, receiver) = mpsc::channel();
    app.subscribe(sender);
    run(&mut app, "KEEP_USER_INTENT").expect("the turn survives the concurrent append");
    assert!(
        script.summaries.load(Ordering::SeqCst) >= 1,
        "the summary ran and its commit was accepted"
    );
    assert!(
        receiver
            .try_iter()
            .any(|event| matches!(event, ApplicationEvent::CompactionRecorded { .. })),
        "the compaction checkpoint was published"
    );
    app.close().unwrap();
    let events = load_events(&storage);
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "job/registered"
                && event.data["jobId"] == json!("concurrent-probe")),
        "the concurrent log-only append survives the compaction commit"
    );
    assert!(
        events.iter().any(|event| event.event_type == "user/message"
            && event.data["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("background note appended"))),
        "the concurrent surface append survives outside the selected span"
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "compaction/summary"),
        "the compaction landed"
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

/// INV-CP1 红②：跨度内节点被并发改写（模拟并发 prune replace）→ 拒绝
/// 提交且**降级不抛**——run 存活，本次压缩作废记账，无 checkpoint 落地。
/// 修前全量相等检查同样拒绝但以 Err 上抛杀死 turn。
#[test]
fn cp1_in_span_rewrite_rejects_commit_as_degraded_not_a_turn_error() {
    let (mut app, script, storage) = fixture(SummaryMode::SpanRewrite, None);
    let (sender, receiver) = mpsc::channel();
    app.subscribe(sender);
    let outcome = run(&mut app, "KEEP_USER_INTENT");
    // 判别：修前此场景以 Err("session changed during context
    // maintenance") 杀 turn；修后跨度改写只降级——run 的任何失败都
    // 只能是后续的容量错，绝不是维护提交错。
    if let Err(failure) = outcome {
        assert!(
            !failure.error.contains("session changed"),
            "a span rewrite must degrade, not kill the turn: {}",
            failure.error
        );
    }
    // 跨度改写每次尝试都被降级（无 usage 记账时连 compaction/end 都
    // 不落），计数只用于熔断；这里只锁"至少跑了一次摘要"。
    assert!(
        script.summaries.load(Ordering::SeqCst) >= 1,
        "at least one summary attempt ran"
    );
    app.close().unwrap();
    let events = load_events(&storage);
    // 判别：被改写的那次尝试必然降级——journal 存在带 span 字样的
    // 失败记账（record_failed_attempt 只在 usage 非零时落 end 对）。
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "compaction/end"
                && event.data["error"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("span"))),
        "the degradation reason names the span change"
    );
    // 且 run 的失败（若有）不是维护提交错。
    let _ = &receiver;
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

/// INV-CP2：计数熔断——同 run 内连续失败到 3 次即暂停自动压缩；下一
/// 轮真实用户输入（新 RunWorker，计数器结构性复位）恢复再试。修前
/// 粘性断路器一次失败即永久停摆。
#[test]
fn cp2_failure_breaker_counts_three_and_resets_on_the_next_user_round() {
    let (mut app, script, storage) = fixture(SummaryMode::Failure, None);
    let first = run(&mut app, "KEEP_USER_INTENT");
    assert!(
        first.is_err(),
        "oversized request still fails the first run"
    );
    let after_first = script.summaries.load(Ordering::SeqCst);
    // 该窄窗的容量恢复链只再给一次维护机会：合计 2 次尝试后熔断
    // （<3 亦未开闸暂停——粘性修前这里只有 1 次）。
    assert_eq!(
        after_first, 2,
        "counting breaker retries after the first failure (sticky: 1)"
    );
    // 第二 run：新用户输入 → 新 RunWorker → 计数结构性复位 → 再试。
    // 该 run 在接纳即超容（清组对 6 条历史泄不动）只给 1 次尝试后硬
    // 拒——复位的最小判别是"有再试"（粘性修前这里只会是 0）。
    let second = run(&mut app, "KEEP_USER_INTENT");
    assert!(second.is_err());
    let after_second = script.summaries.load(Ordering::SeqCst);
    assert!(
        after_second > after_first,
        "the next user round resets the failure count and retries again (sticky: 0)"
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

/// INV-CP5（CP-C4 红①/红②）：压力达标时清组先行——最近 5 组保留、更旧
/// 的文本组替换为占位符、付费摘要零调用（SweepPressure 模式下摘要请求
/// 到达即 panic，因此 run 完成即为零调用判别）。
#[test]
fn cp4_micro_sweep_clears_old_groups_before_any_paid_summary() {
    let (mut app, script, storage) = fixture(SummaryMode::SweepPressure, None);
    // 第一轮：宽窗攒真实工具组（清组的受保护地板保护本轮内容——
    // INV-CP5 的保守面；可清"旧组"来自之前的轮次）。
    configure_test_model_with_budget(&app, u32::MAX);
    run(&mut app, "KEEP_USER_INTENT").expect("seed turns accumulate tool groups");
    // 第二轮：窄窗制造压力——startup/活动维护先清组（旧组 seq < 本轮
    // replay 地板），压力泄到阈下则零付费摘要（摘要到达即 panic）。
    let history = app.context_snapshot().unwrap().total_estimate;
    configure_test_model_with_budget(&app, u32::try_from(history * 11 / 10).unwrap());
    run(&mut app, "KEEP_USER_INTENT").expect("the sweep absorbs the pressure model-free");
    assert_eq!(
        script.summaries.load(Ordering::SeqCst),
        0,
        "no paid summary: the sweep reduced the pressure below the threshold"
    );
    app.close().unwrap();
    let events = load_events(&storage);
    let sweeps = events
        .iter()
        .filter(|event| event.event_type == "compaction/prune")
        .count();
    // 夹具产出 8 轮 × 2 组 = 16 组；保 5 组 → 至多清 11 组（轮内早期
    // 组在压力首次达标前生成，实际清除数 ≥ 压力达标时点的旧组数）。
    assert!(
        sweeps > 0,
        "the durable sweep accounting (compaction/prune) is present"
    );
    let swept_ids = events
        .iter()
        .filter(|event| event.event_type == "tool/result")
        .filter(|event| {
            event
                .data
                .pointer("/message/content/0/text")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| text.contains("[micro-compaction]"))
        })
        .filter_map(|event| event.data["message"]["toolCallId"].as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(
        !swept_ids.is_empty(),
        "a swept group leaves the one-line placeholder with its annotation"
    );
    // Seven model steps each emitted TWO tool calls. Keeping five complete
    // steps means both results in rounds 2..=6 survive. The old
    // results.len()-5 implementation erased some of these recent steps.
    for round in 2..7 {
        for index in 0..2 {
            assert!(
                !swept_ids.contains(format!("read-{round}-{index}").as_str()),
                "micro-compaction must not cut a preserved multi-call model step"
            );
        }
    }
    assert!(
        swept_ids.contains("read-0-0") || swept_ids.contains("read-1-0"),
        "older steps are still eligible for model-free clearing"
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}
