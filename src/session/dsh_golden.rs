//! B8 golden 读腿：既覆盖 DSH 0.1.1-rc.2 的 released-v0 字节，也
//! 覆盖 0.1.3-alpha.1（`d347e70390`）原生写出的 released-v2 字节。与
//! `interop.rs`（/tmp 自跳过的原语级互证）不同，这里的 fixture
//! **提交进库**（`tests/fixtures/dsh-session/`），本模块随主测试套
//! 常跑、零 Node 依赖；再生脚本见同目录 `gen-dsh-fixtures.mts`
//!（dev 侧，含 DSH 读 CLAT 产物的反向腿）。
//!
//! 三条腿分别钉住：
//! - **interrupted 前缀定稿**：流中取消的会话，`assistant/message`
//!   携带 `interrupted: true`（DSH agent.ts:352-368 的取消分支），
//!   未派发的 tool calls 不出现；CLAT 的完整 load → 准入 → 重放
//!   路径识别前缀并还原部分文本。
//! - **team/* 已知类型**：4 个必需信封事件（无 ignorable）在真实
//!   zstd 帧字节中随会话落盘；CLAT 准入放行、重放跳过不重建、会话
//!   其余部分正常还原（CLAT 自产日志不含 team/* 的断言在 catalog
//!   测试中另行钉住）。
//! - **Plan Mode approved 扩展**：DSH 真实 writer 写入并由 DSH 真实 reader
//!   读回 `plan/mode {active:false,approved:{text,digest}}`；提交进库的同一
//!   zstd golden 再由 CLAT load → admission → projection 常跑消费。

#[cfg(test)]
mod tests {
    use crate::session::compat::log_file_name;
    use crate::session::event::SessionEvent;
    use crate::session::header::SessionHeader;
    use crate::session::id::SessionId;
    use crate::session::key::{ProjectKey, SessionKey};
    use crate::session::path_layout::{log_path, project_key};
    use crate::session::persistence::{JsonlBackend, JsonlCompression};
    use crate::session::replay::{ReplayAdapter, ReplayEvent};

    /// fixture 头部由生成脚本固定（见 gen-dsh-fixtures.mts）。
    const FIXTURE_CWD: &str = "/Users/deng/Documents/GitHub/clat";
    const INTERRUPTED_ID: &str = "018f2a64-9d3f-7cde-8123-9a4f2b6c0b01";
    const TEAM_ID: &str = "018f2a64-9d3f-7cde-8123-9a4f2b6c0b02";
    const PLAN_ID: &str = "018f2a64-9d3f-7cde-8123-9a4f2b6c0b03";
    const MODEL_SELECTION_ID: &str = "018f2a64-9d3f-7cde-8123-9a4f2b6c0b04";
    const FEEDBACK_ID: &str = "018f2a64-9d3f-7cde-8123-9a4f2b6c0b05";
    const V2_ID: &str = "018f2a64-9d3f-7cde-8123-9a4f2b6c0d01";
    const APPROVED_PLAN: &str =
        "Inspect the project, preserve invariants, implement the change, then run focused tests.";

    fn fixture_dir() -> std::path::PathBuf {
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("tests/fixtures/dsh-session")
    }

    /// 把 golden 日志按 CLAT 布局放进临时 root（project_key(cwd) /
    /// encode_segment(id) / log 文件名），返回 root 供清理。
    fn mount_fixture(file: &str, id: &str) -> (std::path::PathBuf, JsonlBackend) {
        mount_fixture_generation(file, id, 0)
    }

    fn mount_fixture_generation(
        file: &str,
        id: &str,
        version: u32,
    ) -> (std::path::PathBuf, JsonlBackend) {
        let root = std::env::temp_dir().join(format!(
            "clat-dsh-golden-{id}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let backend = JsonlBackend::new(root.clone(), JsonlCompression::Zstd, false);
        let current_target = log_path(
            &root,
            Some(FIXTURE_CWD),
            &SessionId::new(id),
            JsonlCompression::Zstd,
        );
        let target = current_target.parent().expect("session dir").join(
            crate::session::compat::generation_log_file_name(version, JsonlCompression::Zstd),
        );
        std::fs::create_dir_all(target.parent().expect("parent")).expect("layout dir");
        std::fs::copy(fixture_dir().join(file), &target).expect("copy golden log");
        (root, backend)
    }

    /// DV-1 decisive read leg: bytes were emitted and read back by the pinned
    /// DSH 0.1.3 live persistence path. CLAT must discover the v2 generation,
    /// expand range provenance, admit `series`, and decode every embedded
    /// Assistant stream record variant including a failed attempt.
    #[test]
    fn dsh_013_native_v2_fixture_decodes_the_full_family() {
        let (root, backend) = mount_fixture_generation("v2-session-0.1.3.jsonl.zstd", V2_ID, 2);
        let header = backend.header_snapshot(&key_for(V2_ID)).expect("v2 header");
        assert_eq!(header.version, 2);
        assert!(!header.is_seeded);

        let events = load_golden(&backend, V2_ID);
        assert_eq!(events.len(), 11);
        assert!(
            !events
                .iter()
                .any(|event| event.event_type == "assistant/chunk")
        );

        let replacement = events
            .iter()
            .find(|event| event.seq == 4)
            .expect("range-bearing replacement");
        assert_eq!(replacement.source_event_seqs, Some(vec![1, 2, 3]));

        let request = events
            .iter()
            .find(|event| event.event_type == "request/header")
            .expect("request header");
        assert_eq!(request.data["reason"], "series");
        assert_eq!(request.data["startsSeries"], true);

        let attempt = events
            .iter()
            .find(|event| event.event_type == "assistant/attempt")
            .expect("failed attempt");
        let decoded =
            crate::session::assistant_stream::expand_assistant_stream(&attempt.data["stream"])
                .expect("all four AssistantStreamRecord variants");
        let chunk_types: Vec<&str> = decoded
            .iter()
            .map(|timed| timed.chunk["type"].as_str().expect("chunk type"))
            .collect();
        assert_eq!(
            chunk_types,
            vec!["text-delta", "reasoning-delta", "tool-call-delta", "finish"]
        );

        let message = events
            .iter()
            .find(|event| event.event_type == "assistant/message")
            .expect("settled message");
        assert_eq!(
            crate::session::assistant_stream::expand_assistant_stream(&message.data["stream"])
                .expect("message stream")
                .len(),
            2
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn key_for(id: &str) -> SessionKey {
        SessionKey {
            project: ProjectKey::from_cwd(FIXTURE_CWD),
            id: SessionId::new(id),
        }
    }

    /// 布局发现的黄金路径也顺带钉住：list_headers 能看见 DSH 产的
    /// 会话（project_key 推导与 DSH 的 projectKey 同一 cwd 哈希语义）。
    fn load_golden(backend: &JsonlBackend, id: &str) -> Vec<SessionEvent> {
        let headers: Vec<SessionHeader> = backend
            .list_headers()
            .expect("list headers")
            .into_iter()
            .filter(|header| header.id.as_str() == id)
            .collect();
        assert_eq!(headers.len(), 1, "the golden session is discoverable");
        let mut events = backend
            .load(&key_for(id), true)
            .expect("admission + load succeed on DSH bytes")
            .events;
        // load 返回的事件按 seq 升序（投影折叠依赖），fixture 顺带钉住。
        let mut seqs: Vec<u64> = events.iter().map(|event| event.seq).collect();
        let sorted = {
            seqs.sort();
            seqs.clone()
        };
        let mut seqs: Vec<u64> = events.iter().map(|event| event.seq).collect();
        assert_eq!(seqs, sorted, "events come back seq-ordered");
        seqs.clear();
        events.reverse();
        events.reverse();
        events
    }

    /// B8-a（判别腿）：interrupted 前缀定稿——事件层保留
    /// `interrupted: true`、未派发 tool calls 不出现；重放识别前缀并
    /// 还原部分文本（删除 replay 的 assistant 还原或 load 的字段透传
    /// 即红）。
    #[test]
    fn dsh_interrupted_prefix_finalizes_and_replays() {
        let (root, backend) = mount_fixture("interrupted-session.jsonl.zstd", INTERRUPTED_ID);
        let events = load_golden(&backend, INTERRUPTED_ID);

        let interrupted: Vec<&SessionEvent> = events
            .iter()
            .filter(|event| event.event_type == "assistant/message")
            .filter(|event| event.data.get("interrupted") == Some(&serde_json::json!(true)))
            .collect();
        assert_eq!(
            interrupted.len(),
            1,
            "exactly one interrupted prefix finalization"
        );
        let message = &interrupted[0].data["message"];
        assert_eq!(
            message["content"][0]["text"], "partial answer before ",
            "the partial prefix is the finalized message"
        );
        assert!(
            events.iter().all(|event| event.event_type != "tool/call"),
            "undispatched tool calls never appear (DSH types.ts:273-277)"
        );
        // turn 终态是 aborted（user）。
        let turn_end = events
            .iter()
            .find(|event| event.event_type == "turn/end")
            .expect("turn/end present");
        assert_eq!(turn_end.data["reason"]["kind"], "aborted");

        // 重放：前缀作为 assistant 消息还原，取消不产生孤儿状态。
        let replay = ReplayAdapter::fold(&events);
        let assistant: Vec<&ReplayEvent> = replay
            .iter()
            .filter(|event| matches!(event, ReplayEvent::AssistantMessage { .. }))
            .collect();
        assert_eq!(assistant.len(), 1, "the interrupted prefix replays");
        match assistant[0] {
            ReplayEvent::AssistantMessage {
                text, tool_calls, ..
            } => {
                assert_eq!(text, "partial answer before ");
                assert!(tool_calls.is_empty(), "no tool calls materialize");
            }
            other => panic!("unexpected replay event: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// B8-b（判别腿）：4 个 team/* 已知类型——必需信封、无 ignorable；
    /// 准入放行（删 catalog 补录即红）、重放跳过不重建（把 team 加进
    /// replay 处理即红——折叠结果长度变化）、其余会话正常还原。
    #[test]
    fn dsh_team_events_admit_and_replay_skips() {
        let (root, backend) = mount_fixture("team-events-session.jsonl.zstd", TEAM_ID);
        let events = load_golden(&backend, TEAM_ID);

        let team_types: Vec<&str> = events
            .iter()
            .filter(|event| event.event_type.starts_with("team/"))
            .map(|event| event.event_type.as_str())
            .collect();
        assert_eq!(
            team_types,
            vec![
                "team/member",
                "team/message/queued",
                "team/message/delivered",
                "team/task",
            ],
            "all four team/* known types ride in real DSH bytes"
        );
        for event in &events {
            assert!(
                event.ignorable.is_none(),
                "team fixtures carry required envelopes (no ignorable)"
            );
        }

        // 重放：user + assistant 还原；team 事件跳过不重建。
        let replay = ReplayAdapter::fold(&events);
        let user_texts: Vec<&String> = replay
            .iter()
            .filter_map(|event| match event {
                ReplayEvent::UserMessage { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec!["delegate some work to teammates"],
            "the user message restores normally"
        );
        let assistant: Vec<&ReplayEvent> = replay
            .iter()
            .filter(|event| matches!(event, ReplayEvent::AssistantMessage { .. }))
            .collect();
        assert_eq!(assistant.len(), 1, "the assistant reply restores");
        match assistant[0] {
            ReplayEvent::AssistantMessage { text, .. } => {
                assert_eq!(text, "delegated and collected");
            }
            other => panic!("unexpected replay event: {other:?}"),
        }
        assert_eq!(
            replay.len(),
            3,
            "user + assistant + turn/end only — team events are skipped without rebuilding: {replay:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// DV-5（B3 同型判别腿）：DSH 0.1.2-alpha.4+ 的 3 个 v0 必填事件
    ///（引入提交 822d735356）在 v0 帧字节中随会话落盘——CLAT 准入
    /// 放行（删 catalog 补录即红：RequiredUnknown 拒载整个会话）、
    /// 重放跳过不重建。fixture 出处见 `gen-dv5-fixture.mjs`：钉靶
    /// 0.1.3 写路径只产 v2（`SESSION_FORMAT_VERSION = 2`，无版本
    /// 旋钮），v0 字节无法再由 DSH 产出，改以 DSH 同款原语铸出，
    /// payload 形状取自 DSH 0.1.3 源的三个写点。
    #[test]
    fn dsh_012_model_selection_events_admit_and_replay_skips() {
        let (root, backend) =
            mount_fixture("model-selection-session.jsonl.zstd", MODEL_SELECTION_ID);
        let events = load_golden(&backend, MODEL_SELECTION_ID);

        let new_types: Vec<&str> = events
            .iter()
            .filter(|event| {
                matches!(
                    event.event_type.as_str(),
                    "model/selection"
                        | "session-log-deepseek/delivery-accepted"
                        | "subagent/model-selection-policy"
                )
            })
            .map(|event| event.event_type.as_str())
            .collect();
        assert_eq!(
            new_types,
            vec![
                "model/selection",
                "subagent/model-selection-policy",
                "session-log-deepseek/delivery-accepted",
            ],
            "all three 0.1.2 additions ride in v0 bytes in write order"
        );
        for event in &events {
            assert!(
                event.ignorable.is_none(),
                "the fixture carries required envelopes (no ignorable)"
            );
        }

        // 重放：user + assistant 还原；三个新事件跳过不重建。
        let replay = ReplayAdapter::fold(&events);
        let user_texts: Vec<&String> = replay
            .iter()
            .filter_map(|event| match event {
                ReplayEvent::UserMessage { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec!["pick a model for this turn"],
            "the user message restores normally"
        );
        let assistant: Vec<&ReplayEvent> = replay
            .iter()
            .filter(|event| matches!(event, ReplayEvent::AssistantMessage { .. }))
            .collect();
        assert_eq!(assistant.len(), 1, "the assistant reply restores");
        match assistant[0] {
            ReplayEvent::AssistantMessage { text, .. } => {
                assert_eq!(text, "model selected for the run");
            }
            other => panic!("unexpected replay event: {other:?}"),
        }
        assert_eq!(
            replay.len(),
            3,
            "user + assistant + turn/end only — the three additions are skipped: {replay:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// DW-1（DV-5 同型判别腿，第三次复发）：DSH 0.1.3-alpha.2 的 2 个
    /// 必填评价事件在帧字节中随会话落盘——CLAT 准入放行（删 catalog
    /// 补录即红：RequiredUnknown 拒载整个会话）、重放跳过不重建。
    /// fixture 出处见 `gen-dw1-fixture.mjs`：原语级铸造（钉靶写路径
    /// 只产 v2），payload 形状取自 DSH feedback 包写点（put 后 delete
    /// 的真实使用顺序）。
    #[test]
    fn dsh_013a2_feedback_events_admit_and_replay_skips() {
        let (root, backend) = mount_fixture("feedback-session.jsonl.zstd", FEEDBACK_ID);
        let events = load_golden(&backend, FEEDBACK_ID);

        let new_types: Vec<&str> = events
            .iter()
            .filter(|event| event.event_type.starts_with("feedback/message-"))
            .map(|event| event.event_type.as_str())
            .collect();
        assert_eq!(
            new_types,
            vec!["feedback/message-put", "feedback/message-delete"],
            "both alpha.2 additions ride in v0 bytes in write order"
        );
        for event in &events {
            assert!(
                event.ignorable.is_none(),
                "the fixture carries required envelopes (no ignorable)"
            );
        }

        // 重放：user + assistant 还原；两个评价事件跳过不重建。
        let replay = ReplayAdapter::fold(&events);
        let user_texts: Vec<&String> = replay
            .iter()
            .filter_map(|event| match event {
                ReplayEvent::UserMessage { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec!["answer, then the user rates and withdraws the rating"],
            "the user message restores normally"
        );
        let assistant: Vec<&ReplayEvent> = replay
            .iter()
            .filter(|event| matches!(event, ReplayEvent::AssistantMessage { .. }))
            .collect();
        assert_eq!(assistant.len(), 1, "the assistant reply restores");
        match assistant[0] {
            ReplayEvent::AssistantMessage { text, .. } => {
                assert_eq!(text, "rated, then the rating was withdrawn");
            }
            other => panic!("unexpected replay event: {other:?}"),
        }
        assert_eq!(
            replay.len(),
            3,
            "user + assistant + turn/end only — both feedback events are skipped: {replay:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// AG-3/3-A：钉靶 DSH writer+reader 生成的 approved Plan Mode 扩展，
    /// CLAT 的 load/admission/projection 也必须零适配接受同一字节。生成脚本
    /// 在复制 golden 前已通过 DSH `JsonlSessionPersistence.load` 读回并校验
    /// text+digest；这里把反方向变成主套件常跑腿。
    #[test]
    fn dsh_plan_mode_approved_extension_admits_and_projects() {
        let (root, backend) = mount_fixture("plan-mode-approved-session.jsonl.zstd", PLAN_ID);
        let events = load_golden(&backend, PLAN_ID);
        let plan_events = events
            .iter()
            .filter(|event| event.event_type == "plan/mode")
            .collect::<Vec<_>>();
        assert_eq!(plan_events.len(), 2, "active birth + approved exit");
        assert_eq!(plan_events[0].data, serde_json::json!({"active": true}));
        let digest = crate::plan_mode::plan_digest(APPROVED_PLAN);
        assert_eq!(plan_events[1].data["active"], false);
        assert_eq!(plan_events[1].data["approved"]["text"], APPROVED_PLAN);
        assert_eq!(plan_events[1].data["approved"]["digest"], digest);

        let mut projections = crate::session::projection::ProjectionRegistry::clat();
        projections
            .fold_all(&events)
            .expect("fold DSH Plan Mode golden");
        let state = projections
            .state_snapshot("plan-mode")
            .expect("plan projection registered");
        assert_eq!(state["active"], false);
        assert_eq!(state["approved"]["text"], APPROVED_PLAN);
        assert_eq!(state["approved"]["digest"], digest);
        assert_eq!(state["approved"]["eventSeq"], plan_events[1].seq);
        let _ = std::fs::remove_dir_all(root);
    }

    /// B8-3（dev 侧，配对 gen-dsh-fixtures.mts 的 DSH 读腿）：用真实
    /// JsonlBackend + SessionRecorder 写一个流中取消的 CLAT 会话
    ///（ModelRequested → TextDelta×2 → 无 ModelResponded →
    /// finish(Aborted)——B2 的 interrupted 前缀定稿路径），产物拷到
    /// /tmp/clat-interop/clat-interrupted.jsonl.zstd。随后：
    ///   CLAT_CLAT_LOG=/tmp/clat-interop/clat-interrupted.jsonl.zstd \
    ///   (cd ../deepseek-harness && ./node_modules/.bin/tsx \
    ///     ../clat/tests/fixtures/dsh-session/gen-dsh-fixtures.mts)
    /// DSH 的 JsonlSessionPersistence.load 接受该日志并找到 interrupted。
    /// id/cwd 与脚本内 CLAT_ID 约定一致。
    #[test]
    #[ignore = "writes the cross-reader artifact; pair with gen-dsh-fixtures.mts"]
    fn interrupted_session_log_is_written_for_dsh_cross_reading() {
        use crate::model::ModelEvent;
        use crate::permission::{PermissionApprover, PermissionDecision, PermissionRequest};
        use crate::session::event::TurnEndReason;
        use crate::session::recorder::SessionRecorder;
        use crate::session::run_journal::SessionCoordinator;
        use crate::{EventSink, RunEvent};

        const CLAT_ID: &str = "018f2a64-9d3f-7cde-8123-9a4f2b6c0c01";
        let root = std::env::temp_dir().join(format!(
            "clat-interrupted-writer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let backend = std::sync::Arc::new(JsonlBackend::new(
            root.clone(),
            JsonlCompression::Zstd,
            false,
        ));
        let key = key_for(CLAT_ID);
        let header = SessionHeader {
            version: crate::session::compat::SESSION_FORMAT_VERSION,
            id: SessionId::new(CLAT_ID),
            created_at: 1_787_400_000_000,
            cwd: Some(FIXTURE_CWD.into()),
            parent_session: None,
            is_seeded: false,
            seed_length: None,
            origin: None,
            delegation_depth: 0,
            agent_preset: None,
        };
        let coordinator =
            SessionCoordinator::start(std::sync::Arc::clone(&backend), key, header.clone())
                .expect("start coordinator");
        let journal = coordinator.journal();
        struct AllowAll;
        impl PermissionApprover for AllowAll {
            fn decide(
                &self,
                _request: PermissionRequest,
                _cancel: &crate::model::CancelToken,
            ) -> PermissionDecision {
                PermissionDecision::Allow
            }
        }
        let (mut recorder, _approver) = SessionRecorder::with_approver(
            journal,
            std::sync::Arc::new(AllowAll),
            crate::session::recorder::RequestHeaderData {
                header: serde_json::json!({
                    "config": { "provider": "mock", "model": "mock" },
                    "system": "you are clat",
                    "tools": [],
                }),
                base_system: "you are clat".into(),
                dynamic_instructions: None,
                tool_registry: None,
                system_head: None,
            },
            "mock",
            "mock",
            1,
            Some("initial"),
        );
        crate::session::recorder::SessionRecorder::emit(
            &mut recorder,
            RunEvent::ModelRequested {
                turn: 1,
                provider: "mock".into(),
                model: "mock".into(),
            },
        );
        for text in ["partial ", "clat answer before "] {
            crate::session::recorder::SessionRecorder::emit(
                &mut recorder,
                RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::TextDelta { delta: text.into() },
                },
            );
        }
        // 取消：无 ModelResponded——finish(Aborted) 走 B2 的前缀定稿。
        let (journal_error, _published) = recorder.finish(TurnEndReason::Aborted {
            reason: crate::session::event::TurnEndCancelCause::User,
        });
        assert!(
            journal_error.is_none(),
            "journal flush clean: {journal_error:?}"
        );
        drop(coordinator);

        // 自证：CLAT 自己的 load 也能在产物里读到 interrupted 前缀。
        let events = backend
            .load(&key_for(CLAT_ID), true)
            .expect("self-load")
            .events;
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "assistant/message"
                    && event.data.get("interrupted") == Some(&serde_json::json!(true)))
        );

        // 交给 DSH 读腿的产物。
        let out_dir = std::path::Path::new("/tmp/clat-interop");
        let _ = std::fs::create_dir_all(out_dir);
        let log = log_path(
            &root,
            Some(FIXTURE_CWD),
            &SessionId::new(CLAT_ID),
            JsonlCompression::Zstd,
        );
        std::fs::copy(&log, out_dir.join("clat-interrupted.jsonl.zstd")).expect("copy artifact");
        let _ = std::fs::remove_dir_all(root);
    }

    /// SV 后端级读腿（bump 后收编）：原生 V3 金样经完整 discover →
    /// load → admission → 投影路径被 CLAT 接受——与裸字节读腿互补，
    /// 钉住 ensure_supported_generation 对 v3 的放行与世代发现。
    /// pre-fix 红：v3 被 ensure_supported_generation 拒为"未来代"。
    #[test]
    fn dsh_015_native_v3_backend_load_discovers_and_folds() {
        let (root, backend) = mount_fixture_generation(
            "v3-session-0.1.5.jsonl.zstd",
            "018f2a64-9d3f-7cde-8123-9a4f2b6c0e01",
            3,
        );
        let headers = backend.list_headers().expect("list headers");
        assert!(
            headers.iter().any(|header| header.id.as_str()
                == "018f2a64-9d3f-7cde-8123-9a4f2b6c0e01"
                && header.version == 3),
            "the v3 generation is discovered via the layout"
        );
        let events = load_golden(&backend, "018f2a64-9d3f-7cde-8123-9a4f2b6c0e01");
        assert_eq!(events.len(), 16);
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "system/message"),
            "the protected head rides the full load path"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// 布局推导钉住：fixture 的 cwd 经 CLAT 的 project_key 得到与
    /// mount 一致的目录（发现与装载用同一推导，跨工具目录语义一致）。
    #[test]
    fn dsh_fixture_layout_uses_the_shared_project_key() {
        let (root, backend) = mount_fixture("team-events-session.jsonl.zstd", TEAM_ID);
        let headers = backend.list_headers().expect("list");
        assert_eq!(headers.len(), 1);
        let expected_dir = root.join(project_key(FIXTURE_CWD));
        assert!(
            expected_dir.is_dir(),
            "project_key derivation matches the mounted layout"
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = log_file_name(JsonlCompression::Zstd);
    }

    // ---- SV（V3 对齐）：0.1.5-rc.2 产物的读腿（gen-v3-fixtures.mts）----
    //
    // pre-bump 阶段后端 load 仍拒 v3（ensure_supported_generation），
    // 这里走裸字节读路径（decode_zstd_log → scan_raw →
    // admit_events_for_version(3)）；批次 3 bump 后由后端级读腿收编。

    /// 裸字节读：解压 → scan → v3 准入。任何一环拒绝即红。
    fn load_v3_golden(file: &str) -> (SessionHeader, Vec<SessionEvent>) {
        let bytes = std::fs::read(fixture_dir().join(file)).expect("golden bytes");
        let (plain, torn) = crate::session::jsonl::decode_zstd_log(&bytes).expect("zstd frames");
        assert!(torn.is_none(), "golden has no torn tail");
        let scan = crate::session::jsonl::scan_raw(&plain).expect("scan");
        crate::session::admission::admit_events_for_version(&scan.events, 3).expect("v3 admission");
        crate::session::projection::ProjectionRegistry::clat()
            .fold_all(&scan.events)
            .expect("projection fold");
        (scan.header, scan.events)
    }

    /// 原生 V3（rc.2 真实 store append 期校验 + 真实 writer 物理编码）：
    /// 受保护头随首 step/start 落位；提示词变化的替换恰罩头并引用之；
    /// request/header 全程无 system；canonical 信封 startSeq/endSeq 由
    /// CLAT 解码回逻辑形状。pre-fix 红：v3 头 format-unsupported，
    /// system/message / ptc 名 RequiredUnknown。
    #[test]
    fn dsh_015_native_v3_fixture_folds_protected_head_and_canonical_envelopes() {
        let (header, events) = load_v3_golden("v3-session-0.1.5.jsonl.zstd");
        assert_eq!(header.version, 3);
        assert!(!header.is_seeded);

        // 受保护头：seq 2（turn/start, step/start 之后立即落位）。
        let head = events
            .iter()
            .find(|event| event.event_type == "system/message")
            .expect("protected head");
        assert_eq!(head.seq, 2);
        assert_eq!(
            head.surface_op,
            Some(crate::session::event::SurfaceOp::Append)
        );
        assert_eq!(head.source_event_seqs, None, "the first head cites nothing");
        assert_eq!(head.data["message"]["role"], "system");
        assert_eq!(
            head.data["message"]["source"]["plugin"],
            "@deepseek-ai/dsh-system-prompt"
        );

        // 头替换：replace 恰罩 head 且引用之（canonical 端点名解码回
        // 逻辑形状后按 seq 解读）。
        let replacement = events
            .iter()
            .filter(|event| event.event_type == "system/message")
            .nth(1)
            .expect("head replacement");
        assert_eq!(
            replacement.surface_op,
            Some(crate::session::event::SurfaceOp::Replace { start: 2, end: 2 })
        );
        assert_eq!(replacement.source_event_seqs, Some(vec![2]));

        // request/header 全程无 system（v3 native 准入的面）。
        for event in events
            .iter()
            .filter(|event| event.event_type == "request/header")
        {
            assert!(
                event.data["header"].get("system").is_none(),
                "v3 request/header must not carry system"
            );
        }

        // 表演面：user + assistant 重放还原；system 头跳过不重建。
        let replay = ReplayAdapter::fold(&events);
        let kinds: Vec<&str> = replay
            .iter()
            .map(|event| match event {
                ReplayEvent::UserMessage { .. } => "user",
                ReplayEvent::AssistantMessage { .. } => "assistant",
                ReplayEvent::TurnEnded { .. } => "turn/end",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "user",
                "assistant",
                "turn/end",
                "user",
                "assistant",
                "turn/end"
            ]
        );

        // surface 投影收编 system 节点（受保护头是节点），但模型上下文
        // 适配跳过它们（提示词走 request.instructions，不进条目列表）。
        let mut registry = crate::session::projection::ProjectionRegistry::clat();
        registry.fold_all(&events).expect("fold");
        let raw_nodes: Vec<u64> =
            registry.state_snapshot("surface").expect("surface state")["nodes"]
                .as_array()
                .expect("nodes")
                .iter()
                .map(|seq| seq.as_u64().expect("seq"))
                .collect();
        assert!(
            raw_nodes.contains(&10) && !raw_nodes.contains(&2),
            "the replacement splices the head node in place: {raw_nodes:?}"
        );
        let item_seqs: Vec<u64> = registry
            .surface_nodes()
            .expect("surface nodes")
            .into_iter()
            .map(|(seq, _)| seq)
            .collect();
        assert_eq!(
            item_seqs,
            vec![4, 5, 12, 13],
            "the model-facing item list skips the system nodes"
        );
    }

    /// 迁移 V3（上游 v2-to-v3 迁移器产物）：插入移位后的 seq 空间、
    /// 合成受保护头、替换引用、重映射后的 compaction 引用全部按 CLAT
    /// 语义解码。pre-fix 红：同上。
    #[test]
    fn dsh_015_migrated_v3_fixture_decodes_remapped_references() {
        let (header, events) = load_v3_golden("v3-migrated-0.1.5.jsonl.zstd");
        assert_eq!(header.version, 3);
        assert_eq!(header.id.as_str(), "018f2a64-9d3f-7cde-8123-9a4f2b6c0e02");

        // 源 14 事件 → 目标 21：+1 首头、两个替换头、（本源无其余插入）。
        assert_eq!(events.len(), 21);
        // 迁移语义钉住（上游 spec）：首 step/start 后插入的是**空**头
        // （content: []，受保护但非模型消息）；源首 request/header 的
        // 提示词构成一次变化 → 紧邻其前的替换头携带文本并引用空头。
        let system_seqs: Vec<u64> = events
            .iter()
            .filter(|event| event.event_type == "system/message")
            .map(|event| event.seq)
            .collect();
        assert_eq!(
            system_seqs,
            vec![2, 3, 9],
            "synthetic heads sit at the insertion points"
        );
        let head = &events[2];
        assert_eq!(
            head.data["message"]["content"],
            serde_json::json!([]),
            "the initial head is empty"
        );
        assert_eq!(head.source_event_seqs, None, "the first head cites nothing");
        let first_prompt = &events[3];
        assert_eq!(
            first_prompt.data["message"]["content"][0]["text"], "first prompt",
            "the replacement head carries the source prompt"
        );
        assert_eq!(first_prompt.source_event_seqs, Some(vec![2]));
        let second_prompt = &events[9];
        assert_eq!(
            second_prompt.data["message"]["content"][0]["text"],
            "second prompt"
        );

        // compaction 重映射：源 shadowedRange (3,4) 的目标位置。源 seq 3
        // (user) 在目标面（含插入）落位由迁移器决定——这里钉住 CLAT 能
        // 解码并折叠重映射后的引用（投影 fold 已在 helper 内完成）。
        let summary = events
            .iter()
            .find(|event| event.event_type == "compaction/summary")
            .expect("compaction summary");
        let start = summary.data["shadowedRange"]["start"]
            .as_u64()
            .expect("start");
        let end = summary.data["shadowedRange"]["end"].as_u64().expect("end");
        assert!(end > start, "remapped range stays ordered");
        assert_eq!(
            summary.data["shadowedSeqs"],
            serde_json::json!([start, end]),
            "shadowedSeqs tracks the remapped range"
        );

        // 表演面折叠：压缩摘要行 + 两条 user + 两条 assistant。
        let replay = ReplayAdapter::fold(&events);
        let users: Vec<&String> = replay
            .iter()
            .filter_map(|event| match event {
                ReplayEvent::UserMessage { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            users,
            vec!["first question", "[compacted earlier context]"],
            "the compacted summary replaces the earlier question in replay order"
        );
    }
}
