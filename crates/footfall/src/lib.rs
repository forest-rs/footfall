// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The README's quick start uses the `runtime` and `export` modules, so it is
// the crate documentation, and a doctest, only when they are enabled; docs.rs
// builds with every feature.
#![cfg_attr(
    all(feature = "runtime", feature = "export"),
    doc = include_str!("../README.md")
)]
#![cfg_attr(
    not(all(feature = "runtime", feature = "export")),
    doc = "Low-overhead instrumentation with semantic attribution.\n\n\
           Build the documentation with every feature, as docs.rs does, for the full guide."
)]
//!
//! # How the pieces fit
//!
//! 1. **Instrument.** Code anywhere emits through the functions at the root
//!    of this crate: [`scope()`], [`counter_add`], [`frame_begin`],
//!    [`message`], and the rest. Each takes a static descriptor naming what
//!    happened and a [`ContextHandle`] naming who it happened under.
//! 2. **Collect.** The application installs one [`Backend`] for the
//!    process. `runtime::Runtime` keeps a bounded, newest-first capture; the
//!    `tracy` and `tracing_layer` modules bridge to other tools. With no
//!    backend, emissions are discarded at almost no cost.
//! 3. **Read.** `Runtime::snapshot` returns everything captured, and
//!    `Runtime::snapshot_since` with a cursor returns only what is new.
//! 4. **Use it elsewhere.** `export` writes a snapshot as a Chrome trace;
//!    `session::capture` saves snapshots to a file; `wtransport` serves the
//!    runtime so tools can fetch or subscribe to it live.
//! 5. **Analyze.** `analysis` turns any snapshot, in process, live, or from
//!    a file, into spans, frames, and per-frame counter changes.
//!
//! # Descriptors
//!
//! A descriptor ([`ScopeDesc`], [`CounterDesc`], [`EventDesc`],
//! [`ResourceKindDesc`]) names one kind of observation. Its identity is its
//! address, so declare descriptors as `static` items, never `const`: every use
//! of a `const` would create a fresh value at a fresh address and split one
//! descriptor into many. Footfall makes reaching a descriptor through a
//! `const` a compile error.
//!
//! ```
//! use footfall::{ContextHandle, CounterDesc, ScopeDesc, counter_add};
//!
//! static RECORD: ScopeDesc = ScopeDesc::new("record");
//! static DRAWS: CounterDesc = CounterDesc::count("draws");
//!
//! footfall::scope!(&RECORD, ContextHandle::ROOT);
//! counter_add(&DRAWS, ContextHandle::ROOT, 12);
//! ```
//!
//! The `scope_desc!`, `counter_desc!`, `event_desc!`, and
//! `resource_kind_desc!` macros build descriptors too; the two-argument form
//! of `scope_desc!` also records where the scope is declared, which
//! snapshots carry so tools can lead from a span to its code.
//!
//! # Contexts
//!
//! A [`Context`] names the semantic owner of an observation, such as a pass,
//! a layer, or a cache, as a small set of labels keyed by static
//! [`LabelKey`]s. A context is registered once and emitted by
//! [`ContextHandle`]: the backend learns the labels at registration, and each
//! emission carries only an integer.
//!
//! - [`RegisteredContext`] registers a context for an owner and releases it
//!   when dropped.
//! - `LazyContext`, with the `std` feature, holds a context in a `static`
//!   item and registers it on first use after a backend is installed.
//! - [`ContextHandle::ROOT`] is the empty root context and needs no
//!   registration.
//!
//! ```
//! use footfall::{CounterDesc, LabelKey, RegisteredContext, context, counter_add};
//!
//! static PASS: LabelKey = LabelKey::new("pass");
//! static DRAWS: CounterDesc = CounterDesc::count("draws");
//!
//! let opaque = RegisteredContext::new(&context!(PASS = "opaque"));
//! let transparent = RegisteredContext::new(&opaque.context().with(&PASS, "transparent"));
//! counter_add(&DRAWS, opaque.handle(), 12);
//! counter_add(&DRAWS, transparent.handle(), 3);
//! ```
//!
//! Register once per owner and reuse the handle. Equal contexts registered
//! separately get distinct handles and are not merged.
//!
//! # Frames, passes, and transactions
//!
//! [`frame_begin`]/[`frame_end`], [`pass_begin`]/[`pass_end`], and
//! [`transaction_begin`]/[`transaction_end`] mark the units of work a tool
//! groups by. The runtime records every counter's value at each frame end, so
//! analysis can report how much each counter changed per frame.
//!
//! # Levels
//!
//! An [`InstrumentationLevel`] chooses how much is collected: `Off`,
//! `Counters`, `Scoped` (adds scopes, markers, flows, and messages), or
//! `Trace` (adds resource lifecycles). Emissions above the level return
//! immediately. The installed runtime sets it, through
//! `RuntimeBuilder::level` or `Runtime::set_level`, and
//! [`max_level`] reports it to call sites that want to skip preparing
//! arguments.
//!
//! # Compiling instrumentation out
//!
//! Building with `--cfg footfall_compile_out`, for example
//! `RUSTFLAGS="--cfg footfall_compile_out" cargo build --release`, turns every
//! emission function into an empty `#[inline(always)]` function, so
//! instrumented code compiles to nothing beyond evaluating its arguments. The
//! rest of the API keeps working, so no call site changes.
//!
//! This is a compiler `cfg` rather than a cargo feature on purpose: features
//! unify across the dependency graph and are switched on by
//! `--all-features`, so a feature would let any dependency strip
//! instrumentation from a whole application. Only the final binary should
//! make that choice; libraries must never set it.
#![cfg_attr(
    all(feature = "wtransport", not(target_family = "wasm")),
    doc = r#"
# Serving a live session

With `runtime` and `wtransport`, an application serves its runtime to local
tools. They find it without configuration.

```no_run
# #[cfg(feature = "runtime")]
# async fn serve() {
use footfall::runtime::Runtime;
use footfall::wtransport::{LocalSessionServer, ServedSession};

let runtime = Runtime::builder()
    .process_name("my_app")
    .install_global()
    .expect("no other backend is installed");
let server = LocalSessionServer::start(vec![ServedSession::new("main", runtime)])
    .await
    .expect("the session server starts");
// ... run the application ...
server.shutdown(std::time::Duration::from_secs(1)).await;
# }
```
"#
)]
#![cfg_attr(
    all(
        feature = "wtransport",
        feature = "analysis",
        not(target_family = "wasm")
    ),
    doc = r#"
# Attaching to a session

With `wtransport` and `analysis`, a tool finds every local session, subscribes
to one, and follows its frames across windows in a trace.

```no_run
# async fn attach() -> Result<(), footfall::wtransport::ClientError> {
use footfall::analysis::Trace;
use footfall::wtransport::SessionClient;
use std::time::Duration;

let discovery = SessionClient::discover_local(Duration::from_secs(1)).await?;
let session = &discovery.sessions()[0];
let mut subscription = session
    .client()
    .subscribe(session.id(), None, Duration::from_millis(250))
    .await?;
let mut trace = Trace::new();
loop {
    let window = subscription.next().await?;
    let pushed = trace.push(&window).expect("one producer instance");
    for frame in pushed.frames().iter().filter_map(|id| trace.frame(*id)) {
        println!("{} took {:?}", frame.id(), frame.duration());
    }
}
# }
```
"#
)]
#![no_std]

pub use footfall_core::*;

#[cfg(feature = "analysis")]
#[doc(inline)]
pub use footfall_analysis as analysis;
#[cfg(feature = "export")]
#[doc(inline)]
pub use footfall_export as export;
#[cfg(feature = "runtime")]
#[doc(inline)]
pub use footfall_runtime as runtime;
#[cfg(feature = "session")]
#[doc(inline)]
pub use footfall_session as session;
#[cfg(all(feature = "wtransport", not(target_family = "wasm")))]
#[doc(inline)]
pub use footfall_session_wtransport as wtransport;
#[cfg(feature = "tracing")]
#[doc(inline)]
pub use footfall_tracing as tracing_layer;
#[cfg(all(feature = "tracy", not(target_family = "wasm")))]
#[doc(inline)]
pub use footfall_tracy as tracy;
