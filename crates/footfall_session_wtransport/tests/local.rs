// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Local servers register themselves, so consumers find every producer on
//! the machine.

use footfall_runtime::Runtime;
use footfall_session_wtransport::{LocalSessionServer, ServedSession, SessionClient};

#[test]
fn every_local_server_is_found_and_unregisters_on_drop() {
    // The registry lives in Footfall's state directory. Run the check in a
    // child process pointed at a fresh one, so it neither sees nor disturbs
    // producers already running on this machine.
    if std::env::var_os("FOOTFALL_STATE_DIR").is_none() {
        let state = std::env::temp_dir().join(format!(
            "footfall-local-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock is after the epoch")
                .as_nanos()
        ));
        let status = std::process::Command::new(
            std::env::current_exe().expect("the test binary has a path"),
        )
        .args([
            "--exact",
            "every_local_server_is_found_and_unregisters_on_drop",
        ])
        .env("FOOTFALL_STATE_DIR", &state)
        .status()
        .expect("the test binary should run");
        let _ = std::fs::remove_dir_all(&state);
        assert!(status.success(), "the check failed in the child process");
        return;
    }

    let tokio = tokio::runtime::Runtime::new().expect("tokio should start");
    tokio.block_on(async {
        let first_runtime: &'static Runtime =
            Box::leak(Box::new(Runtime::builder().process_name("first").build()));
        let second_runtime: &'static Runtime =
            Box::leak(Box::new(Runtime::builder().process_name("second").build()));
        let first = LocalSessionServer::start(vec![ServedSession::new("a", first_runtime)])
            .await
            .expect("the first server should start");
        let second = LocalSessionServer::start(vec![ServedSession::new("b", second_runtime)])
            .await
            .expect("a second server should start alongside the first");
        assert_ne!(first.url(), second.url());

        let servers = SessionClient::local_servers()
            .await
            .expect("the registry should be readable");
        let mut names = Vec::new();
        for server in &servers {
            let discovery = server.discover().await.expect("each server answers");
            for session in discovery.sessions() {
                names.push(session.process_name().to_owned());
            }
        }
        names.sort();
        assert_eq!(names, ["first", "second"]);

        drop(second);
        let servers = SessionClient::local_servers()
            .await
            .expect("the registry should be readable");
        assert_eq!(servers.len(), 1, "a dropped server unregisters");
        drop(first);
    });
}
