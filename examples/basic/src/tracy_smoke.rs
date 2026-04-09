// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal direct `tracy-client` smoke test.

use std::{
    thread::sleep,
    time::{Duration, Instant},
};
use tracy_client::Client;

const CONNECT_WAIT: Duration = Duration::from_secs(10);
const CONNECT_POLL: Duration = Duration::from_millis(100);
const RUN_TIME: Duration = Duration::from_secs(30);

fn main() {
    let client = Client::start();
    client.set_thread_name("tracy_smoke");

    println!(
        "tracy_smoke pid={} running={} connected={}",
        std::process::id(),
        Client::is_running(),
        Client::is_connected()
    );
    println!("waiting up to {:?} for Tracy to connect", CONNECT_WAIT);

    let deadline = Instant::now() + CONNECT_WAIT;
    while !Client::is_connected() && Instant::now() < deadline {
        sleep(CONNECT_POLL);
    }

    println!(
        "starting smoke loop for {:?} (connected={})",
        RUN_TIME,
        Client::is_connected()
    );

    let end = Instant::now() + RUN_TIME;
    let mut last_connected = Client::is_connected();
    let mut iteration = 0_u64;
    while Instant::now() < end {
        let connected = Client::is_connected();
        if connected && !last_connected {
            println!("Tracy connected after {} iterations", iteration);
        }
        last_connected = connected;

        {
            let span = client.clone().span_alloc(
                Some("smoke_loop"),
                "tracy_smoke_main",
                file!(),
                line!(),
                0,
            );
            span.emit_text("direct tracy-client smoke loop");
            span.emit_value(iteration);
            client.plot(
                tracy_client::PlotName::new_leak(String::from("smoke_iterations")),
                iteration as f64,
            );
            sleep(Duration::from_millis(50));
        }

        iteration = iteration.saturating_add(1);
    }

    println!("done (connected={})", Client::is_connected());
}
