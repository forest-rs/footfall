// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The demo renderer, shown live in Tracy while Footfall captures it too.
//!
//! Start the Tracy 0.14 profiler and connect to this program: scopes appear as
//! zones, counters as plots, and frame markers as frames. A fan-out backend
//! sends every emission to both Tracy and the runtime.
//!
//! ```sh
//! cargo run -p footfall_examples --bin tracy_live
//! ```

use anstream::println;
use clap::Parser;
use footfall::analysis::{GroupBy, Trace};
use footfall::runtime::Runtime;
use footfall::session::OwnedSnapshot;
use footfall::tracy::TracyBackend;
use footfall_examples::demo::Renderer;
use footfall_examples::report;
use footfall_examples::style::{DIM, HEADING, HINT};
use footfall_fanout::Fanout;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Run the demo renderer with Tracy and the Footfall runtime both attached.
#[derive(Debug, Parser)]
struct Args {
    /// Frames to render; Ctrl-C stops sooner.
    #[arg(long, default_value_t = 3_600)]
    frames: u64,
    /// Frames per second to aim for.
    #[arg(long, default_value_t = 60)]
    fps: u32,
}

fn main() {
    let args = Args::parse();
    let runtime: &'static Runtime = Box::leak(Box::new(
        Runtime::builder().process_name("tracy_live").build(),
    ));
    let tracy: &'static TracyBackend = Box::leak(Box::new(TracyBackend::new()));
    Fanout::new(runtime, tracy)
        .install_global()
        .expect("no other backend is installed");

    println!(
        "{HEADING}footfall tracy_live{HEADING:#} {DIM}· {} frames at {} fps{DIM:#}",
        args.frames, args.fps
    );
    println!(
        "Open the {HINT}Tracy 0.14{HINT:#} profiler and connect to {HINT}tracy_live{HINT:#}; \
         other Tracy versions cannot connect. Ctrl-C stops early.\n"
    );

    let stop = Arc::new(AtomicBool::new(false));
    std::thread::spawn({
        let stop = Arc::clone(&stop);
        move || {
            let _ = wait_for_ctrl_c();
            stop.store(true, Ordering::Relaxed);
        }
    });
    Renderer::new(4).run(args.fps, Some(args.frames), &stop);

    // The runtime saw the same emissions Tracy did.
    let snapshot = OwnedSnapshot::from(runtime.snapshot());
    let trace = Trace::from_snapshot(&snapshot);
    report::frames(trace.frames(), 1_000_000 / u64::from(args.fps.max(1)));
    println!();
    report::spans(&trace.spans().group_by(GroupBy::Name).table(), 8);
}

/// Blocks this thread until Ctrl-C.
fn wait_for_ctrl_c() -> std::io::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(tokio::signal::ctrl_c())
}
