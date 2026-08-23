//! dsh backend（D-2 §1.3）：HTTP 任务编排 + WS 下行线程 + 连接/重连
//! 编排 + 审批/问答应答载荷构造。纯编排库——**无 UI 状态**；会话转录
//! 与弹框状态一律归 App 线程的 `DshState`（tui/dsh_events.rs）。三根
//! 工作线程（双 WS 读泵 + HTTP worker）只发 `DshEvent`，conversation
//! 与 DshState 只在 App 线程被碰（§1.3 分工铁律）。
//!
//! 协议语义与 D-1 零漂移（INV-U4）：信封、方法名、载荷形状、respond
//! 通道全部原样；结构性差异五处——① `Create` 增 `session_id` 收养
//! 参数（§2.6 步骤 2 的 create 收养式切换，web 客户端同款协议）；
//! ② `run_task` 的调用失败从 D-1 的静默 `None` 改为
//! `Failed(message)`（UI 不再无应答挂起；审计 P2-1 补齐 Restore/
//! Create/History/Models 四处漏网）；③ `Models` 携带宿主原始
//! groups/failures（两级 picker 数据全宿主动态，D-1 的三元组折叠不够）；
//! ④ `Frame`/`LinkDown` 携带连接代际（审计 P2-2：重连后旧 WS 泵的
//! 迟到帧/断线不得污染新流，App 按代际过滤）；⑤ `ReconnectFailed`
//! 独立应答（审计 P1-3：重连失败须可识别，UI 才能重新武装重试）。

use crate::dsh::client::DshClient;
use crate::dsh::connect::{self, ConnectFailure};
use crate::dsh::files;
use crate::dsh::frames::{DshFrame, parse_frame};
use crate::dsh::ws::{self, WsMessage};
use crate::session::event::SessionEvent;
use serde_json::{Value, json};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// App 线程外三线程 → App 线程的唯一消息面（D-2 §1.3，四变体定死，
/// 增删语义须上报）。`Frame` 不区分 mux/host 来路——归约按
/// `DshFrame` 变体分发（两路帧词汇不相交）。
pub(crate) enum DshEvent {
    /// WS 下行帧（mux 与 host 两路同形）。`generation` 是发送泵的
    /// 连接代际（审计 P2-2）：App 只接受当前代际的帧，重连后旧泵的
    /// 迟到帧一律作废（重复流不再能造成文本重复）。
    Frame { generation: u64, frame: DshFrame },
    /// HTTP worker 任务应答。
    Reply(TaskReply),
    /// WS 断开（连接期收到 = 初始连接失败，App 按 close_error 处置）。
    /// `generation` 同 `Frame`——旧代际泵的迟到断线不属于新流的断线，
    /// App 按代际过滤（连接期无 downlink，恒 0）。
    LinkDown { generation: u64, reason: String },
    /// 连接/重连成功。字段级修正记档：设计档案 §1.3 只写
    /// `Reconnected(Value)`（新 describe）——重开 WS 与重建 client 都
    /// 需要 port，故携带 `{ port, describe }`；`child` 是本次连接
    /// spawn 的宿主句柄（None = 探测直连，非本进程所有）——DshState
    /// 持有至退出（退出清理拍板 2026-08-23）。变体语义不变。
    Reconnected {
        port: u16,
        describe: Value,
        child: Option<std::process::Child>,
    },
}

/// HTTP 任务（D-1 WorkerTask 十变体原样；`Create` 扩展收养参数）。
#[derive(Debug)]
pub(crate) enum DshTask {
    Restore,
    Prompt {
        session: String,
        steer: bool,
        text: String,
    },
    Cancel {
        session: String,
    },
    /// `session_id = Some` → create 收养式（§2.6 步骤 2：宿主
    /// ensureSession 校验 cwd 必须等于目标会话记录的 cwd）。
    Create {
        session_id: Option<String>,
        cwd: Option<String>,
    },
    History {
        session: String,
    },
    Models {
        session: Option<String>,
    },
    /// 启动期的名字目录 prime（标题栏展示名解析，2026-08-23 负责人
    /// dogfood 反馈）：同一 `session.models` 调用，但失败走 `Status`
    /// 不走 `Failed`——装饰性获取不得触发 UI 的 fail-soft/装载中止
    /// 路径，也不弹 picker。
    ModelNames {
        session: String,
    },
    Select {
        session: String,
        provider: String,
        model: String,
        /// reasoningEffort（档位接入 2026-08-23）：Some → 随 selectModel
        /// 携带；None = 不带（宿主 adapter 默认）。
        effort: Option<String>,
    },
    Rename {
        session: String,
        title: String,
    },
    Respond {
        rpc_id: String,
        result: Value,
    },
    Reconnect,
}

/// 任务应答。`Reconnected` 经 worker 循环改发 `DshEvent::Reconnected`
/// （与初始连接线程同一条 App 侧处理路径）。
#[derive(Debug)]
pub(crate) enum TaskReply {
    Restored {
        session: Option<String>,
    },
    History {
        session: String,
        events: Vec<SessionEvent>,
    },
    Status(String),
    Created(String),
    /// 宿主 `session.models` 原始应答（groups/failures/current）。
    Models(Value),
    /// 同一应答的 prime 形态（`DshTask::ModelNames`）：UI 只折名字
    /// 索引与 `current` 校正，不开 picker——与用户 `/model` 的
    /// `Models` 应答区分开。
    ModelNames(Value),
    /// selectModel 成功（携带所选，供 model_label 刷新——修 D-1 只
    /// flash 不刷新的缺陷）。effort 是宿主解析后的落定档位
    /// （`selected.reasoningEffort`，可能与请求值不同）。
    Selected {
        provider: String,
        model: String,
        effort: Option<String>,
    },
    Failed(String),
    /// 重连尝试失败（审计 P1-3）：独立于 `Failed`——UI 须凭它复位
    /// 单飞守卫并重新排程，普通 `Failed` 只 flash。混用会让
    /// `reconnecting` 永真，自动重连一次失败后永久停摆。
    ReconnectFailed(String),
    Reconnected {
        port: u16,
        describe: Value,
        child: Option<std::process::Child>,
    },
}

/// 初始连接线程：`ensure_online`（探测 → spawn → 就绪轮询）→
/// `Reconnected`；失败以 `LinkDown` 报告（App 连接期收到即退出）。
pub(crate) fn spawn_connect(preferred_port: u16, events: Sender<DshEvent>) {
    std::thread::spawn(move || {
        let home = files::dsh_home();
        match connect::ensure_online(preferred_port, "dsh", home.as_deref()) {
            Ok(online) => {
                let _ = events.send(DshEvent::Reconnected {
                    port: online.port,
                    describe: online.describe,
                    child: online.child,
                });
            }
            Err(ConnectFailure::NotInstalled) => {
                let _ = events.send(DshEvent::LinkDown {
                    generation: 0,
                    reason: "dsh is not installed (no dsh executable and no ~/.dsh)".into(),
                });
            }
            Err(ConnectFailure::Failed(message)) => {
                let _ = events.send(DshEvent::LinkDown {
                    generation: 0,
                    reason: message,
                });
            }
        }
    });
}

/// HTTP worker 线程：拥有自己的 `DshClient`/port 副本（重连时原地
/// 替换）。任务串行执行；`Reconnected` 应答改发 `DshEvent`。
pub(crate) fn spawn_worker(
    client: DshClient,
    port: u16,
    tasks: Receiver<DshTask>,
    events: Sender<DshEvent>,
) {
    std::thread::spawn(move || {
        let mut client = client;
        let mut port = port;
        while let Ok(task) = tasks.recv() {
            let reply = run_task(&task, &mut client, &mut port);
            let delivered = match reply {
                Some(TaskReply::Reconnected {
                    port,
                    describe,
                    child,
                }) => events
                    .send(DshEvent::Reconnected {
                        port,
                        describe,
                        child,
                    })
                    .is_ok(),
                Some(reply) => events.send(DshEvent::Reply(reply)).is_ok(),
                None => true,
            };
            if !delivered {
                return;
            }
        }
    });
}

/// WS 下行读泵（INV-D3：只收不发）。断开/失败发 `LinkDown` 后线程
/// 自然终止；重连由 App 线程重开新连接（每路一条线程）。
///
/// 代际纪律（审计 P2-2）：`generation` 是本泵所属的连接代际，App 开
/// 新代际时写 `epoch`；泵发现 `epoch != generation` 即静默退役（弃连
/// 接、不发 LinkDown）——旧流的死亡不属于新流的断线。
pub(crate) fn open_downlink(
    port: u16,
    path: &'static str,
    events: &Sender<DshEvent>,
    generation: u64,
    epoch: &Arc<AtomicU64>,
) -> Result<(), String> {
    let stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|error| format!("cannot connect {path}: {error}"))?;
    let host = format!("127.0.0.1:{port}");
    let (ws_tx, ws_rx) = channel::<WsMessage>();
    ws::connect_downlink(stream, path, &host, ws_tx)?;
    let events = events.clone();
    let epoch = Arc::clone(epoch);
    std::thread::spawn(move || {
        while let Ok(message) = ws_rx.recv() {
            if epoch.load(Ordering::SeqCst) != generation {
                return;
            }
            match message {
                WsMessage::Text(text) => {
                    let frame = parse_frame(&text);
                    if events.send(DshEvent::Frame { generation, frame }).is_err() {
                        return;
                    }
                }
                WsMessage::Closed(reason) | WsMessage::Failed(reason) => {
                    let _ = events.send(DshEvent::LinkDown { generation, reason });
                    return;
                }
            }
        }
    });
    Ok(())
}

// ---- 应答载荷构造（纯函数，钉靶形状逐字） ----

/// selectModel 载荷（档位接入 2026-08-23）：`reasoningEffort` 只在
/// Some 时携带——缺席 = 交给宿主 adapter 默认（api-proxy.ts:2203 的
/// 可选语义同款）。
pub(crate) fn select_model_payload(
    session: &str,
    provider: &str,
    model: &str,
    effort: Option<&str>,
) -> Value {
    let mut payload = json!({"sessionId": session, "provider": provider, "model": model});
    if let Some(effort) = effort {
        payload["reasoningEffort"] = json!(effort);
    }
    payload
}

/// selectModel 应答的落定档位：`selected.reasoningEffort`（宿主
/// resolveCallConfig 之后的权威值；缺席 = 该选择不带档位）。
fn selected_effort(value: &Value) -> Option<String> {
    value
        .get("selected")
        .and_then(|selected| selected.get("reasoningEffort"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// 审批应答：`{sessionId, approvalId, outcome}`（outcome ∈
/// allowed-once | rejected——api-proxy respond 的 approvalResponsePayload）。
pub(crate) fn approval_response(session_id: &str, approval_id: &str, outcome: &str) -> Value {
    json!({
        "sessionId": session_id,
        "approvalId": approval_id,
        "outcome": outcome,
    })
}

/// 问答全量应答：`{sessionId, answer: {answers: [...]}}`。
pub(crate) fn question_answer_response(session_id: &str, answers: Vec<Value>) -> Value {
    json!({
        "sessionId": session_id,
        "answer": {"answers": answers},
    })
}

/// 问答取消：client-response-error 信封（code "cancelled"，web 客户端
/// 同款——宿主 claimQuestion 走 `result.error.code` 分支）。
pub(crate) fn question_cancelled_response() -> Value {
    json!({
        "type": "client-response-error",
        "result": {"ok": false, "error": {"code": "cancelled", "message": "cancelled", "details": {}}},
    })
}

/// 单题答案条目：选中标签 `{id, selected: [labels]}` / 自由输入
/// `{id, selected: [], custom: text}`。
pub(crate) fn question_answer_entry(
    id: &Value,
    selected: Vec<String>,
    custom: Option<String>,
) -> Value {
    match custom {
        Some(custom) => json!({"id": id, "selected": [], "custom": custom}),
        None => json!({"id": id, "selected": selected}),
    }
}

pub(crate) fn run_task(
    task: &DshTask,
    client: &mut DshClient,
    port: &mut u16,
) -> Option<TaskReply> {
    // 重连是特殊路径：可能 spawn，阻塞到就绪，随后本线程换 client，
    // App 侧收到 Reconnected 后重开 WS。失败走独立的 ReconnectFailed
    //（审计 P1-3：UI 凭它重新武装自动重试）。
    if matches!(task, DshTask::Reconnect) {
        let home = files::dsh_home();
        return match connect::ensure_online(*port, "dsh", home.as_deref()) {
            Ok(online) => {
                *port = online.port;
                *client = DshClient::new(online.port);
                Some(TaskReply::Reconnected {
                    port: online.port,
                    describe: online.describe,
                    child: online.child,
                })
            }
            Err(ConnectFailure::Failed(message)) => Some(TaskReply::ReconnectFailed(message)),
            Err(ConnectFailure::NotInstalled) => Some(TaskReply::ReconnectFailed(
                "dsh is not installed".to_owned(),
            )),
        };
    }
    match task {
        DshTask::Restore => {
            let list = match client.call("session.list", json!({})) {
                Ok(value) => value,
                Err(error) => return Some(TaskReply::Failed(error.to_string())),
            };
            let recent = list
                .get("items")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item.get("blank").and_then(Value::as_bool) != Some(true))
                        .or_else(|| items.first())
                })
                .and_then(|item| item.get("sessionId"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some(TaskReply::Restored { session: recent })
        }
        DshTask::Prompt {
            session,
            steer,
            text,
        } => {
            let mode = if *steer { "steer" } else { "queue" };
            call_status(
                client,
                "session.prompt",
                json!({
                    "sessionId": session,
                    "mode": mode,
                    "content": [{"type": "text", "text": text}],
                }),
                "prompt sent",
            )
        }
        DshTask::Cancel { session } => call_status(
            client,
            "session.cancel",
            json!({"sessionId": session}),
            "cancel sent",
        ),
        DshTask::Create { session_id, cwd } => {
            let mut payload = serde_json::Map::new();
            if let Some(session_id) = session_id {
                payload.insert("sessionId".into(), json!(session_id));
            }
            if let Some(cwd) = cwd {
                payload.insert("cwd".into(), json!(cwd));
            }
            let value = match client.call("session.create", Value::Object(payload)) {
                Ok(value) => value,
                Err(error) => return Some(TaskReply::Failed(error.to_string())),
            };
            let Some(session) = value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                return Some(TaskReply::Failed(
                    "session.create reply lacks sessionId".to_owned(),
                ));
            };
            Some(TaskReply::Created(session))
        }
        DshTask::History { session } => {
            let value = match client.call("session.history", json!({"sessionId": session})) {
                Ok(value) => value,
                Err(error) => return Some(TaskReply::Failed(error.to_string())),
            };
            let mut events = Vec::new();
            if let Some(items) = value.get("events").and_then(Value::as_array) {
                for item in items {
                    if let Ok(event) = serde_json::from_value::<SessionEvent>(
                        item.get("event").cloned().unwrap_or(Value::Null),
                    ) {
                        events.push(event);
                    }
                }
            }
            events.sort_by_key(|event| event.seq);
            Some(TaskReply::History {
                session: session.clone(),
                events,
            })
        }
        DshTask::Models { session } => {
            let Some(session) = session else {
                return Some(TaskReply::Failed("no active session".to_owned()));
            };
            let value = match client.call("session.models", json!({"sessionId": session})) {
                Ok(value) => value,
                Err(error) => return Some(TaskReply::Failed(error.to_string())),
            };
            Some(TaskReply::Models(value))
        }
        DshTask::ModelNames { session } => {
            // prime 形态：同一调用；失败降 Status（装饰性获取——名字
            // 缺席只是标签回落裸 id，不配触发 Failed 的 fail-soft/装载
            // 中止路径，也不打扰用户）。
            let value = match client.call("session.models", json!({"sessionId": session})) {
                Ok(value) => value,
                Err(error) => {
                    return Some(TaskReply::Status(format!(
                        "model names unavailable ({error})"
                    )));
                }
            };
            Some(TaskReply::ModelNames(value))
        }
        DshTask::Select {
            session,
            provider,
            model,
            effort,
        } => {
            let payload = select_model_payload(session, provider, model, effort.as_deref());
            let (provider, model) = (provider.clone(), model.clone());
            match client.call("session.selectModel", payload) {
                Ok(value) => Some(TaskReply::Selected {
                    provider,
                    model,
                    effort: selected_effort(&value),
                }),
                Err(error) => Some(TaskReply::Failed(error.to_string())),
            }
        }
        DshTask::Rename { session, title } => call_status(
            client,
            "session.rename",
            json!({"sessionId": session, "title": title}),
            "renamed",
        ),
        DshTask::Respond { rpc_id, result } => match client.respond(rpc_id, result.clone()) {
            Ok(true) => Some(TaskReply::Status("answer accepted".to_owned())),
            Ok(false) => Some(TaskReply::Status(
                "answer not pending (first answer wins)".to_owned(),
            )),
            Err(error) => Some(TaskReply::Failed(error.to_string())),
        },
        DshTask::Reconnect => unreachable!("handled above"),
    }
}

fn call_status(
    client: &DshClient,
    method: &str,
    payload: Value,
    ok_message: &str,
) -> Option<TaskReply> {
    match client.call(method, payload) {
        Ok(_) => Some(TaskReply::Status(ok_message.to_owned())),
        Err(error) => Some(TaskReply::Failed(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 载荷形状逐字钉靶：approval 应答字段与 outcome 词汇。
    #[test]
    fn approval_response_payload_matches_the_pinned_shape() {
        let allowed = approval_response("s-1", "a-9", "allowed-once");
        assert_eq!(
            allowed,
            json!({"sessionId": "s-1", "approvalId": "a-9", "outcome": "allowed-once"})
        );
        let rejected = approval_response("s-1", "a-9", "rejected");
        assert_eq!(rejected["outcome"], json!("rejected"));
    }

    /// 问答答案条目：选中与自由输入两形态；全量应答包 answers 数组。
    #[test]
    fn question_payloads_match_the_pinned_shapes() {
        let id = json!("q-1");
        let selected = question_answer_entry(&id, vec!["Alpha".into()], None);
        assert_eq!(selected, json!({"id": "q-1", "selected": ["Alpha"]}));
        let custom = question_answer_entry(&id, Vec::new(), Some("free text".into()));
        assert_eq!(
            custom,
            json!({"id": "q-1", "selected": [], "custom": "free text"})
        );
        let full = question_answer_response("s-1", vec![selected, custom]);
        assert_eq!(full["sessionId"], json!("s-1"));
        assert_eq!(full["answer"]["answers"].as_array().map(Vec::len), Some(2));
    }

    /// 取消信封：client-response-error + code cancelled（宿主
    /// claimQuestion 按 result.error.code 分支）。
    #[test]
    fn cancelled_envelope_matches_the_pinned_shape() {
        let envelope = question_cancelled_response();
        assert_eq!(envelope["type"], json!("client-response-error"));
        assert_eq!(envelope["result"]["ok"], json!(false));
        assert_eq!(envelope["result"]["error"]["code"], json!("cancelled"));
    }

    /// 档位接入判别：selectModel 载荷按需携带 reasoningEffort（缺席
    /// 不造字段——宿主 adapter 默认语义）；应答档位取
    /// selected.reasoningEffort（宿主解析后的权威值）。
    #[test]
    fn select_model_payload_carries_effort_only_when_present() {
        let without = select_model_payload("s-1", "deepseek", "m-1", None);
        assert_eq!(
            without,
            json!({"sessionId": "s-1", "provider": "deepseek", "model": "m-1"})
        );
        let with = select_model_payload("s-1", "deepseek", "m-1", Some("high"));
        assert_eq!(with["reasoningEffort"], json!("high"));
        assert_eq!(selected_effort(&json!({"selected": {"provider": "deepseek", "model": "m-1", "reasoningEffort": "max"}})).as_deref(), Some("max"));
        assert_eq!(
            selected_effort(&json!({"selected": {"provider": "deepseek", "model": "m-1"}})),
            None
        );
    }

    /// 审计 P2-1 判别：Restore/History/Create/Models 的 HTTP 调用失败
    /// 必须以 `Failed` 应答浮出——D-1 的 `.ok()?` 静默 None 会让 UI
    /// 收不到任何事件（启动链永久停在 restoring）。
    #[test]
    fn call_failures_surface_as_failed_replies() {
        fn scratch_port() -> u16 {
            // 绑一个临时端口再立刻释放 → 连接必拒（不写死端口，抗环境）。
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("scratch port");
            let port = listener.local_addr().expect("addr").port();
            drop(listener);
            port
        }
        let mut client = DshClient::new(scratch_port());
        let mut port = 0;
        for task in [
            DshTask::Restore,
            DshTask::History {
                session: "s-1".into(),
            },
            DshTask::Create {
                session_id: Some("s-1".into()),
                cwd: Some("/w".into()),
            },
            DshTask::Models {
                session: Some("s-1".into()),
            },
        ] {
            let reply = run_task(&task, &mut client, &mut port);
            assert!(
                matches!(reply, Some(TaskReply::Failed(_))),
                "{task:?} must surface its call failure, got {reply:?}"
            );
        }
    }
}
