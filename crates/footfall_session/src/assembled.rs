// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Snapshots reassembled from their envelopes.

use alloc::vec::Vec;
use core::fmt;
use std::collections::{HashMap, HashSet};

use footfall_core::ContextId;
use footfall_runtime::{
    CounterKind, Drops, EventCursor, EventWindow, SnapshotMetadata, ThreadSnapshot,
};

use crate::{
    Chunking, EnvelopeHeader, OwnedContextSnapshot, OwnedCounterSnapshot, OwnedHistogramSnapshot,
    OwnedSessionEnvelope, OwnedSessionPayload, OwnedSnapshotSummary, OwnedTimedObservation,
    PROTOCOL_VERSION, SnapshotKind,
};

/// One published snapshot, reassembled from its envelopes and checked
/// against the publication contract.
///
/// This is what a consumer works with: the envelopes are only how a snapshot
/// travels. You get one from:
///
/// - a runtime snapshot in the same process, with `OwnedSnapshot::from`;
/// - a capture file, with [`CaptureReader::read_snapshot`](crate::capture::CaptureReader::read_snapshot);
/// - a live session, from `SessionClient::fetch` or `Subscription::next` in
///   `footfall_session_wtransport`;
/// - envelopes from any other transport, with [`OwnedSnapshot::from_envelopes`].
///
/// Read its rows with [`contexts`](Self::contexts),
/// [`counters`](Self::counters), [`histograms`](Self::histograms), and
/// [`events`](Self::events); analyze it with `footfall_analysis`, export it
/// with `footfall_export`, or save it with
/// [`CaptureWriter::write_owned`](crate::capture::CaptureWriter::write_owned).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedSnapshot {
    summary: OwnedSnapshotSummary,
    threads: Vec<ThreadSnapshot>,
    contexts: Vec<OwnedContextSnapshot>,
    retired_contexts: Vec<ContextId>,
    counters: Vec<OwnedCounterSnapshot>,
    histograms: Vec<OwnedHistogramSnapshot>,
    events: Vec<OwnedTimedObservation>,
    drops: Drops,
}

impl OwnedSnapshot {
    /// Reassembles one snapshot publication.
    ///
    /// The envelopes must be exactly one publication: a `SnapshotStart`, the
    /// row payloads in publication order, and a `SnapshotEnd`, with
    /// consecutive chunk indexes from `0` and as many rows of each kind as
    /// `SnapshotStart` announced.
    pub fn from_envelopes(
        envelopes: impl IntoIterator<Item = OwnedSessionEnvelope>,
    ) -> Result<Self, AssembleError> {
        let mut envelopes = envelopes.into_iter();
        let first = envelopes.next().ok_or(AssembleError::Empty)?;
        check_chunk_index(&first, 0)?;
        let OwnedSessionPayload::SnapshotStart(summary) = first.payload().clone() else {
            return Err(AssembleError::MissingStart {
                found: payload_name(first.payload()),
            });
        };

        let mut snapshot = Self {
            summary,
            threads: Vec::new(),
            contexts: Vec::new(),
            retired_contexts: Vec::new(),
            counters: Vec::new(),
            histograms: Vec::new(),
            events: Vec::new(),
            drops: Drops::new(),
        };
        let mut previous = payload_name(first.payload());
        let mut previous_rank = publication_rank(first.payload());
        let mut ended = false;
        for (expected_index, envelope) in (1_u64..).zip(envelopes) {
            let chunk_index = envelope.header().chunk_index();
            if ended {
                return Err(AssembleError::AfterEnd { chunk_index });
            }
            check_chunk_index(&envelope, expected_index)?;
            let payload = envelope.payload();
            let rank = publication_rank(payload);
            // Row payloads may repeat in chunks; everything else occurs once.
            let repeatable = matches!(
                payload,
                OwnedSessionPayload::Contexts(_)
                    | OwnedSessionPayload::RetiredContexts(_)
                    | OwnedSessionPayload::Counters(_)
                    | OwnedSessionPayload::Histograms(_)
                    | OwnedSessionPayload::Events(_)
            );
            if rank < previous_rank || (rank == previous_rank && !repeatable) {
                return Err(AssembleError::OutOfOrder {
                    chunk_index,
                    payload: payload_name(payload),
                    after: previous,
                });
            }
            match payload {
                OwnedSessionPayload::SnapshotStart(_) => {
                    unreachable!("a second SnapshotStart fails the order check")
                }
                OwnedSessionPayload::Threads(rows) => snapshot.threads.extend_from_slice(rows),
                OwnedSessionPayload::Contexts(rows) => snapshot.contexts.extend_from_slice(rows),
                OwnedSessionPayload::RetiredContexts(ids) => {
                    snapshot.retired_contexts.extend_from_slice(ids);
                }
                OwnedSessionPayload::Counters(rows) => snapshot.counters.extend_from_slice(rows),
                OwnedSessionPayload::Histograms(rows) => {
                    snapshot.histograms.extend_from_slice(rows);
                }
                OwnedSessionPayload::Events(rows) => snapshot.events.extend_from_slice(rows),
                OwnedSessionPayload::SnapshotEnd(drops) => {
                    snapshot.drops = *drops;
                    ended = true;
                }
            }
            previous = payload_name(payload);
            previous_rank = rank;
        }
        if !ended {
            return Err(AssembleError::MissingEnd);
        }

        let counts = snapshot.summary.counts();
        for (rows, announced, received) in [
            ("thread", counts.threads(), snapshot.threads.len()),
            ("context", counts.contexts(), snapshot.contexts.len()),
            (
                "retired context",
                counts.retired_contexts(),
                snapshot.retired_contexts.len(),
            ),
            ("counter", counts.counters(), snapshot.counters.len()),
            ("histogram", counts.histograms(), snapshot.histograms.len()),
            ("event", counts.events(), snapshot.events.len()),
        ] {
            if announced != received {
                return Err(AssembleError::RowCount {
                    rows,
                    announced,
                    received,
                });
            }
        }
        Ok(snapshot)
    }

    /// Builds a snapshot from its parts; see the `From<&Snapshot>`
    /// conversion for runtime snapshots.
    pub(crate) const fn from_parts(
        summary: OwnedSnapshotSummary,
        threads: Vec<ThreadSnapshot>,
        contexts: Vec<OwnedContextSnapshot>,
        counters: Vec<OwnedCounterSnapshot>,
        histograms: Vec<OwnedHistogramSnapshot>,
        events: Vec<OwnedTimedObservation>,
        drops: Drops,
    ) -> Self {
        Self {
            summary,
            threads,
            contexts,
            retired_contexts: Vec::new(),
            counters,
            histograms,
            events,
            drops,
        }
    }

    /// Returns the metadata identifying the producing runtime.
    #[must_use]
    pub const fn metadata(&self) -> &SnapshotMetadata {
        self.summary.metadata()
    }

    /// Returns whether this snapshot holds the producer's whole state or
    /// only what changed since the previous snapshot of a subscription.
    ///
    /// [`SessionClient::subscribe`] and capture files hand out full
    /// snapshots; a consumer that assembles envelopes itself sees
    /// [`SnapshotKind::Changes`] and applies them with
    /// [`apply`](Self::apply).
    ///
    /// [`SessionClient::subscribe`]: https://docs.rs/footfall_session_wtransport
    #[must_use]
    pub const fn kind(&self) -> SnapshotKind {
        self.summary.kind()
    }

    /// Returns the contexts retired since the previous snapshot, for a
    /// [`SnapshotKind::Changes`] snapshot; empty for a full one.
    #[must_use]
    pub fn retired_contexts(&self) -> &[ContextId] {
        &self.retired_contexts
    }

    /// Returns the time since runtime creation when the snapshot was taken.
    #[must_use]
    pub const fn captured_at_nanos(&self) -> u64 {
        self.summary.captured_at_nanos()
    }

    /// Returns the range of event sequence numbers the snapshot covers.
    #[must_use]
    pub const fn window(&self) -> EventWindow {
        self.summary.window()
    }

    /// Returns the cursor to request the next snapshot from.
    #[must_use]
    pub const fn next_cursor(&self) -> EventCursor {
        self.summary.next_cursor()
    }

    /// Returns the threads that recorded the snapshot's events, sorted by id.
    #[must_use]
    pub fn threads(&self) -> &[ThreadSnapshot] {
        &self.threads
    }

    /// Returns the producer's contexts, sorted by id and starting with the
    /// root context.
    #[must_use]
    pub fn contexts(&self) -> &[OwnedContextSnapshot] {
        &self.contexts
    }

    /// Returns the counters and gauges.
    #[must_use]
    pub fn counters(&self) -> &[OwnedCounterSnapshot] {
        &self.counters
    }

    /// Returns the value distributions.
    #[must_use]
    pub fn histograms(&self) -> &[OwnedHistogramSnapshot] {
        &self.histograms
    }

    /// Returns the events in the window, in publication order.
    #[must_use]
    pub fn events(&self) -> &[OwnedTimedObservation] {
        &self.events
    }

    /// Returns what the producer could not keep or attribute.
    #[must_use]
    pub const fn drops(&self) -> Drops {
        self.drops
    }

    /// Extends this snapshot with a later one from the same runtime
    /// instance, as if both had been read at once.
    ///
    /// This joins the windows of a subscription, or of snapshots read back
    /// from a capture file, into one view: events from `later` that this
    /// snapshot does not already hold are appended, contexts and threads are
    /// combined with `later`'s winning, and counters, histograms, metadata,
    /// and drop totals become `later`'s, which are cumulative. Contexts stay
    /// even once retired, so every event keeps its attribution. Events
    /// between the two windows that neither holds count as missed. `later`
    /// may be full or hold only changes.
    ///
    /// Fails, leaving this snapshot unchanged, when this snapshot holds only
    /// changes, or `later` is from another instance or ends before this
    /// snapshot does.
    pub fn append(&mut self, later: Self) -> Result<(), AppendError> {
        self.check_later(&later)?;
        let window = self.window();
        let later_window = later.window();
        let gap = later_window.since().saturating_sub(window.next());
        let missed = window
            .missed()
            .saturating_add(gap)
            .saturating_add(later_window.missed());
        let merged_window = EventWindow::new(window.since(), later_window.next(), missed);

        let mut later = later;
        let events = core::mem::take(&mut later.events);
        self.merge_state(later, false);
        self.events.extend(
            events
                .into_iter()
                .filter(|event| event.sequence() >= window.next()),
        );
        self.finish_merge(merged_window);
        Ok(())
    }

    /// Brings this full snapshot up to date with a later one from the same
    /// runtime instance, full or holding only changes.
    ///
    /// Contexts, threads, counters, histograms, metadata, and drop totals
    /// become current: changes are applied, and retired contexts are removed
    /// with their counters and histograms. The events become `later`'s, so
    /// the result is exactly the full snapshot the producer would have sent.
    /// This is how a subscription's snapshots that hold only changes become
    /// full ones.
    ///
    /// Fails, leaving this snapshot unchanged, when this snapshot holds only
    /// changes, or `later` is from another instance or ends before this
    /// snapshot does.
    pub fn apply(&mut self, later: Self) -> Result<(), AppendError> {
        self.check_later(&later)?;
        if later.kind() == SnapshotKind::Full {
            *self = later;
            return Ok(());
        }
        let window = later.window();
        let mut later = later;
        let events = core::mem::take(&mut later.events);
        self.merge_state(later, true);
        self.events = events;
        self.finish_merge(window);
        Ok(())
    }

    fn check_later(&self, later: &Self) -> Result<(), AppendError> {
        if self.kind() != SnapshotKind::Full {
            return Err(AppendError::NotFull);
        }
        let instance = self.metadata().instance_id();
        let later_instance = later.metadata().instance_id();
        if later_instance != instance {
            return Err(AppendError::OtherInstance {
                expected: instance,
                found: later_instance,
            });
        }
        if later.window().next() < self.window().next() {
            return Err(AppendError::Earlier {
                next: self.window().next(),
                later_next: later.window().next(),
            });
        }
        Ok(())
    }

    /// Merges `later`'s state, without its events, into this snapshot.
    fn merge_state(&mut self, later: Self, remove_retired: bool) {
        let Self {
            summary,
            threads,
            contexts,
            retired_contexts,
            counters,
            histograms,
            drops,
            ..
        } = later;
        merge_by_key(&mut self.contexts, contexts, OwnedContextSnapshot::id);
        merge_by_key(&mut self.threads, threads, ThreadSnapshot::id);
        if summary.kind() == SnapshotKind::Full {
            self.counters = counters;
            self.histograms = histograms;
        } else {
            let retired: HashSet<ContextId> = retired_contexts.into_iter().collect();
            if remove_retired {
                self.contexts
                    .retain(|context| !retired.contains(&context.id()));
            }
            self.counters
                .retain(|counter| !retired.contains(&counter.context_id()));
            self.histograms
                .retain(|histogram| !retired.contains(&histogram.context_id()));
            replace_or_push(&mut self.counters, counters, |counter| {
                (
                    counter.desc_id(),
                    counter.context_id(),
                    counter.kind() == CounterKind::Gauge,
                )
            });
            replace_or_push(&mut self.histograms, histograms, |histogram| {
                (histogram.desc_id(), histogram.context_id())
            });
            // Keep the producer's order, so the result equals the full
            // snapshot it would have sent.
            self.counters.sort_by(|left, right| {
                (
                    left.name(),
                    left.context_id(),
                    left.kind() == CounterKind::Gauge,
                )
                    .cmp(&(
                        right.name(),
                        right.context_id(),
                        right.kind() == CounterKind::Gauge,
                    ))
            });
            self.histograms.sort_by(|left, right| {
                (left.name(), left.context_id()).cmp(&(right.name(), right.context_id()))
            });
        }
        self.drops = drops;
        self.summary = summary;
    }

    /// Makes this snapshot full again after a merge, with `window`.
    fn finish_merge(&mut self, window: EventWindow) {
        self.retired_contexts.clear();
        let counts = crate::RowCounts::new(
            self.threads.len(),
            self.contexts.len(),
            0,
            self.counters.len(),
            self.histograms.len(),
            self.events.len(),
        );
        self.summary = OwnedSnapshotSummary::new(
            self.summary.metadata().clone(),
            SnapshotKind::Full,
            self.summary.captured_at_nanos(),
            window,
            counts,
        );
    }

    /// Splits this snapshot into envelopes, as a producer would publish it.
    #[must_use]
    pub fn to_envelopes(&self, chunking: Chunking) -> Vec<OwnedSessionEnvelope> {
        let mut payloads = Vec::new();
        payloads.push(OwnedSessionPayload::SnapshotStart(self.summary.clone()));
        if !self.threads.is_empty() {
            payloads.push(OwnedSessionPayload::Threads(self.threads.clone()));
        }
        for rows in self.contexts.chunks(chunk_size(chunking.max_contexts())) {
            payloads.push(OwnedSessionPayload::Contexts(rows.to_vec()));
        }
        for ids in self
            .retired_contexts
            .chunks(chunk_size(chunking.max_contexts()))
        {
            payloads.push(OwnedSessionPayload::RetiredContexts(ids.to_vec()));
        }
        for rows in self.counters.chunks(chunk_size(chunking.max_counters())) {
            payloads.push(OwnedSessionPayload::Counters(rows.to_vec()));
        }
        for rows in self
            .histograms
            .chunks(chunk_size(chunking.max_histograms()))
        {
            payloads.push(OwnedSessionPayload::Histograms(rows.to_vec()));
        }
        for rows in self.events.chunks(chunk_size(chunking.max_events())) {
            payloads.push(OwnedSessionPayload::Events(rows.to_vec()));
        }
        payloads.push(OwnedSessionPayload::SnapshotEnd(self.drops));
        payloads
            .into_iter()
            .zip(0_u64..)
            .map(|(payload, chunk_index)| {
                OwnedSessionEnvelope::new(
                    EnvelopeHeader::new(PROTOCOL_VERSION, chunk_index),
                    payload,
                )
            })
            .collect()
    }

    /// Resolves one context by id.
    #[must_use]
    pub fn context(&self, context_id: ContextId) -> Option<&OwnedContextSnapshot> {
        self.contexts
            .binary_search_by_key(&context_id, OwnedContextSnapshot::id)
            .ok()
            .map(|index| &self.contexts[index])
    }

    /// Returns the value of the label `key` in the context `context_id`, if
    /// the context exists and has that label.
    #[must_use]
    pub fn label(&self, context_id: ContextId, key: &str) -> Option<&crate::OwnedLabelValue> {
        self.context(context_id)?
            .entries()
            .iter()
            .find(|entry| entry.key() == key)
            .map(crate::OwnedContextEntry::value)
    }

    /// Resolves one thread by id.
    #[must_use]
    pub fn thread(&self, thread_id: u64) -> Option<&ThreadSnapshot> {
        self.threads
            .binary_search_by_key(&thread_id, ThreadSnapshot::id)
            .ok()
            .map(|index| &self.threads[index])
    }
}

/// Why envelopes did not form one valid snapshot publication; see
/// [`OwnedSnapshot::from_envelopes`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AssembleError {
    /// There were no envelopes.
    Empty,
    /// The first envelope was not a `SnapshotStart`.
    MissingStart {
        /// The payload found instead.
        found: &'static str,
    },
    /// An envelope's chunk index was not the next one.
    ChunkIndex {
        /// The chunk index that should have come next.
        expected: u64,
        /// The chunk index found.
        found: u64,
    },
    /// A payload arrived after one that must follow it.
    OutOfOrder {
        /// The chunk index of the misplaced envelope.
        chunk_index: u64,
        /// The misplaced payload.
        payload: &'static str,
        /// The payload it arrived after.
        after: &'static str,
    },
    /// An envelope followed the `SnapshotEnd`.
    AfterEnd {
        /// The chunk index of the extra envelope.
        chunk_index: u64,
    },
    /// The envelopes ended without a `SnapshotEnd`.
    MissingEnd,
    /// The rows of one kind did not add up to the count `SnapshotStart`
    /// announced.
    RowCount {
        /// The kind of row.
        rows: &'static str,
        /// The count `SnapshotStart` announced.
        announced: usize,
        /// The rows received.
        received: usize,
    },
}

impl fmt::Display for AssembleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("snapshot publication has no envelopes"),
            Self::MissingStart { found } => {
                write!(
                    formatter,
                    "snapshot publication starts with {found}, not SnapshotStart"
                )
            }
            Self::ChunkIndex { expected, found } => {
                write!(formatter, "expected chunk {expected}, found chunk {found}")
            }
            Self::OutOfOrder {
                chunk_index,
                payload,
                after,
            } => write!(
                formatter,
                "chunk {chunk_index}: {payload} cannot follow {after}"
            ),
            Self::AfterEnd { chunk_index } => {
                write!(formatter, "chunk {chunk_index} follows SnapshotEnd")
            }
            Self::MissingEnd => formatter.write_str("snapshot publication has no SnapshotEnd"),
            Self::RowCount {
                rows,
                announced,
                received,
            } => write!(
                formatter,
                "SnapshotStart announced {announced} {rows} rows, but {received} arrived"
            ),
        }
    }
}

impl core::error::Error for AssembleError {}

/// Why [`OwnedSnapshot::append`] refused a snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AppendError {
    /// The later snapshot is from another runtime instance, whose ids and
    /// sequence numbers mean something else.
    OtherInstance {
        /// This snapshot's instance id.
        expected: u64,
        /// The later snapshot's instance id.
        found: u64,
    },
    /// This snapshot holds only changes, so there is no whole state to
    /// extend; start from a full snapshot.
    NotFull,
    /// The later snapshot's window ends before this one's.
    Earlier {
        /// Where this snapshot's window ends.
        next: u64,
        /// Where the later snapshot's window ends.
        later_next: u64,
    },
}

impl fmt::Display for AppendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OtherInstance { expected, found } => write!(
                formatter,
                "snapshot is from runtime instance {found:016x}, not {expected:016x}"
            ),
            Self::NotFull => {
                formatter.write_str("this snapshot holds only changes; start from a full snapshot")
            }
            Self::Earlier { next, later_next } => write!(
                formatter,
                "snapshot's events end at {later_next}, before this one's at {next}"
            ),
        }
    }
}

impl core::error::Error for AppendError {}

/// Replaces rows of `rows` with `later`'s rows of the same key and adds the
/// rest, keeping `rows` sorted by key.
fn merge_by_key<T, K: Ord + Copy>(rows: &mut Vec<T>, later: Vec<T>, key: impl Fn(&T) -> K) {
    for row in later {
        match rows.binary_search_by_key(&key(&row), &key) {
            Ok(index) => rows[index] = row,
            Err(index) => rows.insert(index, row),
        }
    }
}

/// Replaces each row of `rows` whose key matches one of `later`'s, and adds
/// the rest.
fn replace_or_push<T, K: Eq + core::hash::Hash>(
    rows: &mut Vec<T>,
    later: Vec<T>,
    key: impl Fn(&T) -> K,
) {
    let mut index: HashMap<K, usize> = rows
        .iter()
        .enumerate()
        .map(|(position, row)| (key(row), position))
        .collect();
    for row in later {
        match index.get(&key(&row)) {
            Some(&position) => rows[position] = row,
            None => {
                index.insert(key(&row), rows.len());
                rows.push(row);
            }
        }
    }
}

const fn chunk_size(max_rows: usize) -> usize {
    if max_rows == 0 { usize::MAX } else { max_rows }
}

fn check_chunk_index(envelope: &OwnedSessionEnvelope, expected: u64) -> Result<(), AssembleError> {
    let found = envelope.header().chunk_index();
    if found == expected {
        Ok(())
    } else {
        Err(AssembleError::ChunkIndex { expected, found })
    }
}

/// Position of a payload kind in the publication order.
const fn publication_rank(payload: &OwnedSessionPayload) -> u8 {
    match payload {
        OwnedSessionPayload::SnapshotStart(_) => 0,
        OwnedSessionPayload::Threads(_) => 1,
        OwnedSessionPayload::Contexts(_) => 2,
        OwnedSessionPayload::RetiredContexts(_) => 3,
        OwnedSessionPayload::Counters(_) => 4,
        OwnedSessionPayload::Histograms(_) => 5,
        OwnedSessionPayload::Events(_) => 6,
        OwnedSessionPayload::SnapshotEnd(_) => 7,
    }
}

const fn payload_name(payload: &OwnedSessionPayload) -> &'static str {
    match payload {
        OwnedSessionPayload::SnapshotStart(_) => "SnapshotStart",
        OwnedSessionPayload::Threads(_) => "Threads",
        OwnedSessionPayload::Contexts(_) => "Contexts",
        OwnedSessionPayload::RetiredContexts(_) => "RetiredContexts",
        OwnedSessionPayload::Counters(_) => "Counters",
        OwnedSessionPayload::Histograms(_) => "Histograms",
        OwnedSessionPayload::Events(_) => "Events",
        OwnedSessionPayload::SnapshotEnd(_) => "SnapshotEnd",
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec::Vec};

    use footfall_core::{Context, ContextId, CounterDescId, Unit};
    use footfall_runtime::{
        ContextSnapshot, CounterKind, CounterSnapshot, Drops, EventWindow, Observation, Snapshot,
        SnapshotMetadata, ThreadSnapshot, TimedObservation,
    };

    use super::{AssembleError, OwnedSnapshot};
    use crate::{
        Chunking, EnvelopeHeader, OwnedSessionEnvelope, OwnedSessionPayload, PROTOCOL_VERSION,
        collect_snapshot,
    };

    fn publication() -> Vec<OwnedSessionEnvelope> {
        let snapshot = Snapshot::builder(SnapshotMetadata::new("demo").with_instance_id(9))
            .window(EventWindow::new(4, 7, 1))
            .thread(ThreadSnapshot::new(1, Some(String::from("main"))))
            .context(ContextSnapshot::new(ContextId::new(0), Context::root()))
            .counter(CounterSnapshot::new(
                CounterDescId::new(1),
                "ops",
                Unit::Count,
                ContextId::new(0),
                CounterKind::Counter,
                3,
                5,
            ))
            .event(TimedObservation::new(
                5,
                4,
                1,
                Observation::ScopeEnd {
                    token: footfall_core::ScopeToken::new(1),
                },
            ))
            .event(TimedObservation::new(
                6,
                6,
                1,
                Observation::ScopeEnd {
                    token: footfall_core::ScopeToken::new(2),
                },
            ))
            .drops(Drops::new().with_evicted_events(1))
            .build();
        collect_snapshot(&snapshot, Chunking::new(1, 1, 1, 1))
    }

    fn reindexed(payloads: Vec<OwnedSessionPayload>) -> Vec<OwnedSessionEnvelope> {
        payloads
            .into_iter()
            .zip(0_u64..)
            .map(|(payload, index)| {
                OwnedSessionEnvelope::new(EnvelopeHeader::new(PROTOCOL_VERSION, index), payload)
            })
            .collect()
    }

    fn payloads(envelopes: &[OwnedSessionEnvelope]) -> Vec<OwnedSessionPayload> {
        envelopes
            .iter()
            .map(|envelope| envelope.payload().clone())
            .collect()
    }

    #[test]
    fn a_publication_reassembles_into_its_snapshot() {
        let snapshot = OwnedSnapshot::from_envelopes(publication()).expect("valid publication");
        assert_eq!(snapshot.metadata().instance_id(), 9);
        assert_eq!(snapshot.next_cursor().sequence(), 7);
        assert_eq!(snapshot.window().missed(), 1);
        assert_eq!(
            snapshot.thread(1).and_then(ThreadSnapshot::name),
            Some("main")
        );
        assert!(snapshot.context(ContextId::new(0)).is_some());
        assert_eq!(snapshot.counters()[0].value(), 3);
        let sequences: Vec<_> = snapshot
            .events()
            .iter()
            .map(|event| event.sequence())
            .collect();
        assert_eq!(sequences, [4, 6]);
        assert_eq!(snapshot.drops().evicted_events(), 1);
    }

    #[test]
    fn malformed_publications_say_what_is_wrong() {
        assert_eq!(
            OwnedSnapshot::from_envelopes(Vec::new()),
            Err(AssembleError::Empty)
        );

        let mut without_start = payloads(&publication());
        without_start.remove(0);
        assert_eq!(
            OwnedSnapshot::from_envelopes(reindexed(without_start)),
            Err(AssembleError::MissingStart { found: "Threads" })
        );

        let mut without_end = publication();
        without_end.pop();
        assert_eq!(
            OwnedSnapshot::from_envelopes(without_end),
            Err(AssembleError::MissingEnd)
        );

        let mut gap = publication();
        gap.remove(2);
        assert_eq!(
            OwnedSnapshot::from_envelopes(gap),
            Err(AssembleError::ChunkIndex {
                expected: 2,
                found: 3
            })
        );

        // Events before counters.
        let mut swapped = payloads(&publication());
        swapped.swap(3, 4);
        assert_eq!(
            OwnedSnapshot::from_envelopes(reindexed(swapped)),
            Err(AssembleError::OutOfOrder {
                chunk_index: 4,
                payload: "Counters",
                after: "Events"
            })
        );

        let mut short = payloads(&publication());
        short.remove(5);
        assert_eq!(
            OwnedSnapshot::from_envelopes(reindexed(short)),
            Err(AssembleError::RowCount {
                rows: "event",
                announced: 2,
                received: 1
            })
        );

        let mut extra = payloads(&publication());
        extra.push(OwnedSessionPayload::SnapshotEnd(Drops::new()));
        assert_eq!(
            OwnedSnapshot::from_envelopes(reindexed(extra)),
            Err(AssembleError::AfterEnd { chunk_index: 7 })
        );
    }
}

#[cfg(test)]
mod append_tests {
    use alloc::vec::Vec;

    use footfall_core::{Backend as _, ContextHandle, MarkerKind, MarkerPhase};
    use footfall_runtime::Runtime;

    use super::{AppendError, OwnedSnapshot};
    use crate::Chunking;

    fn mark(runtime: &Runtime) {
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
    }

    #[test]
    fn consecutive_windows_append_into_one_view() {
        let runtime = Runtime::builder().event_capacity(2).build();
        mark(&runtime);
        let first = runtime.snapshot();
        mark(&runtime);
        let second = runtime.snapshot_since(first.next_cursor());
        // Three more evict one event the next read never sees.
        mark(&runtime);
        mark(&runtime);
        mark(&runtime);
        let third = runtime.snapshot_since(second.next_cursor());

        let mut joined = OwnedSnapshot::from(&first);
        joined
            .append(OwnedSnapshot::from(&second))
            .expect("same instance");
        joined
            .append(OwnedSnapshot::from(&third))
            .expect("same instance");
        let sequences: Vec<_> = joined
            .events()
            .iter()
            .map(|event| event.sequence())
            .collect();
        assert_eq!(sequences, [0, 1, 3, 4]);
        assert_eq!(joined.window().since(), 0);
        assert_eq!(joined.window().next(), 5);
        assert_eq!(joined.window().missed(), 1);
        assert_eq!(joined.next_cursor(), third.next_cursor());

        // The joined snapshot still forms a valid publication.
        let republished = OwnedSnapshot::from_envelopes(joined.to_envelopes(Chunking::default()))
            .expect("valid publication");
        assert_eq!(republished, joined);
    }

    #[test]
    fn snapshots_from_other_instances_or_earlier_windows_are_refused() {
        let runtime = Runtime::default();
        mark(&runtime);
        let mut later = OwnedSnapshot::from(runtime.snapshot());
        let earlier = OwnedSnapshot::from(&Runtime::default().snapshot());
        assert!(matches!(
            later.clone().append(earlier),
            Err(AppendError::OtherInstance { .. })
        ));

        let before = OwnedSnapshot::from(runtime.snapshot());
        mark(&runtime);
        later = OwnedSnapshot::from(runtime.snapshot());
        assert!(matches!(
            later.append(before),
            Err(AppendError::Earlier {
                next: 2,
                later_next: 1
            })
        ));
    }
}
