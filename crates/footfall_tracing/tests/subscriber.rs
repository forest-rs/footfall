// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end coverage: tracing spans and events land in a Footfall snapshot.

use footfall_core::{Context, ScopeDesc, scope};
use footfall_runtime::{Observation, Runtime};
use footfall_tracing::FootfallLayer;
use tracing_subscriber::layer::SubscriberExt as _;

static APP_SCOPE: ScopeDesc = ScopeDesc::new("record_pass");

#[test]
fn spans_and_events_reach_the_runtime() {
    let runtime = Runtime::named("footfall_tracing_test", 64, 64)
        .install_global()
        .expect("runtime should install once for this test binary");

    let subscriber = tracing_subscriber::registry().with(FootfallLayer::default());
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("record_pass", items = 3_u64);
        let _entered = span.enter();
        tracing::info!(bytes = 4096_u64, status = "warm", "uploaded atlas");
    });

    let snapshot = runtime.drain_snapshot();
    let begin = snapshot
        .events
        .iter()
        .find_map(|event| match &event.observation {
            Observation::ScopeBegin { name, args, .. } if *name == "record_pass" => Some(args),
            _ => None,
        })
        .expect("span should record a scope begin");
    assert_eq!(
        begin.get("items").and_then(|value| value.as_unsigned()),
        Some(3)
    );
    assert!(
        snapshot
            .events
            .iter()
            .any(|event| matches!(&event.observation, Observation::ScopeEnd { .. })),
        "span exit should record a scope end"
    );

    let message = snapshot
        .events
        .iter()
        .find_map(|event| match &event.observation {
            Observation::Message { text, args, .. } => Some((text, args)),
            _ => None,
        })
        .expect("event should record a message");
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

    // Bridge descriptors are distinct from application descriptors, even
    // with the same name, and from those of another layer.
    let second_layer = tracing_subscriber::registry().with(FootfallLayer::default());
    tracing::subscriber::with_default(second_layer, || {
        let _app = scope(&APP_SCOPE, Context::root());
        let _entered = tracing::info_span!("record_pass").entered();
    });
    let snapshot_ids = snapshot.events.iter().filter_map(scope_begin_desc_id);
    let mut ids: Vec<_> = snapshot_ids
        .chain(
            runtime
                .snapshot()
                .events
                .iter()
                .filter_map(scope_begin_desc_id),
        )
        .collect();
    assert_eq!(ids.len(), 3, "two bridged spans and one application scope");
    assert!(ids.contains(&APP_SCOPE.id()));
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 3, "no two descriptors share an id");
}

fn scope_begin_desc_id(
    event: &footfall_runtime::TimedObservation,
) -> Option<footfall_core::ScopeDescId> {
    match &event.observation {
        Observation::ScopeBegin { desc_id, .. } => Some(*desc_id),
        _ => None,
    }
}
