// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scope, frame, and counter analysis over Footfall snapshots.
//!
//! Everything here works on an [`OwnedSnapshot`], so it answers the same
//! questions about a snapshot read in process, fetched from a live session,
//! or loaded from a capture file:
//!
//! - [`Spans`] pairs scope begins with their ends into [`Span`]s with
//!   durations, self time, and nesting, and summarizes them by name or by a
//!   context label.
//! - [`frames`] pairs frame markers into [`Frame`]s with their durations and
//!   the change in each counter over the frame, from the counter samples the
//!   runtime records at frame ends.
//! - [`SpanTracker`] and [`FrameTracker`] do the same across consecutive
//!   snapshots, such as a subscription's windows, carrying open scopes,
//!   open frames, and the last counter samples from one to the next, and
//!   reporting gaps and producer restarts as a [`Continuity`].
//!
//! ```
//! use footfall_analysis::{Spans, frames};
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
//! let snapshot = OwnedSnapshot::from(runtime.snapshot());
//! let spans = Spans::from_snapshot(&snapshot);
//! assert_eq!(spans.by_name()["render"].count(), 1);
//! assert_eq!(frames(&snapshot).len(), 1);
//! ```

use std::collections::{BTreeMap, HashMap};

use footfall_core::{ContextId, CounterDescId, MarkerKind, MarkerPhase, ScopeToken, Unit};
use footfall_runtime::CounterKind;
use footfall_session::OwnedObservation;
pub use footfall_session::{OwnedArgEntry, OwnedLabelValue, OwnedSnapshot};

/// One scope that began and ended within a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Span {
    name: String,
    context_id: ContextId,
    thread_id: u64,
    begin_micros: u64,
    end_micros: u64,
    depth: usize,
    child_micros: u64,
    args: Vec<OwnedArgEntry>,
}

impl Span {
    /// Returns the scope's descriptor name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the context the scope was attributed to.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns the thread that began the scope.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    /// Returns when the scope began, in microseconds since runtime creation.
    #[must_use]
    pub const fn begin_micros(&self) -> u64 {
        self.begin_micros
    }

    /// Returns when the scope ended, in microseconds since runtime creation.
    #[must_use]
    pub const fn end_micros(&self) -> u64 {
        self.end_micros
    }

    /// Returns how long the scope lasted.
    #[must_use]
    pub const fn duration_micros(&self) -> u64 {
        self.end_micros.saturating_sub(self.begin_micros)
    }

    /// Returns how long the scope lasted outside the scopes nested in it on
    /// the same thread.
    #[must_use]
    pub const fn self_micros(&self) -> u64 {
        self.duration_micros().saturating_sub(self.child_micros)
    }

    /// Returns how many scopes on the same thread enclosed this one when it
    /// began; top-level scopes have depth `0`.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns the scope's per-instance arguments.
    #[must_use]
    pub fn args(&self) -> &[OwnedArgEntry] {
        &self.args
    }
}

/// A scope that began in a snapshot but had not ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenSpan {
    name: String,
    context_id: ContextId,
    thread_id: u64,
    begin_micros: u64,
}

impl OpenSpan {
    /// Returns the scope's descriptor name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the context the scope was attributed to.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns the thread that began the scope.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    /// Returns when the scope began.
    #[must_use]
    pub const fn begin_micros(&self) -> u64 {
        self.begin_micros
    }
}

/// The scopes of one snapshot, paired by token.
///
/// A snapshot holds a window of events, so some scopes are cut off: those
/// that began but had not ended when the snapshot was taken are listed by
/// [`open`](Self::open), and ends whose begin was evicted or fell before the
/// window are counted by [`unmatched_ends`](Self::unmatched_ends). Neither
/// counts toward durations.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Spans {
    spans: Vec<Span>,
    open: Vec<OpenSpan>,
    unmatched_ends: usize,
}

impl Spans {
    /// Pairs the scopes in `snapshot`.
    #[must_use]
    pub fn from_snapshot(snapshot: &OwnedSnapshot) -> Self {
        SpanTracker::new().push(snapshot)
    }

    /// Returns the completed scopes, by begin time.
    #[must_use]
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// Returns the scopes that had begun but not ended, by begin time.
    #[must_use]
    pub fn open(&self) -> &[OpenSpan] {
        &self.open
    }

    /// Returns how many scope ends had no begin in the snapshot.
    #[must_use]
    pub const fn unmatched_ends(&self) -> usize {
        self.unmatched_ends
    }

    /// Returns the completed scopes that began within `[begin, end)`, in
    /// microseconds since runtime creation, such as those of one frame.
    pub fn within(&self, begin_micros: u64, end_micros: u64) -> impl Iterator<Item = &Span> {
        self.spans
            .iter()
            .filter(move |span| (begin_micros..end_micros).contains(&span.begin_micros))
    }

    /// Summarizes the completed scopes by name.
    #[must_use]
    pub fn by_name(&self) -> BTreeMap<&str, SpanStats> {
        let mut stats = BTreeMap::<&str, SpanStats>::new();
        for span in &self.spans {
            stats.entry(span.name()).or_default().add(span);
        }
        stats
    }

    /// Summarizes the completed scopes by the value of the context label
    /// `key`, then by name. Scopes whose context lacks the label are left
    /// out.
    #[must_use]
    pub fn by_label<'a>(
        &'a self,
        snapshot: &'a OwnedSnapshot,
        key: &str,
    ) -> BTreeMap<&'a OwnedLabelValue, BTreeMap<&'a str, SpanStats>> {
        let mut stats = BTreeMap::<&OwnedLabelValue, BTreeMap<&str, SpanStats>>::new();
        for span in &self.spans {
            if let Some(value) = snapshot.label(span.context_id, key) {
                stats
                    .entry(value)
                    .or_default()
                    .entry(span.name())
                    .or_default()
                    .add(span);
            }
        }
        stats
    }
}

/// A scope begun in an earlier snapshot, waiting for its end.
#[derive(Clone, Debug)]
struct Begun {
    name: String,
    context_id: ContextId,
    thread_id: u64,
    begin_micros: u64,
    depth: usize,
    child_micros: u64,
    args: Vec<OwnedArgEntry>,
}

/// Pairs scopes across consecutive snapshots of one runtime, such as the
/// windows a subscription delivers, without keeping the snapshots.
///
/// Each [`push`](Self::push) returns the scopes that ended in that snapshot,
/// including ones that began in an earlier one, and the scopes still open.
/// The tracker starts over when a snapshot comes from another runtime
/// instance, since its scope tokens mean something else, or when events
/// between two snapshots were evicted, since scope ends may be among them;
/// [`push`](Self::push) reports both in [`Continuity`].
#[derive(Clone, Debug, Default)]
pub struct SpanTracker {
    instance_id: Option<u64>,
    next_sequence: u64,
    begun: HashMap<ScopeToken, Begun>,
    /// Open scopes per thread, innermost last, for nesting and self time.
    stacks: HashMap<u64, Vec<ScopeToken>>,
    continuity: Continuity,
}

/// How a snapshot followed the ones a tracker saw before it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Continuity {
    /// The first snapshot the tracker saw.
    #[default]
    First,
    /// It continues the previous snapshot with no events missing.
    Continuous,
    /// Events between the previous snapshot and this one were evicted or
    /// skipped, so the tracker started over.
    Gap,
    /// It comes from another runtime instance, so the tracker started over.
    Restarted,
}

impl SpanTracker {
    /// Creates a tracker that has seen no snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns how the last pushed snapshot followed the one before it.
    #[must_use]
    pub const fn continuity(&self) -> Continuity {
        self.continuity
    }

    /// Pairs the scopes of `snapshot` with those still open from earlier
    /// snapshots.
    pub fn push(&mut self, snapshot: &OwnedSnapshot) -> Spans {
        self.continuity = continuity(&mut self.instance_id, self.next_sequence, snapshot);
        if matches!(self.continuity, Continuity::Gap | Continuity::Restarted) {
            self.begun.clear();
            self.stacks.clear();
        }
        self.next_sequence = snapshot.window().next();

        let mut spans = Spans::default();
        for event in snapshot.events() {
            match event.observation() {
                OwnedObservation::ScopeBegin {
                    name,
                    context_id,
                    token,
                    args,
                    ..
                } => {
                    let stack = self.stacks.entry(event.thread_id()).or_default();
                    self.begun.insert(
                        *token,
                        Begun {
                            name: name.clone(),
                            context_id: *context_id,
                            thread_id: event.thread_id(),
                            begin_micros: event.at_micros(),
                            depth: stack.len(),
                            child_micros: 0,
                            args: args.clone(),
                        },
                    );
                    stack.push(*token);
                }
                OwnedObservation::ScopeEnd { token } => {
                    let Some(scope) = self.begun.remove(token) else {
                        spans.unmatched_ends += 1;
                        continue;
                    };
                    let duration = event.at_micros().saturating_sub(scope.begin_micros);
                    if let Some(stack) = self.stacks.get_mut(&scope.thread_id) {
                        stack.retain(|open| open != token);
                        if let Some(parent) =
                            stack.last().and_then(|parent| self.begun.get_mut(parent))
                        {
                            parent.child_micros = parent.child_micros.saturating_add(duration);
                        }
                    }
                    spans.spans.push(Span {
                        name: scope.name,
                        context_id: scope.context_id,
                        thread_id: scope.thread_id,
                        begin_micros: scope.begin_micros,
                        end_micros: event.at_micros(),
                        depth: scope.depth,
                        child_micros: scope.child_micros,
                        args: scope.args,
                    });
                }
                _ => {}
            }
        }
        spans.open = self
            .begun
            .values()
            .map(|scope| OpenSpan {
                name: scope.name.clone(),
                context_id: scope.context_id,
                thread_id: scope.thread_id,
                begin_micros: scope.begin_micros,
            })
            .collect();
        spans.open.sort_by_key(|span| span.begin_micros);
        spans.spans.sort_by_key(|span| span.begin_micros);
        spans
    }
}

/// Classifies how `snapshot` follows the previous one and records its
/// instance.
fn continuity(
    instance_id: &mut Option<u64>,
    next_sequence: u64,
    snapshot: &OwnedSnapshot,
) -> Continuity {
    let instance = snapshot.metadata().instance_id();
    let window = snapshot.window();
    let previous = instance_id.replace(instance);
    match previous {
        None => Continuity::First,
        Some(previous) if previous != instance => Continuity::Restarted,
        Some(_) if window.since() != next_sequence || window.missed() > 0 => Continuity::Gap,
        Some(_) => Continuity::Continuous,
    }
}

/// Statistics over a group of completed scopes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpanStats {
    count: u64,
    total_micros: u64,
    self_micros: u64,
    min_micros: u64,
    max_micros: u64,
}

impl SpanStats {
    fn add(&mut self, span: &Span) {
        let duration = span.duration_micros();
        self.min_micros = if self.count == 0 {
            duration
        } else {
            self.min_micros.min(duration)
        };
        self.max_micros = self.max_micros.max(duration);
        self.count += 1;
        self.total_micros = self.total_micros.saturating_add(duration);
        self.self_micros = self.self_micros.saturating_add(span.self_micros());
    }

    /// Returns how many scopes there were.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Returns their summed duration.
    #[must_use]
    pub const fn total_micros(&self) -> u64 {
        self.total_micros
    }

    /// Returns their summed self time; see [`Span::self_micros`].
    #[must_use]
    pub const fn self_micros(&self) -> u64 {
        self.self_micros
    }

    /// Returns the shortest duration.
    #[must_use]
    pub const fn min_micros(&self) -> u64 {
        self.min_micros
    }

    /// Returns the longest duration.
    #[must_use]
    pub const fn max_micros(&self) -> u64 {
        self.max_micros
    }

    /// Returns the mean duration, rounded down.
    #[must_use]
    pub const fn mean_micros(&self) -> u64 {
        match self.total_micros.checked_div(self.count) {
            Some(mean) => mean,
            None => 0,
        }
    }
}

/// One frame, from a frame begin marker to the next frame end marker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    sequence: u64,
    context_id: ContextId,
    begin_micros: u64,
    end_micros: u64,
    counters: Vec<FrameCounter>,
}

impl Frame {
    /// Returns the event sequence number of the frame's begin marker, which
    /// identifies the frame within its runtime instance across snapshots,
    /// captures, and consumers.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the context of the frame begin marker.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns when the frame began.
    #[must_use]
    pub const fn begin_micros(&self) -> u64 {
        self.begin_micros
    }

    /// Returns when the frame ended.
    #[must_use]
    pub const fn end_micros(&self) -> u64 {
        self.end_micros
    }

    /// Returns how long the frame lasted.
    #[must_use]
    pub const fn duration_micros(&self) -> u64 {
        self.end_micros.saturating_sub(self.begin_micros)
    }

    /// Returns the counters and gauges the frame updated, from the counter
    /// samples recorded at its end.
    #[must_use]
    pub fn counters(&self) -> &[FrameCounter] {
        &self.counters
    }

    /// Returns the counter or gauge named `name` under the context
    /// `context_id`, if the frame updated it.
    #[must_use]
    pub fn counter(&self, name: &str, context_id: ContextId) -> Option<&FrameCounter> {
        self.counters
            .iter()
            .find(|counter| counter.name == name && counter.context_id == context_id)
    }
}

/// A counter or gauge a frame updated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameCounter {
    desc_id: CounterDescId,
    name: String,
    unit: Unit,
    context_id: ContextId,
    kind: CounterKind,
    value: u64,
    change: Option<u64>,
}

impl FrameCounter {
    /// Returns the descriptor name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the descriptor id.
    #[must_use]
    pub const fn desc_id(&self) -> CounterDescId {
        self.desc_id
    }

    /// Returns the unit.
    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }

    /// Returns the context the counter was recorded under.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns whether it is an additive counter or a gauge.
    #[must_use]
    pub const fn kind(&self) -> CounterKind {
        self.kind
    }

    /// Returns its value at the end of the frame: cumulative for a counter,
    /// the last value set for a gauge.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }

    /// Returns how much a counter grew during the frame.
    ///
    /// This is the difference from the counter's previous sample in the
    /// snapshot. For a counter's first sample it is the whole value when the
    /// snapshot holds the runtime's complete history, since the counter was
    /// zero before, and `None` otherwise, since the snapshot cannot tell. It
    /// is always `None` for a gauge.
    #[must_use]
    pub const fn change(&self) -> Option<u64> {
        self.change
    }
}

/// Pairs the frame markers in `snapshot` into frames, in order.
///
/// Each frame end closes the most recent frame begin. Counter samples that
/// follow a frame end, on the same thread at the same time, become the
/// frame's [`counters`](Frame::counters). A frame whose begin fell before the
/// snapshot's window is left out; to follow frames across consecutive
/// snapshots, use a [`FrameTracker`].
#[must_use]
pub fn frames(snapshot: &OwnedSnapshot) -> Vec<Frame> {
    FrameTracker::new().push(snapshot)
}

/// Pairs frames across consecutive snapshots of one runtime, such as the
/// windows a subscription delivers, without keeping the snapshots.
///
/// Each [`push`](Self::push) returns the frames that ended in that snapshot,
/// including ones that began in an earlier one, with counter changes computed
/// from the last sample the tracker saw, even in an earlier snapshot. Like a
/// [`SpanTracker`], it starts over after a gap or a restart, reported by
/// [`continuity`](Self::continuity).
#[derive(Clone, Debug, Default)]
pub struct FrameTracker {
    instance_id: Option<u64>,
    next_sequence: u64,
    /// Begun frames: begin sequence, context, and time.
    open: Vec<(u64, ContextId, u64)>,
    previous: HashMap<(CounterDescId, ContextId), u64>,
    /// Whether every event since the runtime started has been seen, so a
    /// counter without a previous sample was zero before.
    complete_history: bool,
    continuity: Continuity,
}

impl FrameTracker {
    /// Creates a tracker that has seen no snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns how the last pushed snapshot followed the one before it.
    #[must_use]
    pub const fn continuity(&self) -> Continuity {
        self.continuity
    }

    /// Pairs the frames of `snapshot` with those still open from earlier
    /// snapshots.
    pub fn push(&mut self, snapshot: &OwnedSnapshot) -> Vec<Frame> {
        self.continuity = continuity(&mut self.instance_id, self.next_sequence, snapshot);
        match self.continuity {
            Continuity::First => {
                self.complete_history =
                    snapshot.window().since() == 0 && snapshot.window().missed() == 0;
            }
            Continuity::Continuous => {}
            Continuity::Gap | Continuity::Restarted => {
                self.open.clear();
                self.previous.clear();
                self.complete_history = false;
            }
        }
        self.next_sequence = snapshot.window().next();

        let mut frames = Vec::new();
        // The frame end whose samples are being collected: thread and time.
        let mut collecting: Option<(u64, u64)> = None;
        for event in snapshot.events() {
            match event.observation() {
                OwnedObservation::FrameMarker {
                    kind: MarkerKind::Frame,
                    phase,
                    context_id,
                } => {
                    collecting = None;
                    match phase {
                        MarkerPhase::Begin => {
                            self.open
                                .push((event.sequence(), *context_id, event.at_micros()));
                        }
                        MarkerPhase::End => {
                            if let Some((sequence, context_id, begin_micros)) = self.open.pop() {
                                frames.push(Frame {
                                    sequence,
                                    context_id,
                                    begin_micros,
                                    end_micros: event.at_micros(),
                                    counters: Vec::new(),
                                });
                                collecting = Some((event.thread_id(), event.at_micros()));
                            }
                        }
                    }
                }
                OwnedObservation::CounterSample {
                    desc_id,
                    name,
                    unit,
                    context_id,
                    kind,
                    value,
                } => {
                    let key = (*desc_id, *context_id);
                    let change = match kind {
                        CounterKind::Counter => match self.previous.get(&key) {
                            Some(before) => Some(value.saturating_sub(*before)),
                            None if self.complete_history => Some(*value),
                            None => None,
                        },
                        _ => None,
                    };
                    self.previous.insert(key, *value);
                    if collecting == Some((event.thread_id(), event.at_micros()))
                        && let Some(frame) = frames.last_mut()
                    {
                        frame.counters.push(FrameCounter {
                            desc_id: *desc_id,
                            name: name.clone(),
                            unit: *unit,
                            context_id: *context_id,
                            kind: *kind,
                            value: *value,
                            change,
                        });
                    }
                }
                _ => {}
            }
        }
        frames
    }
}
