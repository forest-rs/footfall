// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Converting times read from other clocks to the producer's own.

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use footfall_core::{ClockId, TickPeriod};

use crate::{OwnedClockTicks, OwnedObservation, OwnedSnapshot, OwnedTimedObservation};

/// What a producer recorded about clocks other than its own, and the
/// conversion of their times to its own timeline.
///
/// A program records a clock's relation to the runtime with
/// `footfall_core::clock_sync` ("this clock reads N now") or relates two
/// clocks with `footfall_core::clock_pair` ("these read N and M at the same
/// moment"), such as a GPU clock paired with a host clock that is itself
/// synced. Feed every event of every snapshot to [`observe`](Self::observe),
/// or build one from a snapshot with [`from_snapshot`](Self::from_snapshot),
/// then convert with [`to_runtime`](Self::to_runtime).
#[derive(Clone, Debug, Default)]
pub struct Clocks {
    clocks: BTreeMap<ClockId, History>,
}

/// One clock's syncs with the runtime and pairs with other clocks.
#[derive(Clone, Debug)]
struct History {
    name: String,
    period: TickPeriod,
    /// Ticks and runtime nanoseconds, by ticks.
    syncs: Vec<(u64, u64)>,
    /// Ticks, the other clock, its ticks at the same moment, and when the
    /// pair was recorded in runtime nanoseconds, by ticks.
    pairs: Vec<(u64, ClockId, u64, u64)>,
}

/// A time converted to the producer's timeline, as [`Clocks::to_runtime`]
/// returns it, with how the conversion was made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Converted {
    nanos: u64,
    via: Option<ClockId>,
    between_syncs: bool,
    distance: u64,
}

impl Converted {
    /// Returns the time in nanoseconds since the producing runtime was
    /// created, saturating at `0` for times before it.
    #[must_use]
    pub const fn nanos(&self) -> u64 {
        self.nanos
    }

    /// Returns the clock the time was converted through, when its own clock
    /// is only paired with others and never synced with the runtime.
    #[must_use]
    pub const fn via(&self) -> Option<ClockId> {
        self.via
    }

    /// Returns `true` when the time lay between two syncs, so the
    /// conversion follows any drift between the clocks; otherwise it
    /// extends from the nearest sync at the clock's nominal tick period.
    #[must_use]
    pub const fn between_syncs(&self) -> bool {
        self.between_syncs
    }

    /// Returns how far the time lay from the nearest sync or pair the
    /// conversion used, in nanoseconds: the stretch over which an
    /// error in the nominal tick period accumulates.
    #[must_use]
    pub const fn distance(&self) -> u64 {
        self.distance
    }
}

/// One clock as [`Clocks::clock`] describes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockInfo<'a> {
    name: &'a str,
    period: TickPeriod,
    syncs: usize,
    pairs: usize,
}

impl<'a> ClockInfo<'a> {
    /// Returns the clock's name.
    #[must_use]
    pub const fn name(&self) -> &'a str {
        self.name
    }

    /// Returns its tick period, from its latest reading.
    #[must_use]
    pub const fn period(&self) -> TickPeriod {
        self.period
    }

    /// Returns how many syncs with the runtime were recorded.
    #[must_use]
    pub const fn syncs(&self) -> usize {
        self.syncs
    }

    /// Returns how many pairs with other clocks were recorded.
    #[must_use]
    pub const fn pairs(&self) -> usize {
        self.pairs
    }
}

impl Clocks {
    /// Creates an empty set of clocks.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Collects the clocks of one snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &OwnedSnapshot) -> Self {
        let mut clocks = Self::new();
        for event in snapshot.events() {
            clocks.observe(event);
        }
        clocks
    }

    /// Takes in one event, keeping it when it is a clock sync or pair.
    pub fn observe(&mut self, event: &OwnedTimedObservation) {
        match event.observation() {
            OwnedObservation::ClockSync { clock } => {
                let history = self.history(clock);
                insert_sorted(&mut history.syncs, (clock.ticks(), event.at_nanos()));
            }
            OwnedObservation::ClockPair { first, second } => {
                let at = event.at_nanos();
                let entry = (first.ticks(), second.clock_id(), second.ticks(), at);
                insert_sorted(&mut self.history(first).pairs, entry);
                let entry = (second.ticks(), first.clock_id(), first.ticks(), at);
                insert_sorted(&mut self.history(second).pairs, entry);
            }
            _ => {}
        }
    }

    fn history(&mut self, clock: &OwnedClockTicks) -> &mut History {
        let history = self
            .clocks
            .entry(clock.clock_id())
            .or_insert_with(|| History {
                name: String::from(clock.name()),
                period: clock.period(),
                syncs: Vec::new(),
                pairs: Vec::new(),
            });
        history.period = clock.period();
        history
    }

    /// Returns the clocks seen, by id.
    pub fn ids(&self) -> impl Iterator<Item = ClockId> + '_ {
        self.clocks.keys().copied()
    }

    /// Returns what was recorded about the clock `clock`.
    #[must_use]
    pub fn clock(&self, clock: ClockId) -> Option<ClockInfo<'_>> {
        self.clocks.get(&clock).map(|history| ClockInfo {
            name: &history.name,
            period: history.period,
            syncs: history.syncs.len(),
            pairs: history.pairs.len(),
        })
    }

    /// Returns the id of the clock named `name`.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<ClockId> {
        self.clocks
            .iter()
            .find(|(_, history)| history.name == name)
            .map(|(id, _)| *id)
    }

    /// Forgets the syncs and pairs recorded before `before_nanos`, so clocks
    /// that follow a live session keep a bounded history.
    ///
    /// Each clock keeps its last sync, and its last pair with each other
    /// clock, recorded before `before_nanos`, so times read after it convert
    /// as they did before.
    pub fn forget_before(&mut self, before_nanos: u64) {
        for history in self.clocks.values_mut() {
            let anchor = history
                .syncs
                .iter()
                .map(|(_, at)| *at)
                .filter(|at| *at < before_nanos)
                .max();
            if let Some(anchor) = anchor {
                history.syncs.retain(|(_, at)| *at >= anchor);
            }
            let mut anchors = BTreeMap::<ClockId, u64>::new();
            for (_, other, _, at) in &history.pairs {
                if *at < before_nanos {
                    let anchor = anchors.entry(*other).or_insert(*at);
                    *anchor = (*anchor).max(*at);
                }
            }
            history
                .pairs
                .retain(|(_, other, _, at)| anchors.get(other).is_none_or(|anchor| at >= anchor));
        }
    }

    /// Converts `ticks` read from `clock` to nanoseconds since the producing
    /// runtime was created.
    ///
    /// A clock synced with the runtime converts directly: between two syncs
    /// by interpolating between them, so drift between the clocks is
    /// followed, and beyond them from the nearest sync at the clock's tick
    /// period. A clock only paired with others converts through the nearest
    /// pair with a synced clock. Returns `None` when the clock has no path
    /// to the runtime.
    #[must_use]
    pub fn to_runtime(&self, clock: ClockId, ticks: u64) -> Option<Converted> {
        let history = self.clocks.get(&clock)?;
        if !history.syncs.is_empty() {
            return Some(direct(history, ticks));
        }
        let (pair_ticks, other, other_ticks, _) = history
            .pairs
            .iter()
            .filter(|(_, other, ..)| {
                self.clocks
                    .get(other)
                    .is_some_and(|other| !other.syncs.is_empty())
            })
            .min_by_key(|(pair_ticks, ..)| pair_ticks.abs_diff(ticks))
            .copied()?;
        let target = self.clocks.get(&other)?;
        let offset = signed_nanos(history.period, ticks, pair_ticks);
        let other_offset = nanos_to_ticks(target.period, offset);
        let converted = direct(target, offset_ticks(other_ticks, other_offset));
        Some(Converted {
            via: Some(other),
            distance: converted
                .distance
                .max(u64::try_from(offset.unsigned_abs()).unwrap_or(u64::MAX)),
            ..converted
        })
    }
}

fn insert_sorted<T: Ord>(items: &mut Vec<T>, item: T) {
    let at = items.partition_point(|held| *held <= item);
    items.insert(at, item);
}

/// Converts through a clock's own syncs.
fn direct(history: &History, ticks: u64) -> Converted {
    let syncs = &history.syncs;
    let after = syncs.partition_point(|(sync_ticks, _)| *sync_ticks <= ticks);
    if after > 0 && after < syncs.len() {
        let (t0, n0) = syncs[after - 1];
        let (t1, n1) = syncs[after];
        let nanos = if t1 == t0 {
            i128::from(n0)
        } else {
            i128::from(n0)
                + (i128::from(ticks) - i128::from(t0)) * (i128::from(n1) - i128::from(n0))
                    / (i128::from(t1) - i128::from(t0))
        };
        let distance = history.period.ticks_to_nanos((ticks - t0).min(t1 - ticks));
        return Converted {
            nanos: saturate(nanos),
            via: None,
            between_syncs: true,
            distance,
        };
    }
    let (sync_ticks, sync_nanos) = if after == 0 {
        syncs[0]
    } else {
        syncs[syncs.len() - 1]
    };
    let offset = signed_nanos(history.period, ticks, sync_ticks);
    Converted {
        nanos: saturate(i128::from(sync_nanos) + offset),
        via: None,
        between_syncs: false,
        distance: u64::try_from(offset.unsigned_abs()).unwrap_or(u64::MAX),
    }
}

/// Returns `(ticks - from)` in nanoseconds, signed.
fn signed_nanos(period: TickPeriod, ticks: u64, from: u64) -> i128 {
    let delta = i128::from(ticks) - i128::from(from);
    delta * i128::from(period.nanos()) / i128::from(period.ticks())
}

/// Returns how many ticks of `period` last `nanos` nanoseconds, signed.
fn nanos_to_ticks(period: TickPeriod, nanos: i128) -> i128 {
    if period.nanos() == 0 {
        return 0;
    }
    nanos * i128::from(period.ticks()) / i128::from(period.nanos())
}

fn offset_ticks(ticks: u64, offset: i128) -> u64 {
    saturate(i128::from(ticks) + offset)
}

fn saturate(value: i128) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::Clocks;
    use crate::{OwnedClockTicks, OwnedObservation, OwnedTimedObservation};
    use alloc::string::String;
    use alloc::vec::Vec;
    use footfall_core::{ClockId, TickPeriod};

    const HOST: ClockId = ClockId::new(1);
    const GPU: ClockId = ClockId::new(2);
    // Mach-like host ticks: 125/3 ns each, so 24 ticks are 1 µs.
    const HOST_PERIOD: TickPeriod = TickPeriod::new(125, 3);

    fn ticks(clock: ClockId, ticks: u64, period: TickPeriod) -> OwnedClockTicks {
        let name = if clock == HOST { "host" } else { "gpu" };
        OwnedClockTicks::new(clock, String::from(name), ticks, period)
    }

    fn sync(at_nanos: u64, host_ticks: u64) -> OwnedTimedObservation {
        OwnedTimedObservation::new(
            at_nanos,
            0,
            1,
            OwnedObservation::ClockSync {
                clock: ticks(HOST, host_ticks, HOST_PERIOD),
            },
        )
    }

    #[test]
    fn a_synced_clock_converts_between_and_beyond_its_syncs() {
        let mut clocks = Clocks::new();
        // Host ticks 24,000,000 were 1 ms into the runtime; a second sync,
        // 1 s of host time later, shows the clocks drifted by 10 µs.
        clocks.observe(&sync(1_000_000, 24_000_000));
        clocks.observe(&sync(1_001_010_000, 48_000_000));
        assert_eq!(clocks.clock(HOST).map(|info| info.syncs()), Some(2));
        assert_eq!(clocks.find("host"), Some(HOST));

        // Halfway between: half the drift.
        let middle = clocks.to_runtime(HOST, 36_000_000).expect("synced");
        assert_eq!(middle.nanos(), 501_005_000);
        assert!(middle.between_syncs());
        assert_eq!(middle.distance(), 500_000_000);

        // Before the first sync: from it, at the nominal period.
        let before = clocks.to_runtime(HOST, 23_976_000).expect("synced");
        assert_eq!(before.nanos(), 0, "1 ms before is the runtime's start");
        assert!(!before.between_syncs());
        assert_eq!(before.distance(), 1_000_000);
        assert!(clocks.to_runtime(GPU, 0).is_none(), "never recorded");
    }

    #[test]
    fn a_paired_clock_converts_through_its_partner() {
        let mut clocks = Clocks::new();
        clocks.observe(&sync(5_000_000, 24_000_000));
        // A 1 GHz GPU clock read 7,000 when the host read 24,000,240,
        // that is 10 µs after the sync.
        clocks.observe(&OwnedTimedObservation::new(
            9_000_000,
            1,
            1,
            OwnedObservation::ClockPair {
                first: ticks(GPU, 7_000, TickPeriod::NANOSECOND),
                second: ticks(HOST, 24_000_240, HOST_PERIOD),
            },
        ));
        // 2,000 GPU ticks later is 2 µs after the pair: 12 µs after the
        // sync, the farthest the conversion extends.
        let converted = clocks
            .to_runtime(GPU, 9_000)
            .expect("paired with a synced clock");
        assert_eq!(converted.nanos(), 5_012_000);
        assert_eq!(converted.via(), Some(HOST));
        assert_eq!(converted.distance(), 12_000);
    }
    #[test]
    fn forgetting_keeps_what_later_times_need() {
        let pair = |at_nanos, gpu_ticks, host_ticks| {
            OwnedTimedObservation::new(
                at_nanos,
                1,
                1,
                OwnedObservation::ClockPair {
                    first: ticks(GPU, gpu_ticks, TickPeriod::NANOSECOND),
                    second: ticks(HOST, host_ticks, HOST_PERIOD),
                },
            )
        };
        let mut clocks = Clocks::new();
        // A host sync each second, 1 µs of drift each, and a GPU pair
        // each second.
        for second in 0..4_u64 {
            let at = second * 1_000_000_000;
            clocks.observe(&sync(at + second * 1_000, 24_000_000 * second));
            clocks.observe(&pair(at, 7_000 + at, 24_000_000 * second));
        }
        let later = [2_500_000_000_u64, 3_200_000_000];
        let host = |clocks: &Clocks, nanos: u64| clocks.to_runtime(HOST, nanos * 24 / 1_000);
        let before: Vec<_> = later.iter().map(|at| host(&clocks, *at)).collect();
        let gpu_before = clocks.to_runtime(GPU, 7_000 + 2_600_000_000);

        clocks.forget_before(2_500_000_000);
        assert_eq!(
            clocks.clock(HOST).map(|info| (info.syncs(), info.pairs())),
            Some((2, 2)),
            "the syncs and pairs at 2 s, the last before, and 3 s"
        );
        let after: Vec<_> = later.iter().map(|at| host(&clocks, *at)).collect();
        assert_eq!(before, after);
        assert_eq!(clocks.to_runtime(GPU, 7_000 + 2_600_000_000), gpu_before);
    }
}
