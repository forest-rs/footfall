// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end local attach coverage over a real WebTransport connection.

use footfall_core::{Context, ContextId, CounterDescId, LabelKey, MarkerKind, MarkerPhase, Unit};
use footfall_runtime::{
    ContextSnapshot, CounterKind, CounterSnapshot, Observation, Snapshot, SnapshotMetadata,
    TimedObservation,
};
use footfall_session::{Chunking, EventCursor, EventWindow, OwnedSessionPayload};
use footfall_session_wtransport::{
    SessionAnnouncement, SessionIdentity, SessionRequestKind, SessionServer, SessionServerConfig,
    discover_sessions, fetch_named_session,
};

static FRAME: LabelKey = LabelKey::new("frame");

fn demo_snapshot() -> Snapshot {
    let metadata = SnapshotMetadata::new("attach_test")
        .with_session_name("e2e")
        .with_instance_id(0xfeed)
        .with_wall_clock_anchor_micros(1_700_000_000_000_000);
    Snapshot::builder(metadata)
        .captured_at_micros(40)
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
                        .respond_error(&format!("unknown session `{session_id}`"))
                        .await
                        .expect("error response should succeed");
                }
                _ => {
                    request
                        .respond_error("unsupported request")
                        .await
                        .expect("error response should succeed");
                }
            }
        }
    });

    let sessions = discover_sessions(&url, certificate_hash)
        .await
        .expect("discovery should succeed");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id(), "e2e");
    assert_eq!(sessions[0].process_name(), "attach_test");
    assert_eq!(sessions[0].session_name(), Some("e2e"));

    let envelopes = fetch_named_session(
        &url,
        certificate_hash,
        "e2e",
        Some(EventCursor::new(0xfeed, 1)),
    )
    .await
    .expect("snapshot fetch should succeed");

    assert!(matches!(
        envelopes.first().map(|envelope| envelope.payload()),
        Some(OwnedSessionPayload::SnapshotStart(summary))
            if summary.metadata().process_name() == "attach_test"
                && summary.next_cursor() == EventCursor::new(0xfeed, 2)
    ));
    assert!(matches!(
        envelopes.last().map(|envelope| envelope.payload()),
        Some(OwnedSessionPayload::SnapshotEnd(drops)) if drops.evicted_events() == 1
    ));
    let counter = envelopes
        .iter()
        .find_map(|envelope| match envelope.payload() {
            OwnedSessionPayload::Counters(rows) => rows.first(),
            _ => None,
        })
        .expect("publication should carry one counter row");
    assert_eq!(counter.name(), "upload_bytes");
    assert_eq!(counter.unit(), Unit::Bytes);
    let event = envelopes
        .iter()
        .find_map(|envelope| match envelope.payload() {
            OwnedSessionPayload::Events(rows) => rows.first(),
            _ => None,
        })
        .expect("publication should carry one event row");
    assert_eq!(event.thread_id(), 3);

    let error = fetch_named_session(&url, certificate_hash, "missing", None)
        .await
        .expect_err("fetching an unknown session should fail");
    assert!(error.to_string().contains("unknown session"));

    server_task.await.expect("server task should not panic");
}
