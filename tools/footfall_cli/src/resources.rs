// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resource lifetimes and observed scope provenance, without inferred ownership.

use crate::{json::Json, work_json};
use footfall::analysis::{ResourceAction, ResourceObservation, SpanId, Trace};
use std::{path::Path, time::Duration};

fn scopes(trace: &Trace, mut id: Option<SpanId>) -> Json {
    let mut scopes = Vec::new();
    let open = trace.open_spans();
    while let Some(held) = id {
        let scope = if let Some(span) = trace.span(held) {
            crate::inspect::scope(trace, span)
        } else if let Some(span) = open.iter().find(|span| span.id() == held) {
            crate::inspect::open_scope(trace, span)
        } else {
            scopes.push(Json::object([
                ("id", Json::from(held.sequence())),
                ("resolved", Json::from(false)),
            ]));
            break;
        };
        let Json::Object(mut fields) = scope else {
            unreachable!("scope object")
        };
        fields.push(("resolved".into(), Json::from(true)));
        scopes.push(Json::Object(fields));
        id = trace.scope_parent(held).flatten();
    }
    Json::Array(scopes)
}

pub(crate) fn observation(trace: &Trace, event: &ResourceObservation) -> Json {
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
    lifetime: Option<u64>,
) -> crate::Result<()> {
    let (trace, read) = crate::read_trace_saying(path)?;
    if created_in.is_some_and(|id| trace.scope_parent(SpanId::new(id)).is_none()) {
        return Err("--created-in needs the id of a retained scope; use analyze summary --scope NAME --json".into());
    }
    if lifetime.is_some_and(|id| {
        trace
            .resource_lifetime(footfall::analysis::ResourceLifetimeId::new(id))
            .is_none()
    }) {
        return Err("resource lifetime is absent from retained history".into());
    }
    let alive = alive_at_ns.map(Duration::from_nanos);
    if alive.is_some_and(|at| at < trace.observed().start() || at > trace.observed().end()) {
        return Err("--alive-at-ns must lie within the capture's observed time range".into());
    }
    let resources: Vec<_> = trace
        .resources()
        .iter()
        .filter(|resource| {
            lifetime.is_none_or(|id| resource.lifetime_id().sequence() == id)
                && created_in.is_none_or(|id| {
                    resource
                        .creation()
                        .is_some_and(|event| trace.scope_contains(SpanId::new(id), event.span()))
                })
                && alive.is_none_or(|at| {
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
                            .map(|resource| {
                                let Json::Object(mut fields) =
                                    crate::inspect::resource(&trace, resource)
                                else {
                                    unreachable!("resource object")
                                };
                                fields.push((
                                    "size_at_query_bytes".into(),
                                    alive.map_or(Json::Null, |at| Json::from(resource.size_at(at))),
                                ));
                                Json::Object(fields)
                            })
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
