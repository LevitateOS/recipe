//! Coordinator failure categories.
//!
//! A failure's category is a type, not a message prefix: callers classify an
//! error with [`Category::of`], and the text users see is generated here.

/// Why an invocation failed, as reported to the user and to scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// The domain could not be prepared or launched.
    Startup,
    /// The worker or launcher broke the stream or result contract.
    Protocol,
    /// The recipe itself failed inside the domain.
    Workload,
    /// The Ginux supervisor reported its own failure.
    Supervisor,
    /// The supervised execution deadline expired.
    Deadline,
    /// Retained state or published outputs could not be stored.
    Storage,
    /// A signal interrupted the invocation.
    Cancelled,
}

impl Category {
    /// The prefix users and scripts see in front of the message.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Startup => "backend startup failure",
            Self::Protocol => "backend protocol failure",
            Self::Workload => "backend workload failure",
            Self::Supervisor => "backend supervisor failure",
            Self::Deadline => "backend deadline failure",
            Self::Storage => "backend storage failure",
            Self::Cancelled => "backend cancelled",
        }
    }

    /// A categorized error carrying `message`.
    pub fn error(self, message: impl Into<String>) -> Failure {
        Failure {
            category: self,
            message: message.into(),
        }
    }

    /// Give an uncategorized `error` this category, with `message` as context;
    /// an error that already has a category keeps it.
    pub fn wrap(self, error: anyhow::Error, message: impl Into<String>) -> anyhow::Error {
        if Self::of(&error).is_some() {
            error
        } else {
            error.context(self.error(message))
        }
    }

    /// The category of the outermost categorized error in `error`'s chain.
    pub fn of(error: &anyhow::Error) -> Option<Self> {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<Failure>())
            .map(|failure| failure.category)
    }
}

/// An error with a [`Category`]; displays as `<prefix>: <message>`.
#[derive(Debug)]
pub struct Failure {
    category: Category,
    message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.category.prefix(), self.message)
    }
}

impl std::error::Error for Failure {}
