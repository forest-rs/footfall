// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session adapter for Footfall.
//!
//! This crate lives strictly downstream of `footfall_session`. It transports owned session envelopes
//! over a single WebTransport stream; it does not participate in hot-path instrumentation.

use footfall_runtime::{Runtime, Snapshot};
use footfall_session::{
    Chunking, Consumer, Envelope, EventCursor, ProtocolVersion, SessionErrorCode,
    codec::{
        MAX_SESSION_ENVELOPE_BYTES, MAX_SESSION_STRING_BYTES, decode_envelope, encode_envelope,
    },
    publish_snapshot,
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
    error::StreamWriteError, tls::Sha256Digest,
};

mod client;
mod identity;
mod serve;
mod server;
#[cfg(test)]
mod tests;
mod wire;

pub use client::*;
pub use identity::*;
pub use serve::*;
pub use server::*;
use wire::*;

/// Maximum envelope count accepted in one snapshot response.
pub const MAX_SESSION_ENVELOPES: u64 = 1_000_000;
/// Maximum session announcements accepted in one discovery response.
pub const MAX_DISCOVERED_SESSIONS: u64 = 4096;
