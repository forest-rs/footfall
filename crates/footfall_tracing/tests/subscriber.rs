// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end coverage: tracing spans and events land in a Footfall snapshot.

use footfall_core::{ContextHandle, ContextId, ScopeDesc, scope};
use footfall_runtime::{Observation, Runtime};
use footfall_tracing::FootfallLayer;
use tracing::Dispatch;
use tracing_subscriber::layer::SubscriberExt as _;

static APP_SCOPE: ScopeDesc = ScopeDesc::new("record_pass");

#[test]
fn spans_and_events_reach_the_runtime() {
    let runtime = Runtime::builder()
        .process_name("footfall_tracing_test")
        .counter_capacity(64)
        .event_capacity(64)
        .install_global()
        .expect("runtime should install once for this test binary");

    let dispatch = Dispatch::new(tracing_subscriber::registry().with(FootfallLayer::default()));
    tracing::dispatcher::with_default(&dispatch, || {
        let span = tracing::info_span!("record_pass", items = 3_u64);
        let _entered = span.enter();
        tracing::info!(bytes = 4096_u64, status = "warm", "uploaded atlas");
    });

    let snapshot = runtime.drain_snapshot();
    let (begin, span_context) = snapshot
        .events()
        .iter()
        .find_map(|event| match event.observation() {
            Observation::ScopeBegin {
                name,
                args,
                context_id,
                ..
            } if *name == "record_pass" => Some((args, *context_id)),
            _ => None,
        })
        .expect("span should record a scope begin");
    assert_eq!(
        begin.get("items").and_then(|value| value.as_unsigned()),
        Some(3)
    );
    assert_ne!(
        span_context,
        ContextId::new(0),
        "the span registers a context"
    );
    let labels = snapshot
        .context(span_context)
        .expect("the span context should resolve")
        .context();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels.as_slice()[0].key().name(), "items");
    assert!(snapshot.drops().is_empty(), "{:?}", snapshot.drops());
    assert!(
        snapshot
            .events()
            .iter()
            .any(|event| matches!(event.observation(), Observation::ScopeEnd { .. })),
        "span exit should record a scope end"
    );

    let message = snapshot
        .events()
        .iter()
        .find_map(|event| match event.observation() {
            Observation::Message {
                text,
                args,
                context_id,
                ..
            } => Some((text, args, *context_id)),
            _ => None,
        })
        .expect("event should record a message");
    assert_eq!(
        message.2, span_context,
        "events inside a span are attributed to its context"
    );
    assert_eq!(message.0, "uploaded atlas");
    assert_eq!(
        message.1.get("bytes").and_then(|value| value.as_unsigned()),
        Some(4096)
    );
    assert_eq!(
        message
            .1
            .get("level")
            .and_then(|value| value.as_static_str()),
        Some("info")
    );
    assert_eq!(message.1.get("status"), None);

    assert!(
        runtime.snapshot().context(span_context).is_none(),
        "closing the span releases its context, purged once the session ends"
    );

    // Bridge descriptors are distinct from application descriptors, even
    // with the same name, and from those of another layer. Snapshot ids are
    // only comparable within one session, so record all three in the next.
    let second_layer = Dispatch::new(tracing_subscriber::registry().with(FootfallLayer::default()));
    {
        let _app = scope(&APP_SCOPE, ContextHandle::ROOT);
        tracing::dispatcher::with_default(&dispatch, || {
            let _entered = tracing::info_span!("record_pass").entered();
        });
        tracing::dispatcher::with_default(&second_layer, || {
            let _entered = tracing::info_span!("record_pass").entered();
        });
    }
    let mut ids: Vec<_> = runtime
        .snapshot()
        .events()
        .iter()
        .filter_map(scope_begin_desc_id)
        .collect();
    assert_eq!(ids.len(), 3, "two bridged spans and one application scope");
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 3, "no two descriptors share an id");
    assert!(
        ids.iter().all(|id| (1..=3).contains(&id.get())),
        "snapshot descriptor ids are dense, not addresses: {ids:?}"
    );
}

fn scope_begin_desc_id(
    event: &footfall_runtime::TimedObservation,
) -> Option<footfall_core::ScopeDescId> {
    match event.observation() {
        Observation::ScopeBegin { desc_id, .. } => Some(*desc_id),
        _ => None,
    }
}
