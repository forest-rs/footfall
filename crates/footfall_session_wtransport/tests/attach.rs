// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end local attach coverage over a real WebTransport connection.

use footfall_core::{Context, ContextId, CounterDescId, LabelKey, MarkerKind, MarkerPhase, Unit};
use footfall_runtime::{
    ContextSnapshot, CounterKind, CounterSnapshot, Observation, Snapshot, SnapshotMetadata,
    TimedObservation,
};
use footfall_session::{Chunking, EventCursor, EventWindow, PROTOCOL_VERSION, SessionErrorCode};
use footfall_session_wtransport::{
    SessionAnnouncement, SessionClient, SessionIdentity, SessionRequestKind, SessionServer,
    SessionServerConfig,
};

static FRAME: LabelKey = LabelKey::new("frame");

fn demo_snapshot() -> Snapshot {
    let metadata = SnapshotMetadata::new("attach_test")
        .with_session_name("e2e")
        .with_instance_id(0xfeed)
        .with_wall_clock_anchor_nanos(1_700_000_000_000_000);
    Snapshot::builder(metadata)
        .captured_at_nanos(40)
        .window(EventWindow::new(1, 2, 0))
        .context(ContextSnapshot::new(ContextId::new(0), Context::root()))
        .context(ContextSnapshot::new(
            ContextId::new(1),
            Context::root().with(&FRAME, 7_u32),
        ))
        .counter(CounterSnapshot::new(
            CounterDescId::new(1),
            "upload_bytes",
            Unit::Bytes,
            ContextId::new(1),
            CounterKind::Counter,
            4096,
            20,
        ))
        .event(TimedObservation::new(
            25,
            0,
            3,
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context_id: ContextId::new(1),
            },
        ))
        .drops(
            footfall_runtime::Drops::new()
                .with_refused_contexts(0)
                .with_counter_updates(0)
                .with_evicted_events(1),
        )
        .build()
}

#[tokio::test]
async fn discovery_and_snapshot_fetch_round_trip() {
    let identity =
        SessionIdentity::self_signed_localhost().expect("ephemeral identity should build");
    let certificate_hash = identity.certificate_hash();
    let server = SessionServer::bind(&identity, SessionServerConfig::default())
        .expect("server should bind an ephemeral localhost port");
    let url = server.url().expect("bound server should report its URL");

    let announcement = SessionAnnouncement::new("e2e", "attach_test", Some(String::from("e2e")));
    let served_announcement = announcement.clone();

    // The client drives exactly three sequential requests below.
    let server_task = tokio::spawn(async move {
        for _ in 0..3 {
            let request = server
                .accept_request()
                .await
                .expect("server should accept the next request");
            match request.kind().clone() {
                SessionRequestKind::Discover => {
                    request
                        .respond_discovery(core::slice::from_ref(&served_announcement))
                        .await
                        .expect("discovery response should succeed");
                }
                SessionRequestKind::Snapshot { session_id, since }
                    if session_id == served_announcement.id() =>
                {
                    assert_eq!(since, Some(EventCursor::new(0xfeed, 1)));
                    request
                        .respond_snapshot(demo_snapshot(), Chunking::new(1, 1, 1, 1))
                        .await
                        .expect("snapshot response should succeed");
                }
                SessionRequestKind::Snapshot { session_id, .. } => {
                    request
                        .respond_error(
                            SessionErrorCode::NotFound,
                            &format!("unknown session `{session_id}`"),
                        )
                        .await
                        .expect("error response should succeed");
                }
                _ => {
                    request
                        .respond_error(SessionErrorCode::Unsupported, "unsupported request")
                        .await
                        .expect("error response should succeed");
                }
            }
        }
    });

    let client = SessionClient::new(url, certificate_hash);
    let discovery = client.discover().await.expect("discovery should succeed");
    assert_eq!(discovery.protocol_version(), PROTOCOL_VERSION);
    let session = discovery.session("e2e").expect("the server offers e2e");
    assert_eq!(session.process_name(), "attach_test");
    assert_eq!(session.session_name(), Some("e2e"));

    let snapshot = client
        .fetch("e2e", Some(EventCursor::new(0xfeed, 1)))
        .await
        .expect("snapshot fetch should succeed");
    assert_eq!(snapshot.metadata().process_name(), "attach_test");
    assert_eq!(snapshot.next_cursor(), EventCursor::new(0xfeed, 2));
    assert_eq!(snapshot.drops().evicted_events(), 1);
    assert_eq!(snapshot.counters()[0].name(), "upload_bytes");
    assert_eq!(snapshot.counters()[0].unit(), Unit::Bytes);
    assert_eq!(snapshot.events()[0].thread_id(), 3);

    let error = client
        .fetch("missing", None)
        .await
        .expect_err("fetching an unknown session should fail");
    assert_eq!(error.server_code(), Some(SessionErrorCode::NotFound));
    assert!(error.to_string().contains("unknown session"));

    server_task.await.expect("server task should not panic");
}
