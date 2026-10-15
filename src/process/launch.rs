use super::*;

/// Piped and PTY children have identical monitor ownership and failure cleanup.
pub(super) fn monitor(
    entry: &Arc<ProcessEntry>,
    child: ChildHandle,
    readers: Vec<JoinHandle<()>>,
    done: Vec<Arc<AtomicBool>>,
) -> Result<(), String> {
    let monitor_entry = Arc::clone(entry);
    let child_slot = Arc::new(Mutex::new(Some(child)));
    let monitor_child = Arc::clone(&child_slot);
    let reader_slot = Arc::new(Mutex::new(Some((readers, done))));
    let monitor_readers = Arc::clone(&reader_slot);
    let label = if entry.tty { "PTY" } else { "process" };
    let thread = std::thread::Builder::new()
        .name(format!("clat-{label}-{}", entry.id))
        .spawn(move || {
            let child = monitor_child
                .lock()
                .expect("process child slot")
                .take()
                .expect("process child present");
            let (readers, done) = monitor_readers
                .lock()
                .expect("process reader slot")
                .take()
                .expect("process readers present");
            monitor_process(monitor_entry, child, readers, done);
        })
        .map_err(|error| {
            if let Some(mut child) = child_slot.lock().expect("process child slot").take() {
                let _ = child.terminate_tree();
            }
            if let Some((readers, _)) = reader_slot.lock().expect("process reader slot").take() {
                for reader in readers {
                    join_reader_after_terminate(reader);
                }
            }
            format!("{label} monitor spawn failed: {error}")
        })?;
    entry.set_monitor(thread);
    Ok(())
}
