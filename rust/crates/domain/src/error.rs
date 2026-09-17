use contract::ErrorCode;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainError {
    pub code: ErrorCode,
    pub reason: &'static str,
}

impl DomainError {
    pub const fn new(code: ErrorCode, reason: &'static str) -> Self {
        Self { code, reason }
    }

    pub const fn invalid(reason: &'static str) -> Self {
        Self::new(ErrorCode::InvalidArgument, reason)
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for DomainError {}
