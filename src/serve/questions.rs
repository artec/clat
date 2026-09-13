//! Host-owned ephemeral ask-user arbitration. Durable history stays tool-owned.
use super::protocol::RpcError;
use super::state::{ServeShared, SseFrame};
use crate::CancelToken;
use crate::interaction::{AskAnswer, AskQuestion, UserAsker};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const WAIT_LIMIT: Duration = Duration::from_secs(600);
const MAX_TEXT_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct Questions(pub Mutex<BTreeMap<String, PendingQuestion>>);

pub(super) struct PendingQuestion {
    question: AskQuestion,
    sender: mpsc::Sender<AskAnswer>,
    cancel: CancelToken,
    deadline: Instant,
}

pub(super) struct ServeAsker(std::sync::Weak<ServeShared>);

impl ServeAsker {
    pub(super) fn new(shared: &Arc<ServeShared>) -> Self {
        Self(Arc::downgrade(shared))
    }
}

fn notice(kind: &str, payload: Value) -> SseFrame {
    SseFrame {
        event: "notice",
        data: super::shapes::ctl_data(&json!({"kind":kind,"payload":payload})),
    }
}

impl PendingQuestion {
    pub(super) fn frame(&self, id: &str) -> SseFrame {
        notice(
            "question_requested",
            json!({"rpc_id":id,"question":self.question}),
        )
    }
}

impl UserAsker for ServeAsker {
    fn ask(&self, question: AskQuestion, cancel: &CancelToken) -> AskAnswer {
        let Some(shared) = self.0.upgrade() else {
            return AskAnswer::Declined;
        };
        if cancel.is_cancelled()
            || shared.subscriber_count() == 0
            || serde_json::to_vec(&question).map_or(true, |bytes| bytes.len() > MAX_TEXT_BYTES)
        {
            return AskAnswer::Declined;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = mpsc::channel();
        let deadline = Instant::now() + WAIT_LIMIT;
        {
            let mut pending = shared.questions.0.lock().expect("questions lock");
            if pending.len() >= 32 {
                return AskAnswer::Declined;
            }
            let entry = PendingQuestion {
                question,
                sender,
                cancel: cancel.clone(),
                deadline,
            };
            let frame = entry.frame(&id);
            pending.insert(id.clone(), entry);
            shared.broadcast(frame);
        }
        loop {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(answer) => return answer,
                Err(mpsc::RecvTimeoutError::Disconnected) => return AskAnswer::Declined,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if cancel.is_cancelled() || shared.subscriber_count() == 0 || Instant::now() >= deadline
            {
                let mut pending = shared.questions.0.lock().expect("questions lock");
                if pending.remove(&id).is_some() {
                    shared.broadcast(notice("question_resolved", json!({"rpc_id":id})));
                }
                // A response may have won the table lock before cancellation.
                return receiver.try_recv().unwrap_or(AskAnswer::Declined);
            }
        }
    }
}

fn validate_answer(question: &AskQuestion, answer: &AskAnswer) -> Result<(), RpcError> {
    let valid = match answer {
        AskAnswer::Selected(value) => question.options.iter().any(|option| option.label == *value),
        AskAnswer::Custom(value) => {
            (question.allow_custom || question.options.is_empty())
                && !value.trim().is_empty()
                && value.len() <= MAX_TEXT_BYTES
        }
        AskAnswer::Declined => true,
    };
    if valid {
        Ok(())
    } else {
        Err(RpcError::bad_request(
            "answer is not allowed by this question",
        ))
    }
}

pub(super) fn respond(shared: &Arc<ServeShared>, params: &Value) -> Result<Value, RpcError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Response {
        #[serde(rename = "rpcId")]
        id: String,
        answer: AskAnswer,
    }
    let response: Response = serde_json::from_value(params.clone()).map_err(|_| {
        RpcError::bad_request("expected rpcId and a selected/custom/declined answer")
    })?;
    let mut pending = shared.questions.0.lock().expect("questions lock");
    let question = pending
        .get(&response.id)
        .ok_or_else(|| RpcError::not_pending("question is no longer pending"))?;
    if question.cancel.is_cancelled() || Instant::now() >= question.deadline {
        return Err(RpcError::not_pending("question expired or run cancelled"));
    }
    validate_answer(&question.question, &response.answer)?;
    let question = pending
        .remove(&response.id)
        .expect("validated pending question");
    let sent = question.sender.send(response.answer);
    shared.broadcast(notice("question_resolved", json!({"rpc_id":response.id})));
    sent.map_err(|_| RpcError::not_pending("question receiver closed"))?;
    Ok(json!({}))
}

#[cfg(test)]
#[path = "question_tests.rs"]
mod tests;
