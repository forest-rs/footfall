// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session server demo for Spoor.

mod session_demo;

use session_demo::DemoCapture;
use spoor_session::Chunking;
use spoor_session_wtransport::{
    ServedRequestKind, SessionAnnouncement, SessionIdentity, SessionRequestKind, SessionServer,
    SessionServerConfig, default_local_url,
};
use tokio::signal;

#[tokio::main]
async fn main() {
    let identity = SessionIdentity::load_or_create_localhost()
        .await
        .expect("local localhost identity should load or be created");
    let server = SessionServer::bind(&identity, SessionServerConfig::local_default())
        .expect("session server should bind");
    let announcement =
        SessionAnnouncement::new("wt_live", "session_server", Some(String::from("wt_live")));
    let capture = DemoCapture::install("session_server", "wt_live");

    println!("serving Spoor sessions over WebTransport");
    println!("server: {}", default_local_url());
    println!("session: {}", announcement.id);
    println!("press Ctrl-C to stop");

    for frame in 0_u32.. {
        tokio::select! {
            ctrl_c = signal::ctrl_c() => {
                ctrl_c.expect("Ctrl-C handler should remain active");
                return;
            }
            request = server.accept_request() => {
                match request {
                    Ok(request) => match request.kind().clone() {
                        SessionRequestKind::Discover => {
                            let served = request
                                .respond_discovery(core::slice::from_ref(&announcement))
                                .await
                                .expect("discovery response should succeed");
                            if let ServedRequestKind::Discovery { session_count } = served.kind {
                                println!(
                                    "served discovery with {} session(s) to {}{}",
                                    session_count, served.authority, served.path
                                );
                            }
                        }
                        SessionRequestKind::Snapshot { session_id } if session_id == announcement.id => {
                            let snapshot = capture.capture_frame(frame);
                            let served = request
                                .respond_snapshot(snapshot, Chunking::new(4, 4, 4, 32))
                                .await
                                .expect("snapshot response should succeed");
                            if let ServedRequestKind::Snapshot { envelope_count } = served.kind {
                                println!(
                                    "served frame {} as {} envelopes to {}{}",
                                    frame, envelope_count, served.authority, served.path
                                );
                            }
                        }
                        SessionRequestKind::Snapshot { session_id } => {
                            request
                                .respond_error(&format!("unknown session `{session_id}`"))
                                .await
                                .expect("error response should succeed");
                        }
                    }
                    Err(error) => {
                        eprintln!("serve failed for frame {frame}: {error}");
                    }
                }
            }
        }
    }
}
