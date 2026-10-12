// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The smallest useful tool: find a running session, subscribe to it, and
//! print how each batch of frames went.
//!
//! ```sh
//! cargo run -p footfall_examples --bin demo_app      # in one terminal
//! cargo run -p footfall_examples --bin attach        # in another
//! ```

use anstream::{eprintln, println};
use clap::Parser;
use footfall::analysis::{Continuity, FrameTracker, SpanTracker};
use footfall::wtransport::{ClientError, SessionClient};
use footfall_examples::format;
use footfall_examples::report::FrameStats;
use footfall_examples::style::{BAD, DIM, GOOD, HEADING, NAME, WARN};
use std::time::Duration;

/// Follow a running Footfall session and print each window's frames.
#[derive(Debug, Parser)]
struct Args {
    /// The session to follow; needed only when several are running.
    session: Option<String>,
    /// How often to ask for new data, in milliseconds.
    #[arg(long, default_value_t = 1000)]
    interval_ms: u64,
    /// Frame budget in milliseconds; longer frames are slow.
    #[arg(long, default_value_t = 16.7)]
    budget_ms: f64,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if let Err(error) = follow(&args).await {
        eprintln!("{BAD}{error}{BAD:#}");
        std::process::exit(1);
    }
}

async fn follow(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    // Ask every Footfall producer on this machine what it serves.
    let discovery = SessionClient::discover_local(Duration::from_secs(1)).await?;
    let session = match (&args.session, discovery.sessions()) {
        (Some(id), _) => discovery.session(id)?,
        (None, [only]) => only,
        (None, []) => {
            return Err("no Footfall sessions are running; start one with \
                        `cargo run -p footfall_examples --bin demo_app`"
                .into());
        }
        (None, _) => {
            return Err(format!(
                "several sessions are running; name one: {}",
                ids(&discovery)
            )
            .into());
        }
    };
    let about = session.announcement();
    println!(
        "{HEADING}following{HEADING:#} {NAME}{}{NAME:#} {DIM}· {} (pid {}){DIM:#}",
        session.id(),
        about.process_name(),
        about.process_id()
    );

    // Subscribe: the first window holds everything retained, later ones
    // only what is new. The trackers carry frames and scopes that span
    // windows from one to the next.
    let mut subscription = session
        .client()
        .subscribe(session.id(), None, Duration::from_millis(args.interval_ms))
        .await?;
    let mut frames = FrameTracker::new();
    let mut spans = SpanTracker::new();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a budget in ms"
    )]
    let budget = (args.budget_ms * 1000.0) as u64;
    loop {
        let window = match subscription.next().await {
            Ok(window) => window,
            Err(ClientError::Closed) => {
                println!("{DIM}the producer stopped{DIM:#}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let completed = frames.push(&window);
        let scopes = spans.push(&window);
        match frames.continuity() {
            Continuity::Gap => println!("{WARN}events were lost between windows{WARN:#}"),
            Continuity::Restarted => println!("{WARN}the producer restarted{WARN:#}"),
            Continuity::First | Continuity::Continuous => {}
        }

        let Some(stats) = FrameStats::of(&completed, budget) else {
            continue;
        };
        // Where the most time went, per frame, summed over every thread.
        let frame_count = stats.count.max(1);
        let busiest = scopes
            .by_name()
            .into_iter()
            .max_by_key(|(_, stats)| stats.self_micros())
            .map(|(name, stats)| (name, stats.self_micros() / frame_count));
        let slow = if stats.slow == 0 {
            format!("{GOOD}none slow{GOOD:#}")
        } else {
            format!("{BAD}{} slow{BAD:#}", stats.slow)
        };
        println!(
            "{:>4} frames  mean {:>8}  max {:>8}  {slow}  {DIM}most time in {}  ·  {} on the wire{DIM:#}",
            stats.count,
            format::micros(stats.mean),
            format::micros(stats.max),
            busiest.map_or_else(String::new, |(name, micros)| format!(
                "{name} ({} per frame)",
                format::micros(micros)
            )),
            format::bytes(subscription.last_window_bytes() as u64),
        );
    }
}

fn ids(discovery: &footfall::wtransport::LocalDiscovery) -> String {
    let ids: Vec<_> = discovery
        .sessions()
        .iter()
        .map(|session| session.id())
        .collect();
    ids.join(", ")
}
