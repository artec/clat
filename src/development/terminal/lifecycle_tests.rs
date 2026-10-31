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
