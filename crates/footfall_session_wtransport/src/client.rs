// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Discovering and fetching sessions.

use super::*;

/// Fetches one snapshot publication from a WebTransport session server.
pub async fn fetch_session(
    url: &str,
    certificate_hash: CertificateHash,
) -> io::Result<Vec<OwnedSessionEnvelope>> {
    let sessions = discover_sessions(url, certificate_hash).await?;
    match sessions.as_slice() {
        [] => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no discoverable sessions on this server",
        )),
        [session] => fetch_named_session(url, certificate_hash, &session.id, None).await,
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multiple discoverable sessions available; choose one explicitly",
        )),
    }
}

/// Returns the default localhost WebTransport server URL for Footfall sessions.
#[must_use]
pub fn default_local_url() -> String {
    format!("https://127.0.0.1:{DEFAULT_LOCAL_PORT}{DEFAULT_PATH}")
}

/// Discovers sessions from the default local Footfall session server.
pub async fn discover_local_sessions() -> io::Result<Vec<SessionAnnouncement>> {
    discover_sessions(
        &default_local_url(),
        SessionIdentity::load_localhost_certificate_hash().await?,
    )
    .await
}

/// Fetches one named session from the default local Footfall session server.
///
/// See [`fetch_named_session`] for `since`.
pub async fn fetch_local_session(
    session_id: &str,
    since: Option<EventCursor>,
) -> io::Result<Vec<OwnedSessionEnvelope>> {
    fetch_named_session(
        &default_local_url(),
        SessionIdentity::load_localhost_certificate_hash().await?,
        session_id,
        since,
    )
    .await
}

/// Fetches the currently discoverable sessions from a session server.
pub async fn discover_sessions(
    url: &str,
    certificate_hash: CertificateHash,
) -> io::Result<Vec<SessionAnnouncement>> {
    let client_config = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([certificate_hash.to_digest()])
        .build();
    let endpoint = Endpoint::client(client_config)?;
    let connection = endpoint
        .connect(url)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    discover_connection(connection).await
}

/// Fetches one named snapshot publication from a session server.
///
/// With `since`, the snapshot's events are those recorded after the
/// consumer's previous read: pass the previous publication's
/// [`OwnedSnapshotSummary::next_cursor`]. Without it, they are every event
/// the producer retains.
pub async fn fetch_named_session(
    url: &str,
    certificate_hash: CertificateHash,
    session_id: &str,
    since: Option<EventCursor>,
) -> io::Result<Vec<OwnedSessionEnvelope>> {
    let client_config = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([certificate_hash.to_digest()])
        .build();
    let endpoint = Endpoint::client(client_config)?;
    let connection = endpoint
        .connect(url)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    fetch_named_connection(connection, session_id, since).await
}

pub(crate) async fn discover_connection(
    connection: Connection,
) -> io::Result<Vec<SessionAnnouncement>> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|error| io::Error::other(error.to_string()))?
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;

    send.write_all(DISCOVER_MAGIC)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    finish_request_stream(&mut send).await?;

    match read_u8_stream(&mut recv).await? {
        RESPONSE_DISCOVERY => {
            let count = read_u64(&mut recv).await?;
            validate_wire_count(count, MAX_DISCOVERED_SESSIONS, "session announcements")?;
            let count = usize::try_from(count).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "session announcement count does not fit usize",
                )
            })?;
            let mut sessions = Vec::with_capacity(count);
            for _ in 0..count {
                sessions.push(decode_session_announcement_stream(&mut recv).await?);
            }
            Ok(sessions)
        }
        RESPONSE_ERROR => Err(io::Error::other(read_stream_string(&mut recv).await?)),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected discovery response tag: {tag}"),
        )),
    }
}

pub(crate) async fn fetch_named_connection(
    connection: Connection,
    session_id: &str,
    since: Option<EventCursor>,
) -> io::Result<Vec<OwnedSessionEnvelope>> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|error| io::Error::other(error.to_string()))?
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;

    send.write_all(SNAPSHOT_MAGIC)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    write_stream_string(&mut send, session_id).await?;
    let mut cursor = Vec::with_capacity(17);
    match since {
        Some(since) => {
            encode_u8(&mut cursor, 1);
            encode_u64(&mut cursor, since.instance_id());
            encode_u64(&mut cursor, since.sequence());
        }
        None => encode_u8(&mut cursor, 0),
    }
    send.write_all(&cursor)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    finish_request_stream(&mut send).await?;

    match read_u8_stream(&mut recv).await? {
        RESPONSE_SNAPSHOT => {
            let envelope_count = read_u64(&mut recv).await?;
            validate_wire_count(envelope_count, MAX_SESSION_ENVELOPES, "session envelopes")?;
            let envelope_count = usize::try_from(envelope_count).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "session envelope count does not fit usize",
                )
            })?;
            let mut envelopes = Vec::with_capacity(envelope_count);
            for _ in 0..envelope_count {
                let len = usize::try_from(read_u32(&mut recv).await?).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid envelope length")
                })?;
                validate_wire_len(len, MAX_SESSION_ENVELOPE_BYTES, "session envelope frame")?;
                let mut buffer = vec![0_u8; len];
                recv.read_exact(&mut buffer)
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))?;
                envelopes.push(decode_envelope(&buffer)?);
            }
            Ok(envelopes)
        }
        RESPONSE_ERROR => Err(io::Error::other(read_stream_string(&mut recv).await?)),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected snapshot response tag: {tag}"),
        )),
    }
}
