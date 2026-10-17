// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Answers about what a Footfall-instrumented program did, from its
//! snapshots, live or from capture files, with what each answer covers.
//!
//! A [`Trace`] holds everything one runtime instance recorded, as far as its
//! snapshots show: [`Span`]s on per-thread [`Track`]s, [`Frame`]s, counter
//! series, messages, resource lifetimes, and flows. Build one from a
//! snapshot, or push each window of a subscription or capture file.
//!
//! Then ask it questions:
//!
//! - **Where did the time go?** [`Trace::spans`] starts a [`SpanQuery`]:
//!   narrow it to frames, time ranges, names, threads, labels, or
//!   arguments, and group it into a [`SpanTable`] by name, thread, context,
//!   label, or argument, such as how many nodes one layout pass handled.
//! - **Show me examples.** [`SpanQuery::instances`] returns typical and
//!   worst spans, each with a [`SpanId`] that names it in every snapshot and
//!   capture of the instance, and [`Trace::parent`], [`Trace::children`],
//!   [`Trace::frame_at`], and [`Trace::flows_through`] lead from one to the
//!   work around it, and [`Span::location`] to its code.
//! - **What changed?** A [`Comparison`] of two [`Population`]s, such as slow
//!   frames against the rest, or one run against another, splits each
//!   group's change into more spans and slower spans.
//! - **What does a timeline draw?** [`Track::spans_in`] and [`Track::zones`]
//!   return a thread's spans row by row for a visible range, merging those
//!   too short to see; [`CounterSeries::samples_in`],
//!   [`Trace::messages_in`], and [`Trace::resources_alive_at`] fill the rest.
//!
//! ## What an answer covers
//!
//! A number alone cannot say whether it is complete, so every span answer
//! carries a [`Basis`]: whether spans were [`Recorded`] at all, how many
//! were counted and how a [`Selection`] measured those crossing a range's
//! edge, how many were left out because they were still open or began
//! before the trace, and where events are missing. An empty table with a
//! complete basis means nothing ran; one whose basis says spans were not
//! recorded means nothing was looked for.
//!
//! Durations are elapsed wall-clock time on the recording thread; Footfall
//! does not measure CPU time, so a span that waited counts its waiting.
//!
//! ```
//! use footfall_analysis::{GroupBy, Trace};
//! use footfall_core::{Backend as _, ContextHandle, MarkerKind, MarkerPhase, ScopeDesc};
//! use footfall_runtime::Runtime;
//! use footfall_session::OwnedSnapshot;
//!
//! static RENDER: ScopeDesc = ScopeDesc::new("render");
//! let runtime = Runtime::default();
//! runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
//! let token = runtime.scope_begin(&RENDER, ContextHandle::ROOT, &Default::default());
//! runtime.scope_end(token);
//! runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
//!
//! let trace = Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot()));
//! let table = trace.spans().group_by(GroupBy::Name).table();
//! assert_eq!(table.row("render").unwrap().stats().count(), 1);
//! assert!(table.basis().is_complete());
//! assert_eq!(trace.frames().len(), 1);
//! ```

mod basis;
mod compare;
mod frame;
mod pacing;
mod query;
mod signal;
mod span;
mod time;
mod trace;

pub use basis::{Basis, Gap, Recorded, Signal};
pub use compare::{
    Caveat, Comparison, ComparisonQuery, ComparisonRow, Measure, Metric, Population, Side,
};
pub use frame::{Frame, FrameCounter, FrameId};
pub use pacing::{
    BasisCounts, Cadence, CadenceCounts, DeadlineCounts, FramePacing, LateCause, LateCauseCounts,
    OpportunityCounts, Outcome, OutcomeCounts, Pacing, Party, PredictionError, Support,
    SupportCounts, WastedWork, Why,
};
pub use query::{
    DurationStats, GroupBy, Instances, KeyPart, Selected, SpanQuery, SpanStats, SpanTable, TableRow,
};
pub use signal::{CounterSample, CounterSeries, Flow, FlowStep, Message, Resource};
pub use span::{OpenSpan, Placement, Span, SpanId, Track, TrackKind, Zone};
pub use time::{Selection, TimeRange};
pub use trace::{CapturedTrace, Continuity, PushError, Pushed, Trace};

pub use footfall_session::{
    OwnedArgEntry, OwnedLabelValue, OwnedObservation, OwnedSnapshot, OwnedSourceLocation,
};
