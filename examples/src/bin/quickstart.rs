// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The quick start: instrument a loop, capture it, summarize it, and write a
//! trace to open in Perfetto.
//!
//! ```sh
//! cargo run -p footfall_examples --bin quickstart
//! ```

use anstream::println;
use footfall::analysis::{GroupBy, Trace};
use footfall::runtime::Runtime;
use footfall::session::OwnedSnapshot;
use footfall::{
    ContextHandle, CounterDesc, LabelKey, RegisteredContext, ScopeDesc, context, counter_add,
    frame_begin, frame_end,
};
use footfall_examples::report;
use footfall_examples::style::{DIM, HEADING, HINT};
use std::time::Duration;

// Descriptors and label keys are `static` items, declared once.
static PASS: LabelKey = LabelKey::new("pass");
static UPDATE: ScopeDesc = ScopeDesc::new("update");
static RENDER: ScopeDesc = ScopeDesc::new("render");
static UPLOAD_BYTES: CounterDesc = CounterDesc::bytes("upload_bytes");
static DRAW_CALLS: CounterDesc = CounterDesc::count("draw_calls");

/// Where the trace is written.
const TRACE_PATH: &str = "footfall-quickstart.json";

fn main() -> std::io::Result<()> {
    // The application installs a backend once, before emitting.
    let runtime = Runtime::builder()
        .process_name("quickstart")
        .install_global()
        .expect("no other backend is installed");

    // Register a context once per long-lived owner, then emit with its
    // handle: here, two render passes.
    let opaque = RegisteredContext::new(&context!(PASS = "opaque"));
    let transparent = RegisteredContext::new(&context!(PASS = "transparent"));

    for frame in 0..30_u64 {
        frame_begin(ContextHandle::ROOT);
        {
            footfall::scope!(&UPDATE, ContextHandle::ROOT);
            std::thread::sleep(Duration::from_micros(300));
        }
        for (pass, cost) in [(&opaque, 900), (&transparent, 400 + 40 * (frame % 10))] {
            footfall::scope!(&RENDER, pass.handle());
            counter_add(&DRAW_CALLS, pass.handle(), 12);
            counter_add(&UPLOAD_BYTES, pass.handle(), 4096);
            std::thread::sleep(Duration::from_micros(cost));
        }
        frame_end(ContextHandle::ROOT);
    }

    // Read back what was captured. Analysis works on the same snapshot form
    // a tool receives over the network or reads from a capture file.
    let snapshot = OwnedSnapshot::from(runtime.snapshot());
    println!("{HEADING}footfall quickstart{HEADING:#} {DIM}· 30 frames, two passes{DIM:#}\n");
    let trace = Trace::from_snapshot(&snapshot);
    report::frames(trace.frames(), 16_667);
    println!();
    report::spans(&trace.spans().group_by(GroupBy::Name).table(), 10);
    println!();
    report::spans_by_label(&trace, "pass");
    println!();
    report::counters(&snapshot, 10);
    println!();
    report::losses(&snapshot);

    // Export the same snapshot for a trace viewer.
    footfall::export::write_chrome_trace_file(&snapshot, TRACE_PATH)?;
    println!(
        "\nWrote {HINT}{TRACE_PATH}{HINT:#}; open it at {HINT}https://ui.perfetto.dev{HINT:#}."
    );
    Ok(())
}
