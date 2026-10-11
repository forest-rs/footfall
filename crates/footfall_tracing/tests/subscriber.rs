// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end coverage: tracing spans and events land in a Footfall snapshot.

use footfall_runtime::{Observation, Runtime};
use footfall_tracing::FootfallLayer;
use tracing_subscriber::layer::SubscriberExt as _;

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

    // Scope descriptors allocate from the reserved tracing id range.
    let begin_desc_id = snapshot
        .events
        .iter()
        .find_map(|event| match &event.observation {
            Observation::ScopeBegin { desc_id, .. } => Some(desc_id.get()),
            _ => None,
        })
        .expect("scope begin should exist");
    assert!(begin_desc_id >= footfall_tracing::TRACING_DESC_ID_BASE);
}
