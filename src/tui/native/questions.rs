//! Question queue and dialog lifetime only; the host arbitrates every answer.
use super::*;
use crate::interaction::{AskAnswer, AskQuestion};
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct NativeQuestions {
    pending: VecDeque<(String, AskQuestion)>,
    active: Option<String>,
}

impl App {
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
            self.flash_status("invalid host question; no answer sent");
            return;
        };
        let questions = &mut self.native.as_mut().unwrap().questions;
        if questions.pending.iter().any(|(pending, _)| pending == id) {
            return;
        }
        questions.pending.push_back((id.into(), question));
        self.show_native_question();
    }

    fn show_native_question(&mut self) {
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        if native.questions.active.is_some() {
            return;
        }
        let Some((id, question)) = native.questions.pending.front().cloned() else {
            return;
        };
        native.questions.active = Some(id.clone());
        let (epoch, client) = (native.epoch, native.client.clone());
        let (answer_tx, receiver) = mpsc::channel();
        self.handle_worker_message(WorkerMessage::AskUserRequest {
            question,
            answer_tx,
        });
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

    fn native_question_resolved(&mut self, id: &str) {
        let questions = &mut self.native.as_mut().unwrap().questions;
        questions.pending.retain(|(pending, _)| pending != id);
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
                self.flash_status(format!(
                    "{error} · check host before retrying; answer retained"
                ));
            }
        }
    }
}
