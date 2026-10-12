// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session consumer demo for Footfall.
//!
//! `session_consumer --list` lists the sessions of every local producer,
//! `session_consumer [<session-id>]` prints one snapshot, and
//! `session_consumer --watch [<session-id>]` subscribes and prints what each
//! new snapshot adds.

mod session_summary;

use footfall_session_wtransport::{SessionAnnouncement, SessionClient};
use session_summary::{print_summary, summarize};
use std::time::Duration;
use std::{env, process};

#[tokio::main]
async fn main() {
    let mut args = env::args();
    let program = args
        .next()
        .unwrap_or_else(|| String::from("session_consumer"));
    let argument = args.next();
    if let Some(option) = argument.as_deref()
        && option == "--list"
    {
        if args.next().is_some() {
            usage(&program);
        }
        list_sessions().await;
        return;
    }
    let (watch, argument) = match argument.as_deref() {
        Some("--watch") => (true, args.next()),
        _ => (false, argument),
    };
    if args.next().is_some() {
        usage(&program);
    }

    let client = local_client().await;
    let announcement = select_session(&client, argument.as_deref())
        .await
        .unwrap_or_else(|error| fail(&error));
    if watch {
        watch_session(&client, announcement.id()).await;
        return;
    }
    let snapshot = client
        .fetch(announcement.id(), None)
        .await
        .unwrap_or_else(|error| fail(&error.to_string()));

    println!("fetched session {}", announcement.id());
    print_summary(&summarize(&snapshot));
}

async fn watch_session(client: &SessionClient, session_id: &str) {
    let mut subscription = client
        .subscribe(session_id, None, Duration::from_millis(500))
        .await
        .unwrap_or_else(|error| fail(&error.to_string()));
    let error = loop {
        let snapshot = match subscription.next().await {
            Ok(snapshot) => snapshot,
            Err(error) => break error,
        };
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

async fn local_client() -> SessionClient {
    SessionClient::local()
        .await
        .unwrap_or_else(|error| fail(&error.to_string()))
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    process::exit(1);
}

fn usage(program: &str) -> ! {
    eprintln!("usage: {program} [--list | [--watch] [<session-id>]]");
    process::exit(2);
}

async fn list_sessions() {
    let servers = SessionClient::local_servers()
        .await
        .unwrap_or_else(|error| fail(&error.to_string()));
    if servers.is_empty() {
        println!("no local session servers");
        return;
    }
    for server in servers {
        match server.discover().await {
            Ok(discovery) => {
                for announcement in discovery.sessions() {
                    println!(
                        "{} {} process={} session={}",
                        server.url(),
                        announcement.id(),
                        announcement.process_name(),
                        announcement.session_name().unwrap_or("-"),
                    );
                }
            }
            Err(error) => println!("{} unavailable: {error}", server.url()),
        }
    }
}

async fn select_session(
    client: &SessionClient,
    requested_id: Option<&str>,
) -> Result<SessionAnnouncement, String> {
    let discovery = client
        .discover()
        .await
        .map_err(|error| format!("failed to query session server: {error}"))?;
    if let Some(requested_id) = requested_id {
        return discovery
            .session(requested_id)
            .cloned()
            .ok_or_else(|| format!("no discoverable session named `{requested_id}`"));
    }

    let announcements = discovery.sessions();
    match announcements {
        [] => Err(String::from("no discovered sessions")),
        [announcement] => Ok(announcement.clone()),
        _ => Err(format!(
            "found {} discovered sessions; run `session_consumer --list` or specify a session id",
            announcements.len()
        )),
    }
}
