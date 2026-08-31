//! Process-local QR binding state machine shared by terminal and web UIs.
//!
//! QR handles and image content are credentials-in-formation: this type never
//! implements `Debug` or serialization, and the only durable value it can
//! yield is a confirmed [`Credentials`] object for the control store.

use super::ilink::{Client, Credentials, Error, QrChallenge, QrState};

pub(crate) struct BindingSession {
    client: Client,
    challenge: QrChallenge,
}

pub(crate) enum BindingStep {
    Waiting,
    Scanned,
    NeedVerifyCode,
    VerifyCodeBlocked,
    Expired,
    AlreadyBound,
    Confirmed(Credentials),
}

impl BindingSession {
    pub(crate) fn start() -> Result<(Self, String), Error> {
        Self::start_with(Client::new())
    }

    fn start_with(client: Client) -> Result<(Self, String), Error> {
        let challenge = client.start_qr(&[])?;
        let image_content = challenge.image_content.clone();
        Ok((Self { client, challenge }, image_content))
    }

    pub(crate) fn poll(&mut self, verify_code: Option<&str>) -> Result<BindingStep, Error> {
        // Redirect is protocol routing, not a user-visible terminal state. A
        // bounded loop consumes at most one redirect hop per poll call.
        let state = self.client.poll_qr(&self.challenge.code, verify_code)?;
        let state = match state {
            QrState::Redirect { origin } => {
                self.client = self.client.at_origin(&origin)?;
                self.client.poll_qr(&self.challenge.code, verify_code)?
            }
            state => state,
        };
        Ok(match state {
            QrState::Waiting | QrState::Redirect { .. } => BindingStep::Waiting,
            QrState::Scanned => BindingStep::Scanned,
            QrState::NeedVerifyCode => BindingStep::NeedVerifyCode,
            QrState::VerifyCodeBlocked => BindingStep::VerifyCodeBlocked,
            QrState::Expired => BindingStep::Expired,
            QrState::AlreadyBound => BindingStep::AlreadyBound,
            QrState::Confirmed(credentials) => BindingStep::Confirmed(credentials),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::im::ilink::{Request, Response, Transport};
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    struct Fake {
        responses: Mutex<VecDeque<Response>>,
    }

    impl Transport for Fake {
        fn execute(&self, _request: Request) -> Result<Response, Error> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| Error::Transport("binding fixture exhausted unexpectedly".into()))
        }
    }

    #[test]
    fn redirect_is_consumed_without_exposing_a_non_official_origin() {
        let fake = Arc::new(Fake {
            responses: Mutex::new(
                [
                    json!({"qrcode":"opaque","qrcode_img_content":"content"}),
                    json!({"status":"scaned_but_redirect","redirect_host":"regional.weixin.qq.com"}),
                    json!({"status":"confirmed","bot_token":"token","ilink_bot_id":"bot"}),
                ]
                .into_iter()
                .map(|value| Response {
                    status: 200,
                    body: serde_json::to_vec(&value).unwrap(),
                })
                .collect(),
            ),
        });
        let (mut binding, qr) = BindingSession::start_with(Client::with_transport(fake)).unwrap();
        assert_eq!(qr, "content");
        let BindingStep::Confirmed(credentials) = binding.poll(None).unwrap() else {
            panic!("expected confirmed")
        };
        assert_eq!(credentials.origin(), "https://regional.weixin.qq.com");
    }
}
