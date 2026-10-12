// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session adapter for Footfall.
//!
//! This crate lives strictly downstream of `footfall_session`. It transports owned session envelopes
//! over a single WebTransport stream; it does not participate in hot-path instrumentation.

use footfall_core::{
    ContextId, CounterDescId, EventDescId, FlowId, FlowPhase, InstrumentationLevel, MarkerKind,
    MarkerPhase, ResourceId, ResourceKindId, ScopeDescId, ScopeToken, Unit,
};
use footfall_runtime::{CounterKind, Runtime, Snapshot};
use footfall_session::{
    CaptureSettings, Chunking, Consumer, Drops, Envelope, EnvelopeHeader, EventCursor, EventWindow,
    OwnedArgEntry, OwnedContextEntry, OwnedContextSnapshot, OwnedCounterSnapshot,
    OwnedHistogramSnapshot, OwnedLabelValue, OwnedObservation, OwnedSessionEnvelope,
    OwnedSessionPayload, OwnedSnapshotSummary, OwnedTimedObservation, Payload, ProtocolVersion,
    RowCounts, SessionErrorCode, SnapshotMetadata, ThreadSnapshot, publish_snapshot,
};
use std::{
    env, fmt, io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::PathBuf,
    time::Duration,
};
use tokio::fs;
use wtransport::{
    ClientConfig, Connection, Endpoint, Identity, RecvStream, SendStream, ServerConfig,
    error::StreamWriteError,
};

use wtransport::tls::Sha256Digest;

mod client;
mod codec;
mod identity;
mod serve;
mod server;
#[cfg(test)]
mod tests;
mod wire;

pub use client::*;
use codec::*;
pub use identity::*;
pub use serve::*;
pub use server::*;
use wire::*;

/// Maximum UTF-8 string payload accepted by the session wire codec.
pub const MAX_SESSION_STRING_BYTES: usize = 64 * 1024;
/// Maximum encoded envelope frame accepted by the session wire codec.
pub const MAX_SESSION_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum envelope count accepted in one snapshot response.
pub const MAX_SESSION_ENVELOPES: u64 = 1_000_000;
/// Maximum session announcements accepted in one discovery response.
pub const MAX_DISCOVERED_SESSIONS: u64 = 4096;
/// Maximum rows accepted in one encoded envelope payload.
pub const MAX_ROWS_PER_ENVELOPE: usize = 65_536;
/// Maximum context labels accepted for one encoded context row.
pub const MAX_CONTEXT_ENTRIES_PER_CONTEXT: usize = footfall_core::CONTEXT_INLINE_CAPACITY;
/// Maximum per-instance arguments accepted for one encoded observation.
pub const MAX_ARGS_PER_OBSERVATION: usize = footfall_core::ARGS_INLINE_CAPACITY;
