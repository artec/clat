//! File-backed capture avoids waiting for pipe EOF from inherited handles.
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub struct BoundedProcess {
    child: Child,
    label: String,
    stdout: PathBuf,
    stderr: PathBuf,
    endpoint: PathBuf,
    deadline: Instant,
    started: Instant,
}

impl BoundedProcess {
    pub fn spawn(
        command: &mut Command,
        root: &Path,
        label: &str,
        budget: Duration,
    ) -> io::Result<Self> {
        let stdout = root.join(format!("{label}.stdout"));
        let stderr = root.join(format!("{label}.stderr"));
        command
            .stdout(Stdio::from(File::create(&stdout)?))
            .stderr(Stdio::from(File::create(&stderr)?));
        let started = Instant::now();
        let deadline = started + budget;
        Ok(Self {
            child: command.spawn()?,
            label: label.into(),
            stdout,
            stderr,
            endpoint: root.join("home/.clat/host-endpoint.json"),
            deadline,
            started,
        })
    }

    pub fn shorten_deadline(&mut self, budget: Duration) {
        self.deadline = self.deadline.min(Instant::now() + budget);
    }

    pub fn finish(&mut self) -> Result<Output, String> {
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    return Ok(Output {
                        status,
                        stdout: read_capture(&self.stdout),
                        stderr: read_capture(&self.stderr),
                    });
                }
                Err(error) => return Err(self.failure(&format!("try_wait failed: {error}"))),
                Ok(None) => {}
            }
            if Instant::now() >= self.deadline {
                return Err(self.failure("deadline exceeded"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn failure(&mut self, reason: &str) -> String {
        let before = self.child.try_wait();
        let kill = self.child.kill();
        // Polling stays bounded even if the OS does not complete the kill.
        let reap_deadline = Instant::now() + Duration::from_secs(1);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < reap_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        format!(
            "{} pid={}: {reason} after {:?}; try_wait before={before:?}; kill={kill:?}; after={:?}; endpoint={}; stdout={}; stderr={}",
            self.label,
            self.child.id(),
            self.started.elapsed(),
            self.child.try_wait(),
            String::from_utf8_lossy(&read_capture(&self.endpoint)),
            String::from_utf8_lossy(&read_capture(&self.stdout)),
            String::from_utf8_lossy(&read_capture(&self.stderr)),
        )
    }
}

impl Drop for BoundedProcess {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            eprintln!("{}", self.failure("cleaning up remaining child"));
        }
    }
}

fn read_capture(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    match File::open(path).and_then(|file| file.take(64 * 1024).read_to_end(&mut bytes)) {
        Ok(_) => bytes,
        Err(error) => format!("<{}: {error}>", path.display()).into_bytes(),
    }
}
