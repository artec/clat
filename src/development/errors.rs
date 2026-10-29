//! Presentation-only mapping; raw diagnostics and operation identifiers remain intact.
pub(super) fn presentation(reason: &str) -> crate::i18n::UiMessage {
    let code = reason.split(':').next().unwrap_or("failed");
    let key = match code {
        "file_conflict" | "draft_conflict" | "git_changed" => "dev.conflict",
        "cancelled" => "dev.cancelled",
        "path_denied" => "dev.path_denied",
        "model_unavailable" => "dev.model_unavailable",
        "generation_empty" | "generation_failed" | "generation_not_reduced" => {
            "dev.generation_failed"
        }
        "terminal_observer" => "dev.observer",
        "operation_unknown" => "dev.operation_unknown",
        "file_large" => "dev.file_large",
        "directory_large" => "dev.directory_large",
        "terminal_limit" => "dev.terminal_limit",
        "nothing_staged" => "dev.nothing_staged",
        "git_repository_missing" => "dev.git_repository_missing",
        "git_output_large" => "dev.git_output_large",
        "git_scope" => "dev.git_scope",
        "git_busy" | "state_busy" | "terminal_busy" => "dev.busy",
        "state_corrupt" => "dev.state_corrupt",
        _ => "dev.failed",
    };
    crate::i18n::UiMessage::new(key)
}
