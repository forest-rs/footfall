// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Error codes shared by session transports.

use core::fmt;

/// Why a session server refused or failed a request.
///
/// A server sends one of these in an error response; WebTransport clients
/// receive it as `ClientError::Server`. Codes are stable on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SessionErrorCode {
    /// Request payload or method was malformed.
    InvalidRequest,
    /// Requested entity or stream was not found.
    NotFound,
    /// The method is unknown or not supported by this target.
    Unsupported,
    /// The request failed after it was accepted.
    Failed,
    /// The peers speak protocol versions that cannot interoperate.
    Incompatible,
}

impl SessionErrorCode {
    /// Returns the stable lowercase name of this code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::NotFound => "not_found",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
            Self::Incompatible => "incompatible",
        }
    }
}

impl fmt::Display for SessionErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::SessionErrorCode;
    use alloc::string::ToString;

    #[test]
    fn error_codes_have_stable_names() {
        assert_eq!(SessionErrorCode::NotFound.to_string(), "not_found");
        assert_eq!(SessionErrorCode::Incompatible.as_str(), "incompatible");
    }
}
