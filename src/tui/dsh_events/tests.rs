use super::*;
use crate::PermissionMode;
use serde_json::json;

/// 测试用 dsh 态 App：不触网（通道握在测试手里），WS 标记已开。
fn dsh_app() -> (App, Receiver<DshTask>) {
    let mut app = App::open_dsh(3080).expect("dsh app opens");
    app.test_freeze_tick = true;
    app.clipboard_writer = discard_clipboard_sink;
    let (task_tx, task_rx) = mpsc::channel::<DshTask>();
    let (events_tx, _events_rx) = backend::event_channel();
    let mut state = DshState::new(3080, describe_fixture(), task_tx, events_tx);
    state.test_mark_ws_open();
    state.current_session = Some("session-test".into());
    app.dsh = Some(state);
    app.dsh_connect = None;
    app.dsh_connect_rx = None;
    // 记忆文件改道临时路径——测试不得触碰真实 ~/.clat。
    app.dsh_memory_path = std::env::temp_dir().join(format!(
        "clat-dsh-memo-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    (app, task_rx)
}

/// FIX-5/CA-08：丢弃 sink——dsh 测试不写真实终端。
fn discard_clipboard_sink(_: &[u8]) -> bool {
    true
}

fn describe_fixture() -> Value {
    serde_json::json!({
        "version": "0.1.1-rc.2", "cwd": "/home/dev/dsh-project",
        "provider": "deepseek", "model": "test-model",
        "attachedSessions": 1, "home": "/home/dev"
    })
}

/// 绑一个临时端口再立刻释放 → 连接必拒（不写死端口，抗环境）。
fn scratch_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("scratch port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}

fn event(app: &mut App, event: DshEvent) {
    app.handle_dsh_event(event);
}

/// 以 App 当前代际注入帧（代际不匹配的腿在测试里显式手写）。
fn frame(app: &mut App, frame: DshFrame) {
    let generation = app.dsh.as_ref().map(|dsh| dsh.generation).unwrap_or(0);
    event(app, DshEvent::Frame { generation, frame });
}

fn session_event(kind: &str, seq: u64, data: Value) -> SessionEvent {
    SessionEvent::new(kind, seq, 1_700_000_000_000 + seq as i64 * 1000, data)
}

/// 落盘级（surface）事件：转录装配只认带 items 的事件。
fn surface(kind: &str, seq: u64, data: Value) -> SessionEvent {
    session_event(kind, seq, data).append(Vec::new())
}

/// 会话视图的纯文本投影（判内容用）。
fn rendered_text(app: &mut App) -> String {
    use crate::tui::conversation::ToolCardVisibility;
    app.conversation.ensure_rendered(80);
    let total = app.conversation.total_lines(ToolCardVisibility::Collapsed);
    (0..total)
        .map(|row| {
            app.conversation
                .row_plain_text(row, 80, ToolCardVisibility::Collapsed)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// §0-2（断线自动重连拍板）：LinkDown → 断线态 + 重连排程；到点
/// poll 发 Reconnect 任务且单飞；任务在途不重发（判别：删掉排程
/// 或单飞守卫即红）。
#[test]
fn link_down_schedules_and_poll_fires_a_single_reconnect() {
    let (mut app, task_rx) = dsh_app();
    event(
        &mut app,
        DshEvent::LinkDown {
            generation: 0,
            reason: "connection closed".into(),
        },
    );
    let dsh = app.dsh.as_ref().expect("state");
    assert!(!dsh.connected);
    assert!(dsh.reconnect_deadline().is_some(), "a retry is scheduled");
    assert!(
        dsh.banner
            .as_deref()
            .is_some_and(|b| b.contains("reconnecting"))
    );
    // 未到点：不发包。
    app.poll_dsh();
    assert!(task_rx.try_recv().is_err(), "not due yet — no task");
    // 到点（测试钩子把排程拨到过去）→ 恰一个 Reconnect。
    app.dsh.as_mut().unwrap().test_due_reconnect_now();
    app.poll_dsh();
    assert!(matches!(task_rx.try_recv(), Ok(DshTask::Reconnect)));
    assert!(
        task_rx.try_recv().is_err(),
        "single flight — no second task"
    );
    assert!(app.dsh.as_ref().unwrap().reconnecting);
}

/// §0-5 + INV-U7（usage 口径与诚实呈现）：DSH 三计数不相交 →
/// Cache = cacheRead/(input+cacheRead)；contextWindow 缺席整段
/// 隐藏、出席显示 input+cacheRead 分子（判别：用本地口径公式即红）。
#[test]
fn usage_projection_uses_the_dsh_disjoint_counts() {
    let (mut app, _task_rx) = dsh_app();
    frame(
        &mut app,
        DshFrame::SessionEvent {
            session_id: "session-test".into(),
            event: session_event(
                "assistant/message",
                1,
                json!({"usage": {"inputTokens": 300, "cacheReadTokens": 100}}),
            ),
        },
    );
    assert_eq!(app.dsh_status_segments(), vec!["Cache: 25.00%".to_owned()]);
    // contextWindow 到场 → Context 段出现（分子 = 300+100）。
    frame(
        &mut app,
        DshFrame::SessionEvent {
            session_id: "session-test".into(),
            event: session_event(
                "request/context",
                2,
                json!({"provider": "deepseek", "model": "test-model", "contextWindow": 65536}),
            ),
        },
    );
    let segments = app.dsh_status_segments();
    assert_eq!(segments.len(), 2, "{segments:?}");
    assert!(segments[1].starts_with("Context: 400/"), "{}", segments[1]);
}

/// §2.6 步骤 ②（create 收养式判别）：收养任务在 App 侧携带目标
/// 会话自己的 workspace_path 作为 cwd。
#[test]
fn adopt_session_sends_create_with_the_target_workspace_cwd() {
    let (mut app, task_rx) = dsh_app();
    app.dsh_adopt_session(crate::tui::session_picker::DshResumeRow {
        session_id: "session-target".into(),
        workspace_title: "beta".into(),
        workspace_path: "/w/beta".into(),
        title: None,
        activity_ms: 0,
    });
    match task_rx.try_recv() {
        Ok(DshTask::Create { session_id, cwd }) => {
            assert_eq!(session_id.as_deref(), Some("session-target"));
            assert_eq!(cwd.as_deref(), Some("/w/beta"));
        }
        other => panic!("adoption sends Create: {other:?}"),
    }
}

/// §2.6 步骤 ③-⑥（Created 回执驱动切换）：转录重置、preset 投影
/// 清零、标题清空（判别：任一步缺席即红；作用域随 2026-08-23
/// 返工撤销，无⑦）。
#[test]
fn created_reply_switches_and_resets_session_state() {
    let (mut app, task_rx) = dsh_app();
    // 预置旧会话残留：内容 + preset + 标题。
    frame(
        &mut app,
        DshFrame::SessionEvent {
            session_id: "session-test".into(),
            event: session_event("sandbox/mode", 1, json!({"mode": "read-only"})),
        },
    );
    app.session_title = Some("old title".into());
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Created("session-new".into())),
    );
    let dsh = app.dsh.as_ref().expect("state");
    assert_eq!(dsh.current_session(), Some("session-new"));
    assert_eq!(dsh.session_tail, "session-new");
    assert_eq!(dsh.preset, None, "preset re-projects from the new session");
    assert_eq!(app.session_title, None);
    assert!(app.conversation.is_empty());
    assert!(matches!(
        task_rx.try_recv(),
        Ok(DshTask::History { session }) if session == "session-new"
    ));
}

/// SD-T2: the initial DSH page stays bounded, scrolling near the loaded
/// top sends exactly one cursor request, and prepending preserves the
/// bottom-relative reading anchor until `hasMore` is exhausted.
#[test]
fn dsh_history_pages_load_on_scroll_with_single_flight_and_stable_anchor() {
    let (mut app, task_rx) = dsh_app();
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Created("session-window".into())),
    );
    assert!(matches!(
        task_rx.try_recv(),
        Ok(DshTask::History { session }) if session == "session-window"
    ));
    assert!(matches!(
        task_rx.try_recv(),
        Ok(DshTask::ModelNames { session }) if session == "session-window"
    ));
    event(
        &mut app,
        DshEvent::Reply(TaskReply::History {
            session: "session-window".into(),
            events: vec![surface(
                "user/message",
                10,
                json!({"content": [{"type": "text", "text": "newest page"}]}),
            )],
            first_seq: Some(10),
            has_more: true,
        }),
    );
    app.conversation_area = Rect::new(0, 0, 80, 24);
    app.conversation.ensure_rendered(77);
    app.conversation_rows = app.conversation.total_lines(app.card_visibility);
    app.conversation_scroll_from_bottom = 3;
    assert!(app.conversation.toggle_reasoning());
    let lines_before = app.conversation_rows;

    app.maybe_load_older_conversation();
    app.maybe_load_older_conversation();
    assert!(app.conversation_history_loading);
    assert!(matches!(
        task_rx.try_recv(),
        Ok(DshTask::OlderHistory { session, before_seq })
            if session == "session-window" && before_seq == 10
    ));
    assert!(
        task_rx.try_recv().is_err(),
        "an in-flight page suppresses duplicate scroll requests"
    );

    event(
        &mut app,
        DshEvent::Reply(TaskReply::OlderHistory {
            session: "session-window".into(),
            requested_before_seq: 10,
            events: vec![
                surface(
                    "user/message",
                    0,
                    json!({"content": [{"type": "text", "text": "oldest page"}]}),
                ),
                surface(
                    "assistant/message",
                    1,
                    json!({
                        "turn": 1,
                        "step": 1,
                        "message": {
                            "role": "assistant",
                            "content": [
                                {"type": "reasoning", "text": "first thought\nsecond thought"},
                                {"type": "text", "text": "oldest answer"}
                            ],
                            "source": {"provider": "deepseek", "model": "test-model"}
                        }
                    }),
                ),
            ],
            first_seq: Some(0),
            has_more: false,
        }),
    );
    let lines_after = app.conversation_rows;
    assert_eq!(
        app.conversation_scroll_from_bottom,
        3 + lines_after.saturating_sub(lines_before),
        "prepend line count compensates the bottom anchor"
    );
    assert!(!app.conversation_has_more);
    assert!(!app.conversation_history_loading);
    let text = rendered_text(&mut app);
    assert!(text.contains("first thought"), "{text}");
    assert!(text.contains("second thought"), "{text}");
    assert!(
        text.find("oldest page") < text.find("newest page"),
        "older facts prepend in chronological order: {text}"
    );
}

/// INV-U3（Ctrl+O 同一 ConversationModel 天然继承）：dsh 态三态循环。
#[test]
fn ctrl_o_cycles_card_visibility_in_dsh_mode() {
    use crate::tui::conversation::ToolCardVisibility;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let (mut app, _task_rx) = dsh_app();
    let before = app.card_visibility;
    app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('o'),
        KeyModifiers::CONTROL,
    ))));
    assert_ne!(app.card_visibility, before);
    app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('o'),
        KeyModifiers::CONTROL,
    ))));
    assert_eq!(
        app.card_visibility,
        ToolCardVisibility::default().next().next()
    );
}

/// §3（steering 计数校正）：session/queue 帧按 placement=="steering"
/// 的条目数修剪本地回显（判别：删掉修剪即红）。
#[test]
fn queue_frame_trims_the_local_steering_echo() {
    let (mut app, _task_rx) = dsh_app();
    app.conversation.push_pending_steering("first".into());
    app.conversation.push_pending_steering("second".into());
    assert_eq!(app.conversation.pending_steering_count(), 2);
    frame(
        &mut app,
        DshFrame::Queue {
            session_id: "session-test".into(),
            items: json!([
                {"id": "m1", "placement": "steering", "message": {}},
                {"id": "m2", "placement": "queued", "message": {}}
            ]),
        },
    );
    assert_eq!(
        app.conversation.pending_steering_count(),
        1,
        "the host queue says one steering item remains"
    );
}

/// 退出清理（2026-08-23 拍板）：Drop 带走自己 spawn 的宿主；重连
/// respawn 时旧句柄被替换、旧宿主一并带走（判别：去掉任一 kill
/// 即红——sleep 30 会活过断言窗口）；旁观进程不受影响。unix 门控：
/// 进程名不可移植。
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn sleeper() -> command_group::GroupChild {
    use command_group::CommandGroup as _;
    std::process::Command::new("sleep")
        .arg("30")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .group_spawn()
        .expect("sleep spawns")
}

#[cfg(unix)]
#[test]
fn dropping_the_state_kills_the_host_we_spawned() {
    // 旁观进程：只杀自己持有的句柄，别人的进程不动。
    let mut witness = sleeper();
    assert!(alive(witness.id()));

    // 腿 1：Drop 带走自己 spawn 的宿主。
    let (mut app, _task_rx) = dsh_app();
    let host = sleeper();
    let host_pid = host.id();
    app.dsh
        .as_mut()
        .unwrap()
        .adopt_spawned_host(Some(crate::dsh::connect::OwnedDshHost::new(host)));
    assert!(alive(host_pid));
    drop(app);
    assert!(!alive(host_pid), "Drop must take the spawned host down");

    // 腿 2：重连替换——旧宿主被带走、新句柄在位（随后 Drop 一并
    // 带走）。
    let (mut app, _task_rx) = dsh_app();
    let old_host = sleeper();
    let old_pid = old_host.id();
    app.dsh
        .as_mut()
        .unwrap()
        .adopt_spawned_host(Some(crate::dsh::connect::OwnedDshHost::new(old_host)));
    let new_host = sleeper();
    let new_pid = new_host.id();
    app.dsh
        .as_mut()
        .unwrap()
        .adopt_spawned_host(Some(crate::dsh::connect::OwnedDshHost::new(new_host)));
    assert!(!alive(old_pid), "replacement kills the old spawned host");
    assert!(alive(new_pid));
    drop(app);
    assert!(!alive(new_pid));

    // 旁观者全程存活，测试收尾带走。
    let witness_pid = witness.id();
    assert!(alive(witness_pid), "unrelated processes are never touched");
    let _ = witness.kill();
    let _ = witness.wait();
}

/// FIX-3/CA-03（2026-08-24 审计，pre-fix 红）：自启宿主的清理是
/// **树级**的——忽视 TERM 的后代必须随 leader 一起消失。走真实
/// 生产路径：ensure_online（spawn + 就绪行 + probe 指纹）→ 收养 →
/// Drop。pre-fix：普通 spawn + leader-only kill/wait → 后代存活
/// → 红。
#[cfg(unix)]
#[test]
fn spawned_host_cleanup_takes_the_whole_tree() {
    use std::io::{Read as _, Write as _};

    // 一次性 describe 服务：ensure_online 的 probe 指纹闸门所需。
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let describe_port = server.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = server.accept() else {
            return;
        };
        // 读走请求（头到 \r\n\r\n + content-length body）。
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read(&mut byte).is_ok_and(|n| n > 0) {
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf).into_owned();
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.trim()
                            .eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().ok())?
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                if length > 0 {
                    let _ = stream.read_exact(&mut body);
                }
                // 信封 rpcId 必须回显（client 校验）。
                let rpc_id = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| value.get("rpcId").cloned())
                    .unwrap_or(serde_json::json!("rpc-tree-test"));
                let describe = serde_json::json!({
                    "version": "0.1.1-rc.2",
                    "cwd": "/Users/dev/project",
                    "attachedSessions": 1,
                    "home": "/Users/dev",
                });
                let response = serde_json::json!({
                    "type": "server-response",
                    "rpcId": rpc_id,
                    "result": {"ok": true, "value": describe},
                });
                let body = response.to_string();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
                break;
            }
        }
    });

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let pidfile = std::env::temp_dir().join(format!("clat-dsh-tree-{stamp}.pid"));
    let script = std::env::temp_dir().join(format!("clat-dsh-tree-{stamp}.sh"));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n(trap '' TERM; exec sleep 60) &\necho $! > \"{pidfile}\"\n\
             echo \"dsh web: http://127.0.0.1:{describe_port}\"\nsleep 60\n",
            pidfile = pidfile.display(),
            describe_port = describe_port,
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // preferred port：闲置 scratch（probe 失败 → 走 spawn 路径）。
    let scratch = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };
    let home = std::env::temp_dir();
    let online = crate::dsh::connect::ensure_online(scratch, script.to_str().unwrap(), Some(&home))
        .expect("fake dsh connects");
    let leader = online.child.as_ref().expect("we spawned it").id();

    // 等后代 pid 落盘（trap '' TERM + exec sleep：忽视 TERM 的后代）。
    let mut descendant = None;
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(&pidfile)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            descendant = Some(pid);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let descendant = descendant.expect("descendant pid recorded");
    assert!(alive(leader), "the leader is up");
    assert!(alive(descendant), "the TERM-ignoring descendant is up");

    // 收养 + Drop：树级清理必须带走整棵树。
    let (mut app, _task_rx) = dsh_app();
    app.dsh.as_mut().unwrap().adopt_spawned_host(online.child);
    drop(app);
    assert!(!alive(leader), "the leader must be gone");
    assert!(
        !alive(descendant),
        "the TERM-ignoring descendant must not outlive the group cleanup"
    );
    std::fs::remove_file(&script).ok();
    std::fs::remove_file(&pidfile).ok();
}

/// §2.4（preset 投影 → 输入框右上角档位词汇）：journal 值 → DSH
/// 产品标签；未知自定义值原样显示。
#[test]
fn preset_projection_maps_journal_values_to_web_labels() {
    assert_eq!(dsh_preset_label("read-only"), "Read Only");
    assert_eq!(dsh_preset_label("workspace-write"), "Workspace Write");
    assert_eq!(dsh_preset_label("danger-full-access"), "Full Access");
    assert_eq!(dsh_preset_label("custom-audit-mode"), "custom-audit-mode");
    // PermissionMode 与 journal 值互转（dsh /perm 的 Apply 通道）。
    assert_eq!(
        PermissionMode::from_journal_value("workspace-write"),
        Some(PermissionMode::ProjectWrite)
    );
    assert_eq!(
        PermissionMode::ProjectWrite.journal_value(),
        "workspace-write"
    );
}

/// 审计 P1-3 判别：重连失败回执必须复位单飞守卫并重新排程——
/// 删掉复位即红（第二次 poll 不发包、deadline 永久 None）。
#[test]
fn reconnect_failure_re_arms_the_next_attempt() {
    let (mut app, task_rx) = dsh_app();
    event(
        &mut app,
        DshEvent::LinkDown {
            generation: 0,
            reason: "connection closed".into(),
        },
    );
    app.dsh.as_mut().unwrap().test_due_reconnect_now();
    app.poll_dsh();
    assert!(matches!(task_rx.try_recv(), Ok(DshTask::Reconnect)));

    // 重连失败回执：复位 + 重新排程。
    event(
        &mut app,
        DshEvent::Reply(TaskReply::ReconnectFailed("host is down".into())),
    );
    {
        let dsh = app.dsh.as_ref().unwrap();
        assert!(!dsh.reconnecting, "the single-flight guard must reset");
        assert!(
            dsh.reconnect_deadline().is_some(),
            "a retry must be scheduled again"
        );
    }

    // 再次到点 → 第二次重连尝试照发（一次失败不得永久停摆）。
    app.dsh.as_mut().unwrap().test_due_reconnect_now();
    app.poll_dsh();
    assert!(
        matches!(task_rx.try_recv(), Ok(DshTask::Reconnect)),
        "the second attempt must fire"
    );
}

/// 审计 P1-1 判别：旧会话有实质内容时切换——视图即刻清空、
/// live 帧暂存、整页回执后只剩目标历史（旧内容绝不串线；删掉
/// 视图清空或暂存态即红）。
#[test]
fn switching_sessions_replaces_the_view_and_replays_staged_frames() {
    let (mut app, task_rx) = dsh_app();
    frame(
        &mut app,
        DshFrame::SessionEvent {
            session_id: "session-test".into(),
            event: surface(
                "user/message",
                1,
                json!({"content": [{"type": "text", "text": "old session text"}]}),
            ),
        },
    );
    let text = rendered_text(&mut app);
    assert!(text.contains("old session text"), "{text}");

    // 收养新会话：视图即刻接管为空白，History 任务发出。
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Created("session-target".into())),
    );
    assert!(
        app.conversation.is_empty(),
        "the old session's content must leave the view at the switch"
    );
    assert!(matches!(
        task_rx.try_recv(),
        Ok(DshTask::History { session }) if session == "session-target"
    ));

    // 竞态腿：整页回执未到，目标会话的 live 帧先到 → 暂存不显示。
    frame(
        &mut app,
        DshFrame::SessionEvent {
            session_id: "session-target".into(),
            event: surface(
                "user/message",
                8,
                json!({"content": [{"type": "text", "text": "live while loading"}]}),
            ),
        },
    );
    assert!(
        app.conversation.is_empty(),
        "live frames stay staged until the history page lands"
    );

    // 整页回执：重建 + 暂存补放，旧内容绝不回来。
    event(
        &mut app,
        DshEvent::Reply(TaskReply::History {
            session: "session-target".into(),
            first_seq: Some(5),
            has_more: false,
            events: vec![surface(
                "user/message",
                5,
                json!({"content": [{"type": "text", "text": "target history"}]}),
            )],
        }),
    );
    let text = rendered_text(&mut app);
    assert!(text.contains("target history"), "{text}");
    assert!(
        text.contains("live while loading"),
        "the staged live frame is replayed after the page: {text}"
    );
    assert!(
        !text.contains("old session text"),
        "cross-session bleed must not survive the switch: {text}"
    );
}

/// 审计 P1-2 判别：后台会话的审批/问答不得弹进当前视图，别处
/// 的落定不得关掉我们的弹框（删掉任一 session 关联即红）。
#[test]
fn approvals_and_questions_from_other_sessions_stay_out_of_the_view() {
    let (mut app, _task_rx) = dsh_app();
    // 后台会话的审批：不弹框（用户绝不能代答一个自己没在看的会话）。
    frame(
        &mut app,
        DshFrame::ApprovalRequested {
            rpc_id: "rpc-bg".into(),
            session_id: "session-other".into(),
            approval_id: "apr-1".into(),
            tool_name: "bash".into(),
            call_id: None,
            reason: Some("runs a command".into()),
        },
    );
    assert!(
        app.pending_permission.is_none(),
        "a background session's approval must not open the dialog"
    );
    // 当前会话的审批：弹框。
    frame(
        &mut app,
        DshFrame::ApprovalRequested {
            rpc_id: "rpc-mine".into(),
            session_id: "session-test".into(),
            approval_id: "apr-2".into(),
            tool_name: "bash".into(),
            call_id: None,
            reason: None,
        },
    );
    assert!(app.pending_permission.is_some());
    // 后台会话的落定：不动当前弹框。
    frame(
        &mut app,
        DshFrame::ApprovalResolved {
            session_id: "session-other".into(),
            approval_id: "apr-1".into(),
            outcome: "allowed-once".into(),
        },
    );
    assert!(
        app.pending_permission.is_some(),
        "another session's resolution must not close our dialog"
    );
    // 同会话但别的审批的落定：也不动（按 approvalId 精确关联）。
    frame(
        &mut app,
        DshFrame::ApprovalResolved {
            session_id: "session-test".into(),
            approval_id: "apr-9".into(),
            outcome: "rejected".into(),
        },
    );
    assert!(app.pending_permission.is_some());
    // 我们的落定：关框。
    frame(
        &mut app,
        DshFrame::ApprovalResolved {
            session_id: "session-test".into(),
            approval_id: "apr-2".into(),
            outcome: "allowed-once".into(),
        },
    );
    assert!(app.pending_permission.is_none());

    // 问答同规则：Requested 按 session 过滤、Resolved 按 session+rpc。
    frame(
        &mut app,
        DshFrame::QuestionRequested {
            rpc_id: "q-bg".into(),
            session_id: "session-other".into(),
            questions: json!([{"id": "q1", "question": "background"}]),
        },
    );
    assert!(app.pending_ask_user.is_none());
    frame(
        &mut app,
        DshFrame::QuestionRequested {
            rpc_id: "q-mine".into(),
            session_id: "session-test".into(),
            questions: json!([{"id": "q1", "question": "mine"}]),
        },
    );
    assert!(app.pending_ask_user.is_some());
    frame(
        &mut app,
        DshFrame::QuestionResolved {
            session_id: "session-other".into(),
            rpc_id: "q-bg".into(),
            outcome: json!({}),
        },
    );
    assert!(
        app.pending_ask_user.is_some(),
        "another session's resolution must not close our question"
    );
    frame(
        &mut app,
        DshFrame::QuestionResolved {
            session_id: "session-test".into(),
            rpc_id: "q-mine".into(),
            outcome: json!({}),
        },
    );
    assert!(app.pending_ask_user.is_none());
}

/// 审计 P1-4 判别：瞬时 WS 故障重连时探测命中仍健在的自家宿主
///（child=None）——句柄必须保留（删掉甄别即红：宿主被误杀）；
/// Drop 仍然带走它。unix 门控：进程名不可移植。
#[cfg(unix)]
#[test]
fn a_probe_hit_reconnect_never_kills_the_host_we_own() {
    let (mut app, _task_rx) = dsh_app();
    let host = sleeper();
    let pid = host.id();
    app.dsh
        .as_mut()
        .unwrap()
        .adopt_spawned_host(Some(crate::dsh::connect::OwnedDshHost::new(host)));
    assert!(alive(pid));
    // 探测直连的重连成功（child=None，downlink 开在死端口上失败
    // 无妨——断言只关心宿主生死）。
    event(
        &mut app,
        DshEvent::Reconnected {
            port: scratch_port(),
            describe: describe_fixture(),
            era: crate::dsh::client::DshEra::Legacy,
            cookie: None,
            child: None,
        },
    );
    assert!(
        alive(pid),
        "a probe hit must never kill the host we spawned"
    );
    drop(app);
    assert!(!alive(pid), "Drop still owns the host for exit cleanup");
}

/// 审计 P2-2 判别：旧代际的迟到帧/断线一律作废——迟到的旧
/// LinkDown 不得把健康的新连接再标成断线（删掉代际过滤即红）。
#[test]
fn stale_generation_frames_and_link_downs_are_ignored() {
    let (mut app, _task_rx) = dsh_app();
    app.dsh.as_mut().unwrap().test_simulate_stream_generation();
    let current = app.dsh.as_ref().unwrap().generation;
    assert_eq!(current, 1);

    // 旧代际的迟到帧：作废（重复流不得再进入视图）。
    event(
        &mut app,
        DshEvent::Frame {
            generation: current - 1,
            frame: DshFrame::SessionEvent {
                session_id: "session-test".into(),
                event: surface(
                    "user/message",
                    1,
                    json!({"content": [{"type": "text", "text": "from the stale stream"}]}),
                ),
            },
        },
    );
    assert!(app.conversation.is_empty(), "stale frames must be dropped");

    // 旧代际的迟到断线：不得把健康的新连接标成断线。
    event(
        &mut app,
        DshEvent::LinkDown {
            generation: current - 1,
            reason: "late death of an old pump".into(),
        },
    );
    assert!(
        app.dsh.as_ref().unwrap().connected,
        "a stale LinkDown must not disconnect the live generation"
    );

    // 当前代际照常工作。
    event(
        &mut app,
        DshEvent::LinkDown {
            generation: current,
            reason: "real death".into(),
        },
    );
    assert!(!app.dsh.as_ref().unwrap().connected);
}

/// 读到 WS 升级请求头完整（`\r\n\r\n`）——**单次 read 会与 TCP
/// 分片竞态**：Windows CI 实证（2026-09-08，a45c4c5 腿红）：握手
/// 头分片到达 → key 残缺 → accept 错 → mux::open 失败。与
/// dsh/tests.rs 的 ws 腿同款字节循环；本仓库假宿主的既定纪律
///（HTTP 面走 dsh::tests::read_http_request，WS 升级面走本件）。
fn read_upgrade_request(stream: &mut std::net::TcpStream) -> String {
    use std::io::Read as _;
    let mut buffer = Vec::new();
    let mut byte = [0u8; 1];
    while stream.read(&mut byte).is_ok_and(|n| n == 1) {
        buffer.push(byte[0]);
        if buffer.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// 审计 P2-2 场景腿：mux 握手成功、host 握手被拒（「第一条新 WS
/// 开成、第二条失败」）——重试必须重新排程，代际已自增（孤儿 mux
/// 泵属当前代际数据仍可用，下一次重连自增后自然作废）。
#[test]
fn a_partial_stream_open_failure_arms_a_retry() {
    // 迷你服务端：mux 路径回合法握手后静默持连；其余路径回 400。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    // 每连接一线程：mux 连接握手后要被静默持有，不能阻塞后续
    //（host 路径的）accept——否则客户端的第二次握手永远等不到回音。
    fn serve(mut stream: std::net::TcpStream) {
        use std::io::{Read as _, Write};
        let request = read_upgrade_request(&mut stream);
        let key = request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("sec-websocket-key")
                    .then(|| value.trim().to_owned())
            })
            .unwrap_or_default();
        if request.starts_with("GET /api/events.mux") {
            let accept = crate::dsh::ws::expected_accept(&key);
            let response = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
            // 静默持连到 EOF。
            let mut sink = [0u8; 4096];
            let _ = stream.read(&mut sink);
        } else {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        }
    }
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            std::thread::spawn(move || serve(stream));
        }
    });

    let (mut app, _task_rx) = dsh_app();
    let before = app.dsh.as_ref().unwrap().generation;
    event(
        &mut app,
        DshEvent::Reconnected {
            port,
            describe: describe_fixture(),
            era: crate::dsh::client::DshEra::Legacy,
            cookie: None,
            child: None,
        },
    );
    let dsh = app.dsh.as_ref().unwrap();
    assert!(!dsh.connected);
    assert!(
        dsh.reconnect_deadline().is_some(),
        "a retry must be armed after a partial open failure"
    );
    assert!(
        dsh.banner
            .as_deref()
            .is_some_and(|b| b.contains("reconnect failed")),
        "banner: {:?}",
        dsh.banner
    );
    assert_eq!(
        dsh.generation,
        before + 1,
        "the generation advances even on partial failure"
    );
}

/// DV-10 收尾判别（负责人 dogfood 病历 2026-09-07）：Typert 初始
/// 恢复的任务序必须是 **AdoptMux → History**——旧编排沿 Legacy 的
/// "历史装载完成后开 WS"，History 先进 worker 而 controller 恒缺席
///（状态栏 "typert history needs the mux controller (AdoptMux
/// missing)"）。判别：撤 dsh_switch_session 的 Typert 先开下行
/// （era 分支）即红——首任务变 History。
#[test]
fn typert_restore_adopts_the_mux_before_requesting_history() {
    // mux 握手假宿主：回合法 101 后静默持连。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                use std::io::{Read as _, Write};
                let request = read_upgrade_request(&mut stream);
                let key = request
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.trim()
                            .eq_ignore_ascii_case("sec-websocket-key")
                            .then(|| value.trim().to_owned())
                    })
                    .unwrap_or_default();
                let accept = crate::dsh::ws::expected_accept(&key);
                let response = format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                     Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                );
                let _ = stream.write_all(response.as_bytes());
                // 静默持连到 EOF。
                let mut sink = [0u8; 4096];
                let _ = stream.read(&mut sink);
            });
        }
    });

    let (mut app, task_rx) = dsh_app();
    {
        let state = app.dsh.as_mut().expect("dsh state");
        state.port = port;
        state.era = crate::dsh::client::DshEra::Typert;
        state.cookie = Some("dsh-auth-test=v1.x".into());
        state.current_session = None;
        state.ws_open = false; // 初始启动形态：下行未开
    }
    app.dsh_switch_session("session-target".into());

    let first = task_rx
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("a task arrives");
    let banner = app
        .dsh
        .as_ref()
        .and_then(|dsh| dsh.banner.clone())
        .unwrap_or_default();
    match first {
        DshTask::AdoptMux { .. } => {
            // 顺序契约的正例（握手成功）：AdoptMux 严格先于 History。
            let second = task_rx
                .recv_timeout(std::time::Duration::from_secs(15))
                .expect("the history task follows the adoption");
            match second {
                DshTask::History { session } => {
                    assert_eq!(session, "session-target")
                }
                other => panic!("History must follow the adoption, got {other:?}"),
            }
        }
        DshTask::History { session } => {
            // mux 开失败（Windows runner 满载病历 ×3：分段/超时/调度
            // ——环境事实，非顺序回归）时 History 即首任务。失败路径
            // 必须**诚实**：banner 如实记录原因；顺序契约由握手成功
            // 的腿（本测试正例分支）强制执行。
            assert_eq!(session, "session-target");
            assert!(
                banner.contains("cannot open the typert mux"),
                "without AdoptMux the open failure must be honestly reported: \
                 banner={banner:?}"
            );
        }
        other => panic!("AdoptMux or History must lead, got {other:?}"),
    }
}

/// 审计 P2-1 UI 腿：启动链 Restore/History 失败（Failed 回执）时
/// 不得永久悬挂——流照常尝试打开，开不了则转入重连机制（删掉
/// fail-soft 开流即红：banner/排程缺席）。
#[test]
fn failed_startup_replies_never_hang_the_stream_open() {
    let mut app = App::open_dsh(3080).expect("dsh app opens");
    app.test_freeze_tick = true;
    app.clipboard_writer = discard_clipboard_sink;
    let (task_tx, _task_rx) = mpsc::channel::<DshTask>();
    let (events_tx, _events_rx) = backend::event_channel();
    // 不标记 ws_open：模拟启动链 Restore 失败、流尚未打开。
    let state = DshState::new(scratch_port(), describe_fixture(), task_tx, events_tx);
    app.dsh = Some(state);
    app.dsh_connect = None;
    app.dsh_connect_rx = None;
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Failed("session.list refused".into())),
    );
    let dsh = app.dsh.as_ref().unwrap();
    assert!(!dsh.ws_open);
    assert!(
        dsh.reconnect_deadline().is_some(),
        "fail-soft: the reconnect machinery takes over instead of hanging"
    );
    assert!(
        dsh.banner
            .as_deref()
            .is_some_and(|b| b.contains("reconnecting")),
        "the failure must be visible: {:?}",
        dsh.banner
    );
}

/// 审计 P2-4 判别：`request/context` 是会话级模型真来源——live 帧
/// 与历史折叠都刷新 model_label（删掉任一刷新即红：标签残留
/// 前一会话的模型）。
#[test]
fn request_context_refreshes_the_model_label_per_session() {
    let (mut app, _task_rx) = dsh_app();
    assert_eq!(
        app.dsh.as_ref().unwrap().model_label,
        "deepseek · test-model"
    );
    // live 腿。
    frame(
        &mut app,
        DshFrame::SessionEvent {
            session_id: "session-test".into(),
            event: session_event(
                "request/context",
                1,
                json!({"provider": "anthropic", "model": "claude-x", "contextWindow": 200000}),
            ),
        },
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().model_label,
        "anthropic · claude-x"
    );
    // 切换 + 历史折叠腿：目标会话的模型接管标签。
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Created("session-target".into())),
    );
    event(
        &mut app,
        DshEvent::Reply(TaskReply::History {
            session: "session-target".into(),
            first_seq: Some(1),
            has_more: false,
            events: vec![session_event(
                "request/context",
                1,
                json!({"provider": "openai", "model": "gpt-test", "contextWindow": 128000}),
            )],
        }),
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().model_label,
        "openai · gpt-test",
        "the target session's model must take over the header label"
    );
}

/// 负责人 dogfood（2026-08-23 第三轮）判别：标题栏标签用展示名
///（web 端同源 groups[].name / models[].name），不是裸 id；prime
/// 回执不开 picker、不 flash；索引未命中诚实回落裸 id（删掉 fold
/// 即红：标签停在 `deepseek · test-model`）。
#[test]
fn model_label_resolves_display_names_from_the_prime() {
    let (mut app, _task_rx) = dsh_app();
    // 索引未 prime：裸 id 回落（诚实，不编名字）。
    assert_eq!(
        app.dsh.as_ref().unwrap().model_label,
        "deepseek · test-model"
    );
    event(
        &mut app,
        DshEvent::Reply(TaskReply::ModelNames(json!({
            "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
                {"id": "test-model", "name": "Test Model Pro"}
            ]}],
            "current": {"provider": "deepseek", "model": "test-model"}
        }))),
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().model_label,
        "DeepSeek · Test Model Pro",
        "the header must show display names, not raw ids"
    );
    // prime 是装饰性获取：不开 picker。
    assert!(app.picker.is_none());
}

/// prime 只发一次（每连接）：首次切换 History + ModelNames 两任务，
/// 再切换只有 History（目录是宿主全局的）。
#[test]
fn the_name_catalog_primes_once_per_connection() {
    let (mut app, task_rx) = dsh_app();
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Created("session-target".into())),
    );
    assert!(matches!(
        task_rx.try_recv(),
        Ok(DshTask::History { session }) if session == "session-target"
    ));
    assert!(matches!(
        task_rx.try_recv(),
        Ok(DshTask::ModelNames { session }) if session == "session-target"
    ));
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Created("session-target-2".into())),
    );
    assert!(matches!(
        task_rx.try_recv(),
        Ok(DshTask::History { session }) if session == "session-target-2"
    ));
    assert!(
        task_rx.try_recv().is_err(),
        "the catalog primes once per connection"
    );
}

/// /model 应答同步解析名字并按 `current` 校正（会话权威选择——
/// 兼收无 request/context 的新会话），picker 照开。
#[test]
fn models_reply_resolves_names_and_corrects_via_current() {
    let (mut app, _task_rx) = dsh_app();
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Models(json!({
            "groups": [
                {"id": "deepseek", "name": "DeepSeek", "models": [
                    {"id": "test-model", "name": "Test Model Pro"}
                ]},
                {"id": "custom-ollama", "name": "Ollama (custom)", "models": [
                    {"id": "llama-local", "name": "Llama Local"}
                ]}
            ],
            "failures": [],
            "current": {"provider": "custom-ollama", "model": "llama-local"}
        }))),
    );
    assert!(
        app.picker.is_some(),
        "the /model reply still opens the picker"
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().model_label,
        "Ollama (custom) · Llama Local",
        "current corrects the label with display names"
    );
}

/// 档位接入（2026-08-23）判别：`request/header` 是选择与档位的
/// 权威重投影源（历史 fold + 实时同通路）——切换会话后档位由
/// 历史恢复，标题栏档位段显示 efforts 表解析的展示名；切换本身
/// 先清档位（残留即红）。
#[test]
fn request_header_reprojects_selection_and_effort() {
    let (mut app, _task_rx) = dsh_app();
    // efforts 目录 prime（名字 + 档位表 + 当前档位 low）。
    event(
        &mut app,
        DshEvent::Reply(TaskReply::ModelNames(json!({
            "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
                {"id": "test-model", "name": "Test Model Pro",
                 "reasoning": {"efforts": [
                    {"id": "off", "name": "Off"},
                    {"id": "low", "name": "Low"},
                    {"id": "high", "name": "High"}
                 ]}}
            ]}],
            "current": {"provider": "deepseek", "model": "test-model",
                        "reasoningEffort": "low"}
        }))),
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().effort_display().as_deref(),
        Some("Low"),
        "current.reasoningEffort seeds the header effort segment"
    );
    // 切换：档位先清零。
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Created("session-target".into())),
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().effort_display(),
        None,
        "the effort resets with the session switch"
    );
    // 历史折叠腿：request/header 恢复选择与档位。
    event(
        &mut app,
        DshEvent::Reply(TaskReply::History {
            session: "session-target".into(),
            first_seq: Some(1),
            has_more: false,
            events: vec![session_event(
                "request/header",
                1,
                json!({"header": {"config": {
                    "provider": "deepseek", "model": "test-model",
                    "reasoningEffort": "high"
                }}, "reason": "change"}),
            )],
        }),
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().effort_display().as_deref(),
        Some("High"),
        "history request/header re-projects the effort"
    );
    // 实时腿：后续 request/header 到场照收。
    frame(
        &mut app,
        DshFrame::SessionEvent {
            session_id: "session-target".into(),
            event: session_event(
                "request/header",
                2,
                json!({"header": {"config": {
                    "provider": "deepseek", "model": "test-model"
                }}, "reason": "change"}),
            ),
        },
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().effort_display(),
        None,
        "a request header without reasoningEffort clears the segment"
    );
}

/// 档位接入判别：selectModel 落定回执携带宿主解析后的档位（可能
/// 与请求值不同——以回执为准）；主视图 Shift+Tab 从当前档位循环
/// 发出 Select（当前 low → 下一档 high）。
#[test]
fn main_view_shift_tab_cycles_the_current_effort() {
    let (mut app, task_rx) = dsh_app();
    event(
        &mut app,
        DshEvent::Reply(TaskReply::ModelNames(json!({
            "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
                {"id": "test-model", "name": "Test Model Pro",
                 "reasoning": {"efforts": [
                    {"id": "off", "name": "Off"},
                    {"id": "low", "name": "Low"},
                    {"id": "high", "name": "High"}
                 ]}}
            ]}],
            "current": {"provider": "deepseek", "model": "test-model",
                        "reasoningEffort": "low"}
        }))),
    );
    // 清空 prime 期间积压的任务通道。
    while task_rx.try_recv().is_ok() {}
    app.handle_ui_event(UiEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
    )));
    match task_rx.try_recv() {
        Ok(DshTask::Select {
            provider,
            model,
            effort,
            ..
        }) => {
            assert_eq!(provider, "deepseek");
            assert_eq!(model, "test-model");
            assert_eq!(effort.as_deref(), Some("high"), "low cycles to high");
        }
        other => panic!("Shift+Tab cycles the current model's effort: {other:?}"),
    }
    // 落定回执（宿主解析后的权威值）刷新档位段。
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Selected {
            provider: "deepseek".into(),
            model: "test-model".into(),
            effort: Some("max".into()),
        }),
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().effort_display().as_deref(),
        Some("max"),
        "the resolved reply wins even when it differs from the request"
    );
}

/// MM-3 失败恢复判别：dsh 不支持附件时，文本与结构化草稿都保留，
/// 且不得只把文本发给宿主造成一次不可逆的半提交。
#[test]
fn dsh_submit_with_attachments_preserves_the_complete_draft() {
    let (mut app, task_rx) = dsh_app();
    app.attachments
        .add_unchecked_for_test(std::path::PathBuf::from("/tmp/evidence.png"));
    app.input.insert_str("look at this");
    app.submit_input();
    assert!(
        app.status.contains("attachments are not supported"),
        "the warning must actually fire, status: {}",
        app.status
    );
    assert_eq!(app.input.text(), "look at this");
    assert_eq!(app.attachments.len(), 1);
    assert!(task_rx.try_recv().is_err(), "no partial prompt is sent");
}

/// B-4（2026-08-24 负责人对齐：clat dsh 是宿主的终端客户端）判别：
/// 恢复带回会话自己的 workspace——状态栏/标题栏第二行显示会话
/// 项目目录，不再是 describe.cwd（宿主进程目录；被我们 spawn 时
/// 即本地运行目录）。缺席（未记录）诚实回落 describe.cwd。
#[test]
fn restored_session_carries_its_workspace_display() {
    let (mut app, task_rx) = dsh_app();
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Restored {
            session: Some("session-ws".into()),
            cwd: Some("/w/target".into()),
        }),
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().current_session(),
        Some("session-ws")
    );
    assert_eq!(app.dsh.as_ref().unwrap().cwd(), "/w/target");
    assert_eq!(
        app.default_status, "/w/target",
        "the persistent status line follows the session workspace"
    );
    // 切换会发出 History 任务（既有行为不变）。
    assert!(matches!(
        task_rx.try_recv(),
        Ok(DshTask::History { session }) if session == "session-ws"
    ));
    // FIX-4/CA-04（2026-08-24 审计，pre-fix 红）：缺席腿改为**同一
    // App** 序列——旧 workspace 必须被 None 覆盖（清除 → 回落
    // describe.cwd），否则旧值遮住回落（pre-fix：残留 /w/target →
    // 红），且 /new 不得在错误目录建会话。
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Restored {
            session: Some("session-bare".into()),
            cwd: None,
        }),
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().cwd(),
        "/home/dev/dsh-project",
        "absent cwd must clear the stale workspace and fall back to describe.cwd"
    );
    while task_rx.try_recv().is_ok() {}
    app.submit_dsh("/new".into());
    match task_rx.try_recv() {
        Ok(DshTask::Create { session_id, cwd }) => {
            assert_eq!(session_id, None);
            assert_ne!(
                cwd.as_deref(),
                Some("/w/target"),
                "/new must not inherit the previous session's workspace"
            );
        }
        other => panic!("cwd-less restore then /new: {other:?}"),
    }
}

/// B-4 判别：/resume 收养的 workspace 经 pending_adoption 按回执
/// id 匹配跟随；失败（session-conflict）与不匹配回执都不残留。
#[test]
fn adoption_follows_the_workspace_and_failures_do_not_leak() {
    let (mut app, _task_rx) = dsh_app();
    // 成功收养：workspace 跟随目标会话。
    app.dsh_adopt_session(crate::tui::session_picker::DshResumeRow {
        session_id: "session-x".into(),
        workspace_title: "x".into(),
        workspace_path: "/w/x".into(),
        title: None,
        activity_ms: 0,
    });
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Created("session-x".into())),
    );
    assert_eq!(app.dsh.as_ref().unwrap().cwd(), "/w/x");

    // 失败收养（session-conflict）：停留原会话，workspace 不动。
    app.dsh_adopt_session(crate::tui::session_picker::DshResumeRow {
        session_id: "session-y".into(),
        workspace_title: "y".into(),
        workspace_path: "/w/y".into(),
        title: None,
        activity_ms: 0,
    });
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Failed("session-conflict".into())),
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().cwd(),
        "/w/x",
        "a failed adoption must not move the workspace"
    );

    // 不匹配回执（/new 的新 id 等）：pending 清空、不跟随。
    app.dsh_adopt_session(crate::tui::session_picker::DshResumeRow {
        session_id: "session-z".into(),
        workspace_title: "z".into(),
        workspace_path: "/w/z".into(),
        title: None,
        activity_ms: 0,
    });
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Created("session-unrelated".into())),
    );
    assert_eq!(
        app.dsh.as_ref().unwrap().cwd(),
        "/w/x",
        "a non-matching Created reply must not adopt the pending workspace"
    );
}

/// B-4 功能腿：/new 继承**当前会话的 workspace**（此前继承
/// describe.cwd——宿主进程目录，恢复跨工作区会话后会在错误的
/// 目录创建新会话）。
#[test]
fn new_session_inherits_the_current_workspace() {
    let (mut app, task_rx) = dsh_app();
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Restored {
            session: Some("session-ws".into()),
            cwd: Some("/w/target".into()),
        }),
    );
    while task_rx.try_recv().is_ok() {}
    app.submit_dsh("/new".into());
    match task_rx.try_recv() {
        Ok(DshTask::Create { session_id, cwd }) => {
            assert_eq!(session_id, None);
            assert_eq!(cwd.as_deref(), Some("/w/target"));
        }
        other => panic!("/new creates in the current workspace: {other:?}"),
    }
}

/// B-5 铃判别（2026-08-24 负责人 dogfood：后台等待 turn 结束无铃）：
/// dsh turn 结束响铃（此前只有审批/问答弹框响）；aborted = 用户
/// 主动取消不响（同本地语义）。端到端 marker 腿——删掉 turn/end
/// 的 notify 即红（completed 的 marker 不出现）。
#[test]
fn dsh_turn_end_rings_the_bell_except_when_aborted() {
    let marker = std::env::temp_dir().join(format!(
        "clat-dsh-bell-{}.marker",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (mut app, _task_rx) = dsh_app();
    app.bell = BellMode::Command(format!("printf ok > {:?}", marker));
    // 后台（B-1 三态：失焦必响）。
    app.focused = Some(false);
    // 用户取消：不响。
    frame(
        &mut app,
        DshFrame::SessionEvent {
            session_id: "session-test".into(),
            event: session_event(
                "turn/end",
                1,
                json!({"turn": 1, "reason": {"kind": "aborted"}}),
            ),
        },
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !marker.exists(),
        "an aborted (user-cancelled) turn must not ring"
    );
    // 自然结束：响。
    frame(
        &mut app,
        DshFrame::SessionEvent {
            session_id: "session-test".into(),
            event: session_event(
                "turn/end",
                2,
                json!({"turn": 2, "reason": {"kind": "completed"}}),
            ),
        },
    );
    let mut appeared = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if marker.exists() {
            appeared = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(appeared, "a completed turn rings while unfocused");
    let _ = std::fs::remove_file(&marker);
}

/// 拍板 A（2026-08-24 客户端自记）判别：切换即写入「最后打开会话」
/// 记忆（restore/收养//new 全经 dsh_switch_session，单点写入；最后一
/// 次获胜）。删掉写入即红（文件缺席）。
#[test]
fn switching_sessions_remembers_the_last_open_session() {
    let (mut app, _task_rx) = dsh_app();
    let memo = app.dsh_memory_path.clone();
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Restored {
            session: Some("session-aite".into()),
            cwd: Some("/w/aite".into()),
        }),
    );
    assert_eq!(
        crate::dsh::last_session::read_last_session_at(&memo),
        Some("session-aite".to_owned()),
        "a switch writes the client-side memory"
    );
    // 再切换 → 覆写（最后一次获胜）。
    event(
        &mut app,
        DshEvent::Reply(TaskReply::Created("session-clat".into())),
    );
    assert_eq!(
        crate::dsh::last_session::read_last_session_at(&memo),
        Some("session-clat".to_owned())
    );
    let _ = std::fs::remove_file(memo);
}
