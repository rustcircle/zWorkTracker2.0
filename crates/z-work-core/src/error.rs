use thiserror::Error;

/// Errors returned to the UI. `code()` is stable and machine-readable.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreError {
    #[error("{0}")]
    Validation(String),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("cannot {action} a task that is {state}")]
    IllegalTransition {
        action: &'static str,
        state: &'static str,
    },
    #[error("this task is running on another device ({0}); pause it there")]
    RunningElsewhere(String),
    #[error("{0}")]
    Conflict(String),
    #[error("this overlaps time already logged on: {0}")]
    Overlap(String),
    #[error(
        "this launch has not started a task yet, so continuing needs every field filled in again"
    )]
    ColdStartRequired,
    #[error("could not save data: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not read data: {0}")]
    Read(#[source] std::io::Error),
    #[error("could not encode data: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "data file was written by a newer app version ({found}, this app supports {supported}); it was left untouched"
    )]
    FutureVersion { found: u64, supported: u64 },
    #[error("could not build spreadsheet: {0}")]
    Xlsx(String),
    #[error("could not build the PDF: {0}")]
    Pdf(String),
    #[error("data store is not available: {0}")]
    StoreUnavailable(String),
    #[error(
        "{holder} is using zWorkTracker, so this computer is read-only. Take over from the banner to work here."
    )]
    ReadOnly { holder: String },
}

impl CoreError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "validation",
            Self::NotFound(_) => "not_found",
            Self::IllegalTransition { .. } => "illegal_transition",
            Self::RunningElsewhere(_) => "running_elsewhere",
            Self::Conflict(_) => "conflict",
            Self::Overlap(_) => "overlap",
            Self::ColdStartRequired => "cold_start_required",
            Self::Io(_) | Self::Read(_) | Self::Json(_) | Self::Xlsx(_) | Self::Pdf(_) => "io",
            Self::FutureVersion { .. } => "future_version",
            Self::StoreUnavailable(_) => "store_unavailable",
            Self::ReadOnly { .. } => "read_only",
        }
    }

    /// Something else has the file open for now, so trying again can help.
    #[must_use]
    pub fn is_held(&self) -> bool {
        matches!(self, Self::Io(e) | Self::Read(e) if crate::sync::is_transient(e))
    }
}
