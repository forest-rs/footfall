// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A live terminal view of a running Footfall session, in the spirit of
//! `top` and `tokio-console`: frame times against a budget, where the time
//! goes, how counters move per frame, recent messages, and whether anything
//! is being lost.
//!
//! Everything it shows comes from the public API: `SessionClient` to find
//! and subscribe to sessions, and `footfall::analysis` trackers to turn each
//! window into frames and scopes.

mod app;
mod ui;

use app::App;
use footfall::session::OwnedSnapshot;
use footfall::session::capture::CaptureReader;
use footfall::wtransport::{ClientError, LocalSession, SessionClient};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::time::Duration;
use tokio::sync::mpsc;

/// The options of `footfall top`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The session to watch; needed only when several are running.
    #[arg(long)]
    session: Option<String>,
    /// Replay a capture file instead of attaching to a live session.
    #[arg(long, conflicts_with = "session")]
    file: Option<std::path::PathBuf>,
    /// How often to ask for new data, in milliseconds.
    #[arg(long, default_value_t = 250)]
    interval_ms: u64,
    /// Frame budget in milliseconds; longer frames show in red.
    #[arg(long, default_value_t = 16.7, value_parser = crate::milliseconds)]
    budget_ms: f64,
}

/// What a source sends the view.
#[derive(Debug)]
pub(crate) enum Update {
    /// A new window, with its size on the wire when it came over one.
    Window(Box<OwnedSnapshot>, Option<usize>),
    /// The source ended, and why.
    Ended(String),
}

/// Runs the view until the person quits.
pub(crate) async fn run(args: Args) {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a budget in ms"
    )]
    let budget = (args.budget_ms * 1000.0) as u64;
    let interval = Duration::from_millis(args.interval_ms.max(50));

    // Pick the source before taking over the terminal, so errors print
    // normally.
    let (updates_tx, updates) = mpsc::channel(16);
    let source = match &args.file {
        Some(path) => {
            let reader = std::fs::File::open(path)
                .map_err(ClientError::Io)
                .and_then(|file| {
                    CaptureReader::new(std::io::BufReader::new(file)).map_err(ClientError::Io)
                })
                .unwrap_or_else(|error| fail(&format!("cannot read {}: {error}", path.display())));
            tokio::task::spawn_blocking(move || replay(reader, &updates_tx));
            format!("file {}", path.display())
        }
        None => {
            let session = choose_session(args.session.as_deref()).await;
            let label = format!("{} · {}", session.client().url(), session.id());
            tokio::spawn(follow(session, interval, updates_tx));
            label
        }
    };

    let mut app = App::new(source, budget);
    let mut terminal = ratatui::init();
    let mut keys = read_keys();
    let mut updates = updates;
    let mut redraw = tokio::time::interval(Duration::from_millis(250));
    loop {
        terminal
            .draw(|frame| ui::draw(frame, &app))
            .expect("the terminal should accept a frame");
        tokio::select! {
            Some(update) = updates.recv() => app.update(update),
            Some(key) = keys.recv() => {
                if !app.key(key) {
                    break;
                }
            }
            _ = redraw.tick() => {}
        }
    }
    ratatui::restore();
}

/// Finds the session to watch: the one named, or the only one running.
async fn choose_session(requested: Option<&str>) -> LocalSession {
    let discovery = SessionClient::discover_local(Duration::from_secs(1))
        .await
        .unwrap_or_else(|error| fail(&error.to_string()));
    if let Some(id) = requested {
        return discovery
            .session(id)
            .cloned()
            .unwrap_or_else(|error| fail(&error.to_string()));
    }
    match discovery.sessions() {
        [only] => only.clone(),
        [] => fail("no Footfall sessions are running"),
        sessions => {
            let ids: Vec<_> = sessions
                .iter()
                .map(|session| {
                    format!(
                        "{} ({}, pid {})",
                        session.id(),
                        session.announcement().process_name(),
                        session.announcement().process_id()
                    )
                })
                .collect();
            fail(&format!(
                "several sessions are running; choose one with --session: {}",
                ids.join(", ")
            ))
        }
    }
}

/// Subscribes to a live session and forwards each window. Subscription
/// reads run on their own task, because cancelling one part way through
/// would lose its place in the stream.
async fn follow(session: LocalSession, interval: Duration, updates: mpsc::Sender<Update>) {
    let mut subscription = match session
        .client()
        .subscribe(session.id(), None, interval)
        .await
    {
        Ok(subscription) => subscription,
        Err(error) => {
            let _ = updates.send(Update::Ended(error.to_string())).await;
            return;
        }
    };
    loop {
        let update = match subscription.next().await {
            Ok(window) => Update::Window(Box::new(window), Some(subscription.last_window_bytes())),
            Err(ClientError::Closed) => Update::Ended(String::from("the producer stopped")),
            Err(error) => Update::Ended(error.to_string()),
        };
        let ended = matches!(update, Update::Ended(_));
        if updates.send(update).await.is_err() || ended {
            return;
        }
    }
}

/// Replays a capture file, pacing windows as far apart as they were taken.
fn replay<R: std::io::Read>(mut reader: CaptureReader<R>, updates: &mpsc::Sender<Update>) {
    let mut previous: Option<u64> = None;
    loop {
        match reader.read_snapshot() {
            Ok(Some(window)) => {
                let taken = window.captured_at_nanos();
                if let Some(previous) = previous {
                    let gap = taken.saturating_sub(previous).min(2_000_000_000);
                    std::thread::sleep(Duration::from_nanos(gap));
                }
                previous = Some(taken);
                if updates
                    .blocking_send(Update::Window(Box::new(window), None))
                    .is_err()
                {
                    return;
                }
            }
            Ok(None) => {
                let _ =
                    updates.blocking_send(Update::Ended(String::from("end of the capture file")));
                return;
            }
            Err(error) => {
                let _ = updates
                    .blocking_send(Update::Ended(format!("cannot read the capture: {error}")));
                return;
            }
        }
    }
}

/// A key the view handles.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Key {
    /// Quit.
    Quit,
    /// Pause or resume updates.
    Pause,
    /// Switch the scope table's sort between total and self time.
    Sort,
}

/// Reads keys on a thread of its own, since terminal input blocks.
fn read_keys() -> mpsc::Receiver<Key> {
    let (keys, received) = mpsc::channel(8);
    std::thread::spawn(move || {
        loop {
            let Ok(Event::Key(press)) = event::read() else {
                continue;
            };
            if press.kind != KeyEventKind::Press {
                continue;
            }
            let key = match press.code {
                KeyCode::Char('q') | KeyCode::Esc => Key::Quit,
                KeyCode::Char('c') if press.modifiers.contains(KeyModifiers::CONTROL) => Key::Quit,
                KeyCode::Char('p' | ' ') => Key::Pause,
                KeyCode::Char('s') => Key::Sort,
                _ => continue,
            };
            if keys.blocking_send(key).is_err() {
                return;
            }
        }
    });
    received
}

fn fail(message: &str) -> ! {
    anstream::eprintln!(
        "{}{message}{}",
        crate::style::BAD,
        crate::style::BAD.render_reset()
    );
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::{App, Update, ui};
    use footfall::runtime::Runtime;
    use footfall::session::codec::encode_owned_envelope;
    use footfall::session::{Chunking, OwnedSnapshot};
    use footfall_examples::demo::Renderer;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::atomic::AtomicBool;

    /// Feeds a real demo capture through the view and renders it.
    #[test]
    fn renders_a_demo_session() {
        let runtime = Runtime::builder()
            .process_name("demo_app")
            .session_name("main")
            .install_global()
            .expect("the test installs the only backend");
        let mut app = App::new(
            String::from("https://127.0.0.1:43091/footfall/session · main"),
            16_667,
        );
        let mut renderer = Renderer::new(2);
        let mut cursor = runtime.cursor();
        for _ in 0..4 {
            renderer.run(60, Some(60), &AtomicBool::new(false));
            let window = OwnedSnapshot::from(runtime.snapshot_since(cursor));
            cursor = window.next_cursor();
            // What this window takes on the wire, framing aside.
            let bytes = window
                .to_envelopes(Chunking::default())
                .iter()
                .map(|envelope| encode_owned_envelope(envelope).map_or(0, |frame| frame.len()))
                .sum();
            app.update(Update::Window(Box::new(window), Some(bytes)));
        }

        let mut terminal = Terminal::new(TestBackend::new(100, 36)).expect("a test terminal");
        terminal
            .draw(|frame| ui::draw(frame, &app))
            .expect("the view draws");
        let screen = format!("{}", terminal.backend());
        if std::env::var_os("FOOTFALL_TOP_PRINT").is_some() {
            println!("{screen}");
        }
        for expected in ["demo_app", "raster_tile", "draw_calls", "nothing lost"] {
            assert!(
                screen.contains(expected),
                "{expected} missing from:\n{screen}"
            );
        }
    }
}
