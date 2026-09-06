//! Bounded, read-only command projections shared by terminal, exec and serve.

use super::{ApplicationError, TrustedProjectApplication};
use serde::Serialize;

const MEMORY_OVERVIEW_ENTRIES: usize = 64;
const MEMORY_PREVIEW_BYTES: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemoryOverviewDto {
    pub entries: Vec<MemoryEntryDto>,
    pub omitted: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemoryEntryDto {
    pub id: String,
    pub scope: String,
    pub revision: u64,
    pub stale: bool,
    pub source: String,
    pub digest: String,
    pub content: String,
    pub content_truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GoalViewDto {
    pub goal: Option<crate::goal::GoalState>,
    pub armed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SubagentStatusDto {
    pub enabled: bool,
}

impl TrustedProjectApplication {
    pub fn memory_overview(
        &self,
        scope: Option<crate::memory::MemoryScope>,
    ) -> Result<MemoryOverviewDto, ApplicationError> {
        Ok(MemoryOverviewDto::from_hits(
            self.memory_list(scope)?,
            false,
        ))
    }

    pub fn memory_detail(&self, id: &str) -> Result<MemoryOverviewDto, ApplicationError> {
        let hit = self
            .memory_get(id)?
            .ok_or_else(|| ApplicationError::new(format!("memory `{id}` not found")))?;
        Ok(MemoryOverviewDto::from_hits(vec![hit], true))
    }

    pub fn goal_overview(&self) -> Result<GoalViewDto, ApplicationError> {
        let view = self.goal()?;
        Ok(GoalViewDto {
            armed: view.as_ref().is_some_and(|view| view.armed),
            goal: view.map(|view| view.state),
        })
    }
}

impl MemoryOverviewDto {
    fn from_hits(hits: Vec<crate::memory::MemoryHit>, detail: bool) -> Self {
        let omitted = hits.len().saturating_sub(MEMORY_OVERVIEW_ENTRIES);
        let entries = hits
            .into_iter()
            .take(MEMORY_OVERVIEW_ENTRIES)
            .map(|hit| {
                let record = hit.record;
                let mut content = record.content;
                let content_truncated = !detail && content.len() > MEMORY_PREVIEW_BYTES;
                if content_truncated {
                    let mut end = MEMORY_PREVIEW_BYTES;
                    while !content.is_char_boundary(end) {
                        end -= 1;
                    }
                    content.truncate(end);
                }
                MemoryEntryDto {
                    id: record.id,
                    scope: record.scope.as_str().into(),
                    revision: record.revision,
                    stale: hit.stale,
                    source: record.source,
                    digest: record.digest,
                    content,
                    content_truncated,
                }
            })
            .collect();
        Self { entries, omitted }
    }

    /// Plain-text presentation of the same projection, without styling escapes.
    pub fn to_text(&self) -> String {
        if self.entries.is_empty() {
            return "No explicit memories.".into();
        }
        let mut blocks = vec![format!("Memories: {}", self.entries.len())];
        for entry in &self.entries {
            blocks.push(format!(
                "{} · {} · rev {}{}\nSource: {}\nDigest: {}\n\n{}{}",
                entry.id,
                entry.scope,
                entry.revision,
                if entry.stale { " · stale" } else { "" },
                entry.source,
                entry.digest,
                entry.content,
                if entry.content_truncated {
                    format!(
                        "\n[preview truncated; /mem show {} for full content]",
                        entry.id
                    )
                } else {
                    String::new()
                },
            ));
        }
        if self.omitted > 0 {
            blocks.push(format!(
                "{} more memories omitted; use /mem list project|user or /mem show <id>.",
                self.omitted
            ));
        }
        blocks.join("\n\n")
    }
}

impl GoalViewDto {
    pub fn to_text(&self) -> String {
        let Some(state) = &self.goal else {
            return "No current goal.".into();
        };
        let mut text = format!(
            "Goal · {} · {}\n{} · rev {}\n\n{}\n\nRounds: {} / {}\nTokens: {} / {}\nFailures: {} / {}\nElapsed: {} / {} ms",
            state.phase.as_str(),
            if self.armed { "armed" } else { "disarmed" },
            state.id,
            state.revision,
            state.objective,
            state.rounds_started,
            state.limits.max_rounds,
            state.tokens_used,
            state.limits.max_tokens,
            state.failures,
            state.limits.max_failures,
            state.elapsed_ms,
            state.limits.max_time_secs.saturating_mul(1000),
        );
        if let Some(reason) = &state.blocked_reason {
            text.push_str(&format!(
                "\n\nBlocked · {}\n{}",
                reason.code, reason.message
            ));
        }
        if let Some(result) = &state.last_result {
            text.push_str(&format!("\n\nLast result\n{result}"));
        }
        text
    }
}

impl SubagentStatusDto {
    pub fn to_text(&self) -> String {
        format!(
            "Read-only subagents · {}\n\nSession-local experiment; restart resets it off.\n/sub on enables it; /sub off disables it.\n\ndelegate_readonly delegates one or two repository-reading tasks to fixed explorer/reviewer children (depth 1, bounded tokens, time and output).",
            if self.enabled { "enabled" } else { "disabled" },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(content: String) -> crate::memory::MemoryHit {
        crate::memory::MemoryHit {
            record: crate::memory::MemoryRecord {
                id: "m1".into(),
                scope: crate::memory::MemoryScope::Project,
                revision: 2,
                content,
                source: "user".into(),
                digest: "sha256".into(),
                project_key: Some("private-storage-key".into()),
                source_digest: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
            stale: true,
            score: 1,
            reason: "list".into(),
        }
    }

    #[test]
    fn pu_content_dto_goldens_are_explicit_read_only_projections() {
        let memory = MemoryOverviewDto::from_hits(vec![hit("first\nsecond".into())], false);
        assert_eq!(
            serde_json::to_string(&memory).unwrap(),
            r#"{"entries":[{"id":"m1","scope":"project","revision":2,"stale":true,"source":"user","digest":"sha256","content":"first\nsecond","content_truncated":false}],"omitted":0}"#
        );
        assert_eq!(
            serde_json::to_string(&GoalViewDto {
                goal: None,
                armed: false
            })
            .unwrap(),
            r#"{"goal":null,"armed":false}"#
        );
        assert_eq!(
            serde_json::to_string(&SubagentStatusDto { enabled: true }).unwrap(),
            r#"{"enabled":true}"#
        );
        let state: crate::goal::GoalState = serde_json::from_value(serde_json::json!({
            "id":"g1", "objective":"first\nsecond", "acceptance":{"kind":"user"},
            "phase":"blocked", "revision":3, "roundsStarted":1, "failures":1,
            "tokensUsed":20, "elapsedMs":100, "limits":{"maxRounds":2,"maxTokens":64,"maxTimeSecs":8,"maxFailures":2},
            "createdAt":1,"updatedAt":2,"blockedReason":{"code":"missing","message":"need input"},"lastResult":"checked"
        })).unwrap();
        let goal = GoalViewDto {
            goal: Some(state),
            armed: false,
        };
        assert_eq!(
            serde_json::to_string(&goal).unwrap(),
            r#"{"goal":{"id":"g1","objective":"first\nsecond","acceptance":{"kind":"user"},"phase":"blocked","revision":3,"roundsStarted":1,"failures":1,"tokensUsed":20,"elapsedMs":100,"limits":{"maxRounds":2,"maxTokens":64,"maxTimeSecs":8,"maxFailures":2},"createdAt":1,"updatedAt":2,"blockedReason":{"code":"missing","message":"need input"},"lastResult":"checked"},"armed":false}"#
        );
        for expected in [
            "blocked",
            "disarmed",
            "first\nsecond",
            "1 / 2",
            "20 / 64",
            "100 / 8000 ms",
            "need input",
            "checked",
        ] {
            assert!(goal.to_text().contains(expected), "{expected}");
        }
    }

    #[test]
    fn pu_memory_projection_bounds_previews_and_reports_omissions_without_losing_detail() {
        let content = "界".repeat(1024);
        let hits = vec![hit(content.clone()); 65];
        let view = MemoryOverviewDto::from_hits(hits, false);
        assert_eq!(view.entries.len(), 64);
        assert_eq!(view.omitted, 1);
        assert!(
            view.entries
                .iter()
                .all(|entry| entry.content.len() <= 1024 && entry.content_truncated)
        );
        assert!(view.to_text().contains("preview truncated"));
        assert!(view.to_text().contains("1 more memories omitted"));
        let detail = MemoryOverviewDto::from_hits(vec![hit(content.clone())], true);
        assert_eq!(detail.entries[0].content, content);
        assert!(!detail.entries[0].content_truncated);
    }
}
