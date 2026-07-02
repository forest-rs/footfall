// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Demonstrates forwarding the `tracing` ecosystem into Spoor.
//!
//! Spans become scopes, events become messages, and the resulting snapshot is
//! printed as Chrome Trace Event JSON.

use spoor_runtime::Runtime;
use spoor_tracing::SpoorLayer;
use tracing_subscriber::layer::SubscriberExt as _;

fn main() {
    let runtime = Runtime::named("tracing_demo", 128, 256)
        .with_session_name("tracing_demo")
        .install_global()
        .expect("global runtime should install once");

    let subscriber = tracing_subscriber::registry().with(SpoorLayer::new());
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
    println!("{}", spoor_export::to_chrome_trace_json(&snapshot));
}
