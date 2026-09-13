use super::*;
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};

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
