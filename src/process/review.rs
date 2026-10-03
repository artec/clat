//! Read-only current-run process facts. No cursor consumption or lifetime extension.
use super::*;
use serde_json::{Value, json};
const LOG_CAP: usize = 16 * 1024;
const RETENTION: &str = "Current run generation only; in-memory bounded tails. Drained entries may disappear on the next spawn; all entries disappear at run cleanup/host restart. Not background jobs; no process cancel/stdin authority here.";

pub(crate) fn unavailable() -> Value {
    json!({"state":"unavailable","note":RETENTION,"tasks":[]})
}

impl ProcessService {
    pub(crate) fn review_list(&self, session: &str) -> Value {
        let state = self.state.lock().expect("process service lock");
        let Some(owner) = state.owner.as_ref().filter(|o| o.session_id == session) else {
            return unavailable();
        };
        let mut entries: Vec<_> = state
            .entries
            .values()
            .filter(|e| e.owner_generation == owner.generation && e.owner_session_id == session)
            .collect();
        entries.sort_by_key(|e| e.id);
        json!({"state":"available","session":session,"generation":owner.generation,"note":RETENTION,
            "tasks":entries.iter().take(32).map(|e|e.review(false)).collect::<Vec<_>>(),"truncated":entries.len()>32})
    }

    pub(crate) fn review_log(
        &self,
        session: &str,
        generation: u64,
        id: u64,
    ) -> Result<Value, String> {
        let owner = self.current_owner()?;
        if owner.session_id != session || owner.generation != generation {
            return Err("Process log owner/generation changed; refresh tasks".into());
        }
        let entry = self.entry_for_owner(id, &owner)?;
        Ok(entry.review(true))
    }
}

impl ProcessEntry {
    fn review(&self, logs: bool) -> Value {
        let state = self.state.lock().expect("process entry lock");
        let terminal = state.terminal.as_ref();
        let mut value = json!({"id":self.id,"generation":self.owner_generation,"session":self.owner_session_id,
            "command":self.command.chars().take(2048).collect::<String>(),"command_truncated":self.command.chars().count()>2048,
            "state":if terminal.is_some(){"completed"}else{"running"},"tty":self.tty,
            "exit_code":terminal.and_then(|s|s.exit_code),"signal":terminal.and_then(|s|s.signal.as_ref()),
            "timed_out":terminal.is_some_and(|s|s.timed_out),"cancelled":terminal.is_some_and(|s|s.cancelled),"note":RETENTION});
        if logs {
            for (name, ring) in [
                ("stdout", &state.stdout),
                ("stderr", &state.stderr),
                ("pty", &state.pty),
            ] {
                let start = ring.end_offset().saturating_sub(LOG_CAP as u64);
                let (bytes, _, lost, _) = ring.read_from(start, LOG_CAP);
                let (text, utf8_loss, encoded_cut) = output::decode_output(&bytes, LOG_CAP);
                value[name] = json!({"text":text,"truncated":lost || start>0 || encoded_cut,"utf8_lossy":utf8_loss,"end_offset":ring.end_offset()});
            }
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_review_is_current_owner_only_and_does_not_consume_logs() {
        let (root, service) =
            super::super::tests::fixture("readonly-review", ProcessLimits::default());
        let generation = service.bind_run("session", CancelToken::new()).unwrap();
        let id = service
            .start(ProcessStart {
                command: "echo readonly-log".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap();
        let before = service.review_list("other");
        assert_eq!(before["state"], "unavailable");
        let entry = service.entry_for_current_owner(id).unwrap();
        entry.wait_until(Duration::from_secs(2));
        let first = service.review_log("session", generation, id).unwrap();
        let second = service.review_log("session", generation, id).unwrap();
        assert_eq!(first["stdout"], second["stdout"]);
        assert!(
            first["stdout"]["text"]
                .as_str()
                .unwrap()
                .contains("readonly-log")
        );
        assert!(first["stdout"]["text"].as_str().unwrap().len() <= LOG_CAP);
        assert!(service.review_log("other", generation, id).is_err());
        assert!(service.review_log("session", generation + 1, id).is_err());
        let output = service.wait_and_consume(id, Duration::ZERO, 128).unwrap();
        assert!(output.stdout.contains("readonly-log"));
        let before = entry.state.lock().unwrap().last_activity;
        {
            let mut state = entry.state.lock().unwrap();
            state.stdout.append(&vec![0xff; LOG_CAP * 2]);
        }
        let cursor = entry.state.lock().unwrap().cursors.stdout;
        let capped = service.review_log("session", generation, id).unwrap();
        assert!(capped["stdout"]["text"].as_str().unwrap().len() <= LOG_CAP);
        assert_eq!(capped["stdout"]["truncated"], true);
        assert_eq!(capped["stdout"]["utf8_lossy"], true);
        let state = entry.state.lock().unwrap();
        assert_eq!(state.cursors.stdout, cursor);
        assert_eq!(state.last_activity, before);
        drop(state);
        service.unbind_run(generation).unwrap();
        assert_eq!(service.review_list("session")["state"], "unavailable");
        assert!(service.review_log("session", generation, id).is_err());
        crate::test_support::cleanup_tree(&root);
    }
}
