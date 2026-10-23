use super::{LocaleContext, UiMessage};
use crate::application::{GoalViewDto, MemoryOverviewDto, SubagentStatusDto};

pub fn render_parts(locale: LocaleContext, parts: &[UiMessage]) -> String {
    parts
        .iter()
        .map(|m| locale.render(m))
        .collect::<Vec<_>>()
        .join("\n\n")
}
impl MemoryOverviewDto {
    pub fn presentation(&self) -> Vec<UiMessage> {
        if self.entries.is_empty() {
            return vec![UiMessage::new("command.memory_none")];
        }
        let mut parts =
            vec![UiMessage::new("command.memory_count").arg("count", self.entries.len())];
        for entry in &self.entries {
            // Entry bodies are original user content; only their surrounding labels are rendered.
            parts.push(
                UiMessage::new(if entry.stale {
                    "command.memory_entry_stale"
                } else {
                    "command.memory_entry"
                })
                .arg("id", entry.id.clone())
                .arg("scope", entry.scope.clone())
                .arg("revision", entry.revision)
                .arg("source", entry.source.clone())
                .arg("digest", entry.digest.clone())
                .arg("content", entry.content.clone()),
            );
            if entry.content_truncated {
                parts.push(UiMessage::new("command.memory_truncated").arg("id", entry.id.clone()));
            }
        }
        if self.omitted > 0 {
            parts.push(UiMessage::new("command.memory_omitted").arg("count", self.omitted));
        }
        parts
    }
    pub fn localized_text(&self, locale: LocaleContext) -> String {
        let parts = self.presentation();
        render_parts(locale, &parts)
    }
}
impl GoalViewDto {
    pub fn presentation(&self) -> Vec<UiMessage> {
        let Some(state) = &self.goal else {
            return vec![UiMessage::new("command.goal_none")];
        };
        let mut parts = vec![
            UiMessage::new(if self.armed {
                "command.goal_overview_armed"
            } else {
                "command.goal_overview_disarmed"
            })
            .arg("phase", state.phase.as_str())
            .arg("id", state.id.clone())
            .arg("revision", state.revision)
            .arg("objective", state.objective.clone())
            .arg("rounds", state.rounds_started)
            .arg("max_rounds", state.limits.max_rounds)
            .arg("tokens", state.tokens_used)
            .arg("max_tokens", state.limits.max_tokens)
            .arg("failures", state.failures)
            .arg("max_failures", state.limits.max_failures)
            .arg("elapsed", state.elapsed_ms)
            .arg("max_ms", state.limits.max_time_secs.saturating_mul(1000)),
        ];
        if let Some(reason) = &state.blocked_reason {
            parts.push(
                UiMessage::new("command.goal_blocked")
                    .arg("code", reason.code.clone())
                    .arg("message", reason.message.clone()),
            );
        }
        if let Some(result) = &state.last_result {
            parts.push(UiMessage::new("command.goal_result").arg("result", result.clone()));
        }
        parts.push(
            UiMessage::new("command.goal_actions")
                .arg("acceptance", format!("{:?}", state.acceptance)),
        );
        parts
    }
    pub fn localized_text(&self, locale: LocaleContext) -> String {
        let parts = self.presentation();
        render_parts(locale, &parts)
    }
}
impl SubagentStatusDto {
    pub fn presentation(&self) -> Vec<UiMessage> {
        vec![UiMessage::new(if self.enabled {
            "command.subagent_enabled"
        } else {
            "command.subagent_disabled"
        })]
    }
    pub fn localized_text(&self, locale: LocaleContext) -> String {
        render_parts(locale, &self.presentation())
    }
}

impl crate::McpStatusDto {
    pub fn presentation(&self) -> Vec<UiMessage> {
        let mut parts = vec![
            UiMessage::new("command.mcp_summary")
                .arg("connected", self.connected)
                .arg("configured", self.configured)
                .arg("connecting", self.connecting),
        ];
        parts.extend(self.servers.iter().map(|server| {
            UiMessage::new("command.mcp_server")
                .arg("name", server.name.clone())
                .arg("transport", server.transport.clone())
                .arg("protocol", server.protocol_version.clone())
                .arg("count", server.tools)
        }));
        parts.extend(
            self.failures
                .iter()
                .map(|detail| UiMessage::new("command.mcp_failure").arg("detail", detail.clone())),
        );
        parts
    }
    pub fn localized_text(&self, locale: LocaleContext) -> String {
        let connecting = if self.connecting > 0 {
            locale.render(&UiMessage::new("cli.mcp_connecting").arg("count", self.connecting))
        } else {
            String::new()
        };
        let mut lines = vec![
            locale.render(
                &UiMessage::new("cli.mcp_summary")
                    .arg("configured", self.configured)
                    .arg("connected", self.connected)
                    .arg("connecting", connecting),
            ),
        ];
        for server in &self.servers {
            lines.push(
                locale.render(
                    &UiMessage::new("cli.mcp_server")
                        .arg("name", server.name.clone())
                        .arg("transport", server.transport.clone())
                        .arg("protocol", server.protocol_version.clone())
                        .arg("count", server.tools),
                ),
            );
        }
        lines.extend(self.failures.iter().map(|failure| format!("! {failure}")));
        lines.join("\n")
    }
}
impl crate::SkillsOverviewDto {
    pub fn presentation(&self) -> Vec<UiMessage> {
        let mut parts =
            vec![UiMessage::new("command.skills_summary").arg("count", self.entries.len())];
        for entry in &self.entries {
            let message = UiMessage::new("command.skills_entry")
                .arg("name", entry.name.clone())
                .arg("source", entry.source.clone())
                .arg("description", entry.description.clone());
            parts.push(if entry.requires_execution {
                message.arg_message("execution", UiMessage::new("command.skills_execution"))
            } else {
                message.arg("execution", "")
            });
        }
        parts.extend(self.diagnostics.iter().map(|d| {
            UiMessage::new("common.diagnostic").arg(
                "detail",
                format!(
                    "! {}/{}/{}: {}",
                    d.source,
                    d.name.as_deref().unwrap_or("-"),
                    d.kind,
                    d.message
                ),
            )
        }));
        parts
    }
    pub fn localized_text(&self, locale: LocaleContext) -> String {
        let mut lines = vec![
            locale.render(&UiMessage::new("cli.skills_summary").arg("count", self.entries.len())),
        ];
        for entry in &self.entries {
            let execution = if entry.requires_execution {
                locale.text("cli.skills_execution")
            } else {
                ""
            };
            lines.push(format!(
                "● {}  {}{} — {}",
                entry.name, entry.source, execution, entry.description
            ));
        }
        for diagnostic in &self.diagnostics {
            lines.push(format!(
                "! {} / {} / {}: {}",
                diagnostic.source,
                diagnostic.name.as_deref().unwrap_or("-"),
                diagnostic.kind,
                diagnostic.message
            ));
        }
        lines.join("\n")
    }
}
impl crate::ContextEstimateSnapshot {
    fn presentation(&self) -> UiMessage {
        UiMessage::new("cli.context_headless")
            .arg("unit", self.unit.clone())
            .arg("base", self.base_prompt_estimate)
            .arg("project", self.project_instructions_estimate)
            .arg("plan", self.plan_policy_estimate)
            .arg("catalog", self.skill_catalog_estimate)
            .arg("goal", self.goal_policy_estimate)
            .arg("memory", self.memory_estimate)
            .arg("memory_budget", self.memory_budget_bytes)
            .arg("tools", self.tool_schemas_estimate)
            .arg("history", self.history_estimate)
            .arg("output", self.output_reserve_estimate)
            .arg("input", self.input_estimate)
            .arg("total", self.total_estimate)
    }
    pub fn localized_text(&self, locale: LocaleContext) -> String {
        let mut lines = vec![
            locale.render(&self.presentation()),
            locale.render(
                &UiMessage::new("cli.context_tools").arg("names", self.tool_names.join(", ")),
            ),
            locale.render(
                &UiMessage::new("cli.context_skills").arg("names", self.skill_names.join(", ")),
            ),
        ];
        lines.push(
            locale
                .text(if self.skill_diagnostics.is_empty() {
                    "cli.context_no_diagnostics"
                } else {
                    "cli.context_diagnostics"
                })
                .into(),
        );
        for diagnostic in &self.skill_diagnostics {
            lines.push(format!(
                "! {} / {} / {}: {}",
                diagnostic.source,
                diagnostic.name.as_deref().unwrap_or("-"),
                diagnostic.kind,
                diagnostic.message
            ));
        }
        lines.join("\n")
    }
}
