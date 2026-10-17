//! Run-owned, bounded cumulative output observations; never source authentication.
use super::{ProcessOutput, StreamKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

const LINE_BYTES: usize = 4096;
const MAX_SUMMARIES: u64 = 64;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VerificationSpec {
    pub format: Format,
    pub minimum_executed: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(crate) enum Format {
    #[serde(rename = "rust-test-summary")]
    RustTestSummary,
    #[serde(rename = "playwright-summary")]
    PlaywrightSummary,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Self::RustTestSummary => "rust-test-summary",
            Self::PlaywrightSummary => "playwright-summary",
        }
    }
    fn observe(self, line: &str) -> Option<Value> {
        match self {
            Self::RustTestSummary => super::verification::observe_rust(line),
            Self::PlaywrightSummary => super::playwright::observe(line),
        }
    }
}

#[derive(Clone)]
pub(super) struct Context {
    fingerprint: String,
    minimum: Option<u64>,
    format: Option<Format>,
}
impl Context {
    pub(super) fn new(
        argv: &[std::ffi::OsString],
        cwd: &Path,
        spec: Option<VerificationSpec>,
    ) -> Self {
        let mut hash = Sha256::new();
        hash.update(argv.len().to_le_bytes());
        for arg in argv {
            let bytes = arg.as_encoded_bytes();
            hash.update(bytes.len().to_le_bytes());
            hash.update(bytes);
        }
        hash.update(cwd.as_os_str().as_encoded_bytes());
        Self {
            fingerprint: format!("{:x}", hash.finalize()),
            minimum: spec.map(|s| s.minimum_executed),
            format: spec.map(|s| s.format),
        }
    }
}

#[derive(Default)]
struct Line {
    bytes: Vec<u8>,
    dropping: bool,
}

pub(super) struct Receipt {
    context: Context,
    lines: [Line; 2],
    summaries: u64,
    passed: u64,
    failed: u64,
    ignored: u64,
    flaky: u64,
    compile_failed: bool,
    incomplete: bool,
    eof: [bool; 2],
}
impl Receipt {
    #[cfg(test)]
    pub(super) fn test_counts(&self) -> (u64, u64, u64, u64, bool) {
        (
            self.summaries,
            self.passed,
            self.failed,
            self.ignored,
            self.incomplete,
        )
    }
    pub(super) fn new(context: Context) -> Option<Self> {
        context.minimum?;
        Some(Self {
            context,
            lines: Default::default(),
            summaries: 0,
            passed: 0,
            failed: 0,
            ignored: 0,
            flaky: 0,
            compile_failed: false,
            incomplete: false,
            eof: [false; 2],
        })
    }

    pub(super) fn append(&mut self, stream: StreamKind, bytes: &[u8]) {
        let index = match stream {
            StreamKind::Stdout => 0,
            StreamKind::Stderr => 1,
            #[cfg(not(windows))]
            StreamKind::Pty => {
                self.incomplete = true;
                return;
            }
        };
        for &byte in bytes {
            if byte == b'\n' {
                let line = std::mem::take(&mut self.lines[index]);
                if !line.dropping {
                    self.observe(&line.bytes);
                }
            } else if !self.lines[index].dropping {
                if self.lines[index].bytes.len() == LINE_BYTES {
                    self.lines[index].bytes.clear();
                    self.lines[index].dropping = true;
                    self.incomplete = true;
                } else {
                    self.lines[index].bytes.push(byte);
                }
            }
        }
    }

    pub(super) fn finish(&mut self, stream: StreamKind, eof: bool) {
        match stream {
            StreamKind::Stdout => self.eof[0] = eof,
            StreamKind::Stderr => self.eof[1] = eof,
            #[cfg(not(windows))]
            StreamKind::Pty => {}
        }
        if !eof {
            self.incomplete = true;
        }
    }

    fn observe(&mut self, bytes: &[u8]) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            self.incomplete = true;
            return;
        };
        let text = super::verification::plain_signature(text.trim_end_matches('\r'));
        let Some(row) = self
            .context
            .format
            .expect("verification format")
            .observe(&text)
        else {
            if matches!(self.context.format, Some(Format::RustTestSummary))
                && text.starts_with("test result: ")
                || matches!(self.context.format, Some(Format::PlaywrightSummary))
                    && super::playwright::is_summary_candidate(&text)
            {
                self.incomplete = true;
            }
            return;
        };
        if row["status"] == "compile_failed" {
            self.compile_failed = true;
            return;
        }
        if self.summaries == MAX_SUMMARIES {
            self.incomplete = true;
            return;
        }
        self.summaries += 1;
        for (count, key) in [
            (&mut self.passed, "passed"),
            (&mut self.failed, "failed"),
            (&mut self.ignored, "ignored"),
            (&mut self.flaky, "flaky"),
        ] {
            if let Some(sum) = count.checked_add(row[key].as_u64().unwrap_or(0)) {
                *count = sum;
            } else {
                self.incomplete = true;
            }
        }
        if self
            .passed
            .checked_add(self.failed)
            .and_then(|v| v.checked_add(self.flaky))
            .is_none()
        {
            self.incomplete = true;
        }
    }

    pub(super) fn json(&self, output: &ProcessOutput, generation: u64) -> Value {
        let complete = self.eof.iter().all(|eof| *eof)
            && !self.incomplete
            && self.lines.iter().all(|l| l.bytes.is_empty() && !l.dropping);
        let executed = self
            .passed
            .saturating_add(self.failed)
            .saturating_add(self.flaky);
        let (status, action) = self.verdict(output, complete, executed);
        json!({"version":1,"format":self.context.format.expect("verification format").name(),"session_id":output.session_id,"owner_generation":generation,
            "invocation_sha256":self.context.fingerprint,"execution":"argv","exit_scope":"direct_child",
            "status":status,"next_action":action,"exit_code":output.exit_code,"signal":output.signal,
            "timed_out":output.timed_out,"cancelled":output.cancelled,"terminated":output.terminated,
            "evidence":"output_signature","authenticated":false,"scan_complete":complete,
            "summaries":self.summaries,"passed":self.passed,"failed":self.failed,"ignored":self.ignored,
            "flaky":self.flaky,"executed":executed,"minimum_executed":self.context.minimum,"compile_failed":self.compile_failed})
    }

    fn verdict(
        &self,
        output: &ProcessOutput,
        complete: bool,
        executed: u64,
    ) -> (&'static str, &'static str) {
        if output.running {
            return (
                "running",
                "Poll this process; no terminal verification claim yet.",
            );
        }
        if output.cancelled
            || output.timed_out
            || output.terminated
            || output.sandbox_denied
            || output.sandbox_unavailable
        {
            return (
                "inconclusive",
                "Execution was interrupted or refused; inspect lifecycle and sandbox facts.",
            );
        }
        if output.exit_code != Some(0) || self.failed > 0 || self.compile_failed {
            return (
                "failed",
                "Inspect the failure before claiming verification passed; no automatic retry was performed.",
            );
        }
        if self.flaky > 0 {
            return (
                "inconclusive",
                "Flaky results observed; inspect retries before claiming stable verification. No automatic rerun was performed.",
            );
        }
        if !complete || executed < self.context.minimum.expect("verification enabled") {
            return (
                "inconclusive",
                "No complete nonempty verification evidence: inspect the filter, ignored tests or incomplete output.",
            );
        }
        (
            "passed",
            "Direct process succeeded with the requested output evidence; review test relevance, not just counts. This does not authenticate coverage.",
        )
    }
}
