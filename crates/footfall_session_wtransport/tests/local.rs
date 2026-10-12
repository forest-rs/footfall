// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Local servers register themselves, so consumers find every producer on
//! the machine.

use footfall_runtime::Runtime;
use footfall_session_wtransport::{LocalSessionServer, ServedSession, SessionClient};

/// Runs the calling test again in a child process with a fresh
/// `FOOTFALL_STATE_DIR`, so it neither sees nor disturbs producers already
/// running on this machine, and returns `false` in the parent, which should
/// then return; returns `true` in the child.
///
/// The child logs the server's and the transport's debug events, which the
/// test harness prints if the test fails.
fn in_fresh_state_dir(test: &str) -> bool {
    if std::env::var_os("FOOTFALL_STATE_DIR").is_some() {
        log_transport_events();
        return true;
    }
    let state = std::env::temp_dir().join(format!(
        "footfall-{test}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
    ));
    let status =
        std::process::Command::new(std::env::current_exe().expect("the test binary has a path"))
            .args(["--exact", test])
            .env("FOOTFALL_STATE_DIR", &state)
            .status()
            .expect("the test binary should run");
    let _ = std::fs::remove_dir_all(&state);
    assert!(status.success(), "{test} failed in the child process");
    false
}

/// Sends debug events from the session server, WebTransport, and QUIC to the
/// test's captured output.
fn log_transport_events() {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::prelude::*;
    let targets = Targets::new()
        .with_target("footfall_session_wtransport", LevelFilter::DEBUG)
        .with_target("wtransport", LevelFilter::DEBUG)
        .with_target("quinn", LevelFilter::DEBUG)
        .with_target("quinn_proto", LevelFilter::DEBUG);
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_test_writer()
                .with_ansi(false)
                .with_filter(targets),
        )
        .try_init();
}

#[test]
fn every_local_server_is_found_and_unregisters_on_drop() {
    if !in_fresh_state_dir("every_local_server_is_found_and_unregisters_on_drop") {
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
    if !in_fresh_state_dir("duplicate_session_ids_and_stale_registrations_are_reported") {
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

#[test]
fn producers_started_together_are_all_reachable() {
    if !in_fresh_state_dir("producers_started_together_are_all_reachable") {
        return;
    }
    let tokio = tokio::runtime::Runtime::new().expect("tokio should start");
    tokio.block_on(async {
        // Started at once in an empty state directory, these may create
        // different identities; each registers its own pin.
        let runtimes: Vec<&'static Runtime> = (0..3)
            .map(|index| {
                let runtime: &'static Runtime = Box::leak(Box::new(
                    Runtime::builder()
                        .process_name(format!("producer-{index}"))
                        .build(),
                ));
                runtime
            })
            .collect();
        let starts: Vec<_> = runtimes
            .iter()
            .map(|&runtime| {
                tokio::spawn(LocalSessionServer::start(vec![ServedSession::new(
                    "main", runtime,
                )]))
            })
            .collect();
        let mut servers = Vec::new();
        for start in starts {
            servers.push(
                start
                    .await
                    .expect("the start task should not panic")
                    .expect("each server should start"),
            );
        }
        let discovery = SessionClient::discover_local(std::time::Duration::from_secs(2))
            .await
            .expect("discovery should succeed");
        assert!(
            discovery.unreachable().is_empty(),
            "{:?}",
            discovery.unreachable()
        );
        assert_eq!(discovery.sessions().len(), 3);
        drop(servers);
    });
}

#[test]
fn a_graceful_shutdown_sends_subscribers_a_final_window() {
    use footfall_core::{Backend as _, ContextHandle, MarkerKind, MarkerPhase};
    if !in_fresh_state_dir("a_graceful_shutdown_sends_subscribers_a_final_window") {
        return;
    }
    let tokio = tokio::runtime::Runtime::new().expect("tokio should start");
    tokio.block_on(async {
        let runtime: &'static Runtime = Box::leak(Box::new(Runtime::default()));
        let server = LocalSessionServer::start(vec![ServedSession::new("main", runtime)])
            .await
            .expect("the server should start");
        let mut subscription = server
            .client()
            .subscribe("main", None, std::time::Duration::from_millis(10))
            .await
            .expect("subscription should open");
        subscription.next().await.expect("a first snapshot");

        // Events recorded after the last read, then a graceful stop.
        for _ in 0..3 {
            runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
        }
        server.shutdown(std::time::Duration::from_secs(2)).await;

        let last = subscription.next().await.expect("the final window");
        assert_eq!(
            last.events().len(),
            3,
            "nothing recorded before the stop is lost"
        );
        let error = subscription
            .next()
            .await
            .expect_err("the server has stopped");
        assert!(
            matches!(error, footfall_session_wtransport::ClientError::Closed),
            "{error:?}"
        );
    });
}
