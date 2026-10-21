//! The readiness announcement does not transfer ownership of the stdout pipe.
use std::io::BufRead;
use std::sync::mpsc::Sender;

pub fn drain(reader: &mut impl BufRead, ready: Sender<Option<u16>>) -> std::io::Result<()> {
    let mut ready = Some(ready);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            if let Some(ready) = ready.take() {
                let _ = ready.send(None);
            }
            return Ok(());
        }
        if ready.is_some()
            && let Some(port) = line
                .trim()
                .strip_prefix("clat serve listening on http://127.0.0.1:")
                .and_then(|rest| rest.trim_end_matches('/').parse::<u16>().ok())
        {
            let _ = ready.take().unwrap().send(Some(port));
        }
        // Keep consuming after readiness: closing here races later println!
        // calls and can terminate the real Host with a broken stdout pipe.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor, Read};
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    struct DelayedOutput {
        prefix: Cursor<Vec<u8>>,
        tail: Cursor<Vec<u8>>,
        release: Receiver<()>,
        read_tail: Option<Sender<()>>,
    }
    impl Read for DelayedOutput {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let n = self.prefix.read(bytes)?;
            if n > 0 {
                return Ok(n);
            }
            if let Some(entered) = self.read_tail.take() {
                let _ = entered.send(());
                self.release
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(std::io::Error::other)?;
            }
            self.tail.read(bytes)
        }
    }

    #[test]
    fn readiness_is_reported_before_later_output_is_drained_to_eof() {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (tail_tx, tail_rx) = mpsc::channel();
        let output = DelayedOutput {
            prefix: Cursor::new(b"clat serve listening on http://127.0.0.1:12345/\n".to_vec()),
            tail: Cursor::new(b"Press Ctrl-C to stop.\n".repeat(8192)),
            release: release_rx,
            read_tail: Some(tail_tx),
        };
        let worker = std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            drain(&mut reader, ready_tx).unwrap();
            let output = reader.into_inner();
            (output.tail.position(), output.tail.get_ref().len() as u64)
        });
        assert_eq!(
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Some(12345)
        );
        tail_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("readiness must not close the pipe before later startup output");
        release_tx.send(()).unwrap();
        let (consumed, total) = worker.join().unwrap();
        assert_eq!(consumed, total, "all later output must drain through EOF");
    }
}
