// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Codec and request-framing tests.

use super::{SessionAnnouncement, SessionRequestKind, map_finish_error};
use wtransport::{VarInt, error::StreamWriteError};

#[test]
fn certificate_hash_displays_as_hex_and_round_trips() {
    let hash = super::CertificateHash::from_bytes([0xab; 32]);
    assert_eq!(hash.to_string(), "ab".repeat(32));
    assert_eq!(hash.as_bytes(), &[0xab; 32]);
    assert_eq!(super::CertificateHash::from_digest(&hash.to_digest()), hash);
}

#[test]
fn identity_renewal_tracks_certificate_validity() {
    let fresh = wtransport::Identity::self_signed_builder()
        .subject_alt_names(["localhost"])
        .from_now_utc()
        .validity_days(14)
        .build()
        .expect("fresh identity should build");
    let fresh = super::SessionIdentity::from_identity(fresh)
        .expect("fresh identity should have a certificate");
    assert!(!fresh.needs_renewal());

    let expiring = wtransport::Identity::self_signed_builder()
        .subject_alt_names(["localhost"])
        .from_now_utc()
        .validity_days(1)
        .build()
        .expect("expiring identity should build");
    let expiring = super::SessionIdentity::from_identity(expiring)
        .expect("expiring identity should have a certificate");
    assert!(expiring.needs_renewal());
}

#[test]
fn stopped_finish_is_treated_as_non_fatal() {
    let result = map_finish_error(Err(StreamWriteError::Stopped(VarInt::from_u32(0))));
    assert!(result.is_ok(), "stopped finish should not fail publication");
}

#[test]
fn session_request_kind_reports_session_id() {
    let session = SessionAnnouncement::new("wt_live", "demo", Some(String::from("capture")));
    assert_eq!(session.id, "wt_live");
    assert_eq!(session.process_name, "demo");
    assert_eq!(session.session_name.as_deref(), Some("capture"));
    assert_eq!(SessionRequestKind::Discover.session_id(), None);
    assert_eq!(
        SessionRequestKind::Snapshot {
            session_id: String::from("wt_live"),
            since: None,
        }
        .session_id(),
        Some("wt_live")
    );
}
