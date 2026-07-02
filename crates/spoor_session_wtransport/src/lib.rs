// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session adapter for Spoor.
//!
//! This crate lives strictly downstream of `spoor_session`. It transports owned session envelopes
//! over a single WebTransport stream; it does not participate in hot-path instrumentation.

use spoor_core::{
    ContextId, CounterDescId, FlowId, FlowPhase, LabelKey, MarkerKind, MarkerPhase, ResourceId,
    ResourceKindId, ScopeDescId, ScopeToken, Unit,
};
use spoor_runtime::{CounterKind, Snapshot};
use spoor_session::{
    Chunking, Consumer, DroppedSummary, Envelope, EnvelopeHeader, OwnedArgEntry, OwnedContextEntry,
    OwnedContextSnapshot, OwnedCounterSnapshot, OwnedLabelValue, OwnedObservation,
    OwnedSessionEnvelope, OwnedSessionPayload, OwnedSnapshotMetadata, OwnedSnapshotSummary,
    OwnedTimedObservation, Payload, ProtocolVersion, publish_snapshot,
};
use std::{
    env, fmt, io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::PathBuf,
};
use tokio::fs;
use wtransport::{
    ClientConfig, Connection, Endpoint, Identity, RecvStream, SendStream, ServerConfig,
    error::StreamWriteError,
};

pub use wtransport::tls::Sha256Digest;

const DEFAULT_PATH: &str = "/spoor/session";
const DEFAULT_LOCAL_PORT: u16 = 43_091;
const DISCOVER_MAGIC: &[u8; 8] = b"SPOORD01";
const SNAPSHOT_MAGIC: &[u8; 8] = b"SPOORS01";
const CODEC_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0);
const RESPONSE_DISCOVERY: u8 = 0;
const RESPONSE_SNAPSHOT: u8 = 1;
const RESPONSE_ERROR: u8 = 2;

/// Self-signed localhost certificates are valid for 14 days (the WebTransport
/// certificate-hash rule). A persisted identity is regenerated once it has
/// less than this many whole days of validity left, so an attach session
/// started right after load does not race certificate expiry.
const IDENTITY_RENEWAL_MARGIN_DAYS: i64 = 2;

/// Maximum UTF-8 string payload accepted by the session wire codec.
pub const MAX_SESSION_STRING_BYTES: usize = 64 * 1024;
/// Maximum encoded envelope frame accepted by the session wire codec.
pub const MAX_SESSION_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum envelope count accepted in one snapshot response.
pub const MAX_SESSION_ENVELOPES: u64 = 1_000_000;
/// Maximum session announcements accepted in one discovery response.
pub const MAX_DISCOVERED_SESSIONS: u64 = 4096;
/// Maximum rows accepted in one encoded envelope payload.
pub const MAX_ROWS_PER_ENVELOPE: usize = 65_536;
/// Maximum context labels accepted for one encoded context row.
pub const MAX_CONTEXT_ENTRIES_PER_CONTEXT: usize = spoor_core::CONTEXT_INLINE_CAPACITY;
/// Maximum per-instance arguments accepted for one encoded observation.
pub const MAX_ARGS_PER_OBSERVATION: usize = spoor_core::ARGS_INLINE_CAPACITY;

/// Identity and certificate pinning material for a local session server.
#[derive(Debug)]
pub struct SessionIdentity {
    identity: Identity,
    certificate_hash: Sha256Digest,
}

impl SessionIdentity {
    /// Creates a self-signed identity suitable for local Spoor session demos.
    pub fn self_signed_localhost() -> io::Result<Self> {
        let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"])
            .map_err(|error| io::Error::other(error.to_string()))?;
        let certificate = identity
            .certificate_chain()
            .as_slice()
            .first()
            .ok_or_else(|| io::Error::other("self-signed identity produced no certificate"))?;
        Ok(Self {
            certificate_hash: certificate.hash(),
            identity,
        })
    }

    /// Returns the pinned certificate hash clients should use.
    #[must_use]
    pub fn certificate_hash(&self) -> Sha256Digest {
        self.certificate_hash.clone()
    }

    fn clone_identity(&self) -> Identity {
        self.identity.clone_identity()
    }

    fn from_identity(identity: Identity) -> io::Result<Self> {
        let certificate = identity
            .certificate_chain()
            .as_slice()
            .first()
            .ok_or_else(|| io::Error::other("identity produced no certificate"))?;
        Ok(Self {
            certificate_hash: certificate.hash(),
            identity,
        })
    }

    /// Loads the local localhost identity, creating and storing one if needed.
    ///
    /// Self-signed localhost certificates expire after 14 days, so a persisted
    /// identity is regenerated automatically when it is expired or close to
    /// expiry. The pinned certificate hash changes on regeneration; clients
    /// that read the hash through [`Self::load_localhost_certificate_hash`]
    /// pick up the fresh identity transparently.
    pub async fn load_or_create_localhost() -> io::Result<Self> {
        let paths = local_identity_paths()?;
        if paths.cert.exists() && paths.key.exists() {
            let identity = Identity::load_pemfiles(&paths.cert, &paths.key)
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            let loaded = Self::from_identity(identity)?;
            if !loaded.needs_renewal() {
                return Ok(loaded);
            }
        }

        let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"])
            .map_err(|error| io::Error::other(error.to_string()))?;
        fs::create_dir_all(&paths.directory).await?;
        identity
            .certificate_chain()
            .store_pemfile(&paths.cert)
            .await?;
        identity
            .private_key()
            .store_secret_pemfile(&paths.key)
            .await?;
        Self::from_identity(identity)
    }

    /// Returns true when the identity is expired, not yet valid, unparseable,
    /// or within the renewal margin of expiry.
    fn needs_renewal(&self) -> bool {
        let Some(certificate) = self.identity.certificate_chain().as_slice().first() else {
            return true;
        };
        let Ok((_, parsed)) = x509_parser::parse_x509_certificate(certificate.der()) else {
            return true;
        };
        match parsed.validity().time_to_expiration() {
            Some(remaining) => remaining.whole_days() < IDENTITY_RENEWAL_MARGIN_DAYS,
            None => true,
        }
    }

    /// Loads the stable local certificate hash used for localhost session connections.
    pub async fn load_localhost_certificate_hash() -> io::Result<Sha256Digest> {
        let paths = local_identity_paths()?;
        let certificate = wtransport::tls::Certificate::load_pemfile(&paths.cert)
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(certificate.hash())
    }
}

/// Discoverable metadata for one local Spoor WebTransport session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionAnnouncement {
    /// Stable session id unique within one session server.
    pub id: String,
    /// Process name associated with the published session.
    pub process_name: String,
    /// Optional capture-session label.
    pub session_name: Option<String>,
}

impl SessionAnnouncement {
    /// Builds a new discovery row for one logical capture session.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        process_name: impl Into<String>,
        session_name: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            process_name: process_name.into(),
            session_name,
        }
    }
}

/// Request kind received by a session server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionRequestKind {
    /// Request the currently discoverable sessions from this server.
    Discover,
    /// Request one named session snapshot from this server.
    Snapshot {
        /// Session id advertised by discovery.
        session_id: String,
    },
}

impl SessionRequestKind {
    /// Returns the requested session id when this is a snapshot fetch.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Discover => None,
            Self::Snapshot { session_id } => Some(session_id.as_str()),
        }
    }
}

/// Summary of one served request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServedRequest {
    /// Request authority observed by the server.
    pub authority: String,
    /// Request path served by the server.
    pub path: String,
    /// Response kind emitted by the server.
    pub kind: ServedRequestKind,
}

/// Classification for a served request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServedRequestKind {
    /// Discovery request returning one or more session announcements.
    Discovery {
        /// Number of discoverable sessions returned.
        session_count: usize,
    },
    /// Snapshot request returning one chunked snapshot publication.
    Snapshot {
        /// Number of envelopes written to the client.
        envelope_count: usize,
    },
    /// Error response sent back to the client.
    Error,
}

/// Configuration for a one-shot session server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionServerConfig {
    /// Local UDP address to bind.
    pub bind_address: SocketAddr,
    /// WebTransport path accepted by the server.
    pub path: String,
}

impl Default for SessionServerConfig {
    fn default() -> Self {
        Self {
            bind_address: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            path: String::from(DEFAULT_PATH),
        }
    }
}

impl SessionServerConfig {
    /// Returns the fixed localhost session configuration used for local attach workflows.
    #[must_use]
    pub fn local_default() -> Self {
        Self {
            bind_address: SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                DEFAULT_LOCAL_PORT,
            )),
            path: String::from(DEFAULT_PATH),
        }
    }
}

/// One-shot WebTransport server for Spoor session envelopes.
pub struct SessionServer {
    endpoint: Endpoint<wtransport::endpoint::endpoint_side::Server>,
    path: String,
}

impl fmt::Debug for SessionServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionServer")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl SessionServer {
    /// Binds a local WebTransport endpoint using the provided identity and configuration.
    pub fn bind(identity: &SessionIdentity, config: SessionServerConfig) -> io::Result<Self> {
        let server_config = ServerConfig::builder()
            .with_bind_address(config.bind_address)
            .with_identity(identity.clone_identity())
            .build();
        let endpoint = Endpoint::server(server_config)?;
        Ok(Self {
            endpoint,
            path: config.path,
        })
    }

    /// Returns the local UDP socket address used by this server.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Returns the secure URL clients should use to connect to this server.
    pub fn url(&self) -> io::Result<String> {
        let address = self.local_addr()?;
        Ok(format!(
            "https://{}:{}{}",
            address.ip(),
            address.port(),
            self.path
        ))
    }

    /// Accepts the next request for this session server.
    pub async fn accept_request(&self) -> io::Result<SessionRequest> {
        let incoming_session = self.endpoint.accept().await;
        let session_request = incoming_session
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        let authority = session_request.authority().to_owned();
        let path = session_request.path().to_owned();
        if path != self.path {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unexpected session path: {path}"),
            ));
        }

        let connection = session_request
            .accept()
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        let (send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;

        let mut request = [0_u8; DISCOVER_MAGIC.len()];
        recv.read_exact(&mut request)
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        let kind = if &request == DISCOVER_MAGIC {
            SessionRequestKind::Discover
        } else if &request == SNAPSHOT_MAGIC {
            SessionRequestKind::Snapshot {
                session_id: read_stream_string(&mut recv).await?,
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Spoor session request preface",
            ));
        };

        Ok(SessionRequest {
            authority,
            path,
            _connection: connection,
            send,
            kind,
        })
    }
}

/// Accepted request waiting for a session server response.
pub struct SessionRequest {
    authority: String,
    path: String,
    _connection: Connection,
    send: SendStream,
    kind: SessionRequestKind,
}

impl fmt::Debug for SessionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionRequest")
            .field("authority", &self.authority)
            .field("path", &self.path)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl SessionRequest {
    /// Returns the request authority observed by the server.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// Returns the request path observed by the server.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the parsed request kind.
    #[must_use]
    pub fn kind(&self) -> &SessionRequestKind {
        &self.kind
    }

    /// Responds to a discovery request with the currently discoverable sessions.
    pub async fn respond_discovery(
        mut self,
        sessions: &[SessionAnnouncement],
    ) -> io::Result<ServedRequest> {
        if self.kind != SessionRequestKind::Discover {
            return self.respond_error("expected discovery request").await;
        }
        if u64::try_from(sessions.len()).unwrap_or(u64::MAX) > MAX_DISCOVERED_SESSIONS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many session announcements",
            ));
        }

        write_u8_stream(&mut self.send, RESPONSE_DISCOVERY).await?;
        write_u64(
            &mut self.send,
            u64::try_from(sessions.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many sessions"))?,
        )
        .await?;
        for session in sessions {
            encode_session_announcement_stream(&mut self.send, session).await?;
        }
        finish_response_stream(&mut self.send).await?;

        Ok(ServedRequest {
            authority: self.authority,
            path: self.path,
            kind: ServedRequestKind::Discovery {
                session_count: sessions.len(),
            },
        })
    }

    /// Responds to a snapshot request with one chunked snapshot publication.
    pub async fn respond_snapshot(
        mut self,
        snapshot: Snapshot,
        chunking: Chunking,
    ) -> io::Result<ServedRequest> {
        if !matches!(self.kind, SessionRequestKind::Snapshot { .. }) {
            return self.respond_error("expected snapshot request").await;
        }

        let mut consumer = EncodingConsumer::default();
        publish_snapshot(&snapshot, chunking, &mut consumer)?;
        write_u8_stream(&mut self.send, RESPONSE_SNAPSHOT).await?;
        write_u64(
            &mut self.send,
            u64::try_from(consumer.frames.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many envelopes"))?,
        )
        .await?;
        for encoded in &consumer.frames {
            write_u32(
                &mut self.send,
                u32::try_from(encoded.len()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "envelope exceeds u32 length")
                })?,
            )
            .await?;
            self.send
                .write_all(encoded)
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        finish_response_stream(&mut self.send).await?;

        Ok(ServedRequest {
            authority: self.authority,
            path: self.path,
            kind: ServedRequestKind::Snapshot {
                envelope_count: consumer.frames.len(),
            },
        })
    }

    /// Responds with an application-level error.
    pub async fn respond_error(mut self, message: &str) -> io::Result<ServedRequest> {
        write_u8_stream(&mut self.send, RESPONSE_ERROR).await?;
        write_stream_string(&mut self.send, message).await?;
        finish_response_stream(&mut self.send).await?;
        Ok(ServedRequest {
            authority: self.authority,
            path: self.path,
            kind: ServedRequestKind::Error,
        })
    }
}

/// Fetches one snapshot publication from a WebTransport session server.
pub async fn fetch_session(
    url: &str,
    certificate_hash: Sha256Digest,
) -> io::Result<Vec<OwnedSessionEnvelope>> {
    let sessions = discover_sessions(url, certificate_hash.clone()).await?;
    match sessions.as_slice() {
        [] => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no discoverable sessions on this server",
        )),
        [session] => fetch_named_session(url, certificate_hash, &session.id).await,
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multiple discoverable sessions available; choose one explicitly",
        )),
    }
}

/// Returns the default localhost WebTransport server URL for Spoor sessions.
#[must_use]
pub fn default_local_url() -> String {
    format!("https://127.0.0.1:{DEFAULT_LOCAL_PORT}{DEFAULT_PATH}")
}

/// Discovers sessions from the default local Spoor session server.
pub async fn discover_local_sessions() -> io::Result<Vec<SessionAnnouncement>> {
    discover_sessions(
        &default_local_url(),
        SessionIdentity::load_localhost_certificate_hash().await?,
    )
    .await
}

/// Fetches one named session from the default local Spoor session server.
pub async fn fetch_local_session(session_id: &str) -> io::Result<Vec<OwnedSessionEnvelope>> {
    fetch_named_session(
        &default_local_url(),
        SessionIdentity::load_localhost_certificate_hash().await?,
        session_id,
    )
    .await
}

/// Fetches the currently discoverable sessions from a session server.
pub async fn discover_sessions(
    url: &str,
    certificate_hash: Sha256Digest,
) -> io::Result<Vec<SessionAnnouncement>> {
    let client_config = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([certificate_hash])
        .build();
    let endpoint = Endpoint::client(client_config)?;
    let connection = endpoint
        .connect(url)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    discover_connection(connection).await
}

/// Fetches one named snapshot publication from a session server.
pub async fn fetch_named_session(
    url: &str,
    certificate_hash: Sha256Digest,
    session_id: &str,
) -> io::Result<Vec<OwnedSessionEnvelope>> {
    let client_config = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([certificate_hash])
        .build();
    let endpoint = Endpoint::client(client_config)?;
    let connection = endpoint
        .connect(url)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    fetch_named_connection(connection, session_id).await
}

async fn discover_connection(connection: Connection) -> io::Result<Vec<SessionAnnouncement>> {
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

async fn fetch_named_connection(
    connection: Connection,
    session_id: &str,
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

#[derive(Default)]
struct EncodingConsumer {
    frames: Vec<Vec<u8>>,
}

impl Consumer for EncodingConsumer {
    type Error = io::Error;

    fn consume(&mut self, envelope: Envelope<'_>) -> Result<(), Self::Error> {
        if u64::try_from(self.frames.len()).unwrap_or(u64::MAX) == MAX_SESSION_ENVELOPES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many session envelopes",
            ));
        }
        let encoded = encode_borrowed_envelope(envelope)?;
        validate_wire_len(
            encoded.len(),
            MAX_SESSION_ENVELOPE_BYTES,
            "session envelope frame",
        )?;
        self.frames.push(encoded);
        Ok(())
    }
}

async fn write_u32(send: &mut SendStream, value: u32) -> io::Result<()> {
    send.write_all(&value.to_le_bytes())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn write_u64(send: &mut SendStream, value: u64) -> io::Result<()> {
    send.write_all(&value.to_le_bytes())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn read_u32(recv: &mut RecvStream) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(u32::from_le_bytes(bytes))
}

async fn read_u64(recv: &mut RecvStream) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(u64::from_le_bytes(bytes))
}

async fn finish_response_stream(send: &mut SendStream) -> io::Result<()> {
    map_finish_error(send.finish().await)
}

async fn finish_request_stream(send: &mut SendStream) -> io::Result<()> {
    map_finish_error(send.finish().await)
}

async fn write_u8_stream(send: &mut SendStream, value: u8) -> io::Result<()> {
    send.write_all(&[value])
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn read_u8_stream(recv: &mut RecvStream) -> io::Result<u8> {
    let mut bytes = [0_u8; 1];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(bytes[0])
}

async fn write_stream_string(send: &mut SendStream, value: &str) -> io::Result<()> {
    let bytes = value.as_bytes();
    validate_wire_len(bytes.len(), MAX_SESSION_STRING_BYTES, "session string")?;
    write_u32(
        send,
        u32::try_from(bytes.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "string exceeds u32 length")
        })?,
    )
    .await?;
    send.write_all(bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn read_stream_string(recv: &mut RecvStream) -> io::Result<String> {
    let len = usize::try_from(read_u32(recv).await?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid string length"))?;
    validate_wire_len(len, MAX_SESSION_STRING_BYTES, "session string")?;
    let mut bytes = vec![0_u8; len];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

async fn encode_session_announcement_stream(
    send: &mut SendStream,
    session: &SessionAnnouncement,
) -> io::Result<()> {
    write_stream_string(send, &session.id).await?;
    write_stream_string(send, &session.process_name).await?;
    match &session.session_name {
        Some(session_name) => {
            write_u8_stream(send, 1).await?;
            write_stream_string(send, session_name).await?;
        }
        None => write_u8_stream(send, 0).await?,
    }
    Ok(())
}

async fn decode_session_announcement_stream(
    recv: &mut RecvStream,
) -> io::Result<SessionAnnouncement> {
    let id = read_stream_string(recv).await?;
    let process_name = read_stream_string(recv).await?;
    let session_name = match read_u8_stream(recv).await? {
        0 => None,
        1 => Some(read_stream_string(recv).await?),
        tag => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid session-name tag: {tag}"),
            ));
        }
    };
    Ok(SessionAnnouncement {
        id,
        process_name,
        session_name,
    })
}

fn map_finish_error(result: Result<(), StreamWriteError>) -> io::Result<()> {
    match result {
        Ok(()) | Err(StreamWriteError::Stopped(_)) => Ok(()),
        Err(error) => Err(io::Error::other(error.to_string())),
    }
}

fn encode_borrowed_envelope(envelope: Envelope<'_>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    encode_header(&mut bytes, envelope.header);
    match envelope.payload {
        Payload::SnapshotStart(summary) => {
            encode_u8(&mut bytes, 0);
            encode_borrowed_snapshot_summary(&mut bytes, summary)?;
        }
        Payload::Contexts(contexts) => {
            encode_u8(&mut bytes, 1);
            encode_borrowed_contexts(&mut bytes, contexts)?;
        }
        Payload::Counters(counters) => {
            encode_u8(&mut bytes, 2);
            encode_borrowed_counters(&mut bytes, counters)?;
        }
        Payload::Events(events) => {
            encode_u8(&mut bytes, 3);
            encode_borrowed_events(&mut bytes, events)?;
        }
        Payload::SnapshotEnd(dropped) => {
            encode_u8(&mut bytes, 4);
            encode_dropped_summary(&mut bytes, dropped);
        }
    }
    Ok(bytes)
}

#[cfg(test)]
fn encode_envelope(envelope: &OwnedSessionEnvelope) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    encode_header(&mut bytes, envelope.header);
    match &envelope.payload {
        OwnedSessionPayload::SnapshotStart(summary) => {
            encode_u8(&mut bytes, 0);
            encode_snapshot_summary(&mut bytes, summary)?;
        }
        OwnedSessionPayload::Contexts(contexts) => {
            encode_u8(&mut bytes, 1);
            encode_contexts(&mut bytes, contexts)?;
        }
        OwnedSessionPayload::Counters(counters) => {
            encode_u8(&mut bytes, 2);
            encode_counters(&mut bytes, counters)?;
        }
        OwnedSessionPayload::Events(events) => {
            encode_u8(&mut bytes, 3);
            encode_events(&mut bytes, events)?;
        }
        OwnedSessionPayload::SnapshotEnd(dropped) => {
            encode_u8(&mut bytes, 4);
            encode_dropped_summary(&mut bytes, *dropped);
        }
    }
    Ok(bytes)
}

fn decode_envelope(bytes: &[u8]) -> io::Result<OwnedSessionEnvelope> {
    validate_wire_len(
        bytes.len(),
        MAX_SESSION_ENVELOPE_BYTES,
        "session envelope frame",
    )?;
    let mut cursor = Cursor::new(bytes);
    let header = decode_header(&mut cursor)?;
    let payload = match decode_u8(&mut cursor)? {
        0 => OwnedSessionPayload::SnapshotStart(decode_snapshot_summary(&mut cursor)?),
        1 => OwnedSessionPayload::Contexts(decode_contexts(&mut cursor)?),
        2 => OwnedSessionPayload::Counters(decode_counters(&mut cursor)?),
        3 => OwnedSessionPayload::Events(decode_events(&mut cursor)?),
        4 => OwnedSessionPayload::SnapshotEnd(decode_dropped_summary(&mut cursor)?),
        tag => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown session payload tag: {tag}"),
            ));
        }
    };
    if !cursor.is_exhausted() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing bytes after session envelope",
        ));
    }
    Ok(OwnedSessionEnvelope { header, payload })
}

fn encode_header(bytes: &mut Vec<u8>, header: EnvelopeHeader) {
    encode_u16(bytes, header.protocol_version.major());
    encode_u16(bytes, header.protocol_version.minor());
    encode_u64(bytes, header.session_sequence);
    encode_u64(bytes, header.chunk_index);
}

fn decode_header(cursor: &mut Cursor<'_>) -> io::Result<EnvelopeHeader> {
    let protocol_version = ProtocolVersion::new(decode_u16(cursor)?, decode_u16(cursor)?);
    if !CODEC_VERSION.can_decode(protocol_version) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported session codec version {}.{}",
                protocol_version.major(),
                protocol_version.minor()
            ),
        ));
    }
    Ok(EnvelopeHeader {
        protocol_version,
        session_sequence: decode_u64(cursor)?,
        chunk_index: decode_u64(cursor)?,
    })
}

#[cfg(test)]
fn encode_snapshot_summary(bytes: &mut Vec<u8>, summary: &OwnedSnapshotSummary) -> io::Result<()> {
    encode_snapshot_metadata(bytes, &summary.metadata)?;
    encode_u64(bytes, summary.capture_start_micros);
    encode_u64(bytes, summary.captured_at_micros);
    encode_usize(bytes, summary.context_count)?;
    encode_usize(bytes, summary.counter_count)?;
    encode_usize(bytes, summary.event_count)?;
    Ok(())
}

fn encode_borrowed_snapshot_summary(
    bytes: &mut Vec<u8>,
    summary: spoor_session::SnapshotSummary<'_>,
) -> io::Result<()> {
    encode_snapshot_metadata_borrowed(bytes, summary.metadata)?;
    encode_u64(bytes, summary.capture_start_micros);
    encode_u64(bytes, summary.captured_at_micros);
    encode_usize(bytes, summary.context_count)?;
    encode_usize(bytes, summary.counter_count)?;
    encode_usize(bytes, summary.event_count)?;
    Ok(())
}

fn decode_snapshot_summary(cursor: &mut Cursor<'_>) -> io::Result<OwnedSnapshotSummary> {
    Ok(OwnedSnapshotSummary {
        metadata: decode_snapshot_metadata(cursor)?,
        capture_start_micros: decode_u64(cursor)?,
        captured_at_micros: decode_u64(cursor)?,
        context_count: decode_usize(cursor)?,
        counter_count: decode_usize(cursor)?,
        event_count: decode_usize(cursor)?,
    })
}

#[cfg(test)]
fn encode_snapshot_metadata(
    bytes: &mut Vec<u8>,
    metadata: &OwnedSnapshotMetadata,
) -> io::Result<()> {
    encode_string(bytes, &metadata.process_name)?;
    match &metadata.session_name {
        Some(session_name) => {
            encode_u8(bytes, 1);
            encode_string(bytes, session_name)?;
        }
        None => encode_u8(bytes, 0),
    }
    encode_u64(bytes, metadata.session_sequence);
    encode_u64(bytes, metadata.wall_clock_anchor_micros);
    Ok(())
}

fn encode_snapshot_metadata_borrowed(
    bytes: &mut Vec<u8>,
    metadata: &spoor_runtime::SnapshotMetadata,
) -> io::Result<()> {
    encode_string(bytes, &metadata.process_name)?;
    match &metadata.session_name {
        Some(session_name) => {
            encode_u8(bytes, 1);
            encode_string(bytes, session_name)?;
        }
        None => encode_u8(bytes, 0),
    }
    encode_u64(bytes, metadata.session_sequence);
    encode_u64(bytes, metadata.wall_clock_anchor_micros);
    Ok(())
}

fn decode_snapshot_metadata(cursor: &mut Cursor<'_>) -> io::Result<OwnedSnapshotMetadata> {
    let process_name = decode_string(cursor)?;
    let session_name = match decode_u8(cursor)? {
        0 => None,
        1 => Some(decode_string(cursor)?),
        tag => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid session-name tag: {tag}"),
            ));
        }
    };
    Ok(OwnedSnapshotMetadata {
        process_name,
        session_name,
        session_sequence: decode_u64(cursor)?,
        wall_clock_anchor_micros: decode_u64(cursor)?,
    })
}

#[cfg(test)]
fn encode_contexts(bytes: &mut Vec<u8>, contexts: &[OwnedContextSnapshot]) -> io::Result<()> {
    encode_len(bytes, contexts.len())?;
    for context in contexts {
        encode_u64(bytes, context.id.get());
        encode_len(bytes, context.entries.len())?;
        for entry in &context.entries {
            encode_u8(bytes, encode_label_key(entry.key));
            encode_label_value(bytes, &entry.value)?;
        }
    }
    Ok(())
}

fn encode_borrowed_contexts(
    bytes: &mut Vec<u8>,
    contexts: &[spoor_runtime::ContextSnapshot],
) -> io::Result<()> {
    encode_len(bytes, contexts.len())?;
    for context in contexts {
        encode_u64(bytes, context.id.get());
        encode_len(bytes, context.context.len())?;
        for entry in context.context.iter() {
            encode_u8(bytes, encode_label_key(entry.key()));
            encode_borrowed_label_value(bytes, entry.value())?;
        }
    }
    Ok(())
}

fn decode_contexts(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedContextSnapshot>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "context rows")?;
    let mut contexts = Vec::with_capacity(len);
    for _ in 0..len {
        let id = ContextId::new(decode_u64(cursor)?);
        let entry_len =
            decode_bounded_len(cursor, MAX_CONTEXT_ENTRIES_PER_CONTEXT, "context entries")?;
        let mut entries = Vec::with_capacity(entry_len);
        for _ in 0..entry_len {
            entries.push(OwnedContextEntry {
                key: decode_label_key(decode_u8(cursor)?)?,
                value: decode_label_value(cursor)?,
            });
        }
        contexts.push(OwnedContextSnapshot { id, entries });
    }
    Ok(contexts)
}

fn encode_borrowed_label_value(
    bytes: &mut Vec<u8>,
    value: spoor_core::LabelValue,
) -> io::Result<()> {
    match value {
        spoor_core::LabelValue::Unsigned(value) => {
            encode_u8(bytes, 0);
            encode_u64(bytes, value);
        }
        spoor_core::LabelValue::Signed(value) => {
            encode_u8(bytes, 1);
            encode_i64(bytes, value);
        }
        spoor_core::LabelValue::Bool(value) => {
            encode_u8(bytes, 2);
            encode_u8(bytes, u8::from(value));
        }
        spoor_core::LabelValue::StaticStr(value) => {
            encode_u8(bytes, 3);
            encode_string(bytes, value)?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn encode_counters(bytes: &mut Vec<u8>, counters: &[OwnedCounterSnapshot]) -> io::Result<()> {
    encode_len(bytes, counters.len())?;
    for counter in counters {
        encode_u64(bytes, counter.desc_id.get());
        encode_string(bytes, &counter.name)?;
        encode_u8(bytes, encode_unit(counter.unit));
        encode_u64(bytes, counter.context_id.get());
        encode_u8(bytes, encode_counter_kind(counter.kind));
        encode_u64(bytes, counter.value);
        encode_u64(bytes, counter.updated_at_micros);
    }
    Ok(())
}

fn encode_borrowed_counters(
    bytes: &mut Vec<u8>,
    counters: &[spoor_runtime::CounterSnapshot],
) -> io::Result<()> {
    encode_len(bytes, counters.len())?;
    for counter in counters {
        encode_u64(bytes, counter.desc_id.get());
        encode_string(bytes, counter.name)?;
        encode_u8(bytes, encode_unit(counter.unit));
        encode_u64(bytes, counter.context_id.get());
        encode_u8(bytes, encode_counter_kind(counter.kind));
        encode_u64(bytes, counter.value);
        encode_u64(bytes, counter.updated_at_micros);
    }
    Ok(())
}

fn decode_counters(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedCounterSnapshot>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "counter rows")?;
    let mut counters = Vec::with_capacity(len);
    for _ in 0..len {
        counters.push(OwnedCounterSnapshot {
            desc_id: CounterDescId::new(decode_u64(cursor)?),
            name: decode_string(cursor)?,
            unit: decode_unit(decode_u8(cursor)?)?,
            context_id: ContextId::new(decode_u64(cursor)?),
            kind: decode_counter_kind(decode_u8(cursor)?)?,
            value: decode_u64(cursor)?,
            updated_at_micros: decode_u64(cursor)?,
        });
    }
    Ok(counters)
}

#[cfg(test)]
fn encode_events(bytes: &mut Vec<u8>, events: &[OwnedTimedObservation]) -> io::Result<()> {
    encode_len(bytes, events.len())?;
    for event in events {
        encode_u64(bytes, event.at_micros);
        encode_u64(bytes, event.thread_id);
        encode_observation(bytes, &event.observation)?;
    }
    Ok(())
}

fn encode_borrowed_events(
    bytes: &mut Vec<u8>,
    events: &[spoor_runtime::TimedObservation],
) -> io::Result<()> {
    encode_len(bytes, events.len())?;
    for event in events {
        encode_u64(bytes, event.at_micros);
        encode_u64(bytes, event.thread_id);
        encode_borrowed_observation(bytes, &event.observation)?;
    }
    Ok(())
}

fn decode_events(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedTimedObservation>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "event rows")?;
    let mut events = Vec::with_capacity(len);
    for _ in 0..len {
        events.push(OwnedTimedObservation {
            at_micros: decode_u64(cursor)?,
            thread_id: decode_u64(cursor)?,
            observation: decode_observation(cursor)?,
        });
    }
    Ok(events)
}

fn encode_borrowed_observation(
    bytes: &mut Vec<u8>,
    observation: &spoor_runtime::Observation,
) -> io::Result<()> {
    match observation {
        spoor_runtime::Observation::ScopeBegin {
            desc_id,
            name,
            context_id,
            token,
            args,
        } => {
            encode_u8(bytes, 0);
            encode_u64(bytes, desc_id.get());
            encode_string(bytes, name)?;
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, token.get());
            encode_borrowed_args(bytes, *args)?;
        }
        spoor_runtime::Observation::ScopeEnd { token } => {
            encode_u8(bytes, 1);
            encode_u64(bytes, token.get());
        }
        spoor_runtime::Observation::ResourceCreate {
            kind_id,
            kind_name,
            resource_id,
            context_id,
            size,
        } => {
            encode_u8(bytes, 2);
            encode_u64(bytes, kind_id.get());
            encode_string(bytes, kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, *size);
        }
        spoor_runtime::Observation::ResourceUpdate {
            kind_id,
            kind_name,
            resource_id,
            context_id,
            size,
        } => {
            encode_u8(bytes, 3);
            encode_u64(bytes, kind_id.get());
            encode_string(bytes, kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, *size);
        }
        spoor_runtime::Observation::ResourceDrop {
            kind_id,
            kind_name,
            resource_id,
            context_id,
        } => {
            encode_u8(bytes, 4);
            encode_u64(bytes, kind_id.get());
            encode_string(bytes, kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
        }
        spoor_runtime::Observation::FrameMarker {
            kind,
            phase,
            context_id,
        } => {
            encode_u8(bytes, 5);
            encode_u8(bytes, encode_marker_kind(*kind));
            encode_u8(bytes, encode_marker_phase(*phase));
            encode_u64(bytes, context_id.get());
        }
        spoor_runtime::Observation::Flow {
            flow_id,
            phase,
            context_id,
        } => {
            encode_u8(bytes, 6);
            encode_u64(bytes, flow_id.get());
            encode_u8(bytes, encode_flow_phase(*phase));
            encode_u64(bytes, context_id.get());
        }
    }
    Ok(())
}

#[cfg(test)]
fn encode_args(bytes: &mut Vec<u8>, args: &[OwnedArgEntry]) -> io::Result<()> {
    let count = u8::try_from(args.len().min(MAX_ARGS_PER_OBSERVATION))
        .expect("bounded args count fits into u8");
    encode_u8(bytes, count);
    for entry in args.iter().take(usize::from(count)) {
        encode_string(bytes, &entry.key)?;
        encode_label_value(bytes, &entry.value)?;
    }
    Ok(())
}

fn encode_borrowed_args(bytes: &mut Vec<u8>, args: spoor_core::Args) -> io::Result<()> {
    let count = u8::try_from(args.len().min(MAX_ARGS_PER_OBSERVATION))
        .expect("bounded args count fits into u8");
    encode_u8(bytes, count);
    for entry in args.iter().take(usize::from(count)) {
        encode_string(bytes, entry.key())?;
        encode_borrowed_label_value(bytes, entry.value())?;
    }
    Ok(())
}

fn decode_args(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedArgEntry>> {
    let count = usize::from(decode_u8(cursor)?);
    if count > MAX_ARGS_PER_OBSERVATION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("too many observation args: {count}"),
        ));
    }
    let mut args = Vec::with_capacity(count);
    for _ in 0..count {
        args.push(OwnedArgEntry {
            key: decode_string(cursor)?,
            value: decode_label_value(cursor)?,
        });
    }
    Ok(args)
}

fn encode_dropped_summary(bytes: &mut Vec<u8>, dropped: DroppedSummary) {
    encode_u64(bytes, dropped.contexts);
    encode_u64(bytes, dropped.counters);
    encode_u64(bytes, dropped.events);
}

fn decode_dropped_summary(cursor: &mut Cursor<'_>) -> io::Result<DroppedSummary> {
    Ok(DroppedSummary {
        contexts: decode_u64(cursor)?,
        counters: decode_u64(cursor)?,
        events: decode_u64(cursor)?,
    })
}

#[cfg(test)]
fn encode_label_value(bytes: &mut Vec<u8>, value: &OwnedLabelValue) -> io::Result<()> {
    match value {
        OwnedLabelValue::Unsigned(value) => {
            encode_u8(bytes, 0);
            encode_u64(bytes, *value);
        }
        OwnedLabelValue::Signed(value) => {
            encode_u8(bytes, 1);
            encode_i64(bytes, *value);
        }
        OwnedLabelValue::Bool(value) => {
            encode_u8(bytes, 2);
            encode_u8(bytes, u8::from(*value));
        }
        OwnedLabelValue::String(value) => {
            encode_u8(bytes, 3);
            encode_string(bytes, value)?;
        }
    }
    Ok(())
}

fn decode_label_value(cursor: &mut Cursor<'_>) -> io::Result<OwnedLabelValue> {
    match decode_u8(cursor)? {
        0 => Ok(OwnedLabelValue::Unsigned(decode_u64(cursor)?)),
        1 => Ok(OwnedLabelValue::Signed(decode_i64(cursor)?)),
        2 => Ok(OwnedLabelValue::Bool(decode_u8(cursor)? != 0)),
        3 => Ok(OwnedLabelValue::String(decode_string(cursor)?)),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown label-value tag: {tag}"),
        )),
    }
}

#[cfg(test)]
fn encode_observation(bytes: &mut Vec<u8>, observation: &OwnedObservation) -> io::Result<()> {
    match observation {
        OwnedObservation::ScopeBegin {
            desc_id,
            name,
            context_id,
            token,
            args,
        } => {
            encode_u8(bytes, 0);
            encode_u64(bytes, desc_id.get());
            encode_string(bytes, name)?;
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, token.get());
            encode_args(bytes, args)?;
        }
        OwnedObservation::ScopeEnd { token } => {
            encode_u8(bytes, 1);
            encode_u64(bytes, token.get());
        }
        OwnedObservation::ResourceCreate {
            kind_id,
            kind_name,
            resource_id,
            context_id,
            size,
        } => {
            encode_u8(bytes, 2);
            encode_u64(bytes, kind_id.get());
            encode_string(bytes, kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, *size);
        }
        OwnedObservation::ResourceUpdate {
            kind_id,
            kind_name,
            resource_id,
            context_id,
            size,
        } => {
            encode_u8(bytes, 3);
            encode_u64(bytes, kind_id.get());
            encode_string(bytes, kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, *size);
        }
        OwnedObservation::ResourceDrop {
            kind_id,
            kind_name,
            resource_id,
            context_id,
        } => {
            encode_u8(bytes, 4);
            encode_u64(bytes, kind_id.get());
            encode_string(bytes, kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
        }
        OwnedObservation::FrameMarker {
            kind,
            phase,
            context_id,
        } => {
            encode_u8(bytes, 5);
            encode_u8(bytes, encode_marker_kind(*kind));
            encode_u8(bytes, encode_marker_phase(*phase));
            encode_u64(bytes, context_id.get());
        }
        OwnedObservation::Flow {
            flow_id,
            phase,
            context_id,
        } => {
            encode_u8(bytes, 6);
            encode_u64(bytes, flow_id.get());
            encode_u8(bytes, encode_flow_phase(*phase));
            encode_u64(bytes, context_id.get());
        }
    }
    Ok(())
}

fn decode_observation(cursor: &mut Cursor<'_>) -> io::Result<OwnedObservation> {
    match decode_u8(cursor)? {
        0 => Ok(OwnedObservation::ScopeBegin {
            desc_id: ScopeDescId::new(decode_u64(cursor)?),
            name: decode_string(cursor)?,
            context_id: ContextId::new(decode_u64(cursor)?),
            token: ScopeToken::new(decode_u64(cursor)?),
            args: decode_args(cursor)?,
        }),
        1 => Ok(OwnedObservation::ScopeEnd {
            token: ScopeToken::new(decode_u64(cursor)?),
        }),
        2 => Ok(OwnedObservation::ResourceCreate {
            kind_id: ResourceKindId::new(decode_u64(cursor)?),
            kind_name: decode_string(cursor)?,
            resource_id: ResourceId::new(decode_u64(cursor)?),
            context_id: ContextId::new(decode_u64(cursor)?),
            size: decode_u64(cursor)?,
        }),
        3 => Ok(OwnedObservation::ResourceUpdate {
            kind_id: ResourceKindId::new(decode_u64(cursor)?),
            kind_name: decode_string(cursor)?,
            resource_id: ResourceId::new(decode_u64(cursor)?),
            context_id: ContextId::new(decode_u64(cursor)?),
            size: decode_u64(cursor)?,
        }),
        4 => Ok(OwnedObservation::ResourceDrop {
            kind_id: ResourceKindId::new(decode_u64(cursor)?),
            kind_name: decode_string(cursor)?,
            resource_id: ResourceId::new(decode_u64(cursor)?),
            context_id: ContextId::new(decode_u64(cursor)?),
        }),
        5 => Ok(OwnedObservation::FrameMarker {
            kind: decode_marker_kind(decode_u8(cursor)?)?,
            phase: decode_marker_phase(decode_u8(cursor)?)?,
            context_id: ContextId::new(decode_u64(cursor)?),
        }),
        6 => Ok(OwnedObservation::Flow {
            flow_id: FlowId::new(decode_u64(cursor)?),
            phase: decode_flow_phase(decode_u8(cursor)?)?,
            context_id: ContextId::new(decode_u64(cursor)?),
        }),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown observation tag: {tag}"),
        )),
    }
}

fn encode_label_key(key: LabelKey) -> u8 {
    match key {
        LabelKey::Frame => 0,
        LabelKey::Transaction => 1,
        LabelKey::Pass => 2,
        LabelKey::Subsystem => 3,
        LabelKey::Node => 4,
        LabelKey::Widget => 5,
        LabelKey::Layer => 6,
        LabelKey::Resource => 7,
        LabelKey::Cache => 8,
        LabelKey::Reason => 9,
    }
}

fn decode_label_key(tag: u8) -> io::Result<LabelKey> {
    match tag {
        0 => Ok(LabelKey::Frame),
        1 => Ok(LabelKey::Transaction),
        2 => Ok(LabelKey::Pass),
        3 => Ok(LabelKey::Subsystem),
        4 => Ok(LabelKey::Node),
        5 => Ok(LabelKey::Widget),
        6 => Ok(LabelKey::Layer),
        7 => Ok(LabelKey::Resource),
        8 => Ok(LabelKey::Cache),
        9 => Ok(LabelKey::Reason),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown label key tag: {tag}"),
        )),
    }
}

fn encode_counter_kind(kind: CounterKind) -> u8 {
    match kind {
        CounterKind::Counter => 0,
        CounterKind::Gauge => 1,
    }
}

fn decode_counter_kind(tag: u8) -> io::Result<CounterKind> {
    match tag {
        0 => Ok(CounterKind::Counter),
        1 => Ok(CounterKind::Gauge),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown counter kind tag: {tag}"),
        )),
    }
}

fn encode_unit(unit: Unit) -> u8 {
    match unit {
        Unit::Bytes => 0,
        Unit::Count => 1,
        Unit::Nanos => 2,
        Unit::Micros => 3,
        Unit::Ratio => 4,
        Unit::Items => 5,
    }
}

fn decode_unit(tag: u8) -> io::Result<Unit> {
    match tag {
        0 => Ok(Unit::Bytes),
        1 => Ok(Unit::Count),
        2 => Ok(Unit::Nanos),
        3 => Ok(Unit::Micros),
        4 => Ok(Unit::Ratio),
        5 => Ok(Unit::Items),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown unit tag: {tag}"),
        )),
    }
}

fn encode_marker_kind(kind: MarkerKind) -> u8 {
    match kind {
        MarkerKind::Frame => 0,
        MarkerKind::Transaction => 1,
        MarkerKind::Pass => 2,
    }
}

fn decode_marker_kind(tag: u8) -> io::Result<MarkerKind> {
    match tag {
        0 => Ok(MarkerKind::Frame),
        1 => Ok(MarkerKind::Transaction),
        2 => Ok(MarkerKind::Pass),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown marker kind tag: {tag}"),
        )),
    }
}

fn encode_marker_phase(phase: MarkerPhase) -> u8 {
    match phase {
        MarkerPhase::Begin => 0,
        MarkerPhase::End => 1,
    }
}

fn decode_marker_phase(tag: u8) -> io::Result<MarkerPhase> {
    match tag {
        0 => Ok(MarkerPhase::Begin),
        1 => Ok(MarkerPhase::End),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown marker phase tag: {tag}"),
        )),
    }
}

fn encode_flow_phase(phase: FlowPhase) -> u8 {
    match phase {
        FlowPhase::Begin => 0,
        FlowPhase::Step => 1,
        FlowPhase::End => 2,
    }
}

fn decode_flow_phase(tag: u8) -> io::Result<FlowPhase> {
    match tag {
        0 => Ok(FlowPhase::Begin),
        1 => Ok(FlowPhase::Step),
        2 => Ok(FlowPhase::End),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown flow phase tag: {tag}"),
        )),
    }
}

fn encode_len(bytes: &mut Vec<u8>, len: usize) -> io::Result<()> {
    encode_u32(
        bytes,
        u32::try_from(len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "length exceeds u32"))?,
    );
    Ok(())
}

fn encode_usize(bytes: &mut Vec<u8>, value: usize) -> io::Result<()> {
    encode_len(bytes, value)
}

fn decode_usize(cursor: &mut Cursor<'_>) -> io::Result<usize> {
    usize::try_from(decode_u32(cursor)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "u32 length does not fit usize"))
}

fn decode_bounded_len(
    cursor: &mut Cursor<'_>,
    max_len: usize,
    what: &'static str,
) -> io::Result<usize> {
    let len = decode_usize(cursor)?;
    validate_wire_len(len, max_len, what)?;
    if len > cursor.remaining() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("{what} length exceeds remaining envelope bytes"),
        ));
    }
    Ok(len)
}

fn validate_wire_len(len: usize, max_len: usize, what: &'static str) -> io::Result<()> {
    if len <= max_len {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{what} length {len} exceeds maximum {max_len}"),
    ))
}

fn validate_wire_count(count: u64, max_count: u64, what: &'static str) -> io::Result<()> {
    if count <= max_count {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{what} count {count} exceeds maximum {max_count}"),
    ))
}

fn encode_string(bytes: &mut Vec<u8>, value: &str) -> io::Result<()> {
    validate_wire_len(value.len(), MAX_SESSION_STRING_BYTES, "session string")?;
    encode_len(bytes, value.len())?;
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn decode_string(cursor: &mut Cursor<'_>) -> io::Result<String> {
    let len = decode_bounded_len(cursor, MAX_SESSION_STRING_BYTES, "session string")?;
    let bytes = cursor.read_exact(len)?;
    String::from_utf8(bytes.to_vec())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

#[derive(Debug)]
struct IdentityPaths {
    directory: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

fn local_identity_paths() -> io::Result<IdentityPaths> {
    let directory = local_state_directory()?.join("wtransport");
    Ok(IdentityPaths {
        cert: directory.join("localhost-cert.pem"),
        key: directory.join("localhost-key.pem"),
        directory,
    })
}

fn local_state_directory() -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        home_directory().map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("spoor")
        })
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = env::var_os("APPDATA") {
            Ok(PathBuf::from(app_data).join("spoor"))
        } else {
            home_directory().map(|home| home.join("AppData").join("Roaming").join("spoor"))
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(state_home) = env::var_os("XDG_STATE_HOME") {
            Ok(PathBuf::from(state_home).join("spoor"))
        } else {
            home_directory().map(|home| home.join(".local").join("state").join("spoor"))
        }
    }
}

fn home_directory() -> io::Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
}

fn encode_u8(bytes: &mut Vec<u8>, value: u8) {
    bytes.push(value);
}

fn decode_u8(cursor: &mut Cursor<'_>) -> io::Result<u8> {
    Ok(cursor.read_exact(1)?[0])
}

fn encode_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn decode_u16(cursor: &mut Cursor<'_>) -> io::Result<u16> {
    let bytes = cursor.read_exact(2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn encode_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn decode_u32(cursor: &mut Cursor<'_>) -> io::Result<u32> {
    let bytes = cursor.read_exact(4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn encode_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn decode_u64(cursor: &mut Cursor<'_>) -> io::Result<u64> {
    let bytes = cursor.read_exact(8)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

fn encode_i64(bytes: &mut Vec<u8>, value: i64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn decode_i64(cursor: &mut Cursor<'_>) -> io::Result<i64> {
    let bytes = cursor.read_exact(8)?;
    Ok(i64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[derive(Debug)]
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_exact(&mut self, len: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "cursor overflow"))?;
        let bytes = self.bytes.get(self.offset..end).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "truncated session envelope")
        })?;
        self.offset = end;
        Ok(bytes)
    }

    fn is_exhausted(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_CONTEXT_ENTRIES_PER_CONTEXT, MAX_ROWS_PER_ENVELOPE, MAX_SESSION_ENVELOPE_BYTES,
        MAX_SESSION_STRING_BYTES, SessionAnnouncement, SessionRequestKind, decode_envelope,
        encode_envelope, encode_header, encode_u8, encode_u32, encode_u64, map_finish_error,
    };
    use spoor_core::{
        Context, ContextId, CounterDescId, FlowId, FlowPhase, LabelKey, MarkerKind, MarkerPhase,
        ScopeToken,
    };
    use spoor_runtime::{CounterKind, Snapshot, SnapshotMetadata};
    use spoor_session::{
        Chunking, EnvelopeHeader, OwnedArgEntry, OwnedCounterSnapshot, OwnedLabelValue,
        OwnedObservation, OwnedSessionEnvelope, OwnedSessionPayload, OwnedTimedObservation,
        PROTOCOL_VERSION, collect_snapshot,
    };
    use std::io::ErrorKind;
    use wtransport::{VarInt, error::StreamWriteError};

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

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
            hex
        })
    }

    /// Golden wire fixtures. These freeze the encoded byte layout that external
    /// consumers depend on; a change here means the wire format changed and the
    /// wire-format document must be updated to match.
    #[test]
    fn encoded_envelopes_match_golden_fixtures() {
        let counters = OwnedSessionEnvelope {
            header: EnvelopeHeader {
                protocol_version: PROTOCOL_VERSION,
                session_sequence: 4,
                chunk_index: 2,
            },
            payload: OwnedSessionPayload::Counters(vec![OwnedCounterSnapshot {
                desc_id: CounterDescId::new(3),
                name: String::from("ops"),
                unit: spoor_core::Unit::Count,
                context_id: ContextId::new(1),
                kind: CounterKind::Counter,
                value: 7,
                updated_at_micros: 14,
            }]),
        };
        let encoded = encode_envelope(&counters).expect("encoding should succeed");
        assert_eq!(
            to_hex(&encoded),
            "010000000400000000000000020000000000000002010000000300000000000000030000\
             006f70730101000000000000000007000000000000000e00000000000000"
        );

        let events = OwnedSessionEnvelope {
            header: EnvelopeHeader {
                protocol_version: PROTOCOL_VERSION,
                session_sequence: 4,
                chunk_index: 3,
            },
            payload: OwnedSessionPayload::Events(vec![OwnedTimedObservation {
                at_micros: 15,
                thread_id: 2,
                observation: OwnedObservation::ScopeBegin {
                    desc_id: spoor_core::ScopeDescId::new(5),
                    name: String::from("record"),
                    context_id: ContextId::new(1),
                    token: ScopeToken::new(9),
                    args: vec![OwnedArgEntry {
                        key: String::from("items"),
                        value: OwnedLabelValue::Unsigned(3),
                    }],
                },
            }]),
        };
        let encoded = encode_envelope(&events).expect("encoding should succeed");
        assert_eq!(
            to_hex(&encoded),
            "010000000400000000000000030000000000000003010000000f0000000000000002000000\
             00000000000500000000000000060000007265636f7264010000000000000009000000000000\
             0001050000006974656d73000300000000000000"
        );
    }

    #[test]
    fn codec_round_trips_owned_session_envelopes() {
        let snapshot = Snapshot {
            metadata: SnapshotMetadata {
                process_name: String::from("demo"),
                session_name: Some(String::from("wt")),
                session_sequence: 4,
                wall_clock_anchor_micros: 1_700_000_000_000_000,
            },
            capture_start_micros: 12,
            captured_at_micros: 20,
            contexts: vec![spoor_runtime::ContextSnapshot {
                id: ContextId::new(0),
                context: Context::root()
                    .with(LabelKey::Frame, 9_u32)
                    .with(LabelKey::Reason, "invalidate"),
            }],
            counters: vec![spoor_runtime::CounterSnapshot {
                desc_id: CounterDescId::new(3),
                name: "ops",
                unit: spoor_core::Unit::Count,
                context_id: ContextId::new(0),
                kind: CounterKind::Counter,
                value: 7,
                updated_at_micros: 14,
            }],
            events: vec![spoor_runtime::TimedObservation {
                at_micros: 15,
                thread_id: 2,
                observation: spoor_runtime::Observation::Flow {
                    flow_id: FlowId::new(8),
                    phase: FlowPhase::Step,
                    context_id: ContextId::new(0),
                },
            }],
            dropped_contexts: 0,
            dropped_counters: 1,
            dropped_events: 2,
        };

        let envelopes = collect_snapshot(&snapshot, Chunking::new(8, 8, 8));
        let round_tripped = envelopes
            .iter()
            .map(|envelope| {
                decode_envelope(&encode_envelope(envelope).expect("encoding should succeed"))
                    .expect("decoding should succeed")
            })
            .collect::<Vec<_>>();

        assert_eq!(round_tripped, envelopes);
        assert!(matches!(
            &round_tripped[3].payload,
            OwnedSessionPayload::Events(events)
                if matches!(&events[0].observation, OwnedObservation::Flow {
                    flow_id,
                    phase: FlowPhase::Step,
                    context_id
                } if *flow_id == FlowId::new(8) && *context_id == ContextId::new(0))
        ));
    }

    #[test]
    fn cursor_detects_trailing_bytes() {
        let envelope = OwnedSessionEnvelope {
            header: EnvelopeHeader {
                protocol_version: PROTOCOL_VERSION,
                session_sequence: 1,
                chunk_index: 0,
            },
            payload: OwnedSessionPayload::Events(vec![OwnedTimedObservation {
                at_micros: 2,
                thread_id: 1,
                observation: OwnedObservation::FrameMarker {
                    kind: MarkerKind::Frame,
                    phase: MarkerPhase::Begin,
                    context_id: ContextId::new(0),
                },
            }]),
        };
        let mut encoded = encode_envelope(&envelope).expect("encoding should succeed");
        encoded.push(99);
        let error = decode_envelope(&encoded).expect_err("decoder should reject trailing bytes");
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn decoder_rejects_oversized_envelope_frames() {
        let bytes = vec![0_u8; MAX_SESSION_ENVELOPE_BYTES + 1];
        let error = decode_envelope(&bytes).expect_err("decoder should reject oversized frames");

        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn decoder_rejects_oversized_strings_before_payload_allocation() {
        let mut bytes = Vec::new();
        encode_header(
            &mut bytes,
            EnvelopeHeader {
                protocol_version: PROTOCOL_VERSION,
                session_sequence: 1,
                chunk_index: 0,
            },
        );
        encode_u8(&mut bytes, 0);
        encode_u32(
            &mut bytes,
            u32::try_from(MAX_SESSION_STRING_BYTES + 1).expect("test string bound should fit u32"),
        );

        let error = decode_envelope(&bytes).expect_err("decoder should reject oversized strings");

        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn decoder_rejects_oversized_row_counts_before_allocation() {
        let mut bytes = Vec::new();
        encode_header(
            &mut bytes,
            EnvelopeHeader {
                protocol_version: PROTOCOL_VERSION,
                session_sequence: 1,
                chunk_index: 0,
            },
        );
        encode_u8(&mut bytes, 1);
        encode_u32(
            &mut bytes,
            u32::try_from(MAX_ROWS_PER_ENVELOPE + 1).expect("row bound should fit u32"),
        );

        let error =
            decode_envelope(&bytes).expect_err("decoder should reject oversized row counts");

        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn decoder_rejects_oversized_context_entry_counts_before_allocation() {
        let mut bytes = Vec::new();
        encode_header(
            &mut bytes,
            EnvelopeHeader {
                protocol_version: PROTOCOL_VERSION,
                session_sequence: 1,
                chunk_index: 0,
            },
        );
        encode_u8(&mut bytes, 1);
        encode_u32(&mut bytes, 1);
        encode_u64(&mut bytes, 0);
        encode_u32(
            &mut bytes,
            u32::try_from(MAX_CONTEXT_ENTRIES_PER_CONTEXT + 1)
                .expect("context entry bound should fit u32"),
        );

        let error =
            decode_envelope(&bytes).expect_err("decoder should reject oversized context entries");

        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn scope_token_observation_round_trips() {
        let envelope = OwnedSessionEnvelope {
            header: EnvelopeHeader {
                protocol_version: PROTOCOL_VERSION,
                session_sequence: 2,
                chunk_index: 1,
            },
            payload: OwnedSessionPayload::Events(vec![OwnedTimedObservation {
                at_micros: 3,
                thread_id: 4,
                observation: OwnedObservation::ScopeEnd {
                    token: ScopeToken::new(11),
                },
            }]),
        };
        let decoded =
            decode_envelope(&encode_envelope(&envelope).expect("encoding should succeed"))
                .expect("decoding should succeed");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn decoder_rejects_incompatible_codec_versions() {
        let envelope = OwnedSessionEnvelope {
            header: EnvelopeHeader {
                protocol_version: PROTOCOL_VERSION,
                session_sequence: 1,
                chunk_index: 0,
            },
            payload: OwnedSessionPayload::SnapshotEnd(spoor_session::DroppedSummary {
                contexts: 0,
                counters: 0,
                events: 0,
            }),
        };
        let encoded = encode_envelope(&envelope).expect("encoding should succeed");

        // The frame starts with little-endian u16 major and u16 minor.
        let mut different_major = encoded.clone();
        different_major[0..2].copy_from_slice(&2_u16.to_le_bytes());
        let error = decode_envelope(&different_major)
            .expect_err("decoder should reject a different major version");
        assert_eq!(error.kind(), ErrorKind::InvalidData);

        let mut newer_minor = encoded.clone();
        newer_minor[2..4].copy_from_slice(&(crate::CODEC_VERSION.minor() + 1).to_le_bytes());
        let error =
            decode_envelope(&newer_minor).expect_err("decoder should reject a newer minor version");
        assert_eq!(error.kind(), ErrorKind::InvalidData);

        decode_envelope(&encoded).expect("decoder should accept the supported version");
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
            }
            .session_id(),
            Some("wt_live")
        );
    }
}
