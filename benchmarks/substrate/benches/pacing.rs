// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scaling of pacing analysis over exact schedules. Trace construction is
//! outside the timed region. Compare absent opportunities, opportunities
//! without intervals, and stated intervals, for on-time frames and misses.
//!
//! Run `cargo bench -p footfall_benchmarks --bench pacing`, using Criterion's
//! `--save-baseline` and `--baseline` flags to compare changes.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use footfall::{
    Args, ClockDesc, ClockReading, ContextId, FrameNumber, FramePointKind, FrameStream, MarkerKind,
    MarkerPhase, TickPeriod, TimingBasis,
};
use footfall_analysis::Trace;
use footfall_benchmarks::{MEASUREMENT, WARM_UP};
use footfall_runtime::{ClockTicks, Observation, Snapshot, SnapshotMetadata, TimedObservation};
use std::{hint::black_box, time::Duration};

static HOST: ClockDesc = ClockDesc::new("host");
const REFRESH: u64 = 10_000_000;

fn schedule(frames: u64, opportunities: Option<Option<u64>>, misses: bool) -> Trace {
    let ticks = |at| ClockTicks::from(ClockReading::new(&HOST, at, TickPeriod::NANOSECOND));
    let mut events = Vec::new();
    let mut emit = |at, observation| {
        let sequence = u64::try_from(events.len()).expect("benchmark event count fits u64");
        events.push(TimedObservation::new(at, sequence, 1, observation));
    };
    emit(0, Observation::ClockSync { clock: ticks(0) });
    for number in 1..=frames {
        let target = number * REFRESH;
        let begin = target - REFRESH;
        let end = begin + 2_000_000;
        if let Some(interval) = opportunities {
            emit(
                begin,
                Observation::FrameOpportunity {
                    stream: FrameStream::DEFAULT,
                    time: ticks(begin),
                    interval,
                    args: Args::none(),
                },
            );
        }
        for (at, phase) in [(begin, MarkerPhase::Begin), (end, MarkerPhase::End)] {
            emit(
                at,
                Observation::FrameMarker {
                    kind: MarkerKind::Frame,
                    phase,
                    number: Some(FrameNumber::from(number)),
                    context_id: ContextId::new(0),
                },
            );
        }
        let late = if misses && number % 10 == 0 {
            REFRESH
        } else {
            0
        };
        for (kind, at) in [
            (FramePointKind::Deadline, target - 4_000_000),
            (FramePointKind::Target, target),
            (FramePointKind::Presented, target + late),
        ] {
            emit(
                end,
                Observation::FramePoint {
                    number: FrameNumber::from(number),
                    kind,
                    time: Some(ticks(at)),
                    basis: TimingBasis::Observed,
                    args: Args::none(),
                },
            );
        }
    }
    let mut builder = Snapshot::builder(SnapshotMetadata::new("pacing benchmark"))
        .captured_at_nanos((frames + 2) * REFRESH);
    for event in events {
        builder = builder.event(event);
    }
    Trace::from_snapshot(&builder.build().into())
}

fn pacing(c: &mut Criterion) {
    for (name, opportunities) in [
        ("no_opportunities", None),
        ("unstated_interval", Some(None)),
        ("stated_interval", Some(Some(REFRESH))),
    ] {
        for misses in [false, true] {
            let outcome = if misses { "misses" } else { "on_time" };
            let mut group = c.benchmark_group(format!("analysis/pacing/{name}/{outcome}"));
            for frames in [1_000, 4_000, 8_000] {
                let trace = schedule(frames, opportunities, misses);
                let tolerance = Duration::from_millis(1);
                let counts = trace.pacing(tolerance).counts();
                assert_eq!(
                    counts.late,
                    if misses { frames / 10 } else { 0 },
                    "the schedule must exercise the requested misses"
                );
                assert_eq!(
                    counts.on_time + counts.late,
                    frames,
                    "all frames are judged"
                );
                group.throughput(Throughput::Elements(frames));
                group.bench_with_input(BenchmarkId::from_parameter(frames), &trace, |b, trace| {
                    b.iter(|| black_box(black_box(trace).pacing(tolerance)));
                });
            }
            group.finish();
        }
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().warm_up_time(WARM_UP).measurement_time(MEASUREMENT);
    targets = pacing
}
criterion_main!(benches);
