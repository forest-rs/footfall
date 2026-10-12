// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end WebTransport session demo for Footfall: one process serves its
//! runtime and reads it back through the session client.

mod session_demo;
mod session_summary;

use footfall_session_wtransport::{LocalSessionServer, ServedSession};
use session_demo::DemoCapture;
use session_summary::{print_summary, summarize};

#[tokio::main]
async fn main() {
    let capture = DemoCapture::install("session_wtransport", "wt_demo");
    let server = LocalSessionServer::start(vec![ServedSession::new("wt_demo", capture.runtime())])
        .await
        .expect("local session server should start");
    println!("server: {}", server.url());

    let client = server.client();
    let discovery = client
        .discover()
        .await
        .expect("session discovery should succeed");
    let session = discovery
        .session("wt_demo")
        .expect("the served session should be discoverable");

    capture.record_frame(10);
    let first = client
        .fetch(session.id(), None)
        .await
        .expect("session client should fetch a snapshot");
    println!("first read: {} events", first.events().len());

    capture.record_frame(11);
    let second = client
        .fetch(session.id(), Some(first.next_cursor()))
        .await
        .expect("session client should fetch a snapshot");
    println!(
        "second read, from the first's cursor: {} events",
        second.events().len()
    );
    print_summary(&summarize(&second));
}
