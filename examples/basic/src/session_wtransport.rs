// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end WebTransport session demo for Footfall.

mod session_demo;
mod session_summary;

use footfall_session::Chunking;
use footfall_session_wtransport::{
    ServedRequestKind, SessionAnnouncement, SessionIdentity, SessionRequestKind, SessionServer,
    SessionServerConfig, default_local_url, discover_local_sessions, fetch_local_session,
};
use session_demo::DemoCapture;
use session_summary::{print_summary, summarize};

#[tokio::main]
async fn main() {
    let capture = DemoCapture::install("session_wtransport", "wt_demo");
    let identity = SessionIdentity::load_or_create_localhost()
        .await
        .expect("local localhost identity should load or be created");
    let server = SessionServer::bind(&identity, SessionServerConfig::local_default())
        .expect("session server should bind");
    let announcement = SessionAnnouncement::new(
        "wt_demo",
        "session_wtransport",
        Some(String::from("wt_demo")),
    );
    let served_announcement = announcement.clone();

    println!("server: {}", default_local_url());
    println!("session: {}", announcement.id);

    let server_task = tokio::spawn(async move {
        let mut served_snapshot = false;
        while !served_snapshot {
            let request = server
                .accept_request()
                .await
                .expect("session server should accept the next request");
            match request.kind().clone() {
                SessionRequestKind::Discover => {
                    request
                        .respond_discovery(core::slice::from_ref(&served_announcement))
                        .await
                        .expect("discovery response should succeed");
                }
                SessionRequestKind::Snapshot { session_id }
                    if session_id == served_announcement.id =>
                {
                    let snapshot = capture.capture_frame(11);
                    let served = request
                        .respond_snapshot(snapshot, Chunking::new(2, 2, 2, 16))
                        .await
                        .expect("snapshot response should succeed");
                    if let ServedRequestKind::Snapshot { .. } = served.kind {
                        served_snapshot = true;
                    }
                }
                SessionRequestKind::Snapshot { session_id } => {
                    request
                        .respond_error(&format!("unknown session `{session_id}`"))
                        .await
                        .expect("error response should succeed");
                }
            }
        }
    });

    let sessions = discover_local_sessions()
        .await
        .expect("session discovery should succeed");
    let discovered = sessions
        .into_iter()
        .find(|session| session.id == announcement.id)
        .expect("published session should be discoverable");
    let envelopes = fetch_local_session(&discovered.id)
        .await
        .expect("session client should fetch one snapshot");
    server_task
        .await
        .expect("session server task should not panic");
    let summary = summarize(&envelopes);

    print_summary(&summary);
}
