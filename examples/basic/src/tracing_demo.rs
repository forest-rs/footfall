// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Demonstrates forwarding the `tracing` ecosystem into Footfall.
//!
//! Spans become scopes, events become messages, and the resulting snapshot is
//! printed as Chrome Trace Event JSON.

use footfall_runtime::Runtime;
use footfall_tracing::FootfallLayer;
use tracing_subscriber::layer::SubscriberExt as _;

fn main() {
    let runtime = Runtime::builder()
        .process_name("tracing_demo")
        .session_name("tracing_demo")
        .counter_capacity(128)
        .event_capacity(256)
        .install_global()
        .expect("global runtime should install once");

    let subscriber = tracing_subscriber::registry().with(FootfallLayer::new());
    tracing::subscriber::set_global_default(subscriber)
        .expect("tracing subscriber should install once");

    for frame in 0_u64..3 {
        let span = tracing::info_span!("record_frame", frame);
        let _entered = span.enter();
        {
            let span = tracing::info_span!("tessellate", paths = 42_u64);
            let _entered = span.enter();
            tracing::debug!(bytes = 4096_u64, "scratch grew");
        }
        tracing::info!(frame, "frame recorded");
    }

    let snapshot = runtime.drain_snapshot();
    println!("{}", footfall_export::to_chrome_trace_json(&snapshot));
}
