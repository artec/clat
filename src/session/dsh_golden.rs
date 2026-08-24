//! B8（F-2 闭环，2026-08-22）：DSH 钉靶 checkout（0.1.1-rc.2 =
//! `b150a551b8`）真实写路径产出的 golden fixture 读腿。与
//! `interop.rs`（/tmp 自跳过的原语级互证）不同，这里的 fixture
//! **提交进库**（`tests/fixtures/dsh-session/`），本模块随主测试套
//! 常跑、零 Node 依赖；再生脚本见同目录 `gen-dsh-fixtures.mts`
//!（dev 侧，含 DSH 读 CLAT 产物的反向腿）。
//!
//! 两条腿分别钉住：
//! - **interrupted 前缀定稿**：流中取消的会话，`assistant/message`
//!   携带 `interrupted: true`（DSH agent.ts:352-368 的取消分支），
//!   未派发的 tool calls 不出现；CLAT 的完整 load → 准入 → 重放
//!   路径识别前缀并还原部分文本。
//! - **team/* 已知类型**：4 个必需信封事件（无 ignorable）在真实
//!   zstd 帧字节中随会话落盘；CLAT 准入放行、重放跳过不重建、会话
//!   其余部分正常还原（CLAT 自产日志不含 team/* 的断言在 catalog
//!   测试中另行钉住）。

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

    fn fixture_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dsh-session")
    }

    /// 把 golden 日志按 CLAT 布局放进临时 root（project_key(cwd) /
    /// encode_segment(id) / log 文件名），返回 root 供清理。
    fn mount_fixture(file: &str, id: &str) -> (std::path::PathBuf, JsonlBackend) {
        let root = std::env::temp_dir().join(format!(
            "clat-dsh-golden-{id}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let backend = JsonlBackend::new(root.clone(), JsonlCompression::Zstd, false);
        let target = log_path(
            &root,
            Some(FIXTURE_CWD),
            &SessionId::new(id),
            JsonlCompression::Zstd,
        );
        std::fs::create_dir_all(target.parent().expect("parent")).expect("layout dir");
        std::fs::copy(fixture_dir().join(file), &target).expect("copy golden log");
        (root, backend)
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
            version: 0,
            id: SessionId::new(CLAT_ID),
            created_at: 1_787_400_000_000,
            cwd: Some(FIXTURE_CWD.into()),
            parent_session: None,
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
}
