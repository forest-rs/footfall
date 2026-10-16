// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Existing `tracing` instrumentation, captured by Footfall unchanged.
//!
//! The `tracing` layer turns spans into scopes, with their numeric and
//! boolean fields as arguments and as a context their children inherit, and
//! events into messages.
//!
//! ```sh
//! cargo run -p footfall_examples --bin tracing_bridge
//! ```

use anstream::println;
use footfall::analysis::{GroupBy, Trace};
use footfall::runtime::Runtime;
use footfall::session::OwnedSnapshot;
use footfall::tracing_layer::FootfallLayer;
use footfall_examples::report;
use footfall_examples::style::{DIM, HEADING, HINT};
use std::time::Duration;
use tracing::{debug, info, info_span};
use tracing_subscriber::layer::SubscriberExt as _;

const TRACE_PATH: &str = "footfall-tracing.pftrace";

fn main() -> std::io::Result<()> {
    let runtime = Runtime::builder()
        .process_name("tracing_bridge")
        .install_global()
        .expect("no other backend is installed");
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(FootfallLayer::new()),
    )
    .expect("no other tracing subscriber is installed");

    // Ordinary `tracing` code, with nothing Footfall-specific in it.
    for frame in 0..5_u64 {
        let _frame = info_span!("record_frame", frame).entered();
        for paths in [12_u64, 42] {
            let _tessellate = info_span!("tessellate", paths).entered();
            std::thread::sleep(Duration::from_micros(200 * paths));
            debug!(bytes = paths * 128, "scratch grew");
        }
        info!(frame, "frame recorded");
    }

    let snapshot = OwnedSnapshot::from(runtime.snapshot());
    let trace = Trace::from_snapshot(&snapshot);
    println!(
        "{HEADING}footfall tracing_bridge{HEADING:#} {DIM}· five frames of plain `tracing` spans{DIM:#}\n"
    );
    report::spans(
        "Where the time went",
        &trace.spans().group_by(GroupBy::Name).table(),
        10,
    );
    println!();
    // Span fields become context labels, so time can be split by them.
    report::spans_by_label(&trace, "paths");
    println!();
    let messages = snapshot
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.observation(),
                footfall::session::OwnedObservation::Message { .. }
            )
        })
        .count();
    println!("{messages} `tracing` events became Footfall messages.");

    footfall::export::write_perfetto_trace_file(&snapshot, TRACE_PATH)?;
    println!(
        "\nWrote {HINT}{TRACE_PATH}{HINT:#}; open it at {HINT}https://ui.perfetto.dev{HINT:#}."
    );
    Ok(())
}
