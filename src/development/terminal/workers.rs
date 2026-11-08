use super::*;
pub(super) fn start_workers(
    entry: &Arc<Entry>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    queue: mpsc::Receiver<Vec<u8>>,
) -> Result<(), String> {
    read_worker(entry, reader)?;
    write_worker(entry, writer, queue)?;
    monitor_worker(entry)
}
fn read_worker(entry: &Arc<Entry>, mut reader: Box<dyn Read + Send>) -> Result<(), String> {
    let producer = entry.lifetime.producer()?;
    let output = entry.clone();
    let read = std::thread::Builder::new()
        .name("clat-terminal-output".into())
        .spawn(move || {
            let _producer = producer;
            let mut buffer = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                output.append(&buffer[..n]);
            }
            output.output_done.store(true, Ordering::SeqCst);
            output.notify_output();
        })
        .map_err(io)?;
    entry
        .workers
        .lock()
        .map_err(|_| "terminal_busy")?
        .push(read);
    Ok(())
}
fn write_worker(
    entry: &Arc<Entry>,
    mut writer: Box<dyn Write + Send>,
    queue: mpsc::Receiver<Vec<u8>>,
) -> Result<(), String> {
    let producer = entry.lifetime.producer()?;
    let target = entry.clone();
    let write = std::thread::Builder::new()
        .name("clat-terminal-input".into())
        .spawn(move || {
            let _producer = producer;
            while !target.closed.load(Ordering::SeqCst) {
                match queue.recv_timeout(Duration::from_millis(100)) {
                    Ok(bytes) => {
                        if writer
                            .write_all(&bytes)
                            .and_then(|_| writer.flush())
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
            }
        })
        .map_err(io)?;
    entry
        .workers
        .lock()
        .map_err(|_| "terminal_busy")?
        .push(write);
    Ok(())
}
fn monitor_worker(entry: &Arc<Entry>) -> Result<(), String> {
    let producer = entry.lifetime.producer()?;
    let target = entry.clone();
    let monitor = std::thread::Builder::new()
        .name("clat-terminal-lifecycle".into())
        .spawn(move || {
            let _producer = producer;
            loop {
                if target.closed.load(Ordering::SeqCst)
                    || target.platform.lock().expect("PTY").ended().unwrap_or(true)
                {
                    if target.end_platform().is_err() {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    target.closed.store(true, Ordering::SeqCst);
                    target.notify_output();
                    target.metadata.lock().expect("terminal metadata").phase = "ended".into();
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .map_err(io)?;
    entry
        .workers
        .lock()
        .map_err(|_| "terminal_busy")?
        .push(monitor);
    Ok(())
}
