// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A fake renderer that serves its capture to local tools.
//!
//! It renders frames on a pool of worker threads with realistic, varying
//! costs and the occasional hitch, streams assets with ordinary `tracing`
//! spans, and serves everything over local WebTransport until Ctrl-C. Attach
//! to it with `footfall top`, `footfall record`, or the `attach` example.
//!
//! ```sh
//! cargo run -p footfall_examples --bin demo_app
//! ```

use anstream::println;
use clap::Parser;
use footfall::analysis::Trace;
use footfall::runtime::Runtime;
use footfall::session::OwnedSnapshot;
use footfall::tracing_layer::FootfallLayer;
use footfall::wtransport::{LocalSessionServer, ServedSession};
use footfall_examples::demo::Renderer;
use footfall_examples::format;
use footfall_examples::report::FrameStats;
use footfall_examples::style::{BAD, DIM, GOOD, HEADING, HINT, NAME};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt as _;

/// A fake renderer that serves its Footfall capture to local tools.
#[derive(Debug, Parser)]
struct Args {
    /// Frames per second to aim for.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Stop after this many frames instead of running until Ctrl-C.
    #[arg(long)]
    frames: Option<u64>,
    /// Raster worker threads.
    #[arg(long, default_value_t = 4)]
    workers: usize,
    /// The id tools use to find this session.
    #[arg(long, default_value = "main")]
    session: String,
    /// Seconds between status lines; 0 turns them off.
    #[arg(long, default_value_t = 5)]
    status_every: u64,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Capture everything Footfall sees, and send this program's `tracing`
    // spans to it too. Only this program's: the session server's network
    // stack traces every packet, and capturing that would record the act of
    // serving the capture.
    let runtime = Runtime::builder()
        .process_name("demo_app")
        .session_name(&args.session)
        .install_global()
        .expect("no other backend is installed");
    let own_spans = Targets::new().with_target("demo_app", LevelFilter::TRACE);
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(FootfallLayer::new().with_filter(own_spans)),
    )
    .expect("no other tracing subscriber is installed");

    let server = LocalSessionServer::start(vec![ServedSession::new(&args.session, runtime)])
        .await
        .expect("the session server should start");
    print_banner(&args, &server, runtime);

    let stop = Arc::new(AtomicBool::new(false));
    let renderer = std::thread::Builder::new()
        .name(String::from("render"))
        .spawn({
            let stop = Arc::clone(&stop);
            let (fps, frames, workers) = (args.fps, args.frames, args.workers);
            move || Renderer::new(workers).run(fps, frames, &stop)
        })
        .expect("the render thread should start");
    let streamer = std::thread::Builder::new()
        .name(String::from("assets"))
        .spawn({
            let stop = Arc::clone(&stop);
            move || stream_assets(&stop)
        })
        .expect("the asset thread should start");

    let budget = 1_000_000 / u64::from(args.fps.max(1));
    let mut status = tokio::time::interval(Duration::from_secs(args.status_every.max(1)));
    status.tick().await;
    let mut trace = Trace::new();
    let mut cursor = runtime.cursor();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = status.tick(), if args.status_every > 0 => {
                // Read what is new since the last status line, the same way
                // a tool does over the network.
                let window = runtime.snapshot_since(cursor);
                cursor = window.next_cursor();
                let pushed = trace
                    .push(&OwnedSnapshot::from(window))
                    .expect("snapshots of one runtime");
                let completed: Vec<_> = pushed
                    .frames()
                    .iter()
                    .filter_map(|id| trace.frame(*id))
                    .filter(|frame| frame.kind() == footfall::MarkerKind::Frame)
                    .cloned()
                    .collect();
                print_status(&completed, budget);
                trace.forget_before(trace.observed().end());
            }
            () = wait_for(&renderer) => break,
        }
    }

    stop.store(true, Ordering::Relaxed);
    let _ = renderer.join();
    let _ = streamer.join();
    println!("{DIM}stopping: sending attached tools what they have not read yet{DIM:#}");
    server.shutdown(Duration::from_secs(2)).await;
}

fn print_banner(args: &Args, server: &LocalSessionServer, runtime: &Runtime) {
    println!(
        "{HEADING}footfall demo_app{HEADING:#} {DIM}· a fake renderer at {} fps on {} workers{DIM:#}",
        args.fps, args.workers
    );
    println!(
        "  {DIM}session{DIM:#}  {NAME}{}{NAME:#} {DIM}(pid {}, level {}){DIM:#}",
        args.session,
        std::process::id(),
        runtime.level().as_str()
    );
    println!("  {DIM}serving{DIM:#}  {}", server.url());
    println!();
    println!("  {DIM}watch it live{DIM:#}   {HINT}cargo run -p footfall_cli -- top{HINT:#}");
    println!(
        "  {DIM}follow frames{DIM:#}   {HINT}cargo run -p footfall_examples --bin attach{HINT:#}"
    );
    println!(
        "  {DIM}record it{DIM:#}       {HINT}cargo run -p footfall_cli -- record demo.ffcap{HINT:#}"
    );
    println!();
    println!("{DIM}press Ctrl-C to stop{DIM:#}");
}

fn print_status(frames: &[footfall::analysis::Frame], budget: u64) {
    let Some(stats) = FrameStats::of(frames, budget) else {
        return;
    };
    let slow = if stats.slow == 0 {
        format!("{GOOD}none slow{GOOD:#}")
    } else {
        format!("{BAD}{} slow{BAD:#}", stats.slow)
    };
    println!(
        "{DIM}{} frames · mean {} · max {} ·{DIM:#} {slow}",
        format::count(stats.count),
        format::micros(stats.mean),
        format::micros(stats.max),
    );
}

/// Streams an asset every couple of seconds, instrumented only with
/// `tracing`: the layer turns the span into a Footfall scope with its
/// fields as arguments, and the event into a message.
fn stream_assets(stop: &AtomicBool) {
    let mut index = 0_u64;
    while !stop.load(Ordering::Relaxed) {
        index += 1;
        let bytes = 512 * 1024 + (index * 7_919 % 1_024) * 1_024;
        {
            let span = tracing::info_span!("stream_asset", index, bytes);
            let _entered = span.enter();
            std::thread::sleep(Duration::from_millis(3 + index % 5));
            tracing::info!(bytes, "asset ready");
        }
        for _ in 0..20 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Resolves when the thread has finished, for `--frames`.
async fn wait_for(thread: &std::thread::JoinHandle<()>) {
    while !thread.is_finished() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
