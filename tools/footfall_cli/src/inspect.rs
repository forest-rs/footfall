// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Capture adapters for the typed analysis operations.

use crate::{json::Json, work, work_json};
use footfall::analysis::{
    EventFilter, FlowMatch, FlowQuery, Membership, Message, MessageQuery, OpenSpan, OwnedArgEntry,
    OwnedLabelValue, Page, QueryEvidence, Resource, ResourceActivity, ResourceGroupKey,
    ResourceGrouping, ResourceInterval, Span, SpanId, TimeRange, Trace, WorkId,
};
use std::{path::Path, time::Duration};

#[derive(Clone, Debug, Default, clap::Args)]
pub(crate) struct Filter {
    /// Select observations in this scope or its captured descendants, including open scopes.
    #[arg(long)]
    within: Option<u64>,
    /// Inclusive start in runtime nanoseconds.
    #[arg(long, requires = "end_ns")]
    start_ns: Option<u64>,
    /// Exclusive end in runtime nanoseconds.
    #[arg(long, requires = "start_ns")]
    end_ns: Option<u64>,
    /// Exact context ID.
    #[arg(long)]
    context: Option<u64>,
    /// Exact recording thread.
    #[arg(long)]
    thread: Option<u64>,
    /// Typed context-label equality, repeatable (KEY=VALUE).
    #[arg(long)]
    label: Vec<work::Term>,
    /// Continue after the last message sequence or flow ID.
    #[arg(long)]
    after: Option<u64>,
    /// Maximum results; use next in JSON to continue.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
    limit: u32,
}
impl Filter {
    fn query(&self) -> crate::Result<EventFilter> {
        if self
            .start_ns
            .zip(self.end_ns)
            .is_some_and(|(start, end)| start > end)
        {
            return Err("--start-ns must not exceed --end-ns".into());
        }
        Ok(EventFilter {
            within: self.within.map(SpanId::new),
            range: self.start_ns.zip(self.end_ns).map(|(start, end)| {
                TimeRange::new(Duration::from_nanos(start), Duration::from_nanos(end))
            }),
            context: self.context.map(footfall::ContextId::new),
            thread: self.thread,
            labels: self
                .label
                .iter()
                .map(|term| (term.key.clone(), term.value.clone()))
                .collect(),
        })
    }
    fn page(&self) -> Page {
        Page {
            after: self.after,
            limit: self.limit as usize,
        }
    }
}

fn value(value: &OwnedLabelValue) -> Json {
    match value {
        OwnedLabelValue::Unsigned(v) => Json::from(*v),
        OwnedLabelValue::Signed(v) => Json::Signed(*v),
        OwnedLabelValue::Bool(v) => Json::from(*v),
        OwnedLabelValue::String(v) => Json::from(v.as_str()),
        _ => Json::Null,
    }
}
pub(crate) fn term(key: &str, held: &OwnedLabelValue) -> Json {
    let kind = match held {
        OwnedLabelValue::Unsigned(_) => "unsigned",
        OwnedLabelValue::Signed(_) => "signed",
        OwnedLabelValue::Bool(_) => "bool",
        OwnedLabelValue::String(_) => "string",
        _ => "unknown",
    };
    Json::object([
        ("key", Json::from(key)),
        ("value", value(held)),
        ("value_type", Json::from(kind)),
    ])
}
pub(crate) fn args(args: &[OwnedArgEntry]) -> Json {
    Json::Array(
        args.iter()
            .map(|arg| term(arg.key(), arg.value()))
            .collect(),
    )
}
fn context(trace: &Trace, id: footfall::ContextId) -> Json {
    Json::object([
        ("id", Json::from(id.get())),
        (
            "labels",
            trace.context(id).map_or(Json::Null, |context| {
                Json::Array(
                    context
                        .entries()
                        .iter()
                        .map(|label| term(label.key(), label.value()))
                        .collect(),
                )
            }),
        ),
    ])
}
pub(crate) fn evidence(evidence: &QueryEvidence) -> Json {
    Json::object([
        ("recorded", work_json::recorded(&evidence.recorded)),
        (
            "recording_history",
            work_json::recorded(&evidence.recording_history),
        ),
        ("complete", Json::from(evidence.is_complete())),
        ("start_ns", Json::nanos(evidence.range.start())),
        ("end_ns", Json::nanos(evidence.range.end())),
        ("covered", Json::from(evidence.covered)),
        ("open_work", Json::from(evidence.open_work)),
        ("unplaced_spans", Json::from(evidence.unplaced_spans as u64)),
        ("unattributed", Json::from(evidence.unattributed)),
        ("refused_contexts", Json::from(evidence.refused_contexts)),
        (
            "counter_updates_lost",
            Json::from(evidence.counter_updates_lost),
        ),
        ("history_complete", Json::from(evidence.history_complete)),
        (
            "gaps",
            Json::Array(
                evidence
                    .gaps
                    .iter()
                    .map(|gap| {
                        Json::object([
                            ("start_ns", Json::nanos(gap.range().start())),
                            ("end_ns", Json::nanos(gap.range().end())),
                            ("missed", Json::from(gap.missed())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
pub(crate) fn message(trace: &Trace, message: &Message) -> Json {
    Json::object([
        ("sequence", Json::from(message.sequence())),
        ("at_ns", Json::nanos(message.at())),
        ("name", Json::from(message.name())),
        ("text", Json::from(message.text())),
        ("arguments", args(message.args())),
        ("context", context(trace, message.context_id())),
        ("thread", Json::from(message.thread_id())),
        ("span", Json::from(message.span().map(SpanId::sequence))),
    ])
}
pub(crate) fn flow(trace: &Trace, found: &FlowMatch<'_>) -> Json {
    Json::object([
        ("id", Json::from(found.flow.id().get())),
        ("endpoints_present", Json::from(found.flow.is_complete())),
        ("evidence", evidence(&found.evidence)),
        (
            "steps",
            Json::Array(
                found
                    .flow
                    .steps()
                    .iter()
                    .map(|step| {
                        Json::object([
                            ("sequence", Json::from(step.sequence())),
                            ("at_ns", Json::nanos(step.at())),
                            (
                                "phase",
                                Json::from(match step.phase() {
                                    footfall::FlowPhase::Begin => "begin",
                                    footfall::FlowPhase::End => "end",
                                    _ => "step",
                                }),
                            ),
                            (
                                "matched",
                                Json::from(found.matching_steps.contains(&step.sequence())),
                            ),
                            ("context", context(trace, step.context_id())),
                            ("thread", Json::from(step.thread_id())),
                            ("span", Json::from(step.span().map(SpanId::sequence))),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
pub(crate) fn scope(trace: &Trace, span: &Span) -> Json {
    Json::object([
        ("id", Json::from(span.id().sequence())),
        ("parent", Json::from(span.parent().map(SpanId::sequence))),
        ("name", Json::from(span.name())),
        ("arguments", args(span.args())),
        ("begin_ns", Json::nanos(span.begin())),
        ("end_ns", Json::nanos(span.end())),
        ("interrupted", Json::from(span.interrupted())),
        ("thread", Json::from(span.thread_id())),
        ("context", context(trace, span.context_id())),
    ])
}
pub(crate) fn open_scope(trace: &Trace, span: &OpenSpan) -> Json {
    Json::object([
        ("id", Json::from(span.id().sequence())),
        ("parent", Json::from(span.parent().map(SpanId::sequence))),
        ("name", Json::from(span.name())),
        ("arguments", args(span.args())),
        ("begin_ns", Json::nanos(span.begin())),
        ("end_ns", Json::Null),
        ("interrupted", Json::from(span.interrupted())),
        ("thread", Json::from(span.thread_id())),
        ("context", context(trace, span.context_id())),
    ])
}
pub(crate) fn resource(trace: &Trace, resource: &Resource) -> Json {
    Json::object([
        (
            "creation",
            resource.creation().map_or(Json::Null, |event| {
                crate::resources::observation(trace, event)
            }),
        ),
        ("lifetime", Json::from(resource.lifetime_id().sequence())),
        ("kind", Json::from(resource.kind_name())),
        ("kind_id", Json::from(resource.kind_id().get())),
        ("resource_id", Json::from(resource.resource_id().get())),
        ("created_ns", Json::nanos_or_null(resource.created())),
        ("dropped_ns", Json::nanos_or_null(resource.dropped())),
        ("latest_size_bytes", Json::from(resource.size())),
        (
            "observations",
            Json::Array(
                resource
                    .observations()
                    .iter()
                    .map(|event| crate::resources::observation(trace, event))
                    .collect(),
            ),
        ),
    ])
}
fn document(
    path: &Path,
    trace: &Trace,
    read: crate::check::Read,
    fields: impl IntoIterator<Item = (&'static str, Json)>,
) -> Json {
    Json::object(
        [
            ("footfall_version", Json::from(env!("CARGO_PKG_VERSION"))),
            ("capture_read", work_json::capture(path, trace, read)),
        ]
        .into_iter()
        .chain(fields),
    )
}
fn print(document: Json, json: bool) {
    if json {
        anstream::println!("{document}");
    } else if let Json::Object(fields) = document {
        for (key, value) in fields {
            anstream::println!("{key}: {value}");
        }
    }
}

pub(crate) fn messages(
    path: &Path,
    filter: &Filter,
    name: Option<String>,
    sequence: Option<u64>,
    terms: &[work::Term],
    json: bool,
) -> crate::Result<()> {
    let (trace, read) = crate::read_trace_saying(path)?;
    if sequence.is_some_and(|seq| trace.message(seq).is_none()) {
        return Err("message sequence is absent from retained history".into());
    }
    let query = MessageQuery {
        sequence,
        filter: filter.query()?,
        name,
        args: terms
            .iter()
            .map(|term| (term.key.clone(), term.value.clone()))
            .collect(),
    };
    let result = trace.select_messages(&query, filter.page())?;
    print(
        document(
            path,
            &trace,
            read,
            [
                ("evidence", evidence(&result.evidence)),
                ("total", Json::from(result.total as u64)),
                ("next", Json::from(result.next)),
                (
                    "messages",
                    Json::Array(
                        result
                            .items
                            .iter()
                            .map(|item| message(&trace, item))
                            .collect(),
                    ),
                ),
            ],
        ),
        json,
    );
    Ok(())
}
pub(crate) fn flows(
    path: &Path,
    filter: &Filter,
    id: Option<u64>,
    json: bool,
) -> crate::Result<()> {
    let (trace, read) = crate::read_trace_saying(path)?;
    if id.is_some_and(|id| trace.flow(footfall::FlowId::new(id)).is_none()) {
        return Err("flow ID is absent from retained history".into());
    }
    let result = trace.select_flows(
        &FlowQuery {
            filter: filter.query()?,
            id: id.map(footfall::FlowId::new),
        },
        filter.page(),
    )?;
    print(
        document(
            path,
            &trace,
            read,
            [
                ("evidence", evidence(&result.evidence)),
                ("total", Json::from(result.total as u64)),
                ("next", Json::from(result.next)),
                (
                    "flows",
                    Json::Array(result.items.iter().map(|item| flow(&trace, item)).collect()),
                ),
            ],
        ),
        json,
    );
    Ok(())
}
pub(crate) fn work(
    path: &Path,
    selector: &work::Args,
    selected_id: Option<u64>,
    detail: bool,
    json: bool,
) -> crate::Result<()> {
    let (trace, read) = crate::read_trace_saying(path)?;
    let mut units = selector.select(&trace)?;
    if let Some(id) = selected_id {
        units.units.retain(|unit| unit.id == id);
        if units.units.is_empty() {
            let scope = trace.scope_parent(SpanId::new(id)).is_some();
            let marker = [
                footfall::MarkerKind::Frame,
                footfall::MarkerKind::Pass,
                footfall::MarkerKind::Transaction,
            ]
            .into_iter()
            .any(|kind| {
                trace
                    .marks(kind)
                    .iter()
                    .any(|mark| mark.id().sequence() == id)
            });
            let detail = if scope || marker {
                "exists, but does not match the selected --kind/--scope or context/argument/label predicates"
            } else {
                "is absent from retained completed work"
            };
            return Err(format!("work ID {id} {detail}").into());
        }
    }
    if units.units.len() != 1 {
        return Err(format!("work inspection requires exactly one completed unit; matched {}; narrow with --id or --arg/--context/--label, or inspect a scope ID", units.units.len()).into());
    }
    let unit = units.units[0];
    let id = unit.span.map_or_else(
        || WorkId::Marker(footfall::analysis::FrameId::new(unit.id)),
        |span| WorkId::Scope(span.id()),
    );
    inspection(path, &trace, read, id, detail, json)
}
pub(crate) fn span(path: &Path, id: u64, json: bool) -> crate::Result<()> {
    let (trace, read) = crate::read_trace_saying(path)?;
    inspection(
        path,
        &trace,
        read,
        WorkId::Scope(SpanId::new(id)),
        true,
        json,
    )
}
fn inspection(
    path: &Path,
    trace: &Trace,
    read: crate::check::Read,
    id: WorkId,
    detail: bool,
    json: bool,
) -> crate::Result<()> {
    let result = trace.inspect_work(id)?;
    let root = match id {
        WorkId::Scope(id) => trace
            .span(id)
            .map(|span| scope(trace, span))
            .or_else(|| {
                result
                    .open_scopes
                    .iter()
                    .find(|span| span.id() == id)
                    .map(|span| open_scope(trace, span))
            })
            .unwrap_or(Json::Null),
        WorkId::Marker(_) => Json::Null,
    };
    let mut fields = vec![
        ("root_scope", root),
        (
            "id",
            Json::from(match id {
                WorkId::Scope(id) => id.sequence(),
                WorkId::Marker(id) => id.sequence(),
            }),
        ),
        (
            "membership",
            Json::from(match result.membership {
                Membership::CapturedAncestry => "captured_ancestry",
                Membership::IntervalActivity => "interval_activity",
            }),
        ),
        ("open", Json::from(result.open)),
        ("start_ns", Json::nanos(result.range.start())),
        ("end_ns", Json::nanos(result.range.end())),
        (
            "evidence",
            Json::Array(
                result
                    .evidence
                    .iter()
                    .map(|(signal, item)| {
                        Json::object([
                            ("signal", Json::from(format!("{signal:?}"))),
                            ("evidence", evidence(item)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "counts",
            Json::object([
                ("scopes", Json::from(result.scopes.len() as u64)),
                ("open_scopes", Json::from(result.open_scopes.len() as u64)),
                ("messages", Json::from(result.messages.len() as u64)),
                ("flows", Json::from(result.flows.len() as u64)),
                ("checkpoints", Json::from(result.checkpoints.len() as u64)),
                ("resources", Json::from(result.resources.len() as u64)),
            ]),
        ),
        (
            "scope_ids",
            Json::Array(
                result
                    .scopes
                    .iter()
                    .map(|span| Json::from(span.id().sequence()))
                    .collect(),
            ),
        ),
        (
            "message_sequences",
            Json::Array(
                result
                    .messages
                    .iter()
                    .map(|message| Json::from(message.sequence()))
                    .collect(),
            ),
        ),
        (
            "flow_ids",
            Json::Array(
                result
                    .flows
                    .iter()
                    .map(|found| Json::from(found.flow.id().get()))
                    .collect(),
            ),
        ),
        (
            "checkpoint_sequences",
            Json::Array(
                result
                    .checkpoints
                    .iter()
                    .map(|point| Json::from(point.sequence()))
                    .collect(),
            ),
        ),
        (
            "resource_lifetimes",
            Json::Array(
                result
                    .resources
                    .iter()
                    .map(|resource| Json::from(resource.lifetime_id().sequence()))
                    .collect(),
            ),
        ),
    ];
    if detail {
        fields.extend([
            (
                "scopes",
                Json::Array(
                    result
                        .scopes
                        .iter()
                        .map(|span| scope(trace, span))
                        .collect(),
                ),
            ),
            (
                "open_scopes",
                Json::Array(
                    result
                        .open_scopes
                        .iter()
                        .map(|span| open_scope(trace, span))
                        .collect(),
                ),
            ),
            (
                "messages",
                Json::Array(
                    result
                        .messages
                        .iter()
                        .map(|item| message(trace, item))
                        .collect(),
                ),
            ),
            (
                "flows",
                Json::Array(result.flows.iter().map(|item| flow(trace, item)).collect()),
            ),
            (
                "checkpoints",
                Json::Array(
                    result
                        .checkpoints
                        .iter()
                        .map(|point| crate::checkpoints::point(point))
                        .collect(),
                ),
            ),
            (
                "resources",
                Json::Array(
                    result
                        .resources
                        .iter()
                        .map(|item| resource(trace, item))
                        .collect(),
                ),
            ),
        ]);
    }
    fields.push((
        "resource_interval",
        interval(&trace.resource_interval(result.range, Some(ResourceGrouping::Kind))),
    ));
    print(document(path, trace, read, fields), json);
    Ok(())
}
fn activity(activity: &ResourceActivity) -> Json {
    let bytes = |value: footfall::analysis::ObservedBytes| {
        Json::object([
            ("observed_bytes", Json::from(value.observed)),
            ("exact", Json::from(value.exact)),
        ])
    };
    Json::object([
        ("created", bytes(activity.created)),
        ("released", bytes(activity.released)),
        ("grown", bytes(activity.grown)),
        ("shrunk", bytes(activity.shrunk)),
        ("baseline", bytes(activity.baseline)),
        ("ending", bytes(activity.ending)),
        ("peak", bytes(activity.peak)),
        (
            "survivors",
            Json::Array(
                activity
                    .survivors
                    .iter()
                    .map(|id| Json::from(id.sequence()))
                    .collect(),
            ),
        ),
    ])
}
pub(crate) fn interval(result: &ResourceInterval) -> Json {
    Json::object([
        (
            "attribution",
            Json::from(
                "global interval activity; groups describe creation provenance, not ownership or exclusive edit attribution",
            ),
        ),
        ("unit", Json::from("application_reported_bytes")),
        (
            "boundary_state",
            Json::from(
                "before events at each timestamp; activity [start,end), ordered by timestamp then sequence",
            ),
        ),
        ("start_ns", Json::nanos(result.range.start())),
        ("end_ns", Json::nanos(result.range.end())),
        ("evidence", evidence(&result.evidence)),
        ("activity", activity(&result.activity)),
        (
            "groups",
            Json::Array(
                result
                    .groups
                    .iter()
                    .map(|group| {
                        Json::object([
                            (
                                "key",
                                match &group.key {
                                    ResourceGroupKey::Kind(name) => {
                                        Json::object([("kind", Json::from(name.as_str()))])
                                    }
                                    ResourceGroupKey::Scope(id) => {
                                        Json::object([("scope", Json::from(id.sequence()))])
                                    }
                                    ResourceGroupKey::UnknownOrigin => {
                                        Json::object([("unknown_origin", Json::from(true))])
                                    }
                                },
                            ),
                            ("activity", activity(&group.activity)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum Group {
    Kind,
    CreationScope,
    CreationRoot,
}
pub(crate) fn accounting(
    path: &Path,
    start: u64,
    end: u64,
    group: Option<Group>,
    json: bool,
) -> crate::Result<()> {
    if start > end {
        return Err("--start-ns must not exceed --end-ns".into());
    }
    let (trace, read) = crate::read_trace_saying(path)?;
    let result = trace.resource_interval(
        TimeRange::new(Duration::from_nanos(start), Duration::from_nanos(end)),
        group.map(|group| match group {
            Group::Kind => ResourceGrouping::Kind,
            Group::CreationScope => ResourceGrouping::CreationScope,
            Group::CreationRoot => ResourceGrouping::CreationRoot,
        }),
    );
    print(
        document(
            path,
            &trace,
            read,
            [("resource_interval", interval(&result))],
        ),
        json,
    );
    Ok(())
}
