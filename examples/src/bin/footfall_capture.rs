// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Record a running session to a capture file, and work with it later:
//! summarize it, or export it as one Chrome trace.
//!
//! ```sh
//! cargo run -p footfall_examples --bin footfall_capture -- record demo.ffcap
//! cargo run -p footfall_examples --bin footfall_capture -- info demo.ffcap
//! cargo run -p footfall_examples --bin footfall_capture -- export demo.ffcap
//! ```

use anstream::{eprintln, println};
use clap::{Parser, Subcommand};
use footfall::analysis::{Spans, frames};
use footfall::session::OwnedSnapshot;
use footfall::session::capture::{CaptureReader, CaptureWriter};
use footfall::wtransport::{ClientError, SessionClient};
use footfall_examples::style::{BAD, DIM, HEADING, HINT, NAME, WARN};
use footfall_examples::{format, report};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Record Footfall sessions to capture files, and summarize or export them.
#[derive(Debug, Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Subscribe to a running session and save every window until Ctrl-C or
    /// until the producer stops.
    Record {
        /// The capture file to write.
        file: PathBuf,
        /// The session to record; needed only when several are running.
        #[arg(long)]
        session: Option<String>,
        /// How often to ask for new data, in milliseconds.
        #[arg(long, default_value_t = 500)]
        interval_ms: u64,
    },
    /// Summarize a capture: frames, where the time went, counters, losses.
    Info {
        /// The capture file to read.
        file: PathBuf,
        /// Frame budget in milliseconds.
        #[arg(long, default_value_t = 16.7)]
        budget_ms: f64,
    },
    /// Join a capture's windows into one Chrome trace for Perfetto.
    Export {
        /// The capture file to read.
        file: PathBuf,
        /// Where to write the trace; defaults to the capture's name with `.json`.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() {
    let result = match Args::parse().command {
        Command::Record {
            file,
            session,
            interval_ms,
        } => {
            record(
                &file,
                session.as_deref(),
                Duration::from_millis(interval_ms),
            )
            .await
        }
        Command::Info { file, budget_ms } => info(&file, budget_ms),
        Command::Export { file, output } => {
            let output = output.unwrap_or_else(|| file.with_extension("json"));
            export(&file, &output)
        }
    };
    if let Err(error) = result {
        eprintln!("{BAD}{error}{BAD:#}");
        std::process::exit(1);
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn record(path: &Path, session: Option<&str>, interval: Duration) -> Result<()> {
    let discovery = SessionClient::discover_local(Duration::from_secs(1)).await?;
    let session = match (session, discovery.sessions()) {
        (Some(id), _) => discovery.session(id)?,
        (None, [only]) => only,
        (None, []) => return Err("no Footfall sessions are running".into()),
        (None, _) => return Err("several sessions are running; choose one with --session".into()),
    };
    let mut writer = CaptureWriter::new(BufWriter::new(File::create(path)?))?;
    let mut subscription = session
        .client()
        .subscribe(session.id(), None, interval)
        .await?;
    println!(
        "{HEADING}recording{HEADING:#} {NAME}{}{NAME:#} {DIM}({}) to {}; Ctrl-C to stop{DIM:#}",
        session.id(),
        session.announcement().process_name(),
        path.display()
    );

    let (mut windows, mut events, mut bytes) = (0_u64, 0_u64, 0_u64);
    let stopped = loop {
        let window = tokio::select! {
            _ = tokio::signal::ctrl_c() => break "stopped",
            window = subscription.next() => window,
        };
        let window = match window {
            Ok(window) => window,
            Err(ClientError::Closed) => break "the producer stopped",
            Err(error) => return Err(error.into()),
        };
        // Each window is saved whole, so the file can be read back the way
        // a live consumer would have seen it.
        writer.write_owned(&window)?;
        writer.flush()?;
        windows += 1;
        events += window.events().len() as u64;
        bytes += subscription.last_window_bytes() as u64;
        if window.window().missed() > 0 {
            println!(
                "{WARN}lost {} events before window {windows}: raise --interval-ms or the producer's event capacity{WARN:#}",
                format::count(window.window().missed())
            );
        }
    };
    writer.finish()?;
    println!(
        "{stopped}: {} windows, {} events, {} received",
        format::count(windows),
        format::count(events),
        format::bytes(bytes)
    );
    println!(
        "summarize it with {HINT}footfall_capture info {}{HINT:#}",
        path.display()
    );
    Ok(())
}

/// Reads every window of a capture and joins consecutive windows of the
/// same runtime instance into one snapshot.
fn read_joined(path: &Path) -> Result<OwnedSnapshot> {
    let reader = CaptureReader::new(BufReader::new(File::open(path)?))?;
    let mut joined: Option<OwnedSnapshot> = None;
    let mut skipped = 0_usize;
    for window in reader {
        let window = match window {
            Ok(window) => window,
            // A recording stopped abruptly can end part way through a
            // window; keep the complete ones.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                eprintln!(
                    "{WARN}{} ends part way through a window; using the complete ones{WARN:#}",
                    path.display()
                );
                break;
            }
            Err(error) => return Err(error.into()),
        };
        match &mut joined {
            None => joined = Some(window),
            Some(joined) => {
                if joined.append(window).is_err() {
                    skipped += 1;
                }
            }
        }
    }
    if skipped > 0 {
        eprintln!("{WARN}skipped {skipped} windows from another run of the producer{WARN:#}");
    }
    joined.ok_or_else(|| format!("{} holds no snapshots", path.display()).into())
}

fn info(path: &Path, budget_ms: f64) -> Result<()> {
    let snapshot = read_joined(path)?;
    let metadata = snapshot.metadata();
    let capture = metadata.capture();
    println!(
        "{HEADING}{}{HEADING:#} {DIM}· {} pid {} · session {} · level {} · producer {}{DIM:#}",
        path.display(),
        metadata.process_name(),
        metadata.process_id(),
        metadata.session_name().unwrap_or("-"),
        capture.level().as_str(),
        metadata.producer_version(),
    );
    let window = snapshot.window();
    println!(
        "{DIM}events {}..{} · {} kept · instance {:016x}{DIM:#}\n",
        window.since(),
        window.next(),
        format::count(snapshot.events().len() as u64),
        metadata.instance_id()
    );
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a budget in ms"
    )]
    let budget = (budget_ms * 1000.0) as u64;
    report::frames(&frames(&snapshot), budget);
    println!();
    report::spans(&Spans::from_snapshot(&snapshot), 12);
    println!();
    report::counters(&snapshot, 16);
    println!();
    report::losses(&snapshot);
    Ok(())
}

fn export(path: &Path, output: &Path) -> Result<()> {
    let snapshot = read_joined(path)?;
    footfall::export::write_chrome_trace_file(&snapshot, output)?;
    println!(
        "Wrote {HINT}{}{HINT:#} ({} events); open it at {HINT}https://ui.perfetto.dev{HINT:#}.",
        output.display(),
        format::count(snapshot.events().len() as u64)
    );
    Ok(())
}
