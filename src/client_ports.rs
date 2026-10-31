//! Narrow internal client ports. Stateful owners are deliberately not exported.
//! This surface is not a plugin ABI; internal workspace crates evolve together.
pub mod presentation {
    pub use crate::i18n::*;
}
pub mod jobs {
    pub use crate::application::{JobOutput, JobView, JobsReader};
    pub use crate::background_jobs::command::{JobUserCommand, parse_job_command};
}
pub mod schedules {
    pub use crate::application::{
        Consumption, Delivery, DeliveryPhase, Rule, RulePhase, RuleReview, ScheduleDraft, TimeRule,
        Trigger, WorkBudget,
    };
    pub use crate::scheduler::command::{ScheduleCommand, parse_schedule_command};
}
pub mod web {
    pub use crate::application::{FetchProvider, WebSettingsEdit, WebSettingsView};
}
pub use crate::session::id::SessionId;
pub use crate::session::use_cases::SessionSummary;
// AG-DOGFOOD-4 返工 F1（E4）：前端展示面共享 core 的单一脱敏原语
//（INV-K1 的 redact_secrets）。provider/journal 出口已按不变量脱敏；
// 前端转录入口对绕过那些出口的错误来源（异常宿主帧、宿主 worker
// 崩溃文案等）补同一道闸，跨端一致、不另立词表。
pub use crate::redact::redact_secrets;
pub mod host {
    pub use crate::application::{WorkspaceDirectoryEntry, WorkspaceUpdate};
    pub use crate::host_client::HostNotification;
    pub use crate::host_client::PackagedBackend;
    pub fn journal_format_version() -> u32 {
        crate::session::compat::SESSION_FORMAT_VERSION
    }
    pub fn wire_version() -> u64 {
        crate::wire::WIRE_VERSION
    }
    pub use crate::host_client::{BackendConnection, HostManagementArgs, PluginReviewTicket};
    pub use crate::host_client::{
        HOST_PROTOCOL_VERSION, HostCallError, HostClient, HostEvent, HostEvents,
        HostEventsInterrupt, HostModelChoices, decode_host_approval, decode_host_event,
    };
}
pub mod permission {
    pub use crate::permission::{PermissionDecision, PermissionMode, PermissionRequest};
}
pub mod command {
    pub use crate::command::CommandGroup;
}
pub mod input {
    pub use crate::application::{
        InputDraft, PendingInputEdit, PendingInputView, PendingInputsView,
    };
}
pub mod model_setup {
    pub use crate::application::{ModelSetupOperation, ModelSetupView};
}
pub mod interaction {
    pub use crate::interaction::{AskAnswer, AskOption, AskQuestion, UserAsker};
}
pub mod draft {
    pub use crate::draft::DraftImageStore;
}
pub mod private_fs {
    pub use crate::private_fs::write_text_atomic;
}
pub mod session {
    pub mod catalog {
        pub use crate::session::catalog::is_known_type;
    }
    pub mod event {
        pub use crate::session::event::{SessionEvent, SurfaceOp};
    }
    pub mod id {
        pub use crate::session::id::SessionId;
    }
    pub mod path_layout {
        pub use crate::session::path_layout::{encode_segment, project_key};
    }
    pub mod replay {
        pub use crate::session::replay::{ReplayAdapter, ReplayEvent, ReplayTurnEnd};
    }
    pub mod attachments {
        pub use crate::session::attachments::{MAX_IMAGES_PER_MESSAGE, MAX_RAW_BATCH_BYTES};
    }
}

pub mod upgrade {
    pub use crate::upgrade::{UpgradeOutcome, UpgradeProgress, upgrade_with_progress};
}

/// Read-only history search and inspection.
pub mod history {
    pub use crate::application::{
        ConversationMessage, ConversationPage, ConversationRequest, HistoryEventRead,
        HistoryEventRow, HistoryEventsPage, HistoryEventsRequest, HistoryHit, HistoryPage,
        HistoryReadRequest, HistoryReader, HistoryReference, HistorySearch, OperationDetail,
        OperationDetailRequest, OperationFilter, OperationRow, OperationsPage, OperationsRequest,
    };
    pub use crate::session_query::command::{HistoryCommandRequest, parse_history_command};
}

pub mod workflow {
    pub use crate::application::{PlanDocument, WorkflowIntent, WorkflowRequest};
}

pub mod development {
    pub use crate::application::{DevelopmentService, GitModel, OperationView};
}
