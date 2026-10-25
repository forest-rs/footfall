// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Named counter checkpoint inspection and interval changes.

use crate::{json::Json, work_json};
use footfall::analysis::{CheckpointValue, CounterCheckpoint};
use footfall::session::CounterKind;
use std::path::Path;

fn value(value: &CheckpointValue) -> Json {
    Json::object([
        ("id", Json::from(value.desc_id().get())),
        ("name", Json::from(value.name())),
        ("unit", Json::from(value.unit().as_str())),
        ("context", Json::from(value.context_id().get())),
        (
            "kind",
            Json::from(match value.kind() {
                CounterKind::Counter => "counter",
                _ => "gauge",
            }),
        ),
        ("value", Json::from(value.value())),
    ])
}

pub(crate) fn point(point: &CounterCheckpoint) -> Json {
    Json::object([
        ("sequence", Json::from(point.sequence())),
        ("at_ns", Json::nanos(point.at())),
        ("name", Json::from(point.name())),
        ("context", Json::from(point.context_id().get())),
        ("thread", Json::from(point.thread_id())),
        ("span", Json::from(point.span().map(|span| span.sequence()))),
        ("complete", Json::from(point.complete())),
        (
            "values",
            Json::Array(point.values().iter().map(value).collect()),
        ),
    ])
}

pub(crate) fn run(
    path: &Path,
    from: Option<u64>,
    to: Option<u64>,
    json: bool,
) -> crate::Result<()> {
    let (trace, read) = crate::read_trace_saying(path)?;
    let points: Vec<_> = trace
        .counter_checkpoints()
        .iter()
        .filter(|point| {
            from.is_none() || Some(point.sequence()) == from || Some(point.sequence()) == to
        })
        .collect();
    let changes = match from.zip(to) {
        Some((from, to)) => Some(
            trace
                .counter_changes(from, to)
                .ok_or("checkpoint endpoints must exist in this capture and be in order")?,
        ),
        None => None,
    };
    if json {
        let interval = changes.as_ref().map_or(Json::Null, |changes| {
            Json::object([
                ("from", Json::from(from)),
                ("to", Json::from(to)),
                ("complete", Json::from(changes.complete())),
                (
                    "counters",
                    Json::Array(
                        changes
                            .counters()
                            .iter()
                            .map(|counter| {
                                Json::object([
                                    ("counter", value(counter.counter())),
                                    ("before", Json::from(counter.before())),
                                    ("after", Json::from(counter.after())),
                                    ("change", Json::from(counter.change())),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ])
        });
        anstream::println!(
            "{}",
            Json::object([
                ("footfall_version", Json::from(env!("CARGO_PKG_VERSION"))),
                (
                    "attribution",
                    Json::from("global interval activity; overlapping intervals are not exclusive")
                ),
                ("capture", work_json::capture(path, &trace, read)),
                (
                    "recorded",
                    work_json::recorded(
                        &trace.recorded(footfall::analysis::Signal::CounterSamples)
                    )
                ),
                (
                    "checkpoints",
                    Json::Array(points.iter().map(|boundary| point(boundary)).collect())
                ),
                ("interval", interval),
            ])
        );
    } else {
        anstream::println!(
            "Counter checkpoints · {} boundaries · global values sampled across threads",
            trace.counter_checkpoints().len()
        );
        for point in points {
            anstream::println!(
                "  {} {} at {} · {} values · {}",
                point.sequence(),
                point.name(),
                crate::format::duration(point.at()),
                point.values().len(),
                if point.complete() {
                    "complete"
                } else {
                    "incomplete batch"
                }
            );
        }
        if let Some(changes) = changes {
            anstream::println!("Interval activity (not exclusive transaction attribution):");
            for counter in changes.counters() {
                anstream::println!(
                    "  {} · {:?} → {:?} · change {:?}",
                    counter.counter().name(),
                    counter.before(),
                    counter.after(),
                    counter.change()
                );
            }
        }
        crate::report::losses(&trace, read);
    }
    Ok(())
}
