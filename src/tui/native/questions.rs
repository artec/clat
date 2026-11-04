//! Question queue and dialog lifetime only; the host arbitrates every answer.
use super::*;
use crate::interaction::{AskAnswer, AskQuestion};
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct NativeQuestions {
    pending: VecDeque<(String, AskQuestion, Option<Value>)>,
    active: Option<String>,
}

impl NativeQuestions {
    pub(in crate::tui) fn active_id(&self) -> Option<&str> {
        self.active.as_deref()
    }
}

impl App {
    pub(in crate::tui) fn native_active_question(&self) -> Option<&str> {
        self.native
            .as_ref()
            .and_then(|native| native.questions.active_id())
    }

    pub(super) fn clear_native_questions(&mut self) {
        self.pending_ask_user = None;
        self.native.as_mut().unwrap().questions = NativeQuestions::default();
    }

    pub(super) fn native_question_notice(&mut self, notice: &Value) {
        let payload = &notice["payload"];
        let Some(id) = payload["rpc_id"].as_str() else {
            return;
        };
        if notice["kind"] == "question_resolved" {
            self.native_question_resolved(id);
            return;
        }
        let Ok(question) = serde_json::from_value::<AskQuestion>(payload["question"].clone())
        else {
            self.flash_message(UiMessage::new("tui.host_bad_question"));
            return;
        };
        if let Some(document) = payload.get("plan_review").filter(|value| !value.is_null()) {
            let session = document["session"].as_str();
            if session.is_none()
                || self
                    .session_id
                    .as_ref()
                    .is_some_and(|id| Some(id.as_str()) != session)
                || session
                    .and_then(|id| super::super::plan_review::PlanReview::new(document, id))
                    .is_none()
            {
                self.flash_message(UiMessage::new("tui.host_bad_question"));
                return;
            }
        }
        let questions = &mut self.native.as_mut().unwrap().questions;
        if questions
            .pending
            .iter()
            .any(|(pending, _, _)| pending == id)
        {
            return;
        }
        questions.pending.push_back((
            id.into(),
            question,
            payload
                .get("plan_review")
                .filter(|value| !value.is_null())
                .cloned(),
        ));
        self.show_native_question();
    }

    fn show_native_question(&mut self) {
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        if native.questions.active.is_some() {
            return;
        }
        let Some((id, question, plan)) = native.questions.pending.front().cloned() else {
            return;
        };
        native.questions.active = Some(id.clone());
        let (epoch, client) = (native.epoch, native.client.clone());
        let (answer_tx, receiver) = mpsc::channel();
        self.handle_worker_message(WorkerMessage::AskUserRequest {
            question,
            answer_tx,
        });
        if let Some(plan) = plan {
            self.install_native_plan_review(&plan);
        }
        thread::spawn(move || {
            // Dropping this local dialog is not a decline on another client's behalf.
            let Ok(answer) = receiver.recv_timeout(Duration::from_secs(600)) else {
                return;
            };
            let result = client.call("question.respond", &json!({"rpcId":id,"answer":answer}));
            let _ = ui.send(UiEvent::Native(NativeEvent::QuestionReply(
                epoch, id, answer, result,
            )));
        });
    }

    pub(in crate::tui) fn native_question_resolved(&mut self, id: &str) {
        let questions = &mut self.native.as_mut().unwrap().questions;
        questions.pending.retain(|(pending, _, _)| pending != id);
        if questions.active.as_deref() == Some(id) {
            questions.active = None;
            self.pending_ask_user = None;
        }
        self.show_native_question();
    }

    pub(super) fn native_question_reply(
        &mut self,
        epoch: u64,
        id: &str,
        answer: AskAnswer,
        result: Result<Value, String>,
    ) {
        if !self.native.as_ref().is_some_and(|native| {
            native.epoch == epoch && native.questions.active.as_deref() == Some(id)
        }) {
            return;
        }
        match result {
            Ok(_) => self.native_question_resolved(id),
            Err(error) => {
                self.native.as_mut().unwrap().questions.active = None;
                self.show_native_question();
                if let Some(dialog) = &mut self.pending_ask_user {
                    match answer {
                        AskAnswer::Custom(text) => dialog.custom = Some(text),
                        AskAnswer::Selected(label) => {
                            dialog.selection = dialog
                                .question
                                .options
                                .iter()
                                .position(|option| option.label == label)
                                .unwrap_or(0)
                        }
                        AskAnswer::Declined => {}
                    }
                }
                self.flash_message(
                    UiMessage::new("tui.message_value_check_host_before_retrying_answer_retained")
                        .arg("error", (error).to_string()),
                );
            }
        }
    }
}
