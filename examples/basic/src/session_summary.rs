// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use footfall_session::{
    OwnedArgEntry, OwnedContextEntry, OwnedLabelValue, OwnedObservation, OwnedSnapshot,
};
use std::collections::BTreeMap;

#[derive(Debug)]
pub(crate) struct SessionSummary {
    pub(crate) process_name: String,
    pub(crate) session_name: Option<String>,
    pub(crate) context_rows: usize,
    pub(crate) counter_rows: usize,
    pub(crate) histogram_rows: usize,
    pub(crate) event_rows: usize,
    pub(crate) caches: Vec<CacheSummary>,
    pub(crate) resources: Vec<ResourceSummary>,
    pub(crate) histograms: Vec<HistogramSummary>,
    pub(crate) messages: Vec<MessageSummary>,
    pub(crate) scopes: Vec<ScopeSummary>,
    pub(crate) flows: Vec<FlowSummary>,
}

#[derive(Debug)]
pub(crate) struct CacheSummary {
    pub(crate) cache: String,
    pub(crate) current_bytes: u64,
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) evictions: u64,
}

#[derive(Debug)]
pub(crate) struct ResourceSummary {
    pub(crate) kind: String,
    pub(crate) resource_id: u64,
    pub(crate) owner: String,
    pub(crate) creates: u64,
    pub(crate) updates: u64,
    pub(crate) drops: u64,
    pub(crate) last_size: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct HistogramSummary {
    pub(crate) name: String,
    pub(crate) unit: String,
    pub(crate) context: String,
    pub(crate) count: u64,
    pub(crate) min: u64,
    pub(crate) max: u64,
}

#[derive(Debug)]
pub(crate) struct MessageSummary {
    pub(crate) name: String,
    pub(crate) text: String,
    pub(crate) context: String,
    pub(crate) args: String,
}

#[derive(Debug)]
pub(crate) struct ScopeSummary {
    pub(crate) name: String,
    pub(crate) context: String,
    pub(crate) args: String,
}

#[derive(Debug)]
pub(crate) struct FlowSummary {
    pub(crate) flow_id: u64,
    pub(crate) phase: String,
    pub(crate) context: String,
}

pub(crate) fn summarize(snapshot: &OwnedSnapshot) -> SessionSummary {
    let process_name = snapshot.metadata().process_name().to_owned();
    let session_name = snapshot.metadata().session_name().map(String::from);
    let context_rows = snapshot.contexts().len();
    let counter_rows = snapshot.counters().len();
    let histogram_rows = snapshot.histograms().len();
    let event_rows = snapshot.events().len();
    let mut contexts = BTreeMap::<u64, Vec<OwnedContextEntry>>::new();
    let mut caches = BTreeMap::<String, CacheSummary>::new();
    let mut resources = BTreeMap::<(String, u64, String), ResourceSummary>::new();
    let mut histograms = Vec::<HistogramSummary>::new();
    let mut messages = Vec::<MessageSummary>::new();
    let mut scopes = Vec::<ScopeSummary>::new();
    let mut flows = Vec::<FlowSummary>::new();

    for row in snapshot.contexts() {
        contexts.insert(row.id().get(), row.entries().to_vec());
    }

    for row in snapshot.histograms() {
        let context = contexts
            .get(&row.context_id().get())
            .map(|entries| format_context(entries))
            .unwrap_or_else(|| String::from("unknown"));
        histograms.push(HistogramSummary {
            name: row.name().to_owned(),
            unit: row.unit().as_str().to_owned(),
            context,
            count: row.count(),
            min: row.min(),
            max: row.max(),
        });
    }

    for row in snapshot.counters() {
        let Some(entries) = contexts.get(&row.context_id().get()) else {
            continue;
        };
        let Some(cache_name) = context_cache_name(entries) else {
            continue;
        };
        let cache = caches
            .entry(cache_name.clone())
            .or_insert_with(|| CacheSummary {
                cache: cache_name.clone(),
                current_bytes: 0,
                hits: 0,
                misses: 0,
                evictions: 0,
            });
        match row.name() {
            "cache_current_bytes" => cache.current_bytes = row.value(),
            "cache_hits" => cache.hits = cache.hits.saturating_add(row.value()),
            "cache_misses" => cache.misses = cache.misses.saturating_add(row.value()),
            "cache_evictions" => {
                cache.evictions = cache.evictions.saturating_add(row.value());
            }
            _ => {}
        }
    }

    for row in snapshot.events() {
        match row.observation() {
            OwnedObservation::ResourceCreate {
                kind_name,
                resource_id,
                context_id,
                size,
                ..
            } => {
                let owner = contexts
                    .get(&context_id.get())
                    .map(|entries| format_context(entries))
                    .unwrap_or_else(|| String::from("unknown"));
                let resource = resources
                    .entry((kind_name.clone(), resource_id.get(), owner.clone()))
                    .or_insert_with(|| ResourceSummary {
                        kind: kind_name.clone(),
                        resource_id: resource_id.get(),
                        owner,
                        creates: 0,
                        updates: 0,
                        drops: 0,
                        last_size: None,
                    });
                resource.creates = resource.creates.saturating_add(1);
                resource.last_size = Some(*size);
            }
            OwnedObservation::ResourceUpdate {
                kind_name,
                resource_id,
                context_id,
                size,
                ..
            } => {
                let owner = contexts
                    .get(&context_id.get())
                    .map(|entries| format_context(entries))
                    .unwrap_or_else(|| String::from("unknown"));
                let resource = resources
                    .entry((kind_name.clone(), resource_id.get(), owner.clone()))
                    .or_insert_with(|| ResourceSummary {
                        kind: kind_name.clone(),
                        resource_id: resource_id.get(),
                        owner,
                        creates: 0,
                        updates: 0,
                        drops: 0,
                        last_size: None,
                    });
                resource.updates = resource.updates.saturating_add(1);
                resource.last_size = Some(*size);
            }
            OwnedObservation::ResourceDrop {
                kind_name,
                resource_id,
                context_id,
                ..
            } => {
                let owner = contexts
                    .get(&context_id.get())
                    .map(|entries| format_context(entries))
                    .unwrap_or_else(|| String::from("unknown"));
                let resource = resources
                    .entry((kind_name.clone(), resource_id.get(), owner.clone()))
                    .or_insert_with(|| ResourceSummary {
                        kind: kind_name.clone(),
                        resource_id: resource_id.get(),
                        owner,
                        creates: 0,
                        updates: 0,
                        drops: 0,
                        last_size: None,
                    });
                resource.drops = resource.drops.saturating_add(1);
            }
            OwnedObservation::ScopeBegin {
                name,
                context_id,
                args,
                ..
            } => {
                let context = contexts
                    .get(&context_id.get())
                    .map(|entries| format_context(entries))
                    .unwrap_or_else(|| String::from("unknown"));
                scopes.push(ScopeSummary {
                    name: name.clone(),
                    context,
                    args: format_args(args),
                });
            }
            OwnedObservation::Flow {
                flow_id,
                phase,
                context_id,
            } => {
                let context = contexts
                    .get(&context_id.get())
                    .map(|entries| format_context(entries))
                    .unwrap_or_else(|| String::from("unknown"));
                flows.push(FlowSummary {
                    flow_id: flow_id.get(),
                    phase: format!("{phase:?}"),
                    context,
                });
            }
            OwnedObservation::Message {
                name,
                context_id,
                text,
                args,
                ..
            } => {
                let context = contexts
                    .get(&context_id.get())
                    .map(|entries| format_context(entries))
                    .unwrap_or_else(|| String::from("unknown"));
                messages.push(MessageSummary {
                    name: name.clone(),
                    text: text.clone(),
                    context,
                    args: format_args(args),
                });
            }
            OwnedObservation::ScopeEnd { .. } | OwnedObservation::FrameMarker { .. } => {}
            _ => {}
        }
    }

    SessionSummary {
        process_name,
        session_name,
        context_rows,
        counter_rows,
        histogram_rows,
        event_rows,
        caches: caches.into_values().collect(),
        resources: resources.into_values().collect(),
        histograms,
        messages,
        scopes,
        flows,
    }
}

pub(crate) fn print_summary(summary: &SessionSummary) {
    println!("process: {}", summary.process_name);
    if let Some(session_name) = &summary.session_name {
        println!("session: {session_name}");
    }
    println!(
        "rows: {} contexts, {} counters, {} histograms, {} events",
        summary.context_rows, summary.counter_rows, summary.histogram_rows, summary.event_rows
    );
    if !summary.histograms.is_empty() {
        println!("histograms:");
        for histogram in &summary.histograms {
            println!(
                "  {} unit={} count={} min={} max={} context=[{}]",
                histogram.name,
                histogram.unit,
                histogram.count,
                histogram.min,
                histogram.max,
                histogram.context
            );
        }
    }
    if !summary.caches.is_empty() {
        println!("caches:");
        for cache in &summary.caches {
            println!(
                "  {} current={} hits={} misses={} evictions={}",
                cache.cache, cache.current_bytes, cache.hits, cache.misses, cache.evictions
            );
        }
    }
    if !summary.resources.is_empty() {
        println!("resources:");
        for resource in &summary.resources {
            println!(
                "  {}#{} owner=[{}] creates={} updates={} drops={} last_size={}",
                resource.kind,
                resource.resource_id,
                resource.owner,
                resource.creates,
                resource.updates,
                resource.drops,
                resource
                    .last_size
                    .map(|size| size.to_string())
                    .unwrap_or_else(|| String::from("-"))
            );
        }
    }
    if !summary.scopes.is_empty() {
        println!("scopes:");
        for scope in summary.scopes.iter().take(8) {
            println!(
                "  {} context=[{}] args={}",
                scope.name,
                scope.context,
                empty_dash(&scope.args)
            );
        }
    }
    if !summary.messages.is_empty() {
        println!("messages:");
        for message in summary.messages.iter().take(8) {
            println!(
                "  {}: {:?} context=[{}] args={}",
                message.name,
                message.text,
                message.context,
                empty_dash(&message.args)
            );
        }
    }
    if !summary.flows.is_empty() {
        println!("flows:");
        for flow in &summary.flows {
            println!(
                "  flow#{} phase={} context=[{}]",
                flow.flow_id, flow.phase, flow.context
            );
        }
    }
}

fn context_cache_name(entries: &[OwnedContextEntry]) -> Option<String> {
    entries.iter().find_map(|entry| {
        // Remote consumers see label keys by name.
        if entry.key() == "cache" {
            match entry.value() {
                OwnedLabelValue::String(value) => Some(value.clone()),
                _ => None,
            }
        } else {
            None
        }
    })
}

fn format_context(entries: &[OwnedContextEntry]) -> String {
    entries
        .iter()
        .map(|entry| {
            format!(
                "{}={}",
                entry.key(),
                match entry.value() {
                    OwnedLabelValue::Unsigned(value) => value.to_string(),
                    OwnedLabelValue::Signed(value) => value.to_string(),
                    OwnedLabelValue::Bool(value) => value.to_string(),
                    OwnedLabelValue::String(value) => value.clone(),
                    _ => String::from("?"),
                }
            )
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

fn format_args(args: &[OwnedArgEntry]) -> String {
    args.iter()
        .map(|entry| format!("{}={}", entry.key(), format_label_value(entry.value())))
        .collect::<Vec<_>>()
        .join(" / ")
}

fn format_label_value(value: &OwnedLabelValue) -> String {
    match value {
        OwnedLabelValue::Unsigned(value) => value.to_string(),
        OwnedLabelValue::Signed(value) => value.to_string(),
        OwnedLabelValue::Bool(value) => value.to_string(),
        OwnedLabelValue::String(value) => value.clone(),
        _ => String::from("?"),
    }
}

fn empty_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}
