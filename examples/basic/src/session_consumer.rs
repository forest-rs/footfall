// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session consumer demo for Footfall.

mod session_summary;

use footfall_session_wtransport::{SessionAnnouncement, SessionClient};
use session_summary::{print_summary, summarize};
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
    if args.next().is_some() {
        usage(&program);
    }

    let client = local_client().await;
    let announcement = select_session(&client, argument.as_deref())
        .await
        .unwrap_or_else(|error| fail(&error));
    let snapshot = client
        .fetch(announcement.id(), None)
        .await
        .unwrap_or_else(|error| fail(&error.to_string()));

    println!("fetched session {}", announcement.id());
    print_summary(&summarize(&snapshot));
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
    eprintln!("usage: {program} [--list | <session-id>]");
    process::exit(2);
}

async fn list_sessions() {
    let discovery = local_client()
        .await
        .discover()
        .await
        .unwrap_or_else(|error| fail(&error.to_string()));
    let announcements = discovery.sessions();
    if announcements.is_empty() {
        println!("no discovered sessions");
        return;
    }
    for announcement in announcements {
        println!(
            "{} process={} session={}",
            announcement.id(),
            announcement.process_name(),
            announcement.session_name().unwrap_or("-"),
        );
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
