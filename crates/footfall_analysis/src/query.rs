// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Selecting, grouping, and summarizing spans.

use crate::basis::{Basis, Signal};
use crate::frame::FrameId;
use crate::span::{Span, TrackKind};
use crate::time::{Selection, TimeRange, merge_ranges};
use crate::trace::Trace;
use core::fmt;
use core::time::Duration;
use footfall_core::{ContextId, TimelineId};
use footfall_session::{OwnedArgEntry, OwnedLabelValue};
use std::collections::BTreeMap;

/// A query over a trace's completed spans, from [`Trace::spans`].
///
/// Narrow it by time, name, thread, context, label, or argument, choose how
/// spans crossing a range's edge count with a [`Selection`], then ask for
/// every matching span with [`iter`](Self::iter), a summary grouped by
/// name, thread, context, label, or argument with [`table`](Self::table), or
/// typical and worst examples with [`instances`](Self::instances). Every
/// answer carries a [`Basis`] saying what it covers.
///
/// ```
/// # use footfall_analysis::{GroupBy, Selection, Trace};
/// # fn query(trace: &Trace) {
/// let slow: Vec<_> = trace
///     .frames()
///     .iter()
///     .filter(|frame| frame.duration().as_millis() > 16)
///     .map(|frame| frame.id())
///     .collect();
/// let table = trace
///     .spans()
///     .in_frames(&slow)
///     .selecting(Selection::Inside)
///     .group_by(GroupBy::Label("pass".into()))
///     .group_by(GroupBy::Name)
///     .table();
/// for row in table.rows().iter().take(10) {
///     println!("{}: {:?}", row.describe(), row.stats().total());
/// }
/// println!("{}", table.basis());
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct SpanQuery<'t> {
    trace: &'t Trace,
    ranges: Option<Vec<TimeRange>>,
    /// The program's numbers of the frames selected, by which timeline
    /// spans reported with a frame number are selected.
    frame_numbers: Vec<u64>,
    selection: Selection,
    name: Option<String>,
    tracks: Tracks,
    context: Option<ContextId>,
    labels: Vec<(String, Option<OwnedLabelValue>)>,
    args: Vec<(String, Option<OwnedLabelValue>)>,
    group_by: Vec<GroupBy>,
}

/// A span a query selected, with the time it counts for under the query's
/// [`Selection`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selected<'t> {
    span: &'t Span,
    time: Duration,
    self_time: Duration,
}

impl<'t> Selected<'t> {
    /// Returns the span.
    #[must_use]
    pub const fn span(&self) -> &'t Span {
        self.span
    }

    /// Returns the time the span counts for: its whole duration, or under
    /// [`Selection::Inside`] the part inside the selected ranges.
    #[must_use]
    pub const fn time(&self) -> Duration {
        self.time
    }

    /// Returns the self time the span counts for: [`time`](Self::time) less
    /// the time its nested spans count for.
    #[must_use]
    pub const fn self_time(&self) -> Duration {
        self.self_time
    }
}

/// How a [`SpanQuery::table`] groups spans; give several to group by each
/// in turn, like a `GROUP BY` with several columns.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum GroupBy {
    /// The scope descriptor's name.
    Name,
    /// The thread that ran the span, or for a span on a timeline such as a
    /// GPU queue, the timeline.
    Thread,
    /// The context the span was attributed to, as a whole.
    Context,
    /// The value of one label of the span's context. Spans whose context
    /// lacks the label form their own group rather than being left out.
    Label(String),
    /// The value of one of the span's own arguments, such as the number of
    /// nodes one layout pass handled. Spans without the argument form their
    /// own group rather than being left out.
    ///
    /// Each distinct value is a group. For a number that rarely repeats,
    /// read it with [`Span::arg`] from each span [`SpanQuery::iter`]
    /// returns, and group the spans in ranges that suit it.
    Arg(String),
}

/// Which tracks a query covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tracks {
    All,
    Thread(u64),
    Threads,
    Timeline(TimelineId),
    Timelines,
}

/// One part of a [`TableRow`]'s key, for one [`GroupBy`].
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum KeyPart {
    /// A scope name.
    Name(String),
    /// A thread.
    Thread {
        /// The thread's id within the runtime instance.
        id: u64,
        /// The thread's name, if it had one.
        name: Option<String>,
    },
    /// A timeline that is not a CPU thread, such as a GPU queue, for
    /// [`GroupBy::Thread`].
    Timeline {
        /// The timeline's id within the runtime instance.
        id: TimelineId,
        /// The timeline's name.
        name: String,
    },
    /// A context.
    Context {
        /// The context's id within the runtime instance.
        id: ContextId,
        /// Its labels, as [`Trace::describe_context`] renders them.
        labels: String,
    },
    /// A label's value.
    Label {
        /// The label key.
        key: String,
        /// Its value, or `None` for spans whose context lacks the label.
        value: Option<OwnedLabelValue>,
    },
    /// An argument's value.
    Arg {
        /// The argument key.
        key: String,
        /// Its value, or `None` for spans without the argument.
        value: Option<OwnedLabelValue>,
    },
}

impl KeyPart {
    /// Returns what the part means independent of its runtime instance:
    /// names, label and argument values, thread names, and context labels,
    /// but not ids. Comparisons across instances match rows by it.
    #[must_use]
    pub fn semantic(&self) -> String {
        match self {
            Self::Thread {
                name: Some(name), ..
            } => format!("thread {name}"),
            Self::Timeline { name, .. } => format!("timeline {name}"),
            Self::Context { labels, .. } => format!("context {labels}"),
            other => other.to_string(),
        }
    }
}

impl fmt::Display for KeyPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => f.write_str(name),
            Self::Thread {
                name: Some(name), ..
            } => f.write_str(name),
            Self::Thread { id, name: None } => write!(f, "thread {id}"),
            Self::Timeline { name, .. } => f.write_str(name),
            Self::Context { labels, .. } => f.write_str(labels),
            Self::Label {
                key,
                value: Some(value),
            }
            | Self::Arg {
                key,
                value: Some(value),
            } => write!(f, "{key}={value}"),
            Self::Label { key, value: None } | Self::Arg { key, value: None } => {
                write!(f, "(no {key})")
            }
        }
    }
}

/// Spans grouped and summarized, as [`SpanQuery::table`] returns them.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanTable {
    group_by: Vec<GroupBy>,
    rows: Vec<TableRow>,
    total: SpanStats,
    basis: Basis,
}

impl SpanTable {
    /// Returns how the rows are grouped.
    #[must_use]
    pub fn group_by(&self) -> &[GroupBy] {
        &self.group_by
    }

    /// Returns one row per group, most total time first. Without grouping,
    /// one row holds every span.
    #[must_use]
    pub fn rows(&self) -> &[TableRow] {
        &self.rows
    }

    /// Returns the row whose key reads `key`, part by part, as
    /// [`TableRow::describe`] writes it.
    #[must_use]
    pub fn row(&self, key: &str) -> Option<&TableRow> {
        self.rows.iter().find(|row| row.describe() == key)
    }

    /// Returns the statistics over every selected span.
    #[must_use]
    pub const fn total(&self) -> &SpanStats {
        &self.total
    }

    /// Returns what the table covers.
    #[must_use]
    pub const fn basis(&self) -> &Basis {
        &self.basis
    }
}

/// One group of a [`SpanTable`].
#[derive(Clone, Debug, PartialEq)]
pub struct TableRow {
    key: Vec<KeyPart>,
    stats: SpanStats,
}

impl TableRow {
    /// Returns the row's key, one part per [`GroupBy`].
    #[must_use]
    pub fn key(&self) -> &[KeyPart] {
        &self.key
    }

    /// Returns the key as text, parts joined by ` / `.
    #[must_use]
    pub fn describe(&self) -> String {
        self.key
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// Returns the group's statistics.
    #[must_use]
    pub const fn stats(&self) -> &SpanStats {
        &self.stats
    }
}

/// Statistics over a group of durations: count, total, extremes, and
/// percentiles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DurationStats {
    count: u64,
    total: Duration,
    min: Duration,
    max: Duration,
    median: Duration,
    p90: Duration,
    p99: Duration,
}

impl DurationStats {
    /// Summarizes `durations`, such as frame durations.
    #[must_use]
    pub fn of(durations: impl IntoIterator<Item = Duration>) -> Self {
        let mut durations: Vec<_> = durations.into_iter().collect();
        durations.sort_unstable();
        let Some((&min, &max)) = durations.first().zip(durations.last()) else {
            return Self::default();
        };
        // Nearest rank: the smallest value with at least `q` of the values
        // at or below it.
        let rank = |q: f64| {
            let rank = (q * durations.len() as f64).ceil();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "rank is between 1 and the number of durations"
            )]
            let rank = rank as usize;
            durations[rank.clamp(1, durations.len()) - 1]
        };
        Self {
            count: durations.len() as u64,
            total: durations.iter().sum(),
            min,
            max,
            median: rank(0.5),
            p90: rank(0.9),
            p99: rank(0.99),
        }
    }

    /// Returns how many durations there were.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Returns their sum.
    #[must_use]
    pub const fn total(&self) -> Duration {
        self.total
    }

    /// Returns the shortest.
    #[must_use]
    pub const fn min(&self) -> Duration {
        self.min
    }

    /// Returns the longest.
    #[must_use]
    pub const fn max(&self) -> Duration {
        self.max
    }

    /// Returns the mean.
    #[must_use]
    pub fn mean(&self) -> Duration {
        match u32::try_from(self.count) {
            Ok(0) => Duration::ZERO,
            Ok(count) => self.total / count,
            Err(_) => Duration::from_secs_f64(self.total.as_secs_f64() / self.count as f64),
        }
    }

    /// Returns the median, by nearest rank.
    #[must_use]
    pub const fn median(&self) -> Duration {
        self.median
    }

    /// Returns the 90th percentile, by nearest rank.
    #[must_use]
    pub const fn p90(&self) -> Duration {
        self.p90
    }

    /// Returns the 99th percentile, by nearest rank.
    #[must_use]
    pub const fn p99(&self) -> Duration {
        self.p99
    }
}

/// Statistics over a group of selected spans, as a [`TableRow`] holds
/// them. Every figure is of the time each span counts for under the
/// query's [`Selection`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpanStats {
    time: DurationStats,
    self_time: Duration,
    interrupted: u64,
}

impl SpanStats {
    /// Returns the distribution of the spans' times.
    #[must_use]
    pub const fn time(&self) -> &DurationStats {
        &self.time
    }

    /// Returns how many spans there were.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.time.count
    }

    /// Returns their summed time.
    #[must_use]
    pub const fn total(&self) -> Duration {
        self.time.total
    }

    /// Returns their summed self time: the time not spent in spans nested
    /// in them on the same thread.
    #[must_use]
    pub const fn self_time(&self) -> Duration {
        self.self_time
    }

    /// Returns how many of them were [interrupted](Span::interrupted), so
    /// their self time may be too large.
    #[must_use]
    pub const fn interrupted(&self) -> u64 {
        self.interrupted
    }

    fn of(selected: &[Selected<'_>]) -> Self {
        Self {
            time: DurationStats::of(selected.iter().map(|selected| selected.time)),
            self_time: selected.iter().map(|selected| selected.self_time).sum(),
            interrupted: selected
                .iter()
                .filter(|selected| selected.span.interrupted)
                .count() as u64,
        }
    }
}

/// Typical and worst examples of the spans a query selected, as
/// [`SpanQuery::instances`] returns them: evidence to look at, by id.
#[derive(Clone, Debug, PartialEq)]
pub struct Instances<'t> {
    typical: Vec<Selected<'t>>,
    worst: Vec<Selected<'t>>,
    stats: SpanStats,
    basis: Basis,
}

impl<'t> Instances<'t> {
    /// Returns the spans whose times are closest to the median, by begin
    /// time.
    #[must_use]
    pub fn typical(&self) -> &[Selected<'t>] {
        &self.typical
    }

    /// Returns the spans with the longest times, longest first.
    #[must_use]
    pub fn worst(&self) -> &[Selected<'t>] {
        &self.worst
    }

    /// Returns the statistics over every selected span.
    #[must_use]
    pub const fn stats(&self) -> &SpanStats {
        &self.stats
    }

    /// Returns what the examples were drawn from.
    #[must_use]
    pub const fn basis(&self) -> &Basis {
        &self.basis
    }
}

impl<'t> SpanQuery<'t> {
    pub(crate) const fn new(trace: &'t Trace) -> Self {
        Self {
            trace,
            ranges: None,
            frame_numbers: Vec::new(),
            selection: Selection::StartsWithin,
            name: None,
            tracks: Tracks::All,
            context: None,
            labels: Vec::new(),
            args: Vec::new(),
            group_by: Vec::new(),
        }
    }

    /// Selects spans in `range`, in addition to any ranges already
    /// selected. Without a range, the query covers the whole trace.
    #[must_use]
    pub fn within(mut self, range: TimeRange) -> Self {
        self.ranges.get_or_insert_with(Vec::new).push(range);
        self
    }

    /// Selects spans in the frames, passes, or transactions `frames`.
    /// Ids the trace does not hold select nothing.
    #[must_use]
    ///
    /// Spans on timelines that were reported with a frame number, such as
    /// GPU passes, count toward the frame with that number, whole, however
    /// the frame's time compares with theirs: a frame's GPU work usually
    /// runs while the CPU works on the next frame.
    pub fn in_frames(mut self, frames: &[FrameId]) -> Self {
        let ranges = self.ranges.get_or_insert_with(Vec::new);
        for frame in frames.iter().filter_map(|id| self.trace.frame(*id)) {
            ranges.push(frame.range());
            self.frame_numbers.extend(frame.number());
        }
        self
    }

    /// Selects the frame covering `range`, numbered `number` by the program
    /// when it has a number, as [`in_frames`](Self::in_frames) does.
    pub(crate) fn in_frame_range(mut self, range: TimeRange, number: Option<u64>) -> Self {
        self.ranges.get_or_insert_with(Vec::new).push(range);
        self.frame_numbers.extend(number);
        self
    }

    /// Returns `true` when `span` is selected by its frame number rather
    /// than by its time.
    fn by_frame_number(&self, span: &Span) -> bool {
        !self.frame_numbers.is_empty() && span.timeline.is_some() && span.frame.is_some()
    }

    /// Sets how spans count toward the selected ranges; the default is
    /// [`Selection::StartsWithin`].
    #[must_use]
    pub const fn selecting(mut self, selection: Selection) -> Self {
        self.selection = selection;
        self
    }

    /// Selects only spans of the scope named `name`.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Selects only spans run by the thread `thread_id`.
    #[must_use]
    pub const fn on_thread(mut self, thread_id: u64) -> Self {
        self.tracks = Tracks::Thread(thread_id);
        self
    }

    /// Selects only spans on CPU threads, leaving out timelines such as GPU
    /// queues.
    #[must_use]
    pub const fn on_threads(mut self) -> Self {
        self.tracks = Tracks::Threads;
        self
    }

    /// Selects only spans on the timeline `timeline_id`, such as one GPU
    /// queue.
    #[must_use]
    pub const fn on_timeline(mut self, timeline_id: TimelineId) -> Self {
        self.tracks = Tracks::Timeline(timeline_id);
        self
    }

    /// Selects only spans on timelines that are not CPU threads, such as
    /// GPU queues.
    #[must_use]
    pub const fn on_timelines(mut self) -> Self {
        self.tracks = Tracks::Timelines;
        self
    }

    fn allows(&self, kind: TrackKind) -> bool {
        match (self.tracks, kind) {
            (Tracks::All, _)
            | (Tracks::Threads, TrackKind::Thread(_))
            | (Tracks::Timelines, TrackKind::Timeline { .. }) => true,
            (Tracks::Thread(wanted), TrackKind::Thread(id)) => wanted == id,
            (Tracks::Timeline(wanted), TrackKind::Timeline { id, .. }) => wanted == id,
            _ => false,
        }
    }

    /// Selects only spans attributed to the context `context_id`.
    #[must_use]
    pub const fn in_context(mut self, context_id: ContextId) -> Self {
        self.context = Some(context_id);
        self
    }

    /// Selects only spans whose context has the label `key` set to `value`.
    #[must_use]
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<OwnedLabelValue>) -> Self {
        self.labels.push((key.into(), Some(value.into())));
        self
    }

    /// Selects only spans whose context lacks the label `key`.
    #[must_use]
    pub fn without_label(mut self, key: impl Into<String>) -> Self {
        self.labels.push((key.into(), None));
        self
    }

    /// Selects only spans with the argument `key` set to `value`.
    #[must_use]
    pub fn with_arg(mut self, key: impl Into<String>, value: impl Into<OwnedLabelValue>) -> Self {
        self.args.push((key.into(), Some(value.into())));
        self
    }

    /// Selects only spans without the argument `key`.
    #[must_use]
    pub fn without_arg(mut self, key: impl Into<String>) -> Self {
        self.args.push((key.into(), None));
        self
    }

    /// Groups [`table`](Self::table) rows by `group`, after any grouping
    /// already set.
    #[must_use]
    pub fn group_by(mut self, group: GroupBy) -> Self {
        self.group_by.push(group);
        self
    }

    fn merged_ranges(&self) -> Option<Vec<TimeRange>> {
        self.ranges.clone().map(merge_ranges)
    }

    fn matches(&self, name: &str, context_id: ContextId, args: &[OwnedArgEntry]) -> bool {
        self.name.as_deref().is_none_or(|wanted| wanted == name)
            && self.context.is_none_or(|wanted| wanted == context_id)
            && self
                .labels
                .iter()
                .all(|(key, value)| self.trace.label(context_id, key) == value.as_ref())
            && self
                .args
                .iter()
                .all(|(key, value)| arg(args, key) == value.as_ref())
    }

    fn selects(&self, span: &Span) -> bool {
        self.matches(&span.name, span.context_id, &span.args)
    }

    /// Returns every selected span with the time it counts for, by thread,
    /// then depth, then begin time.
    #[must_use]
    pub fn iter(&self) -> Vec<Selected<'t>> {
        let ranges = self.merged_ranges();
        let mut selected = Vec::new();
        for track in self.trace.tracks() {
            if !self.allows(track.kind()) {
                continue;
            }
            for depth in 0..track.depth() {
                let Some(ranges) = &ranges else {
                    selected.extend(
                        track
                            .spans(depth)
                            .iter()
                            .filter(|span| self.selects(span))
                            .map(|span| Selected {
                                span,
                                time: span.duration(),
                                self_time: span.self_time(),
                            }),
                    );
                    continue;
                };
                for (index, range) in ranges.iter().enumerate() {
                    for span in track.spans_in(depth, *range) {
                        if !self.selects(span) || self.by_frame_number(span) {
                            continue;
                        }
                        let include = match self.selection {
                            Selection::StartsWithin => range.contains(span.begin),
                            // A span overlapping several ranges is counted
                            // at the first; it overlaps every range between.
                            Selection::Overlaps | Selection::Inside => {
                                index == 0 || !ranges[index - 1].overlaps(span.begin, span.end)
                            }
                        };
                        if !include {
                            continue;
                        }
                        selected.push(match self.selection {
                            Selection::StartsWithin | Selection::Overlaps => Selected {
                                span,
                                time: span.duration(),
                                self_time: span.self_time(),
                            },
                            Selection::Inside => {
                                let time = clipped(ranges, span.begin, span.end);
                                let self_time = if time == span.duration() {
                                    span.self_time()
                                } else {
                                    let children: Duration = self
                                        .trace
                                        .children(span)
                                        .iter()
                                        .map(|child| clipped(ranges, child.begin, child.end))
                                        .sum();
                                    time.saturating_sub(children)
                                };
                                Selected {
                                    span,
                                    time,
                                    self_time,
                                }
                            }
                        });
                    }
                }
            }
        }
        let mut numbers = self.frame_numbers.clone();
        numbers.sort_unstable();
        numbers.dedup();
        for number in numbers {
            for span in self
                .trace
                .frame_spans(number)
                .filter(|span| self.allows(self.trace.track_kind_of(span)))
                .filter(|span| self.selects(span))
            {
                selected.push(Selected {
                    span,
                    time: span.duration(),
                    self_time: span.self_time(),
                });
            }
        }
        selected
    }

    /// Groups the selected spans as [`group_by`](Self::group_by) says and
    /// summarizes each group, most total time first.
    #[must_use]
    pub fn table(&self) -> SpanTable {
        let selected = self.iter();
        let mut groups = BTreeMap::<Vec<KeyPart>, Vec<Selected<'t>>>::new();
        for one in &selected {
            groups.entry(self.key(one.span)).or_default().push(*one);
        }
        let mut rows: Vec<_> = groups
            .into_iter()
            .map(|(key, members)| TableRow {
                key,
                stats: SpanStats::of(&members),
            })
            .collect();
        rows.sort_by(|a, b| {
            b.stats
                .total()
                .cmp(&a.stats.total())
                .then_with(|| a.key.cmp(&b.key))
        });
        SpanTable {
            group_by: self.group_by.clone(),
            rows,
            total: SpanStats::of(&selected),
            basis: self.basis_of(&selected),
        }
    }

    /// Returns `count` typical spans, those closest to the median time, and
    /// the `count` worst, with statistics over every selected span.
    #[must_use]
    pub fn instances(&self, count: usize) -> Instances<'t> {
        let selected = self.iter();
        let stats = SpanStats::of(&selected);
        let median = stats.time.median;
        let mut by_time = selected.clone();
        by_time.sort_by(|a, b| b.time.cmp(&a.time).then(a.span.id.cmp(&b.span.id)));
        let worst = by_time.iter().take(count).copied().collect();
        by_time.sort_by_key(|one| (one.time.abs_diff(median), one.span.id));
        let mut typical: Vec<_> = by_time.into_iter().take(count).collect();
        typical.sort_by_key(|one| (one.span.begin, one.span.id));
        Instances {
            typical,
            worst,
            stats,
            basis: self.basis_of(&selected),
        }
    }

    /// Returns what the query's answers cover, without computing them.
    #[must_use]
    pub fn basis(&self) -> Basis {
        self.basis_of(&self.iter())
    }

    fn key(&self, span: &Span) -> Vec<KeyPart> {
        self.group_by
            .iter()
            .map(|group| match group {
                GroupBy::Name => KeyPart::Name(span.name.to_string()),
                GroupBy::Thread => match span.timeline {
                    Some(id) => KeyPart::Timeline {
                        id,
                        name: self
                            .trace
                            .timeline(id)
                            .and_then(|track| track.name().map(ToOwned::to_owned))
                            .unwrap_or_default(),
                    },
                    None => KeyPart::Thread {
                        id: span.thread_id,
                        name: self
                            .trace
                            .track(span.thread_id)
                            .and_then(|track| track.name().map(ToOwned::to_owned)),
                    },
                },
                GroupBy::Context => KeyPart::Context {
                    id: span.context_id,
                    labels: self.trace.describe_context(span.context_id),
                },
                GroupBy::Label(key) => KeyPart::Label {
                    key: key.clone(),
                    value: self.trace.label(span.context_id, key).cloned(),
                },
                GroupBy::Arg(key) => KeyPart::Arg {
                    key: key.clone(),
                    value: span.arg(key).cloned(),
                },
            })
            .collect()
    }

    fn basis_of(&self, selected: &[Selected<'_>]) -> Basis {
        let ranges = self.merged_ranges();
        let in_scope = |begin: Duration| match &ranges {
            None => true,
            Some(ranges) => ranges.iter().any(|range| match self.selection {
                Selection::StartsWithin => range.contains(begin),
                // An open span runs from its begin on.
                Selection::Overlaps | Selection::Inside => begin < range.end(),
            }),
        };
        let open = self
            .trace
            .begun()
            .filter(|(name, context_id, thread_id, begin, args)| {
                self.allows(TrackKind::Thread(*thread_id))
                    && self.matches(name, *context_id, args)
                    && in_scope(*begin)
            })
            .count() as u64;
        let observed = self.trace.observed();
        let (unmatched_ends, gaps, unobserved, summary) = match &ranges {
            None => (
                self.trace
                    .unmatched_ends_in(TimeRange::new(Duration::ZERO, Duration::MAX)),
                self.trace.gaps().to_vec(),
                Duration::ZERO,
                None,
            ),
            Some(ranges) => (
                ranges
                    .iter()
                    .map(|range| self.trace.unmatched_ends_in(*range))
                    .sum(),
                self.trace
                    .gaps()
                    .iter()
                    .filter(|gap| {
                        ranges.iter().any(|range| {
                            gap.range.intersection(*range).is_some()
                                || range.contains(gap.range.start())
                        })
                    })
                    .copied()
                    .collect(),
                ranges
                    .iter()
                    .map(|range| {
                        range.length().saturating_sub(
                            range
                                .intersection(observed)
                                .map_or(Duration::ZERO, |inside| inside.length()),
                        )
                    })
                    .sum(),
                Some((ranges.len(), ranges.iter().map(TimeRange::length).sum())),
            ),
        };
        Basis {
            selection: self.selection,
            ranges: summary,
            recorded: self.trace.recorded(Signal::Spans),
            spans: selected.len() as u64,
            interrupted: selected
                .iter()
                .filter(|selected| selected.span.interrupted)
                .count() as u64,
            open,
            unmatched_ends,
            gaps,
            unobserved,
            converted: selected
                .iter()
                .filter(|selected| selected.span.placement.is_some())
                .count() as u64,
            farthest_conversion: selected
                .iter()
                .filter_map(|selected| selected.span.placement)
                .map(|placement| placement.distance())
                .max()
                .unwrap_or_default(),
            unplaced: if matches!(self.tracks, Tracks::Thread(_) | Tracks::Threads) {
                0
            } else {
                self.trace.unplaced_spans() as u64
            },
        }
    }
}

/// Returns the value of the argument `key` among `args`.
pub(crate) fn arg<'a>(args: &'a [OwnedArgEntry], key: &str) -> Option<&'a OwnedLabelValue> {
    args.iter()
        .find(|entry| entry.key() == key)
        .map(OwnedArgEntry::value)
}

/// Returns how much of the interval from `begin` to `end` lies in the
/// sorted, disjoint `ranges`.
fn clipped(ranges: &[TimeRange], begin: Duration, end: Duration) -> Duration {
    let first = ranges.partition_point(|range| range.end() <= begin);
    ranges[first..]
        .iter()
        .take_while(|range| range.start() < end)
        .map(|range| range.clip(begin, end))
        .sum()
}
