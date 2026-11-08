use super::*;
struct Control(Arc<std::sync::atomic::AtomicBool>);
impl platform::Control for Control {
    fn resize(&mut self, _: u16, _: u16) -> Result<(), String> {
        Ok(())
    }
    fn ended(&mut self) -> Result<bool, String> {
        Ok(false)
    }
    fn terminate(&mut self) -> Result<(), String> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err("cleanup_failed".into())
        }
    }
}
#[test]
fn failed_terminal_termination_retains_producer_lease_until_actual_stop() {
    let lifetime = Arc::new(super::lifecycle::Lifecycle::default());
    let lease = Arc::new(());
    let weak = Arc::downgrade(&lease);
    lifetime.attach(lease);
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entry = service::start_entry(
        Metadata {
            id: "lease-probe".into(),
            name: "probe".into(),
            shell: "fixture".into(),
            directory: "".into(),
            phase: "running".into(),
            signature: "".into(),
        },
        "client".into(),
        platform::Session {
            reader: Box::new(std::io::Cursor::new(vec![])),
            writer: Box::new(std::io::sink()),
            control: Box::new(Control(release.clone())),
        },
        (24, 80),
        lifetime.clone(),
    )
    .unwrap();
    lifetime.close();
    let failed = entry.close();
    let retained = weak.upgrade().is_some() && lifetime.active();
    release.store(true, Ordering::SeqCst);
    entry.close().unwrap();
    assert!(failed.is_err(), "failed cleanup must be reported");
    assert!(
        retained,
        "an unfinished process must retain Host exclusion even after its I/O workers stop"
    );
    assert!(weak.upgrade().is_none());
    assert_eq!(entry.metadata.lock().unwrap().phase, "ended");
}
struct ChannelReader(std::sync::mpsc::Receiver<Vec<u8>>);
impl std::io::Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // Bounded: a stalled fixture channel ends its stream instead of
        // parking the read worker forever.
        match self.0.recv_timeout(Duration::from_secs(30)) {
            Ok(bytes) => {
                let count = bytes.len().min(buf.len());
                buf[..count].copy_from_slice(&bytes[..count]);
                Ok(count)
            }
            Err(_) => Ok(0),
        }
    }
}
fn probe_terminals() -> Terminals {
    Terminals::new(
        Project::new(&std::env::temp_dir()),
        store::Store::new(
            std::env::temp_dir().join(format!("clat-terminal-probe-{}", uuid::Uuid::new_v4())),
        ),
    )
}
fn probe_entry(reader: Box<dyn std::io::Read + Send>) -> Arc<Entry> {
    service::start_entry(
        Metadata {
            id: "probe".into(),
            name: "probe".into(),
            shell: "fixture".into(),
            directory: "".into(),
            phase: "running".into(),
            signature: "".into(),
        },
        "client".into(),
        platform::Session {
            reader,
            writer: Box::new(std::io::sink()),
            control: Box::new(Control(Arc::new(std::sync::atomic::AtomicBool::new(true)))),
        },
        (24, 80),
        Arc::new(super::lifecycle::Lifecycle::default()),
    )
    .unwrap()
}
#[test]
fn output_wait_wakes_on_append_instead_of_sleeping_to_the_timeout() {
    let (send, receive) = std::sync::mpsc::channel::<Vec<u8>>();
    let entry = probe_entry(Box::new(ChannelReader(receive)));
    let terminals = Arc::new(probe_terminals());
    terminals
        .entries
        .lock()
        .unwrap()
        .insert("probe".into(), entry.clone());
    let waiter = Arc::clone(&terminals);
    let waiting = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        waiter
            .wait_for_output("probe", 0, Duration::from_millis(500))
            .unwrap();
        started.elapsed()
    });
    std::thread::sleep(Duration::from_millis(60));
    send.send(b"echo!".to_vec()).unwrap();
    let elapsed = waiting.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(400),
        "wait must wake on append rather than burn the full timeout (took {elapsed:?})"
    );
    let output = entry.output.lock().unwrap();
    assert_eq!(
        output.end, 5,
        "the appended bytes must be observable after the wake"
    );
    drop(output);
    drop(send);
    entry.close().unwrap();
}
#[test]
fn output_wait_times_out_without_new_bytes() {
    let (send, receive) = std::sync::mpsc::channel::<Vec<u8>>();
    let entry = probe_entry(Box::new(ChannelReader(receive)));
    let terminals = Arc::new(probe_terminals());
    terminals
        .entries
        .lock()
        .unwrap()
        .insert("probe".into(), entry.clone());
    let started = std::time::Instant::now();
    terminals
        .wait_for_output("probe", 0, Duration::from_millis(300))
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(290),
        "an idle wait must still honor its timeout as a lower bound"
    );
    drop(send);
    entry.close().unwrap();
}
#[test]
fn terminal_list_does_not_queue_behind_the_input_lane() {
    let terminals = Arc::new(probe_terminals());
    let holder = Arc::clone(&terminals);
    let (held, held_receive) = std::sync::mpsc::channel::<()>();
    let lane_holder = std::thread::spawn(move || {
        let _guard = holder.lane.lock().unwrap();
        let _ = held.send(());
        std::thread::park_timeout(Duration::from_secs(5));
    });
    held_receive.recv_timeout(Duration::from_secs(2)).unwrap();
    let probe = Arc::clone(&terminals);
    let (done, done_receive) = std::sync::mpsc::channel::<Result<Value, String>>();
    std::thread::spawn(move || {
        let _ = done.send(probe.request("terminal.list", &json!({})));
    });
    match done_receive.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(value)) => assert!(value["terminals"].is_array(), "{value}"),
        Ok(Err(error)) => panic!("terminal.list failed: {error}"),
        Err(_) => panic!("terminal.list blocked behind the input lane"),
    }
    lane_holder.thread().unpark();
    lane_holder.join().unwrap();
}
