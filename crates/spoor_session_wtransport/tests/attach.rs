// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end local attach coverage over a real WebTransport connection.

use spoor_core::{Context, ContextId, CounterDescId, LabelKey, MarkerKind, MarkerPhase, Unit};
use spoor_runtime::{
    ContextSnapshot, CounterKind, CounterSnapshot, Observation, Snapshot, SnapshotMetadata,
    TimedObservation,
};
use spoor_session::{Chunking, OwnedSessionPayload};
use spoor_session_wtransport::{
    SessionAnnouncement, SessionIdentity, SessionRequestKind, SessionServer, SessionServerConfig,
    discover_sessions, fetch_named_session,
};

fn demo_snapshot() -> Snapshot {
    Snapshot {
        metadata: SnapshotMetadata {
            process_name: String::from("attach_test"),
            session_name: Some(String::from("e2e")),
            session_sequence: 6,
        },
        capture_start_micros: 0,
        captured_at_micros: 40,
        contexts: vec![
            ContextSnapshot {
                id: ContextId::new(0),
                context: Context::root(),
            },
            ContextSnapshot {
                id: ContextId::new(1),
                context: Context::root().with(LabelKey::Frame, 7_u32),
            },
        ],
        counters: vec![CounterSnapshot {
            desc_id: CounterDescId::new(1),
            name: "upload_bytes",
            unit: Unit::Bytes,
            context_id: ContextId::new(1),
            kind: CounterKind::Counter,
            value: 4096,
            updated_at_micros: 20,
        }],
        events: vec![TimedObservation {
            at_micros: 25,
            thread_id: 3,
            observation: Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context_id: ContextId::new(1),
            },
        }],
        dropped_contexts: 0,
        dropped_counters: 0,
        dropped_events: 1,
    }
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
                SessionRequestKind::Snapshot { session_id }
                    if session_id == served_announcement.id =>
                {
                    request
                        .respond_snapshot(demo_snapshot(), Chunking::new(1, 1, 1))
                        .await
                        .expect("snapshot response should succeed");
                }
                SessionRequestKind::Snapshot { session_id } => {
                    request
                        .respond_error(&format!("unknown session `{session_id}`"))
                        .await
                        .expect("error response should succeed");
                }
            }
        }
    });

    let sessions = discover_sessions(&url, certificate_hash.clone())
        .await
        .expect("discovery should succeed");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "e2e");
    assert_eq!(sessions[0].process_name, "attach_test");
    assert_eq!(sessions[0].session_name.as_deref(), Some("e2e"));

    let envelopes = fetch_named_session(&url, certificate_hash.clone(), "e2e")
        .await
        .expect("snapshot fetch should succeed");

    assert!(matches!(
        envelopes.first().map(|envelope| &envelope.payload),
        Some(OwnedSessionPayload::SnapshotStart(summary))
            if summary.metadata.process_name == "attach_test"
    ));
    assert!(matches!(
        envelopes.last().map(|envelope| &envelope.payload),
        Some(OwnedSessionPayload::SnapshotEnd(dropped)) if dropped.events == 1
    ));
    let counter = envelopes
        .iter()
        .find_map(|envelope| match &envelope.payload {
            OwnedSessionPayload::Counters(rows) => rows.first(),
            _ => None,
        })
        .expect("publication should carry one counter row");
    assert_eq!(counter.name, "upload_bytes");
    assert_eq!(counter.unit, Unit::Bytes);
    let event = envelopes
        .iter()
        .find_map(|envelope| match &envelope.payload {
            OwnedSessionPayload::Events(rows) => rows.first(),
            _ => None,
        })
        .expect("publication should carry one event row");
    assert_eq!(event.thread_id, 3);

    let error = fetch_named_session(&url, certificate_hash, "missing")
        .await
        .expect_err("fetching an unknown session should fail");
    assert!(error.to_string().contains("unknown session"));

    server_task.await.expect("server task should not panic");
}
