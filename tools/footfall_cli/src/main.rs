// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `footfall` command: watch a running session live, record it to a
//! capture file, and work with the capture later: summarize it, ask why its
//! slow frames were slow, see its frame pacing, compare it with another run,
//! or export it as one Chrome trace.
//!
//! ```sh
//! footfall top
//! footfall record demo.ffcap
//! footfall analyze summary demo.ffcap
//! footfall analyze slow demo.ffcap --by-arg nodes
//! footfall analyze pacing demo.ffcap
//! footfall analyze compare before.ffcap after.ffcap
//! footfall analyze span demo.ffcap 13909
//! footfall export demo.ffcap
//! footfall top --file demo.ffcap
//! ```

mod format;
mod report;
mod style;
mod top;

use anstream::{eprintln, println};
use clap::{Parser, Subcommand};
use footfall::analysis::{
    Comparison, GroupBy, KeyPart, LateCause, Outcome, Population, Selection, SpanId, Trace,
};
use footfall::session::OwnedSnapshot;
use footfall::session::capture::{CaptureReader, CaptureWriter};
use footfall::wtransport::{ClientError, SessionClient};
use footfall::{DropReason, FramePointKind};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::Duration;
use style::{BAD, DIM, GOOD, HEADING, HINT, NAME, WARN};

/// Watch Footfall sessions live, record them to capture files, and
/// summarize, compare, or export the captures.
#[derive(Debug, Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Watch a running session live in the terminal, or replay a capture
    /// file: frame times against a budget, where the time goes, counters per
    /// frame, messages, and losses.
    Top(top::Args),
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
    /// Ask a capture questions: summarize it, why its slow frames were slow,
    /// how its frames were paced, how it differs from another run, or what
    /// one span did.
    #[command(subcommand)]
    Analyze(Analysis),
    /// Join a capture's windows into one Chrome trace for Perfetto.
    Export {
        /// The capture file to read.
        file: PathBuf,
        /// Where to write the trace; defaults to the capture's name with `.json`.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
}

/// The questions `footfall analyze` answers about captures.
#[derive(Debug, Subcommand)]
enum Analysis {
    /// Summarize a capture: frames, where the time went, counters, losses.
    Summary {
        /// The capture file to read.
        file: PathBuf,
        /// Frame budget in milliseconds.
        #[arg(long, default_value_t = 16.7)]
        budget_ms: f64,
    },
    /// Compare a capture's slow frames with the rest: which scopes account
    /// for the difference, and whether they ran more often or each ran
    /// slower.
    Slow {
        /// The capture file to read.
        file: PathBuf,
        /// Frame budget in milliseconds; longer frames are slow.
        #[arg(long, default_value_t = 16.7)]
        budget_ms: f64,
        /// How many scopes to show.
        #[arg(long, default_value_t = 10)]
        top: usize,
        /// Also split each scope by the value of its argument KEY, such as
        /// `nodes`, to see whether slow spans did more of something.
        #[arg(long, value_name = "KEY")]
        by_arg: Option<String>,
    },
    /// Show frame pacing: which frames were shown on time, late, or not at
    /// all, from what the frame scheduler and display reported, and what
    /// the late frames did differently.
    Pacing {
        /// The capture file to read.
        file: PathBuf,
        /// How late, in milliseconds, a frame may be shown and still count
        /// as on time.
        #[arg(long, default_value_t = 1.0)]
        tolerance_ms: f64,
        /// How many late frames and scopes to show.
        #[arg(long, default_value_t = 8)]
        top: usize,
    },
    /// Compare two captures of the same workload frame by frame: which
    /// scopes changed, and whether they ran more often or each ran slower.
    Compare {
        /// The capture to compare against.
        baseline: PathBuf,
        /// The capture to compare.
        candidate: PathBuf,
        /// How many scopes to show.
        #[arg(long, default_value_t = 10)]
        top: usize,
    },
    /// Show one span and the work around it: where its scope is declared,
    /// arguments, context, what it ran inside, what ran inside it, its
    /// messages, and the flows through it.
    Span {
        /// The capture file to read.
        file: PathBuf,
        /// The span's id: the number after `span@`.
        id: u64,
    },
}

#[tokio::main]
async fn main() {
    let result = match Args::parse().command {
        Command::Top(args) => {
            top::run(args).await;
            Ok(())
        }
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
        Command::Analyze(analysis) => analyze(analysis),
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

/// Answers one `footfall analyze` question.
fn analyze(analysis: Analysis) -> Result<()> {
    match analysis {
        Analysis::Summary { file, budget_ms } => summary(&file, budget_ms),
        Analysis::Slow {
            file,
            budget_ms,
            top,
            by_arg,
        } => slow(&file, budget_ms, top, by_arg.as_deref()),
        Analysis::Compare {
            baseline,
            candidate,
            top,
        } => compare(&baseline, &candidate, top),
        Analysis::Pacing {
            file,
            tolerance_ms,
            top,
        } => pacing(&file, tolerance_ms, top),
        Analysis::Span { file, id } => span(&file, SpanId::new(id)),
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
        "summarize it with {HINT}footfall analyze summary {}{HINT:#}",
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

fn summary(path: &Path, budget_ms: f64) -> Result<()> {
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
    let trace = Trace::from_snapshot(&snapshot);
    report::frames(trace.frames(), budget);
    println!();
    report::spans(
        "Where the time went on the CPU",
        &trace.spans().on_threads().group_by(GroupBy::Name).table(),
        12,
    );
    for timeline in trace.timelines() {
        let Some(id) = timeline.timeline_id() else {
            continue;
        };
        println!();
        report::spans(
            &format!("On {}", track_label(timeline)),
            &trace
                .spans()
                .on_timeline(id)
                .group_by(GroupBy::Name)
                .table(),
            8,
        );
    }
    println!();
    report::counters(&snapshot, 16);
    println!();
    report::losses(&snapshot);
    Ok(())
}

/// Reads every window of a capture into a trace of its first runtime
/// instance.
fn read_trace(path: &Path) -> Result<Trace> {
    let reader = CaptureReader::new(BufReader::new(File::open(path)?))?;
    let mut trace = Trace::new();
    let mut skipped = 0_usize;
    for window in reader {
        let window = match window {
            Ok(window) => window,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        };
        if trace.push(&window).is_err() {
            skipped += 1;
        }
    }
    if skipped > 0 {
        eprintln!("{WARN}skipped {skipped} windows from another run of the producer{WARN:#}");
    }
    if trace.instance_id().is_none() {
        return Err(format!("{} holds no snapshots", path.display()).into());
    }
    Ok(trace)
}

fn slow(path: &Path, budget_ms: f64, top: usize, by_arg: Option<&str>) -> Result<()> {
    let trace = read_trace(path)?;
    let budget = Duration::from_secs_f64(budget_ms / 1000.0);
    let (slow, normal): (Vec<_>, Vec<_>) = trace
        .frames()
        .iter()
        .partition(|frame| frame.duration() > budget);
    println!(
        "{HEADING}Why slow frames were slow{HEADING:#} {DIM}· {} of {} frames over {}{DIM:#}\n",
        format::count(slow.len() as u64),
        format::count(trace.frames().len() as u64),
        format::duration(budget),
    );
    if slow.is_empty() {
        println!("No frame was over budget.");
        return Ok(());
    }
    // Time inside each frame, so work that crosses a frame boundary counts
    // where it ran.
    let (normal, slow_frames) = (
        Population::frames(&trace, normal),
        Population::frames(&trace, slow.iter().copied()),
    );
    let mut comparison = Comparison::between(&normal, &slow_frames)
        .selecting(Selection::Inside)
        .group_by(GroupBy::Name);
    if let Some(key) = by_arg {
        comparison = comparison.group_by(GroupBy::Arg(key.to_owned()));
    }
    let comparison = comparison.run();
    report::comparison(&comparison, "normal", "slow", top);

    // The worst example of the biggest change, to look at.
    if let Some(row) = comparison.rows().first() {
        let name = row.describe();
        let ids: Vec<_> = slow.iter().map(|frame| frame.id()).collect();
        let mut query = trace.spans().in_frames(&ids).selecting(Selection::Inside);
        for part in row.key() {
            query = match part {
                KeyPart::Name(scope) => query.named(scope.as_str()),
                KeyPart::Arg {
                    key,
                    value: Some(value),
                } => query.with_arg(key.as_str(), value.clone()),
                KeyPart::Arg { key, value: None } => query.without_arg(key.as_str()),
                _ => query,
            };
        }
        let instances = query.instances(1);
        if let Some(worst) = instances.worst().first() {
            let span = worst.span();
            let parent = trace.parent(span).map_or_else(
                || String::from("nothing"),
                |parent| parent.name().to_owned(),
            );
            let thread = span_track(&trace, span);
            println!(
                "\n{HEADING}Worst {NAME}{name}{NAME:#}{HEADING:#} {DIM}· {} ({}) on {thread}, inside {parent}, in {}{DIM:#}",
                format::duration(span.duration()),
                span.id(),
                trace
                    .frame_at(span.begin())
                    .map_or_else(|| String::from("no frame"), |frame| frame.id().to_string()),
            );
        }
    }
    Ok(())
}

fn pacing(path: &Path, tolerance_ms: f64, top: usize) -> Result<()> {
    let trace = read_trace(path)?;
    let pacing = trace.pacing(Duration::from_secs_f64(tolerance_ms / 1000.0));
    let counts = pacing.counts();
    println!(
        "{HEADING}Frame pacing{HEADING:#} {DIM}· {} numbered frames · on time within {}{DIM:#}",
        format::count(counts.total()),
        format::duration(pacing.tolerance()),
    );
    if counts.total() == 0 {
        println!("No numbered frames: mark frames with frame_begin_numbered.");
        return Ok(());
    }
    let style = if counts.missed() == 0 { GOOD } else { BAD };
    println!(
        "  {GOOD}{} on time{GOOD:#} · {style}{} late · {} dropped · {} submitted late{style:#} · {DIM}{} skipped · {} unknown{DIM:#}",
        format::count(counts.on_time),
        format::count(counts.late),
        format::count(counts.dropped),
        format::count(counts.submitted_late),
        format::count(counts.skipped),
        format::count(counts.unknown),
    );
    if counts.late > 0 {
        let causes = counts.late_causes;
        let named: Vec<String> = [
            (causes.missed_deadline, "missed their deadline"),
            (causes.backlog, "queued behind a late frame"),
            (causes.gpu, "GPU finished after the target"),
            (causes.display, "held by the display"),
            (
                causes.after_submit,
                "late after submit, no GPU work to tell",
            ),
            (causes.unknown, "cause not known"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, why)| format!("{} {why}", format::count(count)))
        .collect();
        println!("  late: {}", named.join(" · "));
    }
    let intervals = pacing.intervals();
    if intervals.count() > 0 {
        println!(
            "  shown every {} {DIM}(median){DIM:#}, p99 {} · {} stutters",
            format::duration(intervals.median()),
            format::duration(intervals.p99()),
            pacing.stutters(),
        );
    }
    let latency = pacing.latency();
    if latency.count() > 0 {
        println!(
            "  from start of work to shown: median {}, p99 {}",
            format::duration(latency.median()),
            format::duration(latency.p99()),
        );
    }
    let wasted = pacing.wasted();
    if wasted.frames > 0 {
        println!(
            "  never shown: {} frames, {} of CPU work and {} of GPU work spent on them",
            format::count(wasted.frames),
            format::duration(wasted.cpu),
            format::duration(wasted.gpu),
        );
    }
    if counts.unknown == counts.total() {
        println!(
            "  {WARN}no presentation feedback: report Presented and Target frame points to judge pacing{WARN:#}"
        );
    }
    if pacing.unplaced_points() > 0 {
        println!(
            "  {WARN}{} frame points could not be placed: their clock was never synced{WARN:#}",
            pacing.unplaced_points()
        );
    }

    predictions(&pacing);

    let mut late: Vec<_> = pacing
        .frames()
        .iter()
        .filter(|frame| frame.outcome().missed())
        .collect();
    if late.is_empty() {
        return Ok(());
    }
    late.sort_by_key(|frame| core::cmp::Reverse(late_by(frame.outcome())));
    println!("\n{HEADING}Worst misses{HEADING:#}");
    println!(
        "  {DIM}{:>7}  {:<14}  {:<14}  {:>10}  {:>10}  {:>14}{DIM:#}",
        "frame", "outcome", "why", "CPU work", "GPU busy", "past deadline"
    );
    for frame in late.iter().take(top) {
        let (outcome, why) = match frame.outcome() {
            Outcome::Late { by, cause } => (
                format!("late {}", format::duration(by)),
                match cause {
                    LateCause::MissedDeadline => "deadline",
                    LateCause::Backlog => "backlog",
                    LateCause::Gpu => "GPU",
                    LateCause::Display => "display",
                    LateCause::AfterSubmit => "after submit",
                    _ => "-",
                },
            ),
            Outcome::Dropped { reason } => (String::from("dropped"), drop_reason(reason)),
            Outcome::SubmittedLate { .. } => (String::from("submitted late"), "deadline"),
            other => (format!("{other:?}"), "-"),
        };
        let past_deadline = frame
            .deadline()
            .zip(frame.work())
            .and_then(|(deadline, work)| work.end().checked_sub(deadline))
            .map_or_else(|| String::from("-"), format::duration);
        println!(
            "  {:>7}  {BAD}{outcome:<14}{BAD:#}  {why:<14}  {:>10}  {:>10}  {:>14}",
            frame.number(),
            frame
                .work()
                .map_or_else(|| String::from("-"), |work| format::duration(work.length())),
            format::duration(frame.gpu_busy()),
            past_deadline,
        );
    }

    // What the missed frames did differently, CPU and GPU together. GPU
    // spans count toward their frame by number, wherever they ran.
    let missed_ids = pacing.frame_ids(Outcome::missed);
    let on_time_ids = pacing.frame_ids(|outcome| outcome == Outcome::OnTime);
    let frames = |ids: &[footfall::analysis::FrameId]| {
        Population::frames(&trace, ids.iter().filter_map(|id| trace.frame(*id)))
    };
    let (on_time, missed) = (frames(&on_time_ids), frames(&missed_ids));
    let comparison = Comparison::between(&on_time, &missed)
        .selecting(Selection::Inside)
        .group_by(GroupBy::Name)
        .run();
    println!("\n{HEADING}What the missed frames did differently{HEADING:#}");
    report::comparison(&comparison, "on time", "missed", top);
    Ok(())
}

/// Orders missed frames: dropped first, then by how late.
fn late_by(outcome: Outcome) -> Duration {
    match outcome {
        Outcome::Dropped { .. } => Duration::MAX,
        Outcome::Late { by, .. } | Outcome::SubmittedLate { by } => by,
        _ => Duration::ZERO,
    }
}

fn drop_reason(reason: DropReason) -> &'static str {
    match reason {
        DropReason::Discarded => "discarded",
        DropReason::Superseded => "superseded",
        DropReason::MissedDeadline => "deadline",
        DropReason::SurfaceUnavailable => "no surface",
        DropReason::OutputUnavailable => "no display",
        DropReason::Replaced => "replaced",
        _ => "-",
    }
}

/// Prints how far each kind of predicted time was from when frames were
/// shown, for the kinds the trace has.
fn predictions(pacing: &footfall::analysis::Pacing) {
    let rows: Vec<_> = [
        (
            FramePointKind::Predicted,
            "predicted",
            "by the platform, at frame start",
        ),
        (
            FramePointKind::Target,
            "target",
            "by the scheduler, when planned",
        ),
        (FramePointKind::Expected, "expected", "at submit"),
    ]
    .into_iter()
    .map(|(kind, name, when)| (pacing.prediction_error(kind), name, when))
    .filter(|(error, ..)| error.off.count() > 0)
    .collect();
    if rows.is_empty() {
        return;
    }
    println!("\n{HEADING}How far off the predicted display times were{HEADING:#}");
    println!(
        "  {DIM}{:<10} {:>7}  {:>10}  {:>10}  {:>10}  {:>7}  {:>7}  {}{DIM:#}",
        "time",
        "frames",
        "median off",
        "p99 off",
        format!("within {}", format::duration(pacing.tolerance())),
        "early",
        "late",
        "predicted"
    );
    for (error, name, when) in rows {
        println!(
            "  {:<10} {:>7}  {:>10}  {:>10}  {:>10}  {:>7}  {:>7}  {DIM}{when}{DIM:#}",
            name,
            format::count(error.off.count()),
            format::duration(error.off.median()),
            format::duration(error.off.p99()),
            format::count(error.within()),
            format::count(error.early.count()),
            format::count(error.late.count()),
        );
    }
}

fn compare(baseline: &Path, candidate: &Path, top: usize) -> Result<()> {
    let before = read_trace(baseline)?;
    let after = read_trace(candidate)?;
    println!(
        "{HEADING}What changed{HEADING:#} {DIM}· {} ({} frames) → {} ({} frames), per frame{DIM:#}\n",
        baseline.display(),
        format::count(before.frames().len() as u64),
        candidate.display(),
        format::count(after.frames().len() as u64),
    );
    let comparison = Comparison::between(
        &Population::frames(&before, before.frames()),
        &Population::frames(&after, after.frames()),
    )
    .selecting(Selection::Inside)
    .group_by(GroupBy::Name)
    .run();
    report::comparison(&comparison, "before", "after", top);
    Ok(())
}

fn span(path: &Path, id: SpanId) -> Result<()> {
    let trace = read_trace(path)?;
    let Some(span) = trace.span(id) else {
        return Err(format!("{} holds no completed span {id}", path.display()).into());
    };
    let thread = span_track(&trace, span);
    println!(
        "{HEADING}{NAME}{}{NAME:#}{HEADING:#} {DIM}· {id} · {thread} · depth {}{DIM:#}",
        span.name(),
        span.depth()
    );
    if let Some(number) = span.frame_number() {
        println!("  frame number {number}");
    }
    if let Some(placement) = span.placement() {
        let how = if placement.between_syncs() {
            "between two syncs of its clock"
        } else {
            "by extending from its clock's nearest sync"
        };
        println!(
            "  {DIM}timed by another clock; placed {how}, {} from a reading{DIM:#}",
            format::duration(placement.distance())
        );
    }
    println!(
        "  {} from {} to {}, {} of it its own",
        format::duration(span.duration()),
        format::duration(span.begin()),
        format::duration(span.end()),
        format::duration(span.self_time()),
    );
    if let Some(location) = span.location() {
        println!(
            "  declared {} at {}:{}",
            location.function(),
            location.file(),
            location.line()
        );
    }
    println!("  context  {}", trace.describe_context(span.context_id()));
    for arg in span.args() {
        println!("  arg      {}={}", arg.key(), arg.value());
    }
    if span.interrupted() {
        println!("  {WARN}events were lost while it ran; nested spans may be missing{WARN:#}");
    }
    if let Some(frame) = trace.frame_at(span.begin()) {
        println!(
            "  frame    {} took {}",
            frame.id(),
            format::duration(frame.duration())
        );
    }

    let mut chain = Vec::new();
    let mut parent = trace.parent(span);
    while let Some(outer) = parent {
        chain.push(format!("{} ({})", outer.name(), outer.id()));
        parent = trace.parent(outer);
    }
    if !chain.is_empty() {
        println!("  inside   {}", chain.join(" ← "));
    }
    let children = trace.children(span);
    if !children.is_empty() {
        println!("{HEADING}Inside it{HEADING:#}");
        for child in children {
            println!(
                "  {NAME}{}{NAME:#}  {}  {DIM}{}{DIM:#}",
                child.name(),
                format::duration(child.duration()),
                child.id()
            );
        }
    }
    let messages: Vec<_> = trace
        .messages_in(span.range())
        .iter()
        .filter(|message| message.thread_id() == span.thread_id())
        .collect();
    if !messages.is_empty() {
        println!("{HEADING}Messages while it ran{HEADING:#}");
        for message in messages {
            println!("  {NAME}{}{NAME:#}  {}", message.name(), message.text());
        }
    }
    for flow in trace.flows_through(id) {
        println!("{HEADING}Flow {}{HEADING:#}", flow.id().get());
        for step in flow.steps() {
            let at = step
                .span()
                .and_then(|step_span| trace.span(step_span))
                .map_or_else(
                    || String::from("outside any span"),
                    |step_span| format!("in {} ({})", step_span.name(), step_span.id()),
                );
            println!(
                "  {:?} at {} {at}",
                step.phase(),
                format::duration(step.at())
            );
        }
    }
    Ok(())
}

/// Names the track a span ran on: its thread, or its timeline.
fn span_track(trace: &Trace, span: &footfall::analysis::Span) -> String {
    match span.timeline().and_then(|id| trace.timeline(id)) {
        Some(timeline) => track_label(timeline),
        None => trace
            .track(span.thread_id())
            .and_then(|track| track.name())
            .map_or_else(|| format!("thread {}", span.thread_id()), str::to_owned),
    }
}

/// Names a track for a heading: a thread's name, or a timeline's kind and
/// name, such as `GPU graphics`.
fn track_label(track: &footfall::analysis::Track) -> String {
    let name = track.name().unwrap_or("unnamed");
    match track.kind() {
        footfall::analysis::TrackKind::Timeline { kind, .. } => match kind {
            footfall::TimelineKind::Gpu => format!("GPU {name}"),
            footfall::TimelineKind::Display => format!("display {name}"),
            _ => format!("timeline {name}"),
        },
        _ => name.to_owned(),
    }
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
