// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session consumer demo for Footfall.

mod session_summary;

use footfall_session_wtransport::{
    SessionAnnouncement, discover_local_sessions, fetch_local_session,
};
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

    let announcement = select_session(argument.as_deref())
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let envelopes = fetch_local_session(&announcement.id)
        .await
        .expect("session client should fetch one snapshot");
    let summary = summarize(&envelopes);

    println!(
        "fetched {} envelopes from session {}",
        envelopes.len(),
        announcement.id
    );
    print_summary(&summary);
}

fn usage(program: &str) -> ! {
    eprintln!("usage: {program} [--list | <session-id>]");
    process::exit(2);
}

async fn list_sessions() {
    let announcements = discover_local_sessions()
        .await
        .expect("session discovery request should succeed");
    if announcements.is_empty() {
        println!("no discovered sessions");
        return;
    }
    for announcement in announcements {
        println!(
            "{} process={} session={}",
            announcement.id,
            announcement.process_name,
            announcement.session_name.as_deref().unwrap_or("-"),
        );
    }
}

async fn select_session(requested_id: Option<&str>) -> Result<SessionAnnouncement, String> {
    let announcements = discover_local_sessions()
        .await
        .map_err(|error| format!("failed to query session server: {error}"))?;
    if let Some(requested_id) = requested_id {
        return announcements
            .into_iter()
            .find(|announcement| announcement.id == requested_id)
            .ok_or_else(|| format!("no discoverable session named `{requested_id}`"));
    }

    match announcements.as_slice() {
        [] => Err(String::from("no discovered sessions")),
        [announcement] => Ok(announcement.clone()),
        _ => Err(format!(
            "found {} discovered sessions; run `session_consumer --list` or specify a session id",
            announcements.len()
        )),
    }
}
