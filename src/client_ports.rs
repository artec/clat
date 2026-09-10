//! Narrow internal client ports. Stateful owners are deliberately not exported.
//! This surface is not a plugin ABI; internal workspace crates evolve together.
pub use crate::session::id::SessionId;
pub use crate::session::use_cases::SessionSummary;
pub mod permission {
    pub use crate::permission::{PermissionDecision, PermissionMode, PermissionRequest};
}
pub mod command {
    pub use crate::command::CommandGroup;
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
