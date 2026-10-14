// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounded in-process capture for Footfall.
//!
//! A [`Runtime`] is a [`Backend`](footfall_core::Backend): install it once
//! with [`RuntimeBuilder::install_global`] and every emission in the process
//! lands in it. It keeps cumulative counters and histograms and a recent
//! window of timestamped events in fixed-capacity buffers, so memory use is
//! bounded however long the process runs; whatever does not fit is counted
//! in [`Drops`].
//!
//! Reading never drains it. [`Runtime::snapshot`] returns everything
//! retained as a [`Snapshot`]; [`Runtime::snapshot_since`] returns only the
//! events recorded since an [`EventCursor`], so any number of readers can
//! follow the same runtime.
//!
//! ```
//! use footfall_core::{ContextHandle, ScopeDesc, frame_begin, frame_end, scope};
//! use footfall_runtime::Runtime;
//!
//! static RENDER: ScopeDesc = ScopeDesc::new("render");
//!
//! let runtime = Runtime::builder()
//!     .process_name("my_app")
//!     .install_global()
//!     .expect("no other backend is installed");
//!
//! let cursor = runtime.cursor();
//! frame_begin(ContextHandle::ROOT);
//! drop(scope(&RENDER, ContextHandle::ROOT));
//! frame_end(ContextHandle::ROOT);
//!
//! // Only what happened since the cursor: the frame and its scope.
//! let snapshot = runtime.snapshot_since(cursor);
//! assert_eq!(snapshot.window().missed(), 0);
//! assert!(!snapshot.events().is_empty());
//! // The next read starts where this one ended.
//! let _later = runtime.snapshot_since(snapshot.next_cursor());
//! ```
//!
//! A snapshot is a plain value. `footfall_export` writes it as a Chrome
//! trace, `footfall_session` turns it into an `OwnedSnapshot` for consumers
//! and capture files, and `footfall_session_wtransport` serves the runtime
//! to other processes. The `footfall` crate re-exports all of these behind
//! features.
//!
//! Without the `std` feature this crate has only the snapshot types:
//! [`Snapshot`] and what it holds. They need only `alloc`, so a program
//! without `std` can build snapshots with [`Snapshot::builder`] and hand
//! them on. The [`Runtime`] that records them needs `std`.

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "std")]
mod runtime;
mod snapshot;

#[cfg(feature = "std")]
pub use runtime::{MESSAGE_TEXT_MAX_BYTES, Runtime, RuntimeBuilder, current_thread_id};
pub use snapshot::{
    CaptureSettings, ClockTicks, ContextSnapshot, CounterKind, CounterSnapshot, Drops, EventCursor,
    EventWindow, HISTOGRAM_BUCKET_COUNT, HistogramSnapshot, Observation, Snapshot, SnapshotBuilder,
    SnapshotMetadata, ThreadSnapshot, TimedObservation, TimelineWork, histogram_bucket_index,
};
