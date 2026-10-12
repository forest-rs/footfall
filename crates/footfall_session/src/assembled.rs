// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Snapshots reassembled from their envelopes.

use alloc::vec::Vec;
use core::fmt;

use footfall_core::ContextId;
use footfall_runtime::{Drops, EventCursor, EventWindow, SnapshotMetadata, ThreadSnapshot};

use crate::{
    OwnedContextSnapshot, OwnedCounterSnapshot, OwnedHistogramSnapshot, OwnedSessionEnvelope,
    OwnedSessionPayload, OwnedSnapshotSummary, OwnedTimedObservation,
};

/// One published snapshot, reassembled from its envelopes and checked
/// against the publication contract.
///
/// This is what a consumer works with: the envelopes are only how a snapshot
/// travels. Build one with [`OwnedSnapshot::from_envelopes`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedSnapshot {
    summary: OwnedSnapshotSummary,
    threads: Vec<ThreadSnapshot>,
    contexts: Vec<OwnedContextSnapshot>,
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

    /// Returns the metadata identifying the producing runtime.
    #[must_use]
    pub const fn metadata(&self) -> &SnapshotMetadata {
        self.summary.metadata()
    }

    /// Returns the time since runtime creation when the snapshot was taken.
    #[must_use]
    pub const fn captured_at_micros(&self) -> u64 {
        self.summary.captured_at_micros()
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

    /// Resolves one context by id.
    #[must_use]
    pub fn context(&self, context_id: ContextId) -> Option<&OwnedContextSnapshot> {
        self.contexts
            .binary_search_by_key(&context_id, OwnedContextSnapshot::id)
            .ok()
            .map(|index| &self.contexts[index])
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
        OwnedSessionPayload::Counters(_) => 3,
        OwnedSessionPayload::Histograms(_) => 4,
        OwnedSessionPayload::Events(_) => 5,
        OwnedSessionPayload::SnapshotEnd(_) => 6,
    }
}

const fn payload_name(payload: &OwnedSessionPayload) -> &'static str {
    match payload {
        OwnedSessionPayload::SnapshotStart(_) => "SnapshotStart",
        OwnedSessionPayload::Threads(_) => "Threads",
        OwnedSessionPayload::Contexts(_) => "Contexts",
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
