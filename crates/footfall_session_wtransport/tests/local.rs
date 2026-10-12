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

#[test]
fn duplicate_session_ids_and_stale_registrations_are_reported() {
    // As above, run in a child process with a state directory of its own.
    if std::env::var_os("FOOTFALL_STATE_DIR").is_none() {
        let state = std::env::temp_dir().join(format!(
            "footfall-local-dup-test-{}",
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
            "duplicate_session_ids_and_stale_registrations_are_reported",
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
        // Nothing registered yet: an empty discovery, not an error.
        let empty = SessionClient::discover_local(std::time::Duration::from_secs(1))
            .await
            .expect("no producers is not an error");
        assert!(empty.sessions().is_empty());

        let first_runtime: &'static Runtime =
            Box::leak(Box::new(Runtime::builder().process_name("first").build()));
        let second_runtime: &'static Runtime =
            Box::leak(Box::new(Runtime::builder().process_name("second").build()));
        let _first = LocalSessionServer::start(vec![ServedSession::new("main", first_runtime)])
            .await
            .expect("the first server should start");
        let _second = LocalSessionServer::start(vec![ServedSession::new("main", second_runtime)])
            .await
            .expect("the second server should start");

        // A registration left behind by a producer that exited.
        let registry = std::path::Path::new(
            &std::env::var_os("FOOTFALL_STATE_DIR").expect("set by the parent"),
        )
        .join("wtransport")
        .join("servers");
        let free_port = std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.local_addr())
            .expect("an ephemeral port")
            .port();
        let stale = registry.join(free_port.to_string());
        std::fs::write(
            &stale,
            format!("https://127.0.0.1:{free_port}/footfall/session"),
        )
        .expect("the registry is writable");

        let discovery = SessionClient::discover_local(std::time::Duration::from_secs(1))
            .await
            .expect("discovery should succeed");
        assert!(!stale.exists(), "the stale registration is pruned");
        assert!(discovery.unreachable().is_empty());
        assert_eq!(discovery.sessions().len(), 2);
        let error = discovery
            .session("main")
            .expect_err("two producers offer main");
        let message = error.to_string();
        assert!(
            message.contains("first") && message.contains("second"),
            "{message}"
        );
        assert!(
            discovery
                .sessions()
                .iter()
                .all(|session| session.announcement().instance_id() != 0)
        );
    });
}
