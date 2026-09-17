use crate::DomainError;
use contract::{ErrorCode, RequestId};

pub const MAX_RETAINED_REQUESTS: usize = 4096;
pub const REQUEST_RETENTION_MS: u64 = 86_400_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedRequest {
    pub request_id: RequestId,
    pub payload_sha256: String,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DedupDecision {
    Admit,
    Replay,
    BypassForTaskCancel,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DedupIndex {
    entries: Vec<RetainedRequest>,
}

impl DedupIndex {
    pub fn entries(&self) -> &[RetainedRequest] {
        &self.entries
    }

    pub fn decide_and_reserve(
        &mut self,
        request_id: RequestId,
        payload_sha256: String,
        now_ms: u64,
        task_cancel: bool,
    ) -> Result<DedupDecision, DomainError> {
        if task_cancel {
            return Ok(DedupDecision::BypassForTaskCancel);
        }
        if payload_sha256.len() != 64
            || !payload_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DomainError::invalid(
                "payload_sha256 must be lowercase hexadecimal SHA-256",
            ));
        }
        self.entries
            .retain(|entry| entry.expires_at_ms.is_none_or(|expiry| expiry > now_ms));
        if let Some(existing) = self
            .entries
            .iter()
            .find(|entry| entry.request_id == request_id)
        {
            return if existing.payload_sha256 == payload_sha256 {
                Ok(DedupDecision::Replay)
            } else {
                Err(DomainError::invalid(
                    "request_id was retained with different request content",
                ))
            };
        }
        if self.entries.len() == MAX_RETAINED_REQUESTS {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "retained request capacity is full",
            ));
        }
        self.entries.push(RetainedRequest {
            request_id,
            payload_sha256,
            expires_at_ms: None,
        });
        Ok(DedupDecision::Admit)
    }

    pub fn settle(
        &mut self,
        request_id: &RequestId,
        terminal_at_ms: u64,
    ) -> Result<u64, DomainError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| &entry.request_id == request_id)
            .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "request_id is not retained"))?;
        if let Some(expiry) = entry.expires_at_ms {
            return Ok(expiry);
        }
        let expiry = terminal_at_ms
            .checked_add(REQUEST_RETENTION_MS)
            .ok_or_else(|| {
                DomainError::new(ErrorCode::ResourceLimit, "retained request time exhausted")
            })?;
        entry.expires_at_ms = Some(expiry);
        Ok(expiry)
    }
}
