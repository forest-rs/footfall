// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Serve Footfall sessions over local WebTransport, and attach to them.
//!
//! A producer offers its runtimes to tools on the same machine; a tool finds
//! every producer, then fetches snapshots or subscribes to a stream of them.
//! Nothing here runs on the instrumented hot path. Serving and attaching run
//! on Tokio: call them from within a Tokio runtime, for example under
//! `#[tokio::main]`. Outside one they panic.
//!
//! **Producers** call [`LocalSessionServer::start`] with the runtimes to
//! offer, each as a [`ServedSession`]. It manages a self-signed localhost
//! identity, picks a port, registers itself so tools can find it, and answers
//! requests until it is [shut down](LocalSessionServer::shutdown) or dropped.
//!
//! ```no_run
//! # async fn serve() -> std::io::Result<()> {
//! use footfall_runtime::Runtime;
//! use footfall_session_wtransport::{LocalSessionServer, ServedSession};
//!
//! let runtime = Runtime::builder().process_name("my_app").install_global()
//!     .expect("no other backend is installed");
//! let server = LocalSessionServer::start(vec![ServedSession::new("main", runtime)]).await?;
//! // ... run the application ...
//! server.shutdown(std::time::Duration::from_secs(1)).await;
//! # Ok(())
//! # }
//! ```
//!
//! **Tools** use [`SessionClient`]. [`SessionClient::discover_local`] asks
//! every local producer for its sessions at once; each [`LocalSession`] has
//! the client to [`fetch`](SessionClient::fetch) a snapshot from a cursor or
//! [`subscribe`](SessionClient::subscribe) to new ones. Snapshots arrive as
//! `footfall_session::OwnedSnapshot`s, and every failure is a typed
//! [`ClientError`].
//!
//! ```no_run
//! # async fn attach() -> Result<(), Box<dyn std::error::Error>> {
//! use footfall_session_wtransport::SessionClient;
//! use std::time::Duration;
//!
//! let discovery = SessionClient::discover_local(Duration::from_secs(1)).await?;
//! let session = discovery.session("main")?;
//! let mut subscription = session
//!     .client()
//!     .subscribe(session.id(), None, Duration::from_millis(250))
//!     .await?;
//! loop {
//!     let window = subscription.next().await?;
//!     println!("{} new events", window.events().len());
//! }
//! # }
//! ```
//!
//! [`SessionServer`] and [`SessionRequest`] are the lower layer
//! `LocalSessionServer` is built on: bind a server with your own identity and
//! address, accept requests one at a time, and answer each yourself. Use
//! them to serve something other than a runtime, or to control every
//! connection.
//!
//! Serving runs on QUIC, whose implementation traces every packet with
//! `tracing`. A process that forwards `tracing` into Footfall with
//! `footfall_tracing` should filter that layer to its own targets, or the
//! capture fills with the work of serving it.
//!
//! The byte-level encoding is documented in the [wire format].
//!
//! [wire format]: https://github.com/forest-rs/footfall/blob/main/docs/session-wire-format.md

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
