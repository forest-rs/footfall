// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session consumer demo for Footfall.
//!
//! - `session_consumer --list` lists the sessions of every local producer.
//! - `session_consumer [<session-id>]` prints one snapshot.
//! - `session_consumer --watch [<session-id>]` subscribes and prints what
//!   each new snapshot adds.
//! - `session_consumer --record <file> [<session-id>]` does the same and saves
//!   every snapshot to a capture file until Ctrl-C; turn it into a Chrome
//!   trace with the `capture_export` example.
//!
//! Without a session id, the consumer uses the only local session.

mod session_summary;

use footfall_session::capture::CaptureWriter;
use footfall_session_wtransport::{LocalDiscovery, LocalSession, SessionClient};
use session_summary::{print_summary, summarize};
use std::{env, process, time::Duration};

/// How long each local server may take to answer discovery.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() {
    let mut args = env::args();
    let program = args
        .next()
        .unwrap_or_else(|| String::from("session_consumer"));
    let (mode, session_id) = match args.next().as_deref() {
        Some("--list") => (Mode::List, None),
        Some("--watch") => (Mode::Watch, args.next()),
        Some("--record") => {
            let path = args.next().unwrap_or_else(|| usage(&program));
            (Mode::Record(path), args.next())
        }
        Some(option) if option.starts_with("--") => usage(&program),
        other => (Mode::Fetch, other.map(String::from)),
    };
    if args.next().is_some() {
        usage(&program);
    }

    let discovery = SessionClient::discover_local(DISCOVERY_TIMEOUT)
        .await
        .unwrap_or_else(|error| fail(&error.to_string()));
    for (url, error) in discovery.unreachable() {
        eprintln!("skipping {url}: {error}");
    }
    if let Mode::List = mode {
        list_sessions(&discovery);
        return;
    }

    let session =
        select_session(&discovery, session_id.as_deref()).unwrap_or_else(|error| fail(&error));
    match mode {
        Mode::Fetch => {
            let snapshot = session
                .client()
                .fetch(session.id(), None)
                .await
                .unwrap_or_else(|error| fail(&error.to_string()));
            println!("fetched session {}", session.id());
            print_summary(&summarize(&snapshot));
        }
        Mode::Watch => watch_session(session, None).await,
        Mode::Record(path) => watch_session(session, Some(&path)).await,
        Mode::List => unreachable!("listing returned above"),
    }
}

enum Mode {
    List,
    Fetch,
    Watch,
    Record(String),
}

fn list_sessions(discovery: &LocalDiscovery) {
    if discovery.sessions().is_empty() {
        println!("no local sessions");
    }
    for session in discovery.sessions() {
        let announcement = session.announcement();
        println!(
            "{} {} process={} pid={} session={}",
            session.client().url(),
            announcement.id(),
            announcement.process_name(),
            announcement.process_id(),
            announcement.session_name().unwrap_or("-"),
        );
    }
}

fn select_session<'a>(
    discovery: &'a LocalDiscovery,
    requested_id: Option<&str>,
) -> Result<&'a LocalSession, String> {
    if let Some(requested_id) = requested_id {
        return discovery
            .session(requested_id)
            .map_err(|error| error.to_string());
    }
    match discovery.sessions() {
        [] => Err(String::from("no local sessions")),
        [session] => Ok(session),
        sessions => Err(format!(
            "found {} local sessions; run `session_consumer --list` and name one",
            sessions.len()
        )),
    }
}

async fn watch_session(session: &LocalSession, record: Option<&str>) {
    let mut capture = record.map(|path| {
        let file = std::fs::File::create(path)
            .unwrap_or_else(|error| fail(&format!("cannot create {path}: {error}")));
        CaptureWriter::new(std::io::BufWriter::new(file))
            .unwrap_or_else(|error| fail(&format!("cannot write {path}: {error}")))
    });
    let mut subscription = session
        .client()
        .subscribe(session.id(), None, Duration::from_millis(500))
        .await
        .unwrap_or_else(|error| fail(&error.to_string()));
    let error = loop {
        let snapshot = match subscription.next().await {
            Ok(snapshot) => snapshot,
            Err(error) => break error,
        };
        if let Some(capture) = &mut capture {
            capture
                .write_owned(&snapshot)
                .and_then(|()| capture.flush())
                .unwrap_or_else(|error| fail(&format!("cannot record: {error}")));
        }
        let window = snapshot.window();
        println!(
            "events {}..{}: {} new, {} missed; {} counters, {} contexts",
            window.since(),
            window.next(),
            snapshot.events().len(),
            window.missed(),
            snapshot.counters().len(),
            snapshot.contexts().len(),
        );
    };
    fail(&error.to_string());
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    process::exit(1);
}

fn usage(program: &str) -> ! {
    eprintln!("usage: {program} [--list | [--watch | --record <file>] [<session-id>]]");
    process::exit(2);
}
