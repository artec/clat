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

use crate::dsh::client::{DshClient, DshEra};
use crate::dsh::connect::{self, ConnectFailure, OwnedDshHost};
use crate::dsh::files;
use crate::dsh::frames::{DshFrame, parse_frame};
use crate::dsh::mux::MuxController;
use crate::dsh::ws::{self, WsMessage};
use crate::session::event::SessionEvent;
use serde_json::{Value, json};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

/// 每一级只容纳少量完整协议消息。单条 WS 文本已由 `ws` 层限制为
/// 16 MiB；四槽把最坏滞留量固定在每级约 64 MiB，并把背压一直传回
/// socket 读泵。这里宁可让生产者等待，也不能丢协议事件或无限吃内存。
pub(crate) const DSH_QUEUE_CAPACITY: usize = 4;

/// 连接/WS/worker 到 App 转发线程的生产通道。所有生产入口必须走此
/// 构造器，避免某个连接路径悄悄退回无界队列。
pub(crate) fn event_channel() -> (SyncSender<DshEvent>, Receiver<DshEvent>) {
    sync_channel(DSH_QUEUE_CAPACITY)
}

fn ws_message_channel() -> (SyncSender<WsMessage>, Receiver<WsMessage>) {
    sync_channel(DSH_QUEUE_CAPACITY)
}

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
        /// DV-9/S3：世代 + cookie（App 存入 DshState；下行开路与
        /// worker 重建 client 依赖它）。
        era: DshEra,
        cookie: Option<String>,
        /// FIX-3/CA-03：进程组句柄（树级清理语义见 connect.rs）。
        child: Option<OwnedDshHost>,
    },
}

/// HTTP 任务（D-1 WorkerTask 十变体原样；`Create` 扩展收养参数；
/// DV-9/S3 增 `AdoptMux`）。
#[derive(Debug)]
pub(crate) enum DshTask {
    /// `prefer` = 客户端自记的最后打开会话（拍板 A，2026-08-24）：仍在
    /// 宿主列表（仍存在）则优先恢复它；缺席/已删 → 列表头。
    Restore {
        prefer: Option<String>,
    },
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
    OlderHistory {
        session: String,
        before_seq: u64,
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
    /// DV-9/S3：App 开完 mux 连接后把控制器移交 worker（History 的
    /// follow 切换与 Respond 的 clientId 都在 worker 侧消费）。
    AdoptMux {
        controller: MuxController,
    },
    Reconnect,
}

/// 任务应答。`Reconnected` 经 worker 循环改发 `DshEvent::Reconnected`
/// （与初始连接线程同一条 App 侧处理路径）。
#[derive(Debug)]
pub(crate) enum TaskReply {
    Restored {
        session: Option<String>,
        /// 选中会话的 workspace（session.list 条目的 cwd passthrough；
        /// None = 未记录/无会话，UI 回落 describe.cwd）。
        cwd: Option<String>,
    },
    History {
        session: String,
        events: Vec<SessionEvent>,
        first_seq: Option<u64>,
        has_more: bool,
    },
    OlderHistory {
        session: String,
        requested_before_seq: u64,
        events: Vec<SessionEvent>,
        first_seq: Option<u64>,
        has_more: bool,
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
        era: DshEra,
        cookie: Option<String>,
        child: Option<OwnedDshHost>,
    },
}

/// 初始连接线程：`ensure_online`（探测 → spawn → 就绪轮询）→
/// `Reconnected`；失败以 `LinkDown` 报告（App 连接期收到即退出）。
pub(crate) fn spawn_connect(preferred_port: u16, events: SyncSender<DshEvent>) {
    std::thread::spawn(move || {
        let home = files::dsh_home();
        match connect::ensure_online(preferred_port, "dsh", home.as_deref()) {
            Ok(online) => {
                let _ = deliver_reconnected(
                    &events,
                    online.port,
                    online.describe,
                    online.era,
                    online.cookie,
                    online.child,
                );
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
    events: SyncSender<DshEvent>,
) {
    std::thread::spawn(move || {
        let mut client = client;
        let mut port = port;
        // DV-9/S3：Typert mux 控制器（AdoptMux 移交；Legacy 恒 None）。
        let mut mux: Option<MuxController> = None;
        while let Ok(task) = tasks.recv() {
            if matches!(task, DshTask::AdoptMux { .. }) {
                if let DshTask::AdoptMux { controller } = task {
                    mux = Some(controller);
                }
                continue;
            }
            let reply = run_task(&task, &mut client, &mut port, mux.as_ref());
            let delivered = match reply {
                Some(TaskReply::Reconnected {
                    port,
                    describe,
                    era,
                    cookie,
                    child,
                }) => deliver_reconnected(&events, port, describe, era, cookie, child),
                Some(reply) => events.send(DshEvent::Reply(reply)).is_ok(),
                None => true,
            };
            if !delivered {
                return;
            }
        }
    });
}

/// 初连与重连共享的所有权交接点。发送失败时 `SendError` 持有的
/// `OwnedDshHost` 随即 Drop；发送成功但 App 先退出时，接收队列中的
/// 事件被 Drop。两条路径都不会留下宿主树。
fn deliver_reconnected(
    events: &SyncSender<DshEvent>,
    port: u16,
    describe: Value,
    era: DshEra,
    cookie: Option<String>,
    child: Option<OwnedDshHost>,
) -> bool {
    events
        .send(DshEvent::Reconnected {
            port,
            describe,
            era,
            cookie,
            child,
        })
        .is_ok()
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
    events: &SyncSender<DshEvent>,
    generation: u64,
    epoch: &Arc<AtomicU64>,
) -> Result<(), String> {
    let stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|error| format!("cannot connect {path}: {error}"))?;
    let host = format!("127.0.0.1:{port}");
    let (ws_tx, ws_rx) = ws_message_channel();
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
                // DV-9/S3：旧宿主从不 ping——上行不存在的路径静默丢弃
                //（Typert 泵在 mux.rs 内应答心跳，不经此分发）。
                WsMessage::Ping(_) => {}
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

/// DV-9/S4（真实宿主实证）：Typert 严格模式的 args 字段名 = Remote
/// 方法的 TS 参数名（descriptor 由此生成）——`session/list(_request,…)`
/// 用 `_request`，其余单请求对象方法用 `request`；无参方法 `{}`
///（外层 `{args:…}` 由 client 传输缝统一包裹）。Legacy 载荷原样。
pub(crate) fn era_request_args(era: DshEra, param: Option<&str>, payload: Value) -> Value {
    match (era, param) {
        (DshEra::Typert, Some(param)) => json!({param: payload}),
        _ => payload,
    }
}

/// DV-9/S2：世代方法名（Legacy 点名族 ↔ Typert 斜杠族）。
pub(crate) fn era_method(era: DshEra, legacy: &str, typert: &str) -> String {
    match era {
        DshEra::Legacy => legacy.to_owned(),
        DshEra::Typert => typert.to_owned(),
    }
}

/// DV-9/S2：prompt 载荷。Typert 增 `requestId`（客户端铸造、落在
/// 被接受的那条 user/message 上，research §4）；Legacy 不带。
pub(crate) fn prompt_payload(era: DshEra, session: &str, steer: bool, text: &str) -> Value {
    let mode = if steer { "steer" } else { "queue" };
    let mut payload = json!({
        "sessionId": session,
        "mode": mode,
        "content": [{"type": "text", "text": text}],
    });
    if era == DshEra::Typert {
        payload["requestId"] = json!(uuid::Uuid::new_v4().to_string());
    }

    payload
}

/// DV-9/S2：modelCatalog → 旧 `session.models` 应答形状的折算
///（TUI 零改动，计划 §0 裁定 2）。Typert catalog 无按会话 current——
/// `current` 暂取 `default`（部署默认），会话级校正随 S3 的
/// model-selection projection 落位（计划 §2 开题 1 同族）。
pub(crate) fn fold_model_catalog(catalog: &Value) -> Value {
    json!({
        "groups": catalog.get("groups").cloned().unwrap_or(Value::Array(Vec::new())),
        "failures": catalog.get("failures").cloned().unwrap_or(Value::Array(Vec::new())),
        "current": catalog.get("default").cloned().unwrap_or(Value::Null),
    })
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
    mux: Option<&MuxController>,
) -> Option<TaskReply> {
    // 重连是特殊路径：可能 spawn，阻塞到就绪，随后本线程换 client，
    // App 侧收到 Reconnected 后重开 WS。失败走独立的 ReconnectFailed
    //（审计 P1-3：UI 凭它重新武装自动重试）。
    if matches!(task, DshTask::Reconnect) {
        let home = files::dsh_home();
        return match connect::ensure_online(*port, "dsh", home.as_deref()) {
            Ok(online) => {
                *port = online.port;
                let era = online.era;
                let cookie = online.cookie.clone();
                let mut next = DshClient::new(online.port);
                if era == DshEra::Typert {
                    if let Some(cookie) = &cookie {
                        next = next.with_cookie(cookie);
                    }
                    next = next.with_typert_era();
                }
                *client = next;
                Some(TaskReply::Reconnected {
                    port: online.port,
                    describe: online.describe,
                    era,
                    cookie,
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
        DshTask::Restore { prefer } => {
            let list_method = era_method(client.era, "session.list", "session/list");
            let list_payload = era_request_args(client.era, Some("_request"), json!({}));
            let list = match client.call(&list_method, list_payload) {
                Ok(value) => value,
                Err(error) => return Some(TaskReply::Failed(error.to_string())),
            };
            // 选择序（拍板 A，2026-08-24）：客户端自记的最后会话优先
            //（命中 = 仍存在于宿主列表；web 端 localStorage 记忆的
            // CLAT 同款）；缺席/已删回落**列表头**——列表本身已按
            // updatedAt newest-first 排序（最近创建/提示过的会话），
            // 不做 blank 跳过（原「跳过 blank」会在 web 切到新开的空
            // 会话后落到旧的非空会话）。选中条目的 sessionId + cwd
            // 一并带回（cwd 是会话 workspace 的 passthrough——客户端
            // 显示用；缺席 = 未记录，调用方回落 describe.cwd）。
            let items = list.get("items").and_then(Value::as_array);
            let chosen = items.and_then(|items| {
                prefer.as_deref().and_then(|id| {
                    items
                        .iter()
                        .find(|item| item.get("sessionId").and_then(Value::as_str) == Some(id))
                })
            });
            let restored = chosen
                .or_else(|| items.and_then(|items| items.first()))
                .map(|item| {
                    (
                        item.get("sessionId")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        item.get("cwd").and_then(Value::as_str).map(str::to_owned),
                    )
                });
            match restored {
                Some((session, cwd)) => Some(TaskReply::Restored { session, cwd }),
                None => Some(TaskReply::Restored {
                    session: None,
                    cwd: None,
                }),
            }
        }
        DshTask::Prompt {
            session,
            steer,
            text,
        } => {
            let method = era_method(client.era, "session.prompt", "session/prompt");
            let payload = era_request_args(
                client.era,
                Some("request"),
                prompt_payload(client.era, session, *steer, text),
            );
            call_status(client, &method, payload, "prompt sent")
        }
        DshTask::Cancel { session } => call_status(
            client,
            &era_method(client.era, "session.cancel", "session/cancel"),
            era_request_args(client.era, Some("request"), json!({"sessionId": session})),
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
            let create_method = era_method(client.era, "session.create", "session/create");
            let payload = era_request_args(client.era, Some("request"), Value::Object(payload));
            let value = match client.call(&create_method, payload) {
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
            // page 的 throughSeq 是 follow 的真实持久游标；-1 是空
            // 前缀，不是“最新”。只取尾部 50 条；更早页由 TUI 上翻
            // 显式请求，不能在 worker 内急切循环到会话起点。
            if client.era == DshEra::Typert {
                return Some(run_typert_history_page(client, mux, session, None));
            }
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
                first_seq: events.first().map(|event| event.seq),
                events,
                has_more: false,
            })
        }
        DshTask::OlderHistory {
            session,
            before_seq,
        } => {
            if client.era != DshEra::Typert {
                return Some(TaskReply::Failed(
                    "older history pages require the DSH session/page protocol".into(),
                ));
            }
            Some(run_typert_history_page(
                client,
                mux,
                session,
                Some(*before_seq),
            ))
        }
        DshTask::Models { session } => {
            let Some(session) = session else {
                return Some(TaskReply::Failed("no active session".to_owned()));
            };
            // DV-9/S2：Typert 的 modelCatalog 是宿主级目录（无按会话
            // 参数）；`session` 字段仅 Legacy 使用。
            let (method, payload, fold) = if client.era == DshEra::Typert {
                ("session/modelCatalog", json!({}), true)
            } else {
                ("session.models", json!({"sessionId": session}), false)
            };
            let value = match client.call(method, payload) {
                Ok(value) => value,
                Err(error) => return Some(TaskReply::Failed(error.to_string())),
            };
            let value = if fold {
                fold_model_catalog(&value)
            } else {
                value
            };
            Some(TaskReply::Models(value))
        }
        DshTask::ModelNames { session } => {
            // prime 形态：同一调用；失败降 Status（装饰性获取——名字
            // 缺席只是标签回落裸 id，不配触发 Failed 的 fail-soft/装载
            // 中止路径，也不打扰用户）。
            let (method, payload, fold) = if client.era == DshEra::Typert {
                ("session/modelCatalog", json!({}), true)
            } else {
                ("session.models", json!({"sessionId": session}), false)
            };
            let value = match client.call(method, payload) {
                Ok(value) => value,
                Err(error) => {
                    return Some(TaskReply::Status(format!(
                        "model names unavailable ({error})"
                    )));
                }
            };
            let value = if fold {
                fold_model_catalog(&value)
            } else {
                value
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
            let select_method =
                era_method(client.era, "session.selectModel", "session/selectModel");
            let payload = era_request_args(client.era, Some("request"), payload);
            match client.call(&select_method, payload) {
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
            &era_method(client.era, "session.rename", "session/rename"),
            era_request_args(
                client.era,
                Some("request"),
                json!({"sessionId": session, "title": title}),
            ),
            "renamed",
        ),
        DshTask::Respond { rpc_id, result } => {
            // DV-9/S3：Typert 应答走 `$events/result`（research §5：
            // payload 带 {args:…} 包裹）。outcome 词汇两代同源
            //（allowed-once/rejected…）；问答答案取 `answer` 字段。
            if client.era == DshEra::Typert {
                let Some(controller) = mux else {
                    return Some(TaskReply::Failed(
                        "answers need the mux controller (AdoptMux missing)".to_owned(),
                    ));
                };
                let client_id = controller
                    .client_id
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                let Some(client_id) = client_id else {
                    return Some(TaskReply::Failed(
                        "the host event stream is not ready yet (no clientId)".to_owned(),
                    ));
                };
                let outcome_value = result
                    .get("outcome")
                    .cloned()
                    .or_else(|| result.get("answer").cloned())
                    .unwrap_or(Value::Null);
                let payload = json!({
                    "clientId": client_id,
                    "eventId": rpc_id,
                    "outcome": {"kind": "result", "value": outcome_value},
                });
                return match client.call("$events/result", payload) {
                    Ok(_) => Some(TaskReply::Status("answer accepted".to_owned())),
                    Err(error) => Some(TaskReply::Failed(error.to_string())),
                };
            }
            match client.respond(rpc_id, result.clone()) {
                Ok(true) => Some(TaskReply::Status("answer accepted".to_owned())),
                Ok(false) => Some(TaskReply::Status(
                    "answer not pending (first answer wins)".to_owned(),
                )),
                Err(error) => Some(TaskReply::Failed(error.to_string())),
            }
        }
        DshTask::AdoptMux { .. } => unreachable!("intercepted by the worker loop"),
        DshTask::Reconnect => unreachable!("handled above"),
    }
}

fn run_typert_history_page(
    client: &DshClient,
    mux: Option<&MuxController>,
    session: &str,
    before_seq: Option<u64>,
) -> TaskReply {
    let through_seq = if let Some(controller) = mux {
        match controller.history_cursor(session) {
            Ok(cursor) => cursor,
            Err(error) => return TaskReply::Failed(error),
        }
    } else {
        return TaskReply::Failed(
            "typert history needs the mux controller (AdoptMux missing)".to_owned(),
        );
    };
    let mut request = json!({
        "address": {"kind": "session", "sessionId": session},
        "throughSeq": through_seq,
        "maxMessages": 50,
    });
    if let Some(before) = before_seq {
        request["beforeSeq"] = json!(before);
    }
    let payload = era_request_args(client.era, Some("request"), request);
    let value = match client.call("session/page", payload) {
        Ok(value) => value,
        Err(error) => return TaskReply::Failed(error.to_string()),
    };
    let Some(records) = value.get("records").and_then(Value::as_array) else {
        return TaskReply::Failed("session/page reply lacks records".into());
    };
    let Some(has_more) = value.get("hasMore").and_then(Value::as_bool) else {
        return TaskReply::Failed("session/page reply lacks hasMore".into());
    };
    let mut events = Vec::with_capacity(records.len());
    let mut first_seq = None;
    for record in records {
        let event = match serde_json::from_value::<SessionEvent>(
            record.get("event").cloned().unwrap_or(Value::Null),
        ) {
            Ok(event) => event,
            Err(error) => return TaskReply::Failed(format!("invalid history event: {error}")),
        };
        if i64::try_from(event.seq).map_or(true, |seq| seq > through_seq)
            || before_seq.is_some_and(|before| event.seq >= before)
        {
            return TaskReply::Failed(
                "session/page returned an event outside the requested cut".into(),
            );
        }
        first_seq = Some(first_seq.map_or(event.seq, |seq: u64| seq.min(event.seq)));
        events.push(event);
    }
    if has_more && first_seq.is_none_or(|seq| seq == 0) {
        return TaskReply::Failed("session/page hasMore without backwards progress".into());
    }
    events.sort_by_key(|event| event.seq);
    match before_seq {
        Some(requested_before_seq) => TaskReply::OlderHistory {
            session: session.to_owned(),
            requested_before_seq,
            events,
            first_seq,
            has_more,
        },
        None => TaskReply::History {
            session: session.to_owned(),
            events,
            first_seq,
            has_more,
        },
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

    /// RA-02 判别腿：两个生产队列分别由生产构造器创建，填满后必须
    /// 明确返回 Full。删掉任一 `sync_channel` 都会让对应腿不再成立。
    #[test]
    fn both_dsh_transport_queues_apply_backpressure() {
        use std::sync::mpsc::TrySendError;

        let (ws_tx, _ws_rx) = ws_message_channel();
        for index in 0..DSH_QUEUE_CAPACITY {
            assert!(
                ws_tx.try_send(WsMessage::Text(index.to_string())).is_ok(),
                "WS queue slot {index}"
            );
        }
        assert!(matches!(
            ws_tx.try_send(WsMessage::Text("overflow".into())),
            Err(TrySendError::Full(_))
        ));

        let (event_tx, _event_rx) = event_channel();
        for index in 0..DSH_QUEUE_CAPACITY {
            assert!(
                event_tx
                    .try_send(DshEvent::LinkDown {
                        generation: index as u64,
                        reason: "fill".into(),
                    })
                    .is_ok(),
                "event queue slot {index}"
            );
        }
        assert!(matches!(
            event_tx.try_send(DshEvent::LinkDown {
                generation: u64::MAX,
                reason: "overflow".into(),
            }),
            Err(TrySendError::Full(_))
        ));
    }

    /// RA-03：初连和重连共用 `deliver_reconnected`。接收端已经离开时，
    /// 发送失败所携带的所有权守卫必须同步带走刚 spawn 的宿主。
    #[cfg(unix)]
    #[test]
    fn failed_reconnected_delivery_drops_the_owned_host() {
        use command_group::CommandGroup as _;

        let child = std::process::Command::new("sleep")
            .arg("60")
            .group_spawn()
            .expect("sleeper spawns");
        let pid = child.id();
        let (events, receiver) = event_channel();
        drop(receiver);
        assert!(!deliver_reconnected(
            &events,
            3080,
            json!({}),
            crate::dsh::client::DshEra::Legacy,
            None,
            Some(OwnedDshHost::new(child)),
        ));
        assert!(
            !process_alive(pid),
            "failed delivery must not leak the host"
        );
    }

    /// RA-03：发送成功不等于已被 App 收养。事件仍在队列中时 App 退出，
    /// Receiver::drop 必须经守卫清掉 leader 与忽略 TERM 的后代。
    #[cfg(unix)]
    #[test]
    fn queued_reconnected_event_owns_and_cleans_the_whole_tree() {
        use command_group::CommandGroup as _;
        use std::os::unix::fs::PermissionsExt as _;

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pidfile = std::env::temp_dir().join(format!("clat-queued-dsh-{stamp}.pid"));
        let script = std::env::temp_dir().join(format!("clat-queued-dsh-{stamp}.sh"));
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n(trap '' TERM; exec sleep 60) &\necho $! > \"{pidfile}\"\nsleep 60\n",
                pidfile = pidfile.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = std::process::Command::new(&script)
            .group_spawn()
            .expect("tree spawns");
        let leader = child.id();
        let mut descendant = None;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                descendant = Some(pid);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let descendant = descendant.expect("descendant pid recorded");

        let (events, receiver) = event_channel();
        assert!(deliver_reconnected(
            &events,
            3080,
            json!({}),
            crate::dsh::client::DshEra::Legacy,
            None,
            Some(OwnedDshHost::new(child)),
        ));
        drop(receiver);
        assert!(!process_alive(leader), "queued owner cleans the leader");
        assert!(
            !process_alive(descendant),
            "queued owner cleans the descendant"
        );
        std::fs::remove_file(&script).ok();
        std::fs::remove_file(&pidfile).ok();
    }

    #[cfg(unix)]
    fn process_alive(pid: u32) -> bool {
        std::process::Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

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

    // ─── DV-9/S2：双世代方法面判别 ──────────────────────────────────

    /// 世代方法表 + prompt 载荷：Typert 斜杠族 + requestId（uuid 形）；
    /// Legacy 点名族原样、不带 requestId。删任一映射即红。
    #[test]
    fn typert_method_table_and_prompt_payload_differ_by_era() {
        use crate::dsh::client::DshEra;
        for (legacy, typert) in [
            ("session.list", "session/list"),
            ("session.prompt", "session/prompt"),
            ("session.cancel", "session/cancel"),
            ("session.create", "session/create"),
            ("session.selectModel", "session/selectModel"),
            ("session.rename", "session/rename"),
        ] {
            assert_eq!(era_method(DshEra::Legacy, legacy, typert), legacy);
            assert_eq!(era_method(DshEra::Typert, legacy, typert), typert);
        }
        let legacy = prompt_payload(DshEra::Legacy, "s-1", false, "hi");
        assert!(legacy.get("requestId").is_none(), "legacy has no requestId");
        assert_eq!(legacy["mode"], json!("queue"));
        let typert = prompt_payload(DshEra::Typert, "s-1", true, "hi");
        assert_eq!(typert["mode"], json!("steer"));
        let request_id = typert
            .get("requestId")
            .and_then(Value::as_str)
            .expect("typert carries requestId");
        assert!(
            uuid::Uuid::parse_str(request_id).is_ok(),
            "requestId is a uuid: {request_id}"
        );
    }

    /// modelCatalog 折算：groups/failures 原样透传、current ← default
    ///（TUI 零改动的形状契约）；default 缺席 → current:null（TUI 回落
    /// refresh 路径）。
    #[test]
    fn model_catalog_folds_default_into_the_legacy_shape() {
        let catalog = json!({
            "default": {"provider": "deepseek", "model": "m-1", "reasoningEffort": "high"},
            "routableProviders": ["deepseek"],
            "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
                {"id": "m-1", "name": "M1", "reasoning": {"efforts": [{"id": "high", "name": "High"}]}}
            ]}],
            "failures": [{"id": "kimi", "name": "Kimi", "message": "no key"}]
        });
        let folded = fold_model_catalog(&catalog);
        assert_eq!(folded["groups"], catalog["groups"], "groups passthrough");
        assert_eq!(folded["failures"], catalog["failures"]);
        assert_eq!(folded["current"], catalog["default"], "current ← default");
        assert!(
            folded.get("routableProviders").is_none(),
            "extra fields dropped"
        );
        assert_eq!(
            fold_model_catalog(&json!({"groups": []}))["current"],
            Value::Null
        );
    }

    /// 录制式 Typert 假宿主：`POST /api/<endpoint>` 信封往返 + 方法/
    /// 载荷记录。与 client.rs 的 mini host 同款手写 TcpListener 模式。
    struct TypertHost {
        port: u16,
        requests: std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    }

    impl TypertHost {
        fn spawn() -> Self {
            use std::io::{Read as _, Write as _};
            use std::net::TcpListener;
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind typert host");
            let port = listener.local_addr().expect("addr").port();
            let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = std::sync::Arc::clone(&requests);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let seen = std::sync::Arc::clone(&seen);
                    std::thread::spawn(move || {
                        let mut buffer = Vec::new();
                        let mut chunk = [0u8; 4096];
                        loop {
                            let text = String::from_utf8_lossy(&buffer).into_owned();
                            if let Some(header_end) = text.find("\r\n\r\n") {
                                let body_have = buffer.len() - header_end - 4;
                                let want = text
                                    .lines()
                                    .find_map(|line| {
                                        let lowered = line.to_ascii_lowercase();
                                        lowered
                                            .strip_prefix("content-length:")
                                            .and_then(|v| v.trim().parse::<usize>().ok())
                                    })
                                    .unwrap_or(0);
                                if body_have >= want {
                                    break;
                                }
                            }
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                            }
                            if buffer.len() > 65536 {
                                break;
                            }
                        }
                        let text = String::from_utf8_lossy(&buffer).into_owned();
                        let endpoint = text
                            .lines()
                            .next()
                            .and_then(|request_line| request_line.split(' ').nth(1))
                            .unwrap_or_default()
                            .trim_start_matches("/api/")
                            .to_owned();
                        let body = text
                            .split_once("\r\n\r\n")
                            .map(|(_, body)| body)
                            .unwrap_or_default();
                        let envelope: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                        let payload = envelope.get("payload").cloned().unwrap_or(Value::Null);
                        let rpc_id = envelope
                            .get("rpcId")
                            .and_then(Value::as_str)
                            .unwrap_or("x")
                            .to_owned();
                        seen.lock()
                            .expect("requests")
                            .push((endpoint.clone(), payload.clone()));
                        let value: Value = match endpoint.as_str() {
                            "session/list" => json!({"items": []}),
                            "session/modelCatalog" => json!({
                                "default": {"provider": "p", "model": "m", "reasoningEffort": "high"},
                                "groups": [{"id": "p", "name": "P", "models": [{"id": "m", "name": "M"}]}],
                                "failures": []
                            }),
                            "session/create" => json!({"sessionId": "s-new"}),
                            "session/selectModel" => json!({"selected": {
                                "provider": "p", "model": "m", "reasoningEffort": "max"
                            }}),
                            _ => json!({"accepted": true}),
                        };
                        let reply = json!({
                            "type": "server-response",
                            "rpcId": rpc_id,
                            "result": {"ok": true, "value": value}
                        });
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            reply.to_string().len(),
                            reply
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    });
                }
            });
            Self { port, requests }
        }

        fn client(&self) -> DshClient {
            DshClient::new(self.port).with_typert_era()
        }

        fn methods(&self) -> Vec<String> {
            self.requests
                .lock()
                .expect("requests")
                .iter()
                .map(|(method, _)| method.clone())
                .collect()
        }

        fn payload_of(&self, method: &str) -> Value {
            self.requests
                .lock()
                .expect("requests")
                .iter()
                .find(|(seen, _)| seen == method)
                .map(|(_, payload)| payload.clone())
                .expect("method was called")
        }
    }

    /// DV-9/S2 端到端腿：Typert 世代走斜杠方法族、prompt 带
    /// requestId、modelCatalog 无参且折回旧形状、selectModel 落定档位
    /// 透传；同一宿主上 Legacy 世代仍发点名族（世代判别）。
    /// 半桥惰性：History/Respond 在 Typert 世代明确拒绝（S3 范围）。
    #[test]
    fn typert_era_round_trips_the_slash_method_family() {
        let host = TypertHost::spawn();
        let mut client = host.client();
        let mut port = 0;

        let reply = run_task(
            &DshTask::Restore { prefer: None },
            &mut client,
            &mut port,
            None,
        );
        assert!(
            matches!(reply, Some(TaskReply::Restored { session: None, .. })),
            "restore reply: {reply:?}"
        );

        let reply = run_task(
            &DshTask::Prompt {
                session: "s-1".into(),
                steer: true,
                text: "go".into(),
            },
            &mut client,
            &mut port,
            None,
        );
        assert!(matches!(reply, Some(TaskReply::Status(_))));
        let prompt = host.payload_of("session/prompt");
        assert!(
            uuid::Uuid::parse_str(
                prompt["args"]["request"]["requestId"]
                    .as_str()
                    .unwrap_or("")
            )
            .is_ok(),
            "requestId rides inside args.request: {prompt}"
        );

        let reply = run_task(
            &DshTask::Cancel {
                session: "s-1".into(),
            },
            &mut client,
            &mut port,
            None,
        );
        assert!(
            matches!(reply, Some(TaskReply::Status(_))),
            "cancel reply must not fail silently (a transport failure here once masqueraded \
             as a missing-method assertion): {reply:?}"
        );
        let reply = run_task(
            &DshTask::Create {
                session_id: None,
                cwd: Some("/w".into()),
            },
            &mut client,
            &mut port,
            None,
        );
        assert!(matches!(reply, Some(TaskReply::Created(id)) if id == "s-new"));

        let reply = run_task(
            &DshTask::Models {
                session: Some("s-1".into()),
            },
            &mut client,
            &mut port,
            None,
        );
        match reply {
            Some(TaskReply::Models(value)) => {
                assert_eq!(value["current"]["model"], json!("m"), "catalog folded");
                assert_eq!(value["groups"][0]["models"][0]["id"], json!("m"));
            }
            other => panic!("Models reply: {other:?}"),
        }
        assert_eq!(
            host.payload_of("session/modelCatalog"),
            json!({"args": {}}),
            "catalog takes no endpoint args (args wrapper at the seam)"
        );

        let reply = run_task(
            &DshTask::Select {
                session: "s-1".into(),
                provider: "p".into(),
                model: "m".into(),
                effort: Some("high".into()),
            },
            &mut client,
            &mut port,
            None,
        );
        assert!(matches!(
            reply,
            Some(TaskReply::Selected { effort: Some(e), .. }) if e == "max"
        ));

        let reply = run_task(
            &DshTask::Rename {
                session: "s-1".into(),
                title: "t".into(),
            },
            &mut client,
            &mut port,
            None,
        );
        assert!(
            matches!(&reply, Some(TaskReply::Status(message)) if message == "renamed"),
            "rename reply must reach the host (CI 2026-09-10: a pooled-connection reuse \
             failure surfaced as a missing session/rename): {reply:?}"
        );

        let methods = host.methods();
        for expected in [
            "session/list",
            "session/prompt",
            "session/cancel",
            "session/create",
            "session/modelCatalog",
            "session/selectModel",
            "session/rename",
        ] {
            assert!(
                methods.iter().any(|m| m == expected),
                "missing {expected} in {methods:?}"
            );
        }
        assert!(
            !methods.iter().any(|m| m.contains('.')),
            "legacy dotted names never fire in typert era: {methods:?}"
        );

        // 半桥惰性：S3 范围的 History/Respond 在 Typert 世代明确拒绝。
        for task in [
            DshTask::History {
                session: "s-1".into(),
            },
            DshTask::Respond {
                rpc_id: "r-1".into(),
                result: json!({}),
            },
        ] {
            let reply = run_task(&task, &mut client, &mut port, None);
            assert!(
                matches!(reply, Some(TaskReply::Failed(_))),
                "{task:?} must defer to S3, got {reply:?}"
            );
        }

        // 世代判别：同一宿主上 Legacy 客户端仍发点名族。
        let mut legacy = DshClient::new(host.port);
        let _ = run_task(
            &DshTask::Restore { prefer: None },
            &mut legacy,
            &mut port,
            None,
        );
        assert!(
            host.methods().iter().any(|m| m == "session.list"),
            "legacy era keeps the dotted family"
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
            DshTask::Restore { prefer: None },
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
            let reply = run_task(&task, &mut client, &mut port, None);
            assert!(
                matches!(reply, Some(TaskReply::Failed(_))),
                "{task:?} must surface its call failure, got {reply:?}"
            );
        }
    }
}
