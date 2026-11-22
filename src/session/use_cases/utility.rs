//! CLAT-private utility scheduling. Same-turn naming is coalesced before I/O.
use super::*;
use crate::i18n::Locale;
use crate::session::catalog::conversation_summary::{self, ConversationSummary};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::Read as _;
use std::sync::Weak;

const BUDGET_FILE: &str = "clat-utility-budget.json";
const BYTE_CAP: u64 = 1024;
const CONTEXT_CHARS: usize = 6000;
const CONTEXT_MESSAGES: usize = conversation_summary::MAX_SOURCE_MESSAGES;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UtilityBudget {
    version: u64,
    title_attempts: u64,
    #[serde(default)]
    suggestion_attempts: u64,
    last_title_turn: u64,
    last_title_ms: i64,
}

impl Default for UtilityBudget {
    fn default() -> Self {
        Self {
            version: 1,
            title_attempts: 0,
            suggestion_attempts: 0,
            last_title_turn: 0,
            last_title_ms: 0,
        }
    }
}

pub(crate) struct TitleAttempt {
    pub(crate) expectation: SetTitleExpectation,
    pub(crate) context: String,
    pub(crate) message_seqs: Vec<u64>,
    pub(crate) include_title: bool,
    pub(crate) summary_locale: Option<Locale>,
    coordinator: Weak<SessionCoordinator>,
}

pub(crate) struct SuggestionAttempt {
    pub(crate) context: String,
}

impl SessionService {
    #[cfg(test)]
    pub(crate) fn prepare_title_attempt(
        &self,
        session: &SessionId,
    ) -> Result<Option<TitleAttempt>, SessionError> {
        self.prepare_title_attempt_at(session, now_ms())
    }

    #[cfg(test)]
    pub(super) fn prepare_title_attempt_at(
        &self,
        session: &SessionId,
        time_ms: i64,
    ) -> Result<Option<TitleAttempt>, SessionError> {
        self.prepare_overview(session, time_ms, true, false, None)
    }

    pub(crate) fn prepare_overview(
        &self,
        session: &SessionId,
        time_ms: i64,
        naming_enabled: bool,
        summary_enabled: bool,
        client_locale: Option<Locale>,
    ) -> Result<Option<TitleAttempt>, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(None);
        };
        if active.key.id != *session || active.coordinator.is_read_only() {
            return Ok(None);
        }
        fold_if_behind(active, &self.backend)?;
        let (turns, expectation, include_title, summary_locale) = {
            let projections = active.projections.lock().expect("projections");
            let title = projections.state_snapshot("title").unwrap_or_default();
            let include_title = naming_enabled && title["source"].as_str() != Some("user");
            if !include_title && !summary_enabled {
                return Ok(None);
            }
            let summary_locale = summary_enabled.then(|| {
                client_locale
                    .or_else(|| summary_snapshot(&projections).map(|item| item.locale))
                    .unwrap_or_default()
            });
            let turns = projections
                .state_snapshot("stats")
                .and_then(|stats| stats.get("turns").and_then(Value::as_u64));
            let expectation = title
                .get("eventSeq")
                .and_then(Value::as_u64)
                .map(SetTitleExpectation::Exact)
                .unwrap_or(SetTitleExpectation::NoTitle);
            (
                turns.unwrap_or(0),
                expectation,
                include_title,
                summary_locale,
            )
        };
        // Admission and settlement of the first turn share one naming slot.
        // The bounded replay context below still requires durable input.
        let turns = turns.max(1);
        let dir = self.backend.open_session_dir(&active.key)?;
        let mut budget = read_budget(&dir)?;
        if budget.title_attempts > 0 && turns <= budget.last_title_turn {
            return Ok(None);
        }
        catch_up_replay(
            &self.backend,
            &active.key,
            active.coordinator.committed_seq(),
            &active.replay,
        )?;
        let (context, message_seqs) =
            bounded_context(&active.replay.lock().expect("replay").replay);
        if context.is_empty() {
            return Ok(None);
        }
        budget.title_attempts = budget.title_attempts.saturating_add(1);
        budget.last_title_turn = turns;
        budget.last_title_ms = time_ms;
        write_budget(&dir, &budget)?;
        Ok(Some(TitleAttempt {
            expectation,
            context,
            message_seqs,
            include_title,
            summary_locale,
            coordinator: Arc::downgrade(&active.coordinator),
        }))
    }

    pub(crate) fn record_conversation_summary(
        &self,
        attempt: &TitleAttempt,
        text: &str,
        locale: Locale,
    ) -> Result<bool, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(false);
        };
        if !attempt
            .coordinator
            .ptr_eq(&Arc::downgrade(&active.coordinator))
        {
            return Ok(false);
        }
        if !conversation_summary::within_limit(text, locale) {
            return Err(SessionError::Corruption(
                "invalid generated summary text".into(),
            ));
        }
        let event = crate::session::run_journal::NewSessionEvent::new(
            conversation_summary::EVENT,
            json!({"text": text, "locale": locale, "messageSeqs": attempt.message_seqs, "updatedAtMs": now_ms()}),
        )
        .log_only();
        active
            .shared_journal(&self.backend)?
            .append_atomic_durable(&[event])
            .map_err(SessionError::Corruption)?;
        Ok(true)
    }

    pub(crate) fn conversation_summary(&self) -> Result<Option<ConversationSummary>, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(None);
        };
        fold_if_behind(active, &self.backend)?;
        let projections = active.projections.lock().expect("projections");
        Ok(summary_snapshot(&projections).and_then(|mut summary| {
            summary.text = conversation_summary::fit_text(&summary.text, summary.locale)?;
            Some(summary)
        }))
    }

    pub(crate) fn prepare_suggestion_attempt(
        &self,
        session: &SessionId,
    ) -> Result<Option<SuggestionAttempt>, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(None);
        };
        if active.key.id != *session || active.coordinator.is_read_only() {
            return Ok(None);
        }
        fold_if_behind(active, &self.backend)?;
        catch_up_replay(
            &self.backend,
            &active.key,
            active.coordinator.committed_seq(),
            &active.replay,
        )?;
        let (context, _) = bounded_context(&active.replay.lock().expect("replay").replay);
        if context.is_empty() {
            return Ok(None);
        }
        Ok(Some(SuggestionAttempt { context }))
    }
}

fn summary_snapshot(projections: &ProjectionRegistry) -> Option<ConversationSummary> {
    serde_json::from_value(projections.state_snapshot(conversation_summary::PROJECTION)?).ok()
}

fn write_budget(dir: &Dir, budget: &UtilityBudget) -> Result<(), SessionError> {
    let text = serde_json::to_string(budget)
        .map_err(|error| SessionError::Corruption(error.to_string()))?;
    crate::private_fs::write_text_atomic_in_dir(dir, BUDGET_FILE, &text).map_err(SessionError::Io)
}

fn read_budget(dir: &Dir) -> Result<UtilityBudget, SessionError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = match dir.open_with(BUDGET_FILE, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A broken symlink is not a missing budget.
            if dir.symlink_metadata(BUDGET_FILE).is_ok() {
                return Err(SessionError::Corruption(
                    "invalid utility budget entry".into(),
                ));
            }
            return Ok(UtilityBudget::default());
        }
        Err(error) => return Err(SessionError::Io(error.to_string())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| SessionError::Io(error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > BYTE_CAP {
        return Err(SessionError::Corruption(
            "utility budget must be a bounded regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(BYTE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| SessionError::Io(error.to_string()))?;
    if bytes.len() as u64 > BYTE_CAP {
        return Err(SessionError::Corruption(
            "utility budget exceeds byte limit".into(),
        ));
    }
    let budget: UtilityBudget = serde_json::from_slice(&bytes)
        .map_err(|error| SessionError::Corruption(format!("invalid utility budget: {error}")))?;
    if budget.version != 1 {
        return Err(SessionError::Corruption(
            "unsupported utility budget".into(),
        ));
    }
    Ok(budget)
}

fn bounded_context(replay: &[ReplayEvent]) -> (String, Vec<u64>) {
    let mut remaining = CONTEXT_CHARS;
    let mut rows = Vec::new();
    for event in replay.iter().rev() {
        let (role, seq, text) = match event {
            ReplayEvent::UserMessage { seq, text, .. } => ("user", *seq, text),
            ReplayEvent::AssistantMessage { seq, text, .. } => ("assistant", *seq, text),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        let prefix = format!("{role}: ");
        let overhead = prefix.chars().count() + 1;
        if remaining <= overhead {
            break;
        }
        // Give recent user intent a seat even after a very long assistant reply.
        // A per-message excerpt bounds tokens without limiting feature usage.
        let clipped: String = text
            .chars()
            .take((remaining - overhead).min(1000))
            .collect();
        let row = format!("{prefix}{clipped}\n");
        remaining -= row.chars().count();
        rows.push((seq, row));
        if rows.len() == CONTEXT_MESSAGES || remaining == 0 {
            break;
        }
    }
    rows.reverse();
    let seqs = rows.iter().map(|(seq, _)| *seq).collect();
    (rows.into_iter().map(|(_, row)| row).collect(), seqs)
}

pub(super) fn title_payload(
    state: &Value,
    expectation: SetTitleExpectation,
    title: &str,
    source: TitleSource<'_>,
) -> Option<Value> {
    // Ownership and CAS are checked while the caller holds active through
    // append. Neither a fresh sequence nor Force transfers user ownership.
    if !matches!(source, TitleSource::User)
        && state.get("source").and_then(Value::as_str) == Some("user")
    {
        return None;
    }
    let seq = state.get("eventSeq").and_then(Value::as_u64);
    let matches = match expectation {
        SetTitleExpectation::NoTitle => seq.is_none(),
        SetTitleExpectation::Exact(expected) => seq == Some(expected),
        SetTitleExpectation::Force => true,
    };
    if !matches {
        return None;
    }
    Some(match source {
        TitleSource::User => payloads::session_title(title, Vec::new(), "user"),
        TitleSource::Provider {
            provider,
            model,
            message_seqs,
        } => {
            let seqs = message_seqs.map(<[u64]>::to_vec).unwrap_or_else(|| {
                state
                    .get("firstUserSeq")
                    .and_then(Value::as_u64)
                    .into_iter()
                    .collect()
            });
            payloads::session_title_provider(title, seqs, provider, model)
        }
    })
}
