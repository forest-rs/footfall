// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A served runtime answers discovery, cursor reads, and subscriptions.

use footfall_core::{Backend as _, ContextHandle, InstrumentationLevel, MarkerKind, MarkerPhase};
use footfall_runtime::Runtime;
use footfall_session::SessionErrorCode;
use footfall_session_wtransport::{
    ServedSession, SessionClient, SessionIdentity, SessionServer, SessionServerConfig,
};
use std::time::Duration;

fn runtime() -> &'static Runtime {
    Box::leak(Box::new(
        Runtime::builder()
            .process_name("serve_test")
            .session_name("capture")
            .level(InstrumentationLevel::Trace)
            .build(),
    ))
}

static UPLOADS: footfall_core::CounterDesc = footfall_core::CounterDesc::bytes("uploads");

fn mark(runtime: &Runtime) {
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
}

#[tokio::test]
async fn served_runtimes_answer_discovery_reads_and_subscriptions() {
    let runtime = runtime();
    let identity = SessionIdentity::self_signed_localhost().expect("identity should build");
    let server = SessionServer::bind(&identity, SessionServerConfig::default())
        .expect("server should bind an ephemeral localhost port");
    let client = SessionClient::new(
        server.url().expect("bound server should report its URL"),
        identity.certificate_hash(),
    );
    let serving = tokio::spawn(server.serve(vec![ServedSession::new("main", runtime)]));

    let discovery = client.discover().await.expect("discovery should succeed");
    let main = discovery.session("main").expect("the runtime is offered");
    assert_eq!(main.process_name(), "serve_test");
    assert_eq!(main.session_name(), Some("capture"));

    mark(runtime);
    mark(runtime);
    let first = client
        .fetch("main", None)
        .await
        .expect("fetch should succeed");
    assert_eq!(first.events().len(), 2);
    assert_eq!(first.metadata().instance_id(), runtime.instance_id());

    mark(runtime);
    let next = client
        .fetch("main", Some(first.next_cursor()))
        .await
        .expect("fetch should succeed");
    assert_eq!(next.events().len(), 1, "only the event since the cursor");
    assert_eq!(next.window().missed(), 0);

    runtime.counter_add(&UPLOADS, ContextHandle::ROOT, 5);
    let mut subscription = client
        .subscribe("main", Some(next.next_cursor()), Duration::from_millis(20))
        .await
        .expect("subscription should open");
    let immediate = subscription.next().await.expect("a first snapshot");
    assert!(immediate.events().is_empty(), "nothing new yet");
    let uploads = |snapshot: &footfall_session::OwnedSnapshot| {
        snapshot
            .counters()
            .iter()
            .find(|counter| counter.name() == "uploads")
            .map(footfall_session::OwnedCounterSnapshot::value)
    };
    assert_eq!(uploads(&immediate), Some(5));
    mark(runtime);
    runtime.counter_add(&UPLOADS, ContextHandle::ROOT, 2);
    let later = loop {
        let snapshot = subscription.next().await.expect("later snapshots arrive");
        if !snapshot.events().is_empty() {
            break snapshot;
        }
    };
    assert_eq!(
        later.events().len(),
        1,
        "each snapshot holds only new events"
    );
    assert_eq!(
        later.kind(),
        footfall_session::SnapshotKind::Full,
        "the client applies changes to its state"
    );
    assert_eq!(uploads(&later), Some(7), "counters stay current");
    assert_eq!(later.contexts().len(), immediate.contexts().len());
    drop(subscription);

    let error = client
        .fetch("other", None)
        .await
        .expect_err("no such session");
    assert_eq!(error.server_code(), Some(SessionErrorCode::NotFound));
    assert!(error.to_string().contains("[\"main\"]"), "{error}");

    serving.abort();
}

#[tokio::test]
async fn a_slow_subscriber_gets_fresh_snapshots_and_a_closed_server_says_so() {
    let runtime = runtime();
    let identity = SessionIdentity::self_signed_localhost().expect("identity should build");
    let server = SessionServer::bind(&identity, SessionServerConfig::default())
        .expect("server should bind an ephemeral localhost port");
    let client = SessionClient::new(
        server.url().expect("bound server should report its URL"),
        identity.certificate_hash(),
    );
    let serving = tokio::spawn(server.serve(vec![ServedSession::new("main", runtime)]));

    let mut subscription = client
        .subscribe("main", None, Duration::from_millis(10))
        .await
        .expect("subscription should open");
    let first = subscription.next().await.expect("a first snapshot");

    // Read far more slowly than the interval while the producer keeps
    // emitting: each snapshot must still be taken when it is asked for.
    for round in 0..3 {
        for _ in 0..50 {
            mark(runtime);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let asked_at = std::time::Instant::now();
        let snapshot = subscription.next().await.expect("a later snapshot");
        let age = runtime_now_micros(runtime).saturating_sub(snapshot.captured_at_micros());
        assert!(
            age < 100_000,
            "round {round}: snapshot is {age} µs old, so snapshots queued"
        );
        assert!(asked_at.elapsed() < Duration::from_millis(150));
        assert_eq!(
            snapshot.events().len(),
            50,
            "everything since the last read"
        );
    }
    assert_eq!(first.metadata().instance_id(), runtime.instance_id());

    serving.abort();
    let _ = serving.await;
    let error = subscription.next().await.expect_err("the server is gone");
    assert!(
        matches!(
            error,
            footfall_session_wtransport::ClientError::Closed
                | footfall_session_wtransport::ClientError::TimedOut { .. }
        ),
        "{error:?}"
    );
}

/// The runtime's clock now, in microseconds since its creation.
fn runtime_now_micros(runtime: &Runtime) -> u64 {
    runtime.snapshot().captured_at_micros()
}
