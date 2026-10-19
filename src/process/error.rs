//! Internal failure identity. Display text is compatibility output, never control flow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProcessError {
    WorkdirOutOfScope,
    SessionUnavailable(u64),
    Other(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkdirOutOfScope => {
                f.write_str("process absolute workdir must be inside the active scratch directory")
            }
            Self::SessionUnavailable(id) => {
                write!(f, "process session {id} is not available in this run")
            }
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl From<String> for ProcessError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

impl From<&str> for ProcessError {
    fn from(message: &str) -> Self {
        Self::Other(message.to_owned())
    }
}

impl From<ProcessError> for String {
    fn from(error: ProcessError) -> Self {
        error.to_string()
    }
}
