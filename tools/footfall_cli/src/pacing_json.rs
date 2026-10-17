// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `analyze pacing --json`: the pacing judgment as a document for other
//! programs, every frame included.
//!
//! Times are nanoseconds since the runtime started, durations nanoseconds,
//! and names snake case. The README describes the document.

use core::fmt::Debug;
use std::path::Path;

use footfall::analysis::{
    BasisCounts, Cadence, CadenceCounts, DurationStats, FramePacing, LateCauseCounts, Outcome,
    Pacing, PredictionError, SupportCounts, Trace, Why,
};
use footfall::session::OwnedLabelValue;
use footfall::{BeginsAt, FramePointKind, TimingBasis};

use crate::json::Json;

/// The kinds of frame point a frame can have a time for, in the order a
/// frame meets them.
const TIMED: [FramePointKind; 8] = [
    FramePointKind::Scheduled(BeginsAt::Opportunity),
    FramePointKind::Predicted,
    FramePointKind::Deadline,
    FramePointKind::Target,
    FramePointKind::Sampled,
    FramePointKind::Submitted,
    FramePointKind::Expected,
    FramePointKind::Presented,
];

/// The document for `pacing`, judged from `trace`, read from `path`.
pub(crate) fn document(path: &Path, trace: &Trace, pacing: &Pacing) -> Json {
    let counts = pacing.counts();
    let deadlines = pacing.deadlines();
    let capture = trace.metadata().map_or(Json::Null, |metadata| {
        Json::object([
            ("path", Json::from(path.display().to_string())),
            ("process", Json::from(metadata.process_name())),
            ("process_id", Json::from(metadata.process_id())),
            (
                "instance",
                Json::from(format!("{:016x}", metadata.instance_id())),
            ),
            ("producer_version", Json::from(metadata.producer_version())),
        ])
    });
    Json::object([
        ("footfall_version", Json::from(env!("CARGO_PKG_VERSION"))),
        ("capture", capture),
        ("tolerance_ns", Json::nanos(pacing.tolerance())),
        (
            "counts",
            Json::object([
                ("frames", Json::from(counts.total())),
                ("on_time", Json::from(counts.on_time)),
                ("early", Json::from(counts.early)),
                ("late", Json::from(counts.late)),
                ("dropped", Json::from(counts.dropped)),
                ("skipped", Json::from(counts.skipped)),
                ("submitted_late", Json::from(counts.submitted_late)),
                ("unknown", Json::from(counts.unknown)),
                ("missed", Json::from(counts.missed())),
                ("late_causes", late_causes(counts.late_causes)),
                ("late_support", late_support(counts.late_support)),
            ]),
        ),
        (
            "deadlines",
            Json::object([
                ("met", Json::from(deadlines.met)),
                ("missed", Json::from(deadlines.missed)),
            ]),
        ),
        (
            "presentation_basis",
            basis_counts(pacing.presentation_basis()),
        ),
        (
            "intervals",
            Json::object([
                ("stats", stats(&pacing.intervals())),
                ("breaks", Json::from(pacing.interval_breaks())),
                ("stutters", Json::from(pacing.stutters() as u64)),
                (
                    "streams",
                    Json::Array(
                        pacing
                            .streams()
                            .into_iter()
                            .map(|stream| {
                                let one = pacing.in_stream(stream);
                                Json::object([
                                    ("stream", Json::from(stream.id())),
                                    ("generation", Json::from(stream.generation())),
                                    ("stats", stats(&one.intervals())),
                                    ("breaks", Json::from(one.interval_breaks())),
                                    ("stutters", Json::from(one.stutters() as u64)),
                                    ("opportunities", Json::from(one.opportunities().recorded)),
                                    (
                                        "skipped_opportunities",
                                        Json::from(one.opportunities().skipped),
                                    ),
                                    ("cadence", cadence_counts(one.cadence())),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]),
        ),
        ("cadence", cadence_counts(pacing.cadence())),
        ("opportunities", {
            let opportunities = pacing.opportunities();
            Json::object([
                ("recorded", Json::from(opportunities.recorded)),
                ("skipped", Json::from(opportunities.skipped)),
                ("unplaced", Json::from(opportunities.unplaced)),
                ("intervals", stats(&pacing.opportunity_intervals())),
            ])
        }),
        ("begin_lateness", stats(&pacing.begin_lateness())),
        ("lateness", stats(&pacing.lateness())),
        ("latency", stats(&pacing.latency())),
        (
            "prediction_error",
            Json::object([
                (
                    "predicted",
                    prediction(&pacing.prediction_error(FramePointKind::Predicted)),
                ),
                (
                    "target",
                    prediction(&pacing.prediction_error(FramePointKind::Target)),
                ),
                (
                    "expected",
                    prediction(&pacing.prediction_error(FramePointKind::Expected)),
                ),
            ]),
        ),
        ("wasted", {
            let wasted = pacing.wasted();
            Json::object([
                ("frames", Json::from(wasted.frames)),
                ("cpu_frames", Json::from(wasted.cpu_frames)),
                ("cpu_ns", Json::nanos(wasted.cpu)),
                ("gpu_frames", Json::from(wasted.gpu_frames)),
                ("gpu_ns", Json::nanos(wasted.gpu)),
            ])
        }),
        ("unplaced_points", Json::from(pacing.unplaced_points())),
        (
            "frames",
            Json::Array(pacing.frames().iter().map(frame).collect()),
        ),
    ])
}

fn frame(frame: &FramePacing) -> Json {
    let number = frame.number();
    let outcome = frame.outcome();
    let (name, by) = match outcome {
        Outcome::OnTime => ("on_time", None),
        Outcome::Early { by } => ("early", Some(by)),
        Outcome::Late { by, .. } => ("late", Some(by)),
        Outcome::Dropped { .. } => ("dropped", None),
        Outcome::SubmittedLate { by } => ("submitted_late", Some(by)),
        _ => ("unknown", None),
    };
    let mut points: Vec<(String, Json)> = TIMED
        .into_iter()
        .filter_map(|kind| {
            let at = frame.time_of(kind)?;
            Some((snake(kind), point(frame, kind, Json::nanos(at))))
        })
        .collect();
    if let Some(reason) = frame.dropped() {
        points.push((
            String::from("dropped"),
            point(
                frame,
                FramePointKind::Dropped(reason),
                Json::from(snake(reason)),
            ),
        ));
    }
    Json::object([
        ("label", Json::from(number.to_string())),
        ("stream", Json::from(number.stream().id())),
        ("generation", Json::from(number.stream().generation())),
        ("number", Json::from(number.number())),
        (
            "frame_id",
            Json::from(frame.frame().map(|id| id.sequence())),
        ),
        ("outcome", Json::from(name)),
        ("by_ns", Json::nanos_or_null(by)),
        ("missed", Json::from(outcome.missed())),
        ("why", frame.why().map_or(Json::Null, why)),
        ("cadence", frame.cadence().map_or(Json::Null, cadence)),
        ("met_deadline", Json::from(frame.met_deadline())),
        (
            "submitted_late_ns",
            Json::nanos_or_null(frame.submitted_late()),
        ),
        (
            "begins_at",
            frame
                .begins_at()
                .map_or(Json::Null, |begins_at| Json::from(snake(begins_at))),
        ),
        ("begin_judged", Json::from(frame.begin_judged())),
        ("began_late_ns", Json::nanos_or_null(frame.began_late())),
        (
            "opportunity_late_ns",
            Json::nanos_or_null(frame.opportunity_late()),
        ),
        (
            "work",
            range(frame.work().map(|work| (work.start(), work.end()))),
        ),
        (
            "gpu",
            range(frame.gpu().map(|gpu| (gpu.start(), gpu.end()))),
        ),
        ("gpu_busy_ns", Json::nanos_or_null(frame.gpu_busy())),
        ("latency_ns", Json::nanos_or_null(frame.latency())),
        ("unplaced_points", Json::from(frame.unplaced_points())),
        ("points", Json::Object(points)),
    ])
}

/// A frame point: its time (or a dropped point's reason), how the time was
/// known, and the recorder's args.
fn point(frame: &FramePacing, kind: FramePointKind, value: Json) -> Json {
    let key = if matches!(kind, FramePointKind::Dropped(_)) {
        "reason"
    } else {
        "at_ns"
    };
    Json::object([
        (key, value),
        ("basis", Json::from(basis(frame.basis(kind)))),
        (
            "args",
            Json::Object(
                frame
                    .args(kind)
                    .iter()
                    .map(|arg| (arg.key().to_owned(), label(arg.value())))
                    .collect(),
            ),
        ),
    ])
}

fn why(why: &Why) -> Json {
    Json::object([
        ("cause", Json::from(snake(why.cause))),
        ("party", Json::from(snake(why.cause.party()))),
        ("support", Json::from(snake(why.support))),
        ("submitted_late_ns", Json::nanos_or_null(why.submitted_late)),
        (
            "opportunity_late_ns",
            Json::nanos_or_null(why.opportunity_late),
        ),
        ("began_late_ns", Json::nanos_or_null(why.began_late)),
        ("begin_judged", Json::from(why.begin_judged)),
        ("submit_reported", Json::from(why.submit_reported)),
        ("submitted_basis", Json::from(basis(why.submitted))),
        ("gpu_late_ns", Json::nanos_or_null(why.gpu_late)),
        ("gpu_recorded", Json::from(why.gpu_recorded)),
        ("behind_previous", Json::from(why.behind_previous)),
        ("whole_refreshes", Json::from(why.whole_refreshes)),
        ("presented_basis", Json::from(basis(why.presented))),
    ])
}

fn late_causes(causes: LateCauseCounts) -> Json {
    Json::object([
        ("missed_deadline", Json::from(causes.missed_deadline)),
        (
            "skipped_opportunity",
            Json::from(causes.skipped_opportunity),
        ),
        ("began_late", Json::from(causes.began_late)),
        ("backlog", Json::from(causes.backlog)),
        ("gpu", Json::from(causes.gpu)),
        ("mispredicted", Json::from(causes.mispredicted)),
        ("display", Json::from(causes.display)),
        ("after_submit", Json::from(causes.after_submit)),
        ("unknown", Json::from(causes.unknown)),
    ])
}

fn cadence(cadence: Cadence) -> Json {
    let (kind, by) = match cadence {
        Cadence::Stalled { by } => ("stalled", Some(by)),
        Cadence::Hurried { by } => ("hurried", Some(by)),
        _ => ("steady", None),
    };
    Json::object([
        ("kind", Json::from(kind)),
        ("by_ns", Json::nanos_or_null(by)),
    ])
}

fn cadence_counts(counts: CadenceCounts) -> Json {
    Json::object([
        ("judged", Json::from(counts.judged())),
        ("steady", Json::from(counts.steady)),
        ("late_but_steady", Json::from(counts.late_but_steady)),
        ("stalled", Json::from(counts.stalled)),
        ("hurried", Json::from(counts.hurried)),
        ("hitches", Json::from(counts.hitches())),
    ])
}

fn late_support(support: SupportCounts) -> Json {
    Json::object([
        ("observed", Json::from(support.observed)),
        ("observed_unstated", Json::from(support.observed_unstated)),
        ("inferred", Json::from(support.inferred)),
        ("unresolved", Json::from(support.unresolved)),
    ])
}

fn basis_counts(counts: BasisCounts) -> Json {
    Json::object([
        ("observed", Json::from(counts.observed)),
        ("estimated", Json::from(counts.estimated)),
        ("unknown", Json::from(counts.unknown)),
    ])
}

fn prediction(error: &PredictionError) -> Json {
    Json::object([
        ("within", Json::from(error.within())),
        ("off", stats(&error.off)),
        ("early", stats(&error.early)),
        ("late", stats(&error.late)),
    ])
}

/// How many durations, and, when there are any, their distribution.
fn stats(stats: &DurationStats) -> Json {
    if stats.count() == 0 {
        return Json::object([("count", Json::from(0_u64))]);
    }
    Json::object([
        ("count", Json::from(stats.count())),
        ("mean_ns", Json::nanos(stats.mean())),
        ("median_ns", Json::nanos(stats.median())),
        ("p90_ns", Json::nanos(stats.p90())),
        ("p99_ns", Json::nanos(stats.p99())),
        ("min_ns", Json::nanos(stats.min())),
        ("max_ns", Json::nanos(stats.max())),
    ])
}

fn range(range: Option<(core::time::Duration, core::time::Duration)>) -> Json {
    range.map_or(Json::Null, |(start, end)| {
        Json::object([
            ("start_ns", Json::nanos(start)),
            ("end_ns", Json::nanos(end)),
        ])
    })
}

fn basis(basis: TimingBasis) -> String {
    snake(basis)
}

fn label(value: &OwnedLabelValue) -> Json {
    match value {
        OwnedLabelValue::Unsigned(value) => Json::from(*value),
        OwnedLabelValue::Signed(value) => Json::Signed(*value),
        OwnedLabelValue::Bool(value) => Json::from(*value),
        OwnedLabelValue::String(value) => Json::from(value.as_str()),
        // A kind of value added later: its text.
        other => Json::from(other.to_string()),
    }
}

/// A unit variant's name in snake case, so names added later need no
/// mapping here: `MissedDeadline` is `missed_deadline`.
fn snake(value: impl Debug) -> String {
    let name = format!("{value:?}");
    let name = name.split('(').next().unwrap_or(&name);
    let mut snake = String::with_capacity(name.len() + 4);
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                snake.push('_');
            }
            snake.push(ch.to_ascii_lowercase());
        } else {
            snake.push(ch);
        }
    }
    snake
}

#[cfg(test)]
mod tests {
    use core::time::Duration;
    use std::path::Path;

    use footfall::analysis::Trace;
    use footfall::runtime::Runtime;
    use footfall::session::OwnedSnapshot;
    use footfall::{
        Backend as _, BeginsAt, ClockDesc, ClockReading, DropReason, FrameOpportunity, FramePoint,
        FramePointKind, FrameStream, TickPeriod, TimingBasis,
    };

    use super::{document, snake};

    static HOST: ClockDesc = ClockDesc::new("host");

    #[test]
    fn documents_hold_every_frame_with_its_points_and_why() {
        // Not installed: the top test installs the global backend.
        let runtime = Runtime::default();
        runtime.clock_sync(ClockReading::new(&HOST, 0, TickPeriod::NANOSECOND));
        let at = |number: u64, kind, ms: u64| {
            FramePoint::at(number, kind, &HOST, TickPeriod::NANOSECOND, ms * 1_000_000)
        };
        use FramePointKind::{Deadline, Presented, Target};
        runtime.frame_point(&at(1, Target, 16));
        runtime.frame_point(&at(1, Presented, 16).with_basis(TimingBasis::Observed));
        runtime.frame_point(
            &at(2, Deadline, 20).with_args(footfall::args!("pipeline_depth" => 2_u32)),
        );
        runtime.frame_point(&at(2, Target, 33));
        runtime.frame_point(&at(2, Presented, 41).with_basis(TimingBasis::Estimated));
        runtime.frame_point(&FramePoint::dropped(3, DropReason::Superseded));
        runtime.frame_point(&at(1, FramePointKind::Scheduled(BeginsAt::Opportunity), 0));
        for ms in [0, 16, 33] {
            runtime.frame_opportunity(
                &FrameOpportunity::at(
                    FrameStream::DEFAULT,
                    &HOST,
                    TickPeriod::NANOSECOND,
                    ms * 1_000_000,
                )
                .with_interval(16_666_667),
            );
        }
        let trace = Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot()));
        let pacing = trace.pacing(Duration::from_millis(1));

        let text = document(Path::new("run.ffcap"), &trace, &pacing).to_string();
        for expected in [
            r#""tolerance_ns":1000000"#,
            r#""counts":{"frames":3,"on_time":1,"early":0,"late":1,"dropped":0,"skipped":1"#,
            r#""presentation_basis":{"observed":1,"estimated":1,"unknown":0}"#,
            r#""label":"2","stream":0,"generation":0,"number":2"#,
            r#""deadlines":{"met":0,"missed":0}"#,
            r#""streams":[{"stream":0,"generation":0,"stats":{"count":1"#,
            r#""outcome":"late","by_ns":8000000,"missed":true,"why":{"cause":"#,
            r#""met_deadline":null,"submitted_late_ns":null"#,
            r#""deadline":{"at_ns":"#,
            r#""args":{"pipeline_depth":2}"#,
            r#""presented":{"at_ns":"#,
            r#""basis":"estimated""#,
            r#""outcome":"dropped","by_ns":null,"missed":false"#,
            r#""dropped":{"reason":"superseded","basis":"unknown","args":{}}"#,
            r#""opportunities":{"recorded":3,"skipped":0,"unplaced":0,"intervals":{"count":2"#,
            r#""stutters":0,"opportunities":3,"skipped_opportunities":0"#,
            r#""begins_at":"opportunity","begin_judged":false"#,
            r#""scheduled":{"at_ns":"#,
            r#""mispredicted":1"#,
            r#""cause":"mispredicted","party":"platform","support":"inferred""#,
            r#""whole_refreshes":false"#,
            r#""cadence":{"judged":1,"steady":0,"late_but_steady":0,"stalled":1,"hurried":0,"hitches":1}"#,
            r#""cadence":{"kind":"stalled","by_ns":8000000}"#,
        ] {
            assert!(text.contains(expected), "{expected} missing from {text}");
        }
    }

    #[test]
    fn names_are_snake_case() {
        assert_eq!(snake(DropReason::MissedDeadline), "missed_deadline");
        assert_eq!(
            snake(FramePointKind::Dropped(DropReason::Replaced)),
            "dropped"
        );
        assert_eq!(snake(FramePointKind::Presented), "presented");
    }
}
