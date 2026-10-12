// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session adapter for Footfall.
//!
//! This crate lives strictly downstream of `footfall_session`. It transports owned session envelopes
//! over a single WebTransport stream; it does not participate in hot-path instrumentation.

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

/// Maximum envelope count accepted in one snapshot response.
pub const MAX_SESSION_ENVELOPES: u64 = 1_000_000;
/// Maximum session announcements accepted in one discovery response.
pub const MAX_DISCOVERED_SESSIONS: u64 = 4096;
