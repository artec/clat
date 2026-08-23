//! 审批回调象限（§6）：`PermissionApprover` 的网络化——象限③
//! `approval.requested` 帧 + 象限④ `approval.respond` 方法。
//!
//! fail-closed 三路（run 取消 / 订阅者全断 / 超时）全部返回 `Deny`
//!（宪法 Permission First：从严）。审批**结果**已在 RunEvent 词汇与
//! journal；审批**请求**是活控制面帧，不落 durable（§6.1，不触发
//! 四道门）。`escalate_to` 是 v1 定形未实现的保留字段——出现即
//! `bad-request`（静默忽略会误导客户端以为升档生效，§13-⑤）。

use super::state::ServeShared;
use crate::model::CancelToken;
use crate::permission::{PermissionApprover, PermissionDecision, PermissionRequest};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// 取消轮询步长（与既有 approver 先例同款节奏）。
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 审批等待上限（§6.1-d）。
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub(crate) struct ServeApprover {
    shared: Arc<ServeShared>,
}

impl ServeApprover {
    pub(crate) fn new(shared: Arc<ServeShared>) -> Self {
        Self { shared }
    }
}

impl PermissionApprover for ServeApprover {
    fn decide(&self, request: PermissionRequest, cancel: &CancelToken) -> PermissionDecision {
        let rpc_id = uuid::Uuid::new_v4().to_string();
        let (decision_tx, decision_rx) = std::sync::mpsc::channel::<PermissionDecision>();
        self.shared
            .pending
            .lock()
            .expect("serve pending lock")
            .insert(
                rpc_id.clone(),
                super::state::PendingApproval { decision_tx },
            );

        let ctl = super::shapes::approval_requested_ctl(&rpc_id, &request);
        self.shared.broadcast(super::state::SseFrame {
            event: "approval.requested",
            data: super::shapes::ctl_data(&ctl),
        });

        let deadline = Instant::now() + APPROVAL_TIMEOUT;
        let decision = wait_for_decision(&self.shared, &rpc_id, &decision_rx, cancel, deadline);
        self.shared
            .pending
            .lock()
            .expect("serve pending lock")
            .remove(&rpc_id);
        decision
    }
}

/// 四路等待：a) oneshot 首答即赢；b) run 取消 → Deny；c) 订阅者全断
/// → Deny（loopback PWA 关页即离场，拒一次工具调用、run 继续——
/// 保守正确）；d) 超时 → Deny。
fn wait_for_decision(
    shared: &Arc<ServeShared>,
    rpc_id: &str,
    decision_rx: &Receiver<PermissionDecision>,
    cancel: &CancelToken,
    deadline: Instant,
) -> PermissionDecision {
    loop {
        match decision_rx.recv_timeout(POLL_INTERVAL) {
            Ok(decision) => {
                // 首答即赢：pending 表按 rpcId 单发，重复 respond 在
                // 表上已是 not-pending（respond 侧原子 remove）。
                let _ = rpc_id;
                return decision;
            }
            Err(RecvTimeoutError::Timeout) => {
                if cancel.is_cancelled() {
                    return PermissionDecision::Deny {
                        reason: "run cancelled".into(),
                    };
                }
                if shared.subscriber_count() == 0 {
                    return PermissionDecision::Deny {
                        reason: "client disconnected".into(),
                    };
                }
                if Instant::now() >= deadline {
                    return PermissionDecision::Deny {
                        reason: "approval timeout".into(),
                    };
                }
            }
            // 发送端被摘除且未发值：视为无人可答，fail-closed。
            Err(RecvTimeoutError::Disconnected) => {
                return PermissionDecision::Deny {
                    reason: "no permission decision available".into(),
                };
            }
        }
    }
}

/// `approval.respond`（象限④）：回填 rpcId，绝不新铸。
pub(crate) fn respond(
    shared: &Arc<ServeShared>,
    params: &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, super::protocol::RpcError> {
    use super::protocol::RpcError;

    if params.contains_key("escalate_to") {
        return Err(RpcError::bad_request(
            "escalate_to is reserved for a future revision; the v1 endpoint accepts only allow/deny",
        ));
    }
    let rpc_id = params
        .get("rpcId")
        .and_then(|value| value.as_str())
        .ok_or_else(|| RpcError::bad_request("missing field: rpcId"))?;
    let decision = match params.get("decision").and_then(|value| value.as_str()) {
        Some("allow") => PermissionDecision::Allow,
        Some("deny") => PermissionDecision::Deny {
            reason: "denied by client".into(),
        },
        _ => {
            return Err(RpcError::bad_request(
                "decision must be \"allow\" or \"deny\"",
            ));
        }
    };
    // 原子 remove：赢家唯一。迟到的第二次 respond 拿不到表项。
    let pending = shared
        .pending
        .lock()
        .expect("serve pending lock")
        .remove(rpc_id);
    match pending {
        Some(approval) => {
            if approval.decision_tx.send(decision).is_err() {
                // approver 已按取消/断线/超时返回——正是 not-pending 语义。
                return Err(RpcError::not_pending(format!(
                    "approval {rpc_id} is no longer pending"
                )));
            }
            Ok(serde_json::json!({}))
        }
        None => Err(RpcError::not_pending(format!(
            "no pending approval for rpcId {rpc_id}"
        ))),
    }
}
