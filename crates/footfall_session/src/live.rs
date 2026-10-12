// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Live-session request, response, and stream control messages.

use alloc::string::String;
use core::fmt;

/// Stable id for one live session request.
///
/// Request ids are scoped to one logical tool session. They are assigned by the
/// requester and copied into the matching [`SessionResponse`] so transports can
/// pipeline requests without relying on response ordering.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionRequestId(u64);

impl SessionRequestId {
    /// Creates a request id from its raw session-local value.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the raw session-local value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable id for one live session stream.
///
/// Stream ids are scoped to one logical tool session. A stream carries ordered
/// [`SessionStreamItem`] rows until a [`SessionStreamEnd`] is sent.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionStreamId(u64);

impl SessionStreamId {
    /// Creates a stream id from its raw session-local value.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the raw session-local value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Domain-qualified method name for a live session message.
///
/// Footfall owns the request/response/stream envelope, but it does not own the
/// domain method vocabulary. For example, an Overstory inspector can use
/// methods such as `overstory.inspection.elementDetails` without making Footfall
/// understand element ids or UI trees.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionMethod {
    /// Product or subsystem domain, such as `footfall.trace` or
    /// `overstory.inspection`.
    domain: String,
    /// Method name inside the domain.
    name: String,
}

impl SessionMethod {
    /// Returns the product or subsystem domain, such as `footfall.trace` or
    /// `overstory.inspection`.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Returns the method name inside the domain.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl SessionMethod {
    /// Creates a method from a domain and method name.
    #[must_use]
    pub fn new(domain: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
            name: name.into(),
        }
    }
}

/// One live request sent by a consumer or tool frontend.
///
/// `P` is intentionally generic. Same-process integrations can use typed Rust
/// payloads, while network transports can use byte buffers or another encoded
/// representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionRequest<P> {
    /// Request id copied into the corresponding response.
    id: SessionRequestId,
    /// Domain-qualified method to execute.
    method: SessionMethod,
    /// Domain-owned request payload.
    payload: P,
}

impl<P> SessionRequest<P> {
    /// Returns the request id copied into the corresponding response.
    #[must_use]
    pub const fn id(&self) -> SessionRequestId {
        self.id
    }

    /// Returns the domain-qualified method to execute.
    #[must_use]
    pub const fn method(&self) -> &SessionMethod {
        &self.method
    }

    /// Returns the domain-owned request payload.
    #[must_use]
    pub const fn payload(&self) -> &P {
        &self.payload
    }

    /// Consumes this value and returns its payload.
    #[must_use]
    pub fn into_payload(self) -> P {
        self.payload
    }
}

impl<P> SessionRequest<P> {
    /// Creates a new live request.
    #[must_use]
    pub fn new(id: SessionRequestId, method: SessionMethod, payload: P) -> Self {
        Self {
            id,
            method,
            payload,
        }
    }

    /// Maps the request payload while preserving routing metadata.
    #[must_use]
    pub fn map_payload<Q>(self, f: impl FnOnce(P) -> Q) -> SessionRequest<Q> {
        SessionRequest {
            id: self.id,
            method: self.method,
            payload: f(self.payload),
        }
    }
}

/// Result payload carried by a [`SessionResponse`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionResult<P> {
    /// The request completed successfully.
    Ok(P),
    /// The request failed at the live-session layer or in the domain handler.
    Err(SessionError),
}

impl<P> SessionResult<P> {
    /// Returns `true` when this is a successful result.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }

    /// Returns `true` when this is an error result.
    #[must_use]
    pub const fn is_err(&self) -> bool {
        matches!(self, Self::Err(_))
    }
}

/// One response to a live session request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionResponse<P> {
    /// Request id this response completes.
    request_id: SessionRequestId,
    /// Request outcome.
    result: SessionResult<P>,
}

impl<P> SessionResponse<P> {
    /// Returns the request id this response completes.
    #[must_use]
    pub const fn request_id(&self) -> SessionRequestId {
        self.request_id
    }

    /// Returns the request outcome.
    #[must_use]
    pub const fn result(&self) -> &SessionResult<P> {
        &self.result
    }

    /// Consumes this response and returns its result.
    #[must_use]
    pub fn into_result(self) -> SessionResult<P> {
        self.result
    }
}

impl<P> SessionResponse<P> {
    /// Creates a successful response.
    #[must_use]
    pub const fn ok(request_id: SessionRequestId, payload: P) -> Self {
        Self {
            request_id,
            result: SessionResult::Ok(payload),
        }
    }

    /// Creates an error response.
    #[must_use]
    pub const fn err(request_id: SessionRequestId, error: SessionError) -> Self {
        Self {
            request_id,
            result: SessionResult::Err(error),
        }
    }
}

/// Error carried by live session responses and stream endings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionError {
    /// Stable error classification.
    code: SessionErrorCode,
    /// Human-readable diagnostic message.
    message: String,
}

impl SessionError {
    /// Returns the stable error classification.
    #[must_use]
    pub const fn code(&self) -> SessionErrorCode {
        self.code
    }

    /// Returns the human-readable diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl core::error::Error for SessionError {}

impl SessionError {
    /// Creates a live session error.
    #[must_use]
    pub fn new(code: SessionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Stable live session error classification.
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

/// A server-side stream was opened.
///
/// A stream may be the result of a subscription request or another
/// domain-specific operation. The optional `request_id` links it back to the
/// request that created it when such a request exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionStreamStart<P> {
    /// Request that opened the stream, when applicable.
    request_id: Option<SessionRequestId>,
    /// Stream id used by subsequent stream items and the terminal event.
    stream_id: SessionStreamId,
    /// Domain-qualified stream method.
    method: SessionMethod,
    /// Domain-owned stream start payload.
    payload: P,
}

impl<P> SessionStreamStart<P> {
    /// Returns the request that opened the stream, when applicable.
    #[must_use]
    pub const fn request_id(&self) -> &Option<SessionRequestId> {
        &self.request_id
    }

    /// Returns the stream id used by subsequent stream items and the terminal event.
    #[must_use]
    pub const fn stream_id(&self) -> SessionStreamId {
        self.stream_id
    }

    /// Returns the domain-qualified stream method.
    #[must_use]
    pub const fn method(&self) -> &SessionMethod {
        &self.method
    }

    /// Returns the domain-owned stream start payload.
    #[must_use]
    pub const fn payload(&self) -> &P {
        &self.payload
    }

    /// Consumes this value and returns its payload.
    #[must_use]
    pub fn into_payload(self) -> P {
        self.payload
    }
}

impl<P> SessionStreamStart<P> {
    /// Creates a stream start row.
    #[must_use]
    pub fn new(
        request_id: Option<SessionRequestId>,
        stream_id: SessionStreamId,
        method: SessionMethod,
        payload: P,
    ) -> Self {
        Self {
            request_id,
            stream_id,
            method,
            payload,
        }
    }
}

/// One ordered item in a live session stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionStreamItem<P> {
    /// Stream this item belongs to.
    stream_id: SessionStreamId,
    /// Stream-local monotonically increasing sequence number.
    sequence: u64,
    /// Domain-owned stream item payload.
    payload: P,
}

impl<P> SessionStreamItem<P> {
    /// Returns the stream this item belongs to.
    #[must_use]
    pub const fn stream_id(&self) -> SessionStreamId {
        self.stream_id
    }

    /// Returns the stream-local monotonically increasing sequence number.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the domain-owned stream item payload.
    #[must_use]
    pub const fn payload(&self) -> &P {
        &self.payload
    }

    /// Consumes this value and returns its payload.
    #[must_use]
    pub fn into_payload(self) -> P {
        self.payload
    }
}

impl<P> SessionStreamItem<P> {
    /// Creates a stream item.
    #[must_use]
    pub const fn new(stream_id: SessionStreamId, sequence: u64, payload: P) -> Self {
        Self {
            stream_id,
            sequence,
            payload,
        }
    }
}

/// Terminal event for a live session stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionStreamEnd {
    /// Stream that ended.
    stream_id: SessionStreamId,
    /// Terminal stream status.
    status: SessionStreamStatus,
}

impl SessionStreamEnd {
    /// Returns the stream that ended.
    #[must_use]
    pub const fn stream_id(&self) -> SessionStreamId {
        self.stream_id
    }

    /// Returns the terminal stream status.
    #[must_use]
    pub const fn status(&self) -> &SessionStreamStatus {
        &self.status
    }
}

impl SessionStreamEnd {
    /// Creates a terminal stream event.
    #[must_use]
    pub const fn new(stream_id: SessionStreamId, status: SessionStreamStatus) -> Self {
        Self { stream_id, status }
    }
}

/// Terminal status for a live session stream.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SessionStreamStatus {
    /// The stream completed normally.
    Completed,
    /// The stream was cancelled by either peer.
    Cancelled,
    /// The stream ended with an error.
    Error(SessionError),
}

/// Direction-neutral live session message.
///
/// This is a transport-neutral control-plane vocabulary. It intentionally does
/// not define a wire encoding and does not define any product-specific payload
/// variants.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SessionMessage<P> {
    /// One-shot request.
    Request(SessionRequest<P>),
    /// One-shot response.
    Response(SessionResponse<P>),
    /// Stream creation event.
    StreamStart(SessionStreamStart<P>),
    /// Ordered stream item.
    StreamItem(SessionStreamItem<P>),
    /// Terminal stream event.
    StreamEnd(SessionStreamEnd),
    /// Request cancellation of a live stream.
    CancelStream {
        /// Stream to cancel.
        stream_id: SessionStreamId,
    },
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::{
        SessionError, SessionErrorCode, SessionMethod, SessionRequest, SessionRequestId,
        SessionResponse, SessionStreamEnd, SessionStreamId, SessionStreamItem, SessionStreamStatus,
    };

    #[test]
    fn session_errors_display_code_and_message() {
        let error = SessionError::new(SessionErrorCode::NotFound, "no such stream");
        assert_eq!(error.to_string(), "not_found: no such stream");
        let _: &dyn core::error::Error = &error;
    }

    #[test]
    fn live_session_ids_expose_raw_values() {
        assert_eq!(SessionRequestId::new(7).get(), 7);
        assert_eq!(SessionStreamId::new(11).get(), 11);
    }

    #[test]
    fn live_session_request_maps_payload_without_losing_routing() {
        let request = SessionRequest::new(
            SessionRequestId::new(3),
            SessionMethod::new("overstory.inspection", "elementDetails"),
            "42",
        );

        let request = request.map_payload(|value| value.parse::<u64>().expect("numeric payload"));

        assert_eq!(request.id, SessionRequestId::new(3));
        assert_eq!(request.method.domain, "overstory.inspection");
        assert_eq!(request.method.name, "elementDetails");
        assert_eq!(request.payload, 42);
    }

    #[test]
    fn live_session_responses_and_streams_preserve_correlation_ids() {
        let request_id = SessionRequestId::new(5);
        let stream_id = SessionStreamId::new(9);

        let response = SessionResponse::ok(request_id, "ready");
        assert!(response.result.is_ok());
        assert_eq!(response.request_id, request_id);

        let item = SessionStreamItem::new(stream_id, 2, "delta");
        assert_eq!(item.stream_id, stream_id);
        assert_eq!(item.sequence, 2);

        let end = SessionStreamEnd::new(stream_id, SessionStreamStatus::Completed);
        assert_eq!(end.stream_id, stream_id);
        assert_eq!(end.status, SessionStreamStatus::Completed);
    }
}
