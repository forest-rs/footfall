// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Structured work analysis, with its population, metric, and evidence limits.

use crate::{check, json::Json, work};
use footfall::analysis::{Basis, Caveat, Comparison, OwnedLabelValue, Recorded, SpanTable, Trace};
use std::path::Path;

pub(crate) fn recorded(recorded: &Recorded) -> Json {
    let status = match recorded {
        Recorded::Yes => "recorded",
        Recorded::BelowLevel { .. } => "below_level",
        Recorded::CompiledOut => "compiled_out",
        Recorded::SamplesOff => "sampling_disabled",
        _ => "unknown",
    };
    let mut fields = vec![("status", Json::from(status))];
    if let Recorded::BelowLevel { level, needs } = recorded {
        fields.push(("level", Json::from(level.as_str())));
        fields.push(("needs", Json::from(needs.as_str())));
    }
    Json::object(fields)
}

fn caveat(caveat: &Caveat) -> Json {
    let kind = match caveat {
        Caveat::FewUnits => "few_units",
        Caveat::DifferentInstances => "different_instances",
        Caveat::DifferentProducers { .. } => "different_producers",
        Caveat::DifferentCapture => "different_capture",
        Caveat::Incomplete => "incomplete",
        _ => "unknown",
    };
    let mut fields = vec![("kind", Json::from(kind))];
    if let Caveat::DifferentProducers {
        baseline,
        candidate,
    } = caveat
    {
        fields.push(("baseline", Json::from(baseline.as_str())));
        fields.push(("candidate", Json::from(candidate.as_str())));
    }
    Json::object(fields)
}

pub(crate) fn basis(basis: &Basis) -> Json {
    Json::object([
        ("complete", Json::from(basis.is_complete())),
        ("recorded", recorded(&basis.recorded())),
        ("spans", Json::from(basis.spans())),
        ("interrupted_spans", Json::from(basis.interrupted())),
        ("open_spans", Json::from(basis.open())),
        ("unmatched_ends", Json::from(basis.unmatched_ends())),
        ("missed_events", Json::from(basis.missed_events())),
        ("unobserved_ns", Json::nanos(basis.unobserved())),
        ("unplaced_spans", Json::from(basis.unplaced())),
    ])
}

pub(crate) fn capture(path: &Path, trace: &Trace, read: check::Read) -> Json {
    let loss = check::losses(trace, read);
    Json::object([
        (
            "missed_events",
            Json::from(trace.gaps().iter().map(|gap| gap.missed()).sum::<u64>()),
        ),
        (
            "dropped_counter_updates",
            Json::from(trace.drops().counter_updates()),
        ),
        (
            "refused_contexts",
            Json::from(trace.drops().refused_contexts()),
        ),
        (
            "unattributed_observations",
            Json::from(trace.drops().unattributed()),
        ),
        (
            "updates_after_release",
            Json::from(trace.drops().updated_after_release()),
        ),
        ("path", Json::from(path.display().to_string())),
        (
            "instance",
            trace.metadata().map_or(Json::Null, |m| {
                Json::from(format!("{:016x}", m.instance_id()))
            }),
        ),
        (
            "producer_version",
            trace
                .metadata()
                .map_or(Json::Null, |m| Json::from(m.producer_version())),
        ),
        (
            "read_complete",
            Json::from(!read.truncated && read.skipped == 0),
        ),
        ("truncated", Json::from(read.truncated)),
        ("skipped_snapshots", Json::from(read.skipped)),
        ("observation_loss_reported", Json::from(!loss.satisfied())),
        ("loss_detail", Json::from(loss.found)),
        (
            "runtime_evicted_events",
            Json::from(trace.drops().evicted_events()),
        ),
    ])
}

pub(crate) fn selection(args: &work::Args, units: &work::Units<'_>) -> Json {
    let terms = |terms: &[work::Term]| {
        Json::Array(
            terms
                .iter()
                .map(|term| {
                    Json::object([
                        ("key", Json::from(term.key.as_str())),
                        (
                            "value",
                            match &term.value {
                                OwnedLabelValue::Unsigned(v) => Json::from(*v),
                                OwnedLabelValue::Signed(v) => Json::Signed(*v),
                                OwnedLabelValue::Bool(v) => Json::from(*v),
                                OwnedLabelValue::String(v) => Json::from(v.as_str()),
                                _ => Json::Null,
                            },
                        ),
                    ])
                })
                .collect(),
        )
    };
    Json::object([
        ("kind", Json::from(args.name())),
        ("scope", Json::from(args.scope.as_deref())),
        ("context", Json::from(args.context)),
        ("labels", terms(&args.label)),
        ("arguments", terms(&args.arg)),
        ("count", Json::from(units.units.len() as u64)),
        (
            "units",
            Json::Array(
                units
                    .units
                    .iter()
                    .map(|unit| {
                        Json::object([
                            ("id", Json::from(unit.id)),
                            ("context", Json::from(unit.context.get())),
                            ("thread", Json::from(unit.thread)),
                            ("begin_ns", Json::nanos(unit.range.start())),
                            ("end_ns", Json::nanos(unit.range.end())),
                            ("duration_ns", Json::nanos(unit.duration())),
                            (
                                "scope_arguments",
                                unit.span.map_or(Json::Null, |span| {
                                    Json::Array(
                                        span.args()
                                            .iter()
                                            .map(|arg| {
                                                Json::object([
                                                    ("key", Json::from(arg.key())),
                                                    (
                                                        "value",
                                                        match arg.value() {
                                                            OwnedLabelValue::Unsigned(v) => {
                                                                Json::from(*v)
                                                            }
                                                            OwnedLabelValue::Signed(v) => {
                                                                Json::Signed(*v)
                                                            }
                                                            OwnedLabelValue::Bool(v) => {
                                                                Json::from(*v)
                                                            }
                                                            OwnedLabelValue::String(v) => {
                                                                Json::from(v.as_str())
                                                            }
                                                            _ => Json::Null,
                                                        },
                                                    ),
                                                ])
                                            })
                                            .collect(),
                                    )
                                }),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

pub(crate) fn table(table: &SpanTable) -> Json {
    Json::object([
        ("basis", basis(table.basis())),
        (
            "rows",
            Json::Array(
                table
                    .rows()
                    .iter()
                    .map(|row| {
                        let stats = row.stats();
                        Json::object([
                            ("scope", Json::from(row.describe())),
                            ("count", Json::from(stats.count())),
                            ("total_ns", Json::nanos(stats.total())),
                            ("self_ns", Json::nanos(stats.self_time())),
                            ("median_ns", Json::nanos(stats.time().median())),
                            ("max_ns", Json::nanos(stats.time().max())),
                            ("interrupted", Json::from(stats.interrupted())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

pub(crate) fn comparison(comparison: &Comparison) -> Json {
    Json::object([
        ("metric", Json::from("self_elapsed_wall_time_ns_per_unit")),
        ("baseline_units", Json::from(comparison.baseline().units())),
        (
            "candidate_units",
            Json::from(comparison.candidate().units()),
        ),
        ("baseline_basis", basis(comparison.baseline().basis())),
        ("candidate_basis", basis(comparison.candidate().basis())),
        (
            "caveats",
            Json::Array(comparison.caveats().iter().map(caveat).collect()),
        ),
        (
            "rows",
            Json::Array(
                comparison
                    .rows()
                    .iter()
                    .map(|row| {
                        Json::object([
                            ("scope", Json::from(row.describe())),
                            (
                                "baseline_count_per_unit",
                                Json::from(row.baseline().count_per_unit()),
                            ),
                            (
                                "candidate_count_per_unit",
                                Json::from(row.candidate().count_per_unit()),
                            ),
                            (
                                "baseline_ns_per_unit",
                                Json::from(row.baseline().micros_per_unit() * 1_000.0),
                            ),
                            (
                                "candidate_ns_per_unit",
                                Json::from(row.candidate().micros_per_unit() * 1_000.0),
                            ),
                            (
                                "change_ns_per_unit",
                                Json::from(row.change_micros() * 1_000.0),
                            ),
                            (
                                "volume_ns_per_unit",
                                Json::from(row.volume_micros() * 1_000.0),
                            ),
                            ("cost_ns_per_unit", Json::from(row.cost_micros() * 1_000.0)),
                            (
                                "standard_error_ns_per_unit",
                                row.standard_error_micros()
                                    .map_or(Json::Null, |v| Json::from(v * 1_000.0)),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

pub(crate) fn document(fields: impl IntoIterator<Item = (&'static str, Json)>) -> Json {
    Json::object([
        ("footfall_version", Json::from(env!("CARGO_PKG_VERSION"))),
        ("metric", Json::from("elapsed_wall_time_ns")),
        ("attribution", Json::from("activity during selected intervals; concurrent work is not exclusive to a unit")),
    ].into_iter().chain(fields))
}
