// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resource lifetimes and observed scope provenance, without inferred ownership.

use crate::{json::Json, work_json};
use footfall::analysis::{OwnedLabelValue, ResourceAction, ResourceObservation, SpanId, Trace};
use std::{path::Path, time::Duration};

fn scopes(trace: &Trace, mut id: Option<SpanId>) -> Json {
    let mut scopes = Vec::new();
    while let Some(held) = id {
        let Some(span) = trace.span(held) else {
            scopes.push(Json::object([
                ("id", Json::from(held.sequence())),
                ("resolved", Json::from(false)),
            ]));
            break;
        };
        scopes.push(Json::object([
            ("id", Json::from(held.sequence())),
            ("resolved", Json::from(true)),
            ("name", Json::from(span.name())),
            (
                "arguments",
                Json::Array(
                    span.args()
                        .iter()
                        .map(|arg| {
                            Json::object([
                                ("key", Json::from(arg.key())),
                                (
                                    "value",
                                    match arg.value() {
                                        OwnedLabelValue::Unsigned(value) => Json::from(*value),
                                        OwnedLabelValue::Signed(value) => Json::Signed(*value),
                                        OwnedLabelValue::Bool(value) => Json::from(*value),
                                        OwnedLabelValue::String(value) => {
                                            Json::from(value.as_str())
                                        }
                                        _ => Json::Null,
                                    },
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]));
        id = trace.parent(span).map(|parent| parent.id());
    }
    Json::Array(scopes)
}

fn within(trace: &Trace, mut id: Option<SpanId>, ancestor: SpanId) -> bool {
    while let Some(held) = id {
        if held == ancestor {
            return true;
        }
        id = trace
            .span(held)
            .and_then(|span| trace.parent(span))
            .map(|parent| parent.id());
    }
    false
}

fn observation(trace: &Trace, event: &ResourceObservation) -> Json {
    Json::object([
        ("sequence", Json::from(event.sequence())),
        ("at_ns", Json::nanos(event.at())),
        ("thread", Json::from(event.thread_id())),
        ("context", Json::from(event.context_id().get())),
        ("span", Json::from(event.span().map(|id| id.sequence()))),
        ("scope_chain", scopes(trace, event.span())),
        (
            "action",
            Json::from(match event.action() {
                ResourceAction::Create => "create",
                ResourceAction::Update => "update",
                ResourceAction::Drop => "drop",
                _ => "unknown",
            }),
        ),
        ("size_bytes", Json::from(event.size())),
    ])
}

pub(crate) fn run(
    path: &Path,
    created_in: Option<u64>,
    alive_at_ns: Option<u64>,
    json: bool,
) -> crate::Result<()> {
    let (trace, read) = crate::read_trace_saying(path)?;
    if created_in.is_some_and(|id| trace.span(SpanId::new(id)).is_none()) {
        return Err("--created-in needs the id of a retained completed scope; use analyze summary --scope NAME --json".into());
    }
    let alive = alive_at_ns.map(Duration::from_nanos);
    if alive.is_some_and(|at| at < trace.observed().start() || at > trace.observed().end()) {
        return Err("--alive-at-ns must lie within the capture's observed time range".into());
    }
    let resources: Vec<_> = trace
        .resources()
        .iter()
        .filter(|resource| {
            created_in.is_none_or(|id| {
                resource
                    .creation()
                    .is_some_and(|event| within(&trace, event.span(), SpanId::new(id)))
            }) && alive.is_none_or(|at| {
                resource.alive_at(at)
                    && resource
                        .observations()
                        .first()
                        .is_some_and(|event| event.at() <= at)
            })
        })
        .collect();
    if json {
        anstream::println!(
            "{}",
            Json::object([
                ("footfall_version", Json::from(env!("CARGO_PKG_VERSION"))),
                ("capture", work_json::capture(path, &trace, read)),
                (
                    "recorded",
                    work_json::recorded(&trace.recorded(footfall::analysis::Signal::Resources))
                ),
                ("created_in", Json::from(created_in)),
                ("alive_at_ns", Json::from(alive_at_ns)),
                ("count", Json::from(resources.len() as u64)),
                (
                    "without_creation_scope",
                    Json::from(
                        trace
                            .resources()
                            .iter()
                            .filter(|resource| resource
                                .creation()
                                .and_then(|event| event.span())
                                .is_none())
                            .count() as u64
                    )
                ),
                (
                    "unknown_at_query",
                    Json::from(alive.map(|at| {
                        trace
                            .resources()
                            .iter()
                            .filter(|resource| {
                                resource.created().is_none()
                                    && resource
                                        .observations()
                                        .first()
                                        .is_some_and(|event| event.at() > at)
                            })
                            .count() as u64
                    }))
                ),
                (
                    "retention_basis",
                    Json::from(
                        "observed resource events; missing events can hide lifecycle changes; ownership roots require application observations"
                    )
                ),
                (
                    "resources",
                    Json::Array(
                        resources
                            .iter()
                            .map(|resource| Json::object([
                                ("kind", Json::from(resource.kind_name())),
                                ("kind_id", Json::from(resource.kind_id().get())),
                                ("resource_id", Json::from(resource.resource_id().get())),
                                ("created_ns", Json::nanos_or_null(resource.created())),
                                ("dropped_ns", Json::nanos_or_null(resource.dropped())),
                                ("latest_size_bytes", Json::from(resource.size())),
                                (
                                    "size_at_query_bytes",
                                    alive.map_or(Json::Null, |at| Json::from(resource.size_at(at)))
                                ),
                                (
                                    "creation",
                                    resource
                                        .creation()
                                        .map_or(Json::Null, |event| observation(&trace, event))
                                ),
                                (
                                    "observations",
                                    Json::Array(
                                        resource
                                            .observations()
                                            .iter()
                                            .map(|event| observation(&trace, event))
                                            .collect()
                                    )
                                ),
                            ]))
                            .collect()
                    )
                ),
            ])
        );
    } else {
        anstream::println!(
            "{} observed resource lifetimes · sizes are application-reported bytes",
            resources.len()
        );
        for resource in resources {
            let origin = resource
                .creation()
                .and_then(|event| event.span())
                .map_or_else(
                    || String::from("unknown or unscoped"),
                    |span| span.to_string(),
                );
            anstream::println!(
                "  {}#{} · size {:?} · created in {} · {}",
                resource.kind_name(),
                resource.resource_id().get(),
                resource.size(),
                origin,
                if resource.dropped().is_some() {
                    "released"
                } else {
                    "no drop observed"
                }
            );
        }
        crate::report::losses(&trace, read);
    }
    Ok(())
}
