// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Spans, and the per-thread tracks that hold them for a timeline.

use crate::time::TimeRange;
use core::fmt;
use core::time::Duration;
use footfall_core::{
    ClockId, ContextId, FrameNumber, ScopeDescId, ScopeToken, TimelineId, TimelineKind,
};
use footfall_session::{OwnedArgEntry, OwnedLabelValue, OwnedSourceLocation};
use std::sync::Arc;

/// Identifies a span within its runtime instance: the sequence number of
/// the event that began it.
///
/// It names the same span in every snapshot, capture file, and consumer of
/// that instance, so it can be written down and looked up again with
/// [`Trace::span`](crate::Trace::span). It means nothing for another
/// instance; [`Trace::instance_id`](crate::Trace::instance_id) says which.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SpanId(u64);

impl SpanId {
    /// Creates the id of the span begun by the event with `sequence`.
    #[must_use]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Returns the sequence number of the event that began the span.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "span@{}", self.0)
    }
}

/// One completed scope: where it ran, under which context, for how long,
/// and inside which other span.
///
/// Times are elapsed wall-clock time on the recording thread, measured from
/// the runtime's creation; Footfall does not measure CPU time.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub(crate) id: SpanId,
    pub(crate) end_sequence: u64,
    pub(crate) token: ScopeToken,
    pub(crate) desc_id: ScopeDescId,
    pub(crate) name: Arc<str>,
    pub(crate) location: Option<OwnedSourceLocation>,
    pub(crate) context_id: ContextId,
    pub(crate) thread_id: u64,
    pub(crate) begin: Duration,
    pub(crate) end: Duration,
    pub(crate) depth: u32,
    pub(crate) parent: Option<SpanId>,
    pub(crate) child_time: Duration,
    pub(crate) interrupted: bool,
    pub(crate) args: Vec<OwnedArgEntry>,
    pub(crate) timeline: Option<TimelineId>,
    pub(crate) frame: Option<FrameNumber>,
    pub(crate) placement: Option<Placement>,
}

/// How a span timed by another clock, such as a GPU pass, was placed on the
/// trace's timeline, as [`Span::placement`] returns it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Placement {
    pub(crate) between_syncs: bool,
    pub(crate) distance: Duration,
    pub(crate) via: Option<ClockId>,
}

impl Placement {
    /// Returns `true` when both of the span's times lay between two syncs of
    /// their clock, so drift between the clocks was followed.
    #[must_use]
    pub const fn between_syncs(&self) -> bool {
        self.between_syncs
    }

    /// Returns how far the farther of the span's times lay from the clock
    /// reading used to place it: the stretch over which an error in the
    /// clock's nominal tick period accumulates.
    #[must_use]
    pub const fn distance(&self) -> Duration {
        self.distance
    }

    /// Returns the clock the times were converted through, when their own
    /// clock was only paired with another.
    #[must_use]
    pub const fn via(&self) -> Option<ClockId> {
        self.via
    }
}

impl Span {
    /// Returns the span's id.
    #[must_use]
    pub const fn id(&self) -> SpanId {
        self.id
    }

    /// Returns the sequence number of the event that ended the span.
    #[must_use]
    pub const fn end_sequence(&self) -> u64 {
        self.end_sequence
    }

    /// Returns the token the runtime issued for the scope.
    #[must_use]
    pub const fn token(&self) -> ScopeToken {
        self.token
    }

    /// Returns the scope descriptor's id, which is local to the runtime
    /// instance; match spans across instances by [`name`](Self::name).
    #[must_use]
    pub const fn desc_id(&self) -> ScopeDescId {
        self.desc_id
    }

    /// Returns the scope descriptor's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns where the scope descriptor is declared: function, file, and
    /// line. It is `None` when the descriptor does not record a location.
    #[must_use]
    pub const fn location(&self) -> Option<&OwnedSourceLocation> {
        self.location.as_ref()
    }

    /// Returns the context the span was attributed to; resolve its labels
    /// with [`Trace::context`](crate::Trace::context).
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns the thread that ran the span; resolve its name with
    /// [`Trace::track`](crate::Trace::track). For a span on a timeline, such
    /// as a GPU pass, it is the thread that reported the span.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    /// Returns the timeline the span ran on, such as a GPU queue, or `None`
    /// for a span on a CPU thread; resolve it with
    /// [`Trace::timeline`](crate::Trace::timeline).
    #[must_use]
    pub const fn timeline(&self) -> Option<TimelineId> {
        self.timeline
    }

    /// Returns the program's number for the frame the span belongs to, when
    /// it was reported with one, as timeline spans are.
    #[must_use]
    pub const fn frame_number(&self) -> Option<FrameNumber> {
        self.frame
    }

    /// Returns how a span timed by another clock was placed on the trace's
    /// timeline, or `None` for a span timed by the runtime itself.
    #[must_use]
    pub const fn placement(&self) -> Option<Placement> {
        self.placement
    }

    /// Returns when the span began.
    #[must_use]
    pub const fn begin(&self) -> Duration {
        self.begin
    }

    /// Returns when the span ended.
    #[must_use]
    pub const fn end(&self) -> Duration {
        self.end
    }

    /// Returns the time the span covers.
    #[must_use]
    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.begin, self.end)
    }

    /// Returns how long the span lasted.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.end.saturating_sub(self.begin)
    }

    /// Returns how long the span lasted outside the spans nested in it on
    /// the same thread: its own elapsed time, not CPU time.
    ///
    /// When the span is [`interrupted`](Self::interrupted), nested spans
    /// may be missing, so this can be too large.
    #[must_use]
    pub fn self_time(&self) -> Duration {
        self.duration().saturating_sub(self.child_time)
    }

    /// Returns how many spans on the same thread enclosed this one when it
    /// began; top-level spans have depth `0`. It is the span's row on a
    /// timeline; see [`Track`].
    #[must_use]
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// Returns the span that enclosed this one when it began, on the same
    /// thread.
    #[must_use]
    pub const fn parent(&self) -> Option<SpanId> {
        self.parent
    }

    /// Returns `true` when events were lost while the span was open, so its
    /// duration is exact but its nested spans, and with them its
    /// [`self_time`](Self::self_time), may be incomplete.
    #[must_use]
    pub const fn interrupted(&self) -> bool {
        self.interrupted
    }

    /// Returns the span's per-instance arguments.
    #[must_use]
    pub fn args(&self) -> &[OwnedArgEntry] {
        &self.args
    }

    /// Returns the value of the argument `key`, if the span has it.
    #[must_use]
    pub fn arg(&self, key: &str) -> Option<&OwnedLabelValue> {
        crate::query::arg(&self.args, key)
    }
}

/// A scope that has begun but not yet ended, as
/// [`Trace::open_spans`](crate::Trace::open_spans) returns it.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenSpan {
    pub(crate) id: SpanId,
    pub(crate) name: Arc<str>,
    pub(crate) location: Option<OwnedSourceLocation>,
    pub(crate) context_id: ContextId,
    pub(crate) thread_id: u64,
    pub(crate) begin: Duration,
    pub(crate) depth: u32,
    pub(crate) interrupted: bool,
}

impl OpenSpan {
    /// Returns the id the span will keep once it ends.
    #[must_use]
    pub const fn id(&self) -> SpanId {
        self.id
    }

    /// Returns the scope descriptor's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns where the scope descriptor is declared; see
    /// [`Span::location`].
    #[must_use]
    pub const fn location(&self) -> Option<&OwnedSourceLocation> {
        self.location.as_ref()
    }

    /// Returns the context the span is attributed to.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns the thread running the span.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    /// Returns when the span began.
    #[must_use]
    pub const fn begin(&self) -> Duration {
        self.begin
    }

    /// Returns the span's depth; see [`Span::depth`].
    #[must_use]
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// Returns `true` when events were lost since the span began, so its end
    /// may have been among them and it may never complete.
    #[must_use]
    pub const fn interrupted(&self) -> bool {
        self.interrupted
    }
}

/// One thread's spans, or one timeline's, such as a GPU queue's, in rows by
/// depth, for drawing a timeline.
///
/// Row `d` holds the spans of depth `d`, ordered by begin time, so a
/// timeline draws [`spans_in`](Self::spans_in) for each visible row, or
/// [`zones`](Self::zones) to merge spans too short to see at its zoom.
#[derive(Clone, Debug)]
pub struct Track {
    pub(crate) kind: TrackKind,
    pub(crate) name: Option<String>,
    pub(crate) rows: Vec<Row>,
}

/// What a [`Track`] holds: one CPU thread's spans, or one timeline's.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum TrackKind {
    /// A CPU thread, by its id within the runtime instance.
    Thread(u64),
    /// A timeline that is not a CPU thread, such as a GPU queue.
    Timeline {
        /// The timeline's id within the runtime instance.
        id: TimelineId,
        /// What the timeline stands for.
        kind: TimelineKind,
    },
}

/// One row of a track: spans by begin time, with the latest end so far at
/// each position, so finding the spans that overlap a range is a binary
/// search even when spans in a row overlap.
#[derive(Clone, Debug, Default)]
pub(crate) struct Row {
    pub(crate) spans: Vec<Span>,
    latest_end: Vec<Duration>,
}

impl Row {
    pub(crate) fn insert(&mut self, span: Span) {
        let key = (span.begin, span.id);
        let at = self
            .spans
            .partition_point(|held| (held.begin, held.id) <= key);
        self.spans.insert(at, span);
        self.latest_end.truncate(at);
        let mut latest = at
            .checked_sub(1)
            .map_or(Duration::ZERO, |before| self.latest_end[before]);
        for span in &self.spans[at..] {
            latest = latest.max(span.end);
            self.latest_end.push(latest);
        }
    }

    pub(crate) fn find(&self, begin: Duration, id: SpanId) -> Option<&Span> {
        let at = self
            .spans
            .partition_point(|held| (held.begin, held.id) < (begin, id));
        self.spans.get(at).filter(|span| span.id == id)
    }

    pub(crate) fn find_mut(&mut self, begin: Duration, id: SpanId) -> Option<&mut Span> {
        let at = self
            .spans
            .partition_point(|held| (held.begin, held.id) < (begin, id));
        self.spans.get_mut(at).filter(|span| span.id == id)
    }

    pub(crate) fn overlapping(&self, range: TimeRange) -> impl Iterator<Item = &Span> {
        let first = self.latest_end.partition_point(|end| *end < range.start());
        self.spans[first..]
            .iter()
            .take_while(move |span| span.begin < range.end())
            .filter(move |span| range.overlaps(span.begin, span.end))
    }

    /// Takes out the spans that begin at or after `from`.
    pub(crate) fn take_from(&mut self, from: Duration) -> Vec<Span> {
        let at = self.spans.partition_point(|held| held.begin < from);
        self.latest_end.truncate(at);
        self.spans.split_off(at)
    }

    /// Drops the spans that ended before `before`.
    pub(crate) fn forget(&mut self, before: Duration) -> Vec<SpanId> {
        let mut forgotten = Vec::new();
        self.spans.retain(|span| {
            let keep = span.end >= before;
            if !keep {
                forgotten.push(span.id);
            }
            keep
        });
        self.latest_end.clear();
        let mut latest = Duration::ZERO;
        for span in &self.spans {
            latest = latest.max(span.end);
            self.latest_end.push(latest);
        }
        forgotten
    }
}

impl Track {
    pub(crate) const fn new(kind: TrackKind) -> Self {
        Self {
            kind,
            name: None,
            rows: Vec::new(),
        }
    }

    /// Returns whether the track is a CPU thread's or a timeline's.
    #[must_use]
    pub const fn kind(&self) -> TrackKind {
        self.kind
    }

    /// Returns the thread's id within the runtime instance, for a thread's
    /// track.
    #[must_use]
    pub const fn thread_id(&self) -> Option<u64> {
        match self.kind {
            TrackKind::Thread(id) => Some(id),
            TrackKind::Timeline { .. } => None,
        }
    }

    /// Returns the timeline's id within the runtime instance, for a
    /// timeline's track.
    #[must_use]
    pub const fn timeline_id(&self) -> Option<TimelineId> {
        match self.kind {
            TrackKind::Thread(_) => None,
            TrackKind::Timeline { id, .. } => Some(id),
        }
    }

    /// Returns the thread's or timeline's name, if it had one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns how many rows of spans the track has: one more than its
    /// deepest span's depth.
    #[must_use]
    pub fn depth(&self) -> u32 {
        u32::try_from(self.rows.len()).unwrap_or(u32::MAX)
    }

    /// Returns every span of depth `depth`, by begin time.
    #[must_use]
    pub fn spans(&self, depth: u32) -> &[Span] {
        self.row(depth).map_or(&[], |row| &row.spans)
    }

    /// Returns the spans of depth `depth` that share time with `range`, by
    /// begin time.
    pub fn spans_in(&self, depth: u32, range: TimeRange) -> impl Iterator<Item = &Span> {
        self.row(depth)
            .into_iter()
            .flat_map(move |row| row.overlapping(range))
    }

    /// Returns what a timeline should draw for row `depth` over `range`
    /// when anything shorter than `min_width` is too small to see: spans at
    /// least that long, and runs of shorter spans merged into one
    /// [`Zone::Merged`].
    ///
    /// Spans join a run while the gap before each is also shorter than
    /// `min_width`, so the run covers what would blur together on screen.
    pub fn zones(&self, depth: u32, range: TimeRange, min_width: Duration) -> Vec<Zone<'_>> {
        let mut zones = Vec::new();
        let mut run: Option<(TimeRange, usize)> = None;
        for span in self.spans_in(depth, range) {
            if span.duration() >= min_width {
                if let Some((range, count)) = run.take() {
                    zones.push(Zone::Merged { range, count });
                }
                zones.push(Zone::Span(span));
                continue;
            }
            run = match run {
                Some((merged, count)) if span.begin.saturating_sub(merged.end()) < min_width => {
                    Some((
                        TimeRange::new(merged.start(), merged.end().max(span.end)),
                        count + 1,
                    ))
                }
                Some(finished) => {
                    zones.push(Zone::Merged {
                        range: finished.0,
                        count: finished.1,
                    });
                    Some((span.range(), 1))
                }
                None => Some((span.range(), 1)),
            };
        }
        if let Some((range, count)) = run {
            zones.push(Zone::Merged { range, count });
        }
        zones
    }

    fn row(&self, depth: u32) -> Option<&Row> {
        self.rows.get(usize::try_from(depth).ok()?)
    }

    pub(crate) fn row_mut(&mut self, depth: u32) -> &mut Row {
        let depth = usize::try_from(depth).unwrap_or(usize::MAX);
        if self.rows.len() <= depth {
            self.rows.resize_with(depth + 1, Row::default);
        }
        &mut self.rows[depth]
    }
}

/// What a timeline draws in one row at its current zoom, as
/// [`Track::zones`] returns it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zone<'a> {
    /// A span long enough to draw on its own.
    Span(&'a Span),
    /// Consecutive spans too short to tell apart, drawn as one.
    Merged {
        /// From the first span's begin to the last one's end.
        range: TimeRange,
        /// How many spans were merged.
        count: usize,
    },
}
