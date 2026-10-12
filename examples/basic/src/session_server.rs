// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session server demo for Footfall: records a frame every
//! 100 ms and serves the runtime to local consumers until Ctrl-C.

mod session_demo;

use footfall_session_wtransport::{LocalSessionServer, ServedSession};
use session_demo::DemoCapture;
use std::time::Duration;
use tokio::signal;

#[tokio::main]
async fn main() {
    let capture = DemoCapture::install("session_server", "wt_live");
    let server = LocalSessionServer::start(vec![ServedSession::new("wt_live", capture.runtime())])
        .await
        .expect("local session server should start");

    println!("serving Footfall sessions at {}", server.url());
    println!("certificate hash: {}", server.certificate_hash());
    println!("session: wt_live");
    println!("press Ctrl-C to stop");

    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    for frame in 0_u32.. {
        tokio::select! {
            ctrl_c = signal::ctrl_c() => {
                ctrl_c.expect("Ctrl-C handler should remain active");
                return;
            }
            _ = ticker.tick() => capture.record_frame(frame),
        }
    }
}
