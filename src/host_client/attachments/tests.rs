use super::*;
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};

#[test]
fn shared_input_preserves_references_with_images_and_selection() {
    let share = crate::application::SessionShareSelection {
        reference: crate::application::HistoryReference {
            project: Some("/project".into()),
            session: "source".into(),
            seq: None,
        },
        recent_turns: Some(2),
        version: "version".into(),
    };
    let params = message_params(
        "",
        "message",
        7,
        &[share],
        Some(("scope", vec!["upload".into()])),
    );
    assert_eq!(params["sessionShares"][0]["reference"]["session"], "source");
    assert_eq!(params["attachments"][0], "upload");
    assert_eq!(params["draftScopeId"], "scope");
    assert_eq!(params["expected_selection_generation"], 7);
}

fn stream(frames: Vec<(&str, Value)>) -> super::super::HostEvents {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let (mut server, _) = listener.accept().unwrap();
    for (kind, value) in frames {
        write!(server, "event: {kind}\ndata: {value}\n\n").unwrap();
    }
    super::super::HostEvents::new(BufReader::new(client))
}

#[test]
fn image_steering_waits_for_its_own_durable_claim_and_retains_unclaimed_draft() {
    let event = |id: &str| {
        let receipt = crate::AdmissionReceipt::committed(id.into(), "message".into(), vec![]);
        let run = crate::RunEvent::SteeringApplied {
            message: crate::MessageContent::text("image steering"),
            client_message_id: Some(id.into()),
            request_digest: None,
            receipt: Some(Box::new(receipt)),
        };
        serde_json::from_str::<Value>(&crate::wire::envelope_line(&run)).unwrap()
    };
    let mut events = stream(vec![("event", event("other")), ("event", event("wanted"))]);
    let result = wait_for_image_claim(&mut events, "wanted").unwrap();
    assert_eq!(result["receipt"]["client_message_id"], "wanted");
    assert_eq!(result["receipt"]["state"], "committed");
    let mut events = stream(vec![(
        "prompt.settled",
        json!({"v":1,"ctl":{"outcome":{"type":"cancelled"}}}),
    )]);
    assert!(wait_for_image_claim(&mut events, "wanted").is_err());
    let mut events = stream(vec![]);
    assert!(wait_for_image_claim(&mut events, "wanted").is_err());
}
