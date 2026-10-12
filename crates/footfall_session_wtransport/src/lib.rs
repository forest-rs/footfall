// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebTransport session adapter for Footfall.
//!
//! This crate lives strictly downstream of `footfall_session`. It transports owned session envelopes
//! over a single WebTransport stream; it does not participate in hot-path instrumentation.

use footfall_core::{
    ContextId, CounterDescId, EventDescId, FlowId, FlowPhase, InstrumentationLevel, MarkerKind,
    MarkerPhase, ResourceId, ResourceKindId, ScopeDescId, ScopeToken, Unit,
};
use footfall_runtime::{CounterKind, Snapshot};
use footfall_session::{
    CaptureSettings, Chunking, Consumer, Drops, Envelope, EnvelopeHeader, EventCursor, EventWindow,
    OwnedArgEntry, OwnedContextEntry, OwnedContextSnapshot, OwnedCounterSnapshot,
    OwnedHistogramSnapshot, OwnedLabelValue, OwnedObservation, OwnedSessionEnvelope,
    OwnedSessionPayload, OwnedSnapshotSummary, OwnedTimedObservation, Payload, ProtocolVersion,
    RowCounts, SnapshotMetadata, ThreadSnapshot, publish_snapshot,
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

use wtransport::tls::Sha256Digest;

/// SHA-256 hash of a session server's certificate, used to pin connections.
///
/// Clients pass the hash a server reports through
/// [`SessionIdentity::certificate_hash`], or the one stored with the local
/// identity, so they can trust that server's self-signed certificate without a
/// certificate authority. It displays as 64 lowercase hex digits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CertificateHash([u8; 32]);

impl CertificateHash {
    /// Creates a hash from its raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the raw hash bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn from_digest(digest: &Sha256Digest) -> Self {
        Self(*digest.as_ref())
    }

    fn to_digest(self) -> Sha256Digest {
        Sha256Digest::new(self.0)
    }
}

impl fmt::Display for CertificateHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

const DEFAULT_PATH: &str = "/footfall/session";
const DEFAULT_LOCAL_PORT: u16 = 43_091;
const DISCOVER_MAGIC: &[u8; 8] = b"FFALLD01";
const SNAPSHOT_MAGIC: &[u8; 8] = b"FFALLS01";
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
pub const MAX_CONTEXT_ENTRIES_PER_CONTEXT: usize = footfall_core::CONTEXT_INLINE_CAPACITY;
/// Maximum per-instance arguments accepted for one encoded observation.
pub const MAX_ARGS_PER_OBSERVATION: usize = footfall_core::ARGS_INLINE_CAPACITY;

/// Identity and certificate pinning material for a local session server.
#[derive(Debug)]
pub struct SessionIdentity {
    identity: Identity,
    certificate_hash: CertificateHash,
}

impl SessionIdentity {
    /// Creates a self-signed identity suitable for local Footfall session demos.
    pub fn self_signed_localhost() -> io::Result<Self> {
        let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"])
            .map_err(|error| io::Error::other(error.to_string()))?;
        let certificate = identity
            .certificate_chain()
            .as_slice()
            .first()
            .ok_or_else(|| io::Error::other("self-signed identity produced no certificate"))?;
        Ok(Self {
            certificate_hash: CertificateHash::from_digest(&certificate.hash()),
            identity,
        })
    }

    /// Returns the pinned certificate hash clients should use.
    #[must_use]
    pub const fn certificate_hash(&self) -> CertificateHash {
        self.certificate_hash
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
            certificate_hash: CertificateHash::from_digest(&certificate.hash()),
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

    /// Returns true when the identity is expired, not yet valid, unparsable,
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
    pub async fn load_localhost_certificate_hash() -> io::Result<CertificateHash> {
        let paths = local_identity_paths()?;
        let certificate = wtransport::tls::Certificate::load_pemfile(&paths.cert)
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(CertificateHash::from_digest(&certificate.hash()))
    }
}

/// Discoverable metadata for one local Footfall WebTransport session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionAnnouncement {
    /// Stable session id unique within one session server.
    id: String,
    /// Process name associated with the published session.
    process_name: String,
    /// Optional capture-session label.
    session_name: Option<String>,
}

impl SessionAnnouncement {
    /// Returns the stable session id unique within one session server.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the process name associated with the published session.
    #[must_use]
    pub fn process_name(&self) -> &str {
        &self.process_name
    }

    /// Returns the optional capture-session label.
    #[must_use]
    pub fn session_name(&self) -> Option<&str> {
        self.session_name.as_deref()
    }
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
#[non_exhaustive]
pub enum SessionRequestKind {
    /// Request the currently discoverable sessions from this server.
    Discover,
    /// Request one named session snapshot from this server.
    Snapshot {
        /// Session id advertised by discovery.
        session_id: String,
        /// Where the consumer's previous read ended, from
        /// [`OwnedSnapshotSummary::next_cursor`], or `None` for everything
        /// retained. Pass it to `Runtime::snapshot_since`.
        since: Option<EventCursor>,
    },
}

impl SessionRequestKind {
    /// Returns the requested session id when this is a snapshot fetch.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Discover => None,
            Self::Snapshot { session_id, .. } => Some(session_id.as_str()),
        }
    }
}

/// Summary of one served request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServedRequest {
    /// Request authority observed by the server.
    authority: String,
    /// Request path served by the server.
    path: String,
    /// Response kind emitted by the server.
    kind: ServedRequestKind,
}

impl ServedRequest {
    /// Returns the request authority observed by the server.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// Returns the request path served by the server.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the response kind emitted by the server.
    #[must_use]
    pub const fn kind(&self) -> &ServedRequestKind {
        &self.kind
    }
}

/// Classification for a served request.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
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
    bind_address: SocketAddr,
    /// WebTransport path accepted by the server.
    path: String,
}

impl SessionServerConfig {
    /// Returns the local UDP address to bind.
    #[must_use]
    pub const fn bind_address(&self) -> SocketAddr {
        self.bind_address
    }

    /// Returns the WebTransport path accepted by the server.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
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
    /// Sets the local UDP address to bind.
    ///
    /// The default binds to localhost. The session protocol has no client
    /// authentication, so binding to any other interface exposes snapshots,
    /// including message text, to every client that can reach the port.
    #[must_use]
    pub const fn with_bind_address(mut self, bind_address: SocketAddr) -> Self {
        self.bind_address = bind_address;
        self
    }

    /// Sets the WebTransport path accepted by the server.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

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

/// One-shot WebTransport server for Footfall session envelopes.
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
            let session_id = read_stream_string(&mut recv).await?;
            let since = match read_u8_stream(&mut recv).await? {
                0 => None,
                1 => Some(EventCursor::new(
                    read_u64(&mut recv).await?,
                    read_u64(&mut recv).await?,
                )),
                tag => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid snapshot cursor tag: {tag}"),
                    ));
                }
            };
            SessionRequestKind::Snapshot { session_id, since }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Footfall session request preface",
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

// Payload tags, numbered in publication order.
const TAG_SNAPSHOT_START: u8 = 0;
const TAG_THREADS: u8 = 1;
const TAG_CONTEXTS: u8 = 2;
const TAG_COUNTERS: u8 = 3;
const TAG_HISTOGRAMS: u8 = 4;
const TAG_EVENTS: u8 = 5;
const TAG_SNAPSHOT_END: u8 = 6;

fn encode_borrowed_envelope(envelope: Envelope<'_>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    encode_header(&mut bytes, envelope.header());
    match envelope.payload() {
        Payload::SnapshotStart(summary) => {
            encode_u8(&mut bytes, TAG_SNAPSHOT_START);
            encode_snapshot_summary(
                &mut bytes,
                summary.metadata(),
                summary.captured_at_micros(),
                summary.window(),
                summary.counts(),
            )?;
        }
        Payload::Threads(threads) => {
            encode_u8(&mut bytes, TAG_THREADS);
            encode_threads(&mut bytes, threads)?;
        }
        Payload::Contexts(contexts) => {
            encode_u8(&mut bytes, TAG_CONTEXTS);
            encode_borrowed_contexts(&mut bytes, contexts)?;
        }
        Payload::Counters(counters) => {
            encode_u8(&mut bytes, TAG_COUNTERS);
            encode_borrowed_counters(&mut bytes, counters)?;
        }
        Payload::Histograms(histograms) => {
            encode_u8(&mut bytes, TAG_HISTOGRAMS);
            encode_borrowed_histograms(&mut bytes, histograms)?;
        }
        Payload::Events(events) => {
            encode_u8(&mut bytes, TAG_EVENTS);
            encode_borrowed_events(&mut bytes, events)?;
        }
        Payload::SnapshotEnd(drops) => {
            encode_u8(&mut bytes, TAG_SNAPSHOT_END);
            encode_drops(&mut bytes, drops);
        }
        _ => return Err(unsupported("payload")),
    }
    Ok(bytes)
}

#[cfg(test)]
fn encode_envelope(envelope: &OwnedSessionEnvelope) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    encode_header(&mut bytes, envelope.header());
    match envelope.payload() {
        OwnedSessionPayload::SnapshotStart(summary) => {
            encode_u8(&mut bytes, TAG_SNAPSHOT_START);
            encode_snapshot_summary(
                &mut bytes,
                summary.metadata(),
                summary.captured_at_micros(),
                summary.window(),
                summary.counts(),
            )?;
        }
        OwnedSessionPayload::Threads(threads) => {
            encode_u8(&mut bytes, TAG_THREADS);
            encode_threads(&mut bytes, threads)?;
        }
        OwnedSessionPayload::Contexts(contexts) => {
            encode_u8(&mut bytes, TAG_CONTEXTS);
            encode_contexts(&mut bytes, contexts)?;
        }
        OwnedSessionPayload::Counters(counters) => {
            encode_u8(&mut bytes, TAG_COUNTERS);
            encode_counters(&mut bytes, counters)?;
        }
        OwnedSessionPayload::Histograms(histograms) => {
            encode_u8(&mut bytes, TAG_HISTOGRAMS);
            encode_histograms(&mut bytes, histograms)?;
        }
        OwnedSessionPayload::Events(events) => {
            encode_u8(&mut bytes, TAG_EVENTS);
            encode_events(&mut bytes, events)?;
        }
        OwnedSessionPayload::SnapshotEnd(drops) => {
            encode_u8(&mut bytes, TAG_SNAPSHOT_END);
            encode_drops(&mut bytes, *drops);
        }
        _ => return Err(unsupported("payload")),
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
        TAG_SNAPSHOT_START => {
            OwnedSessionPayload::SnapshotStart(decode_snapshot_summary(&mut cursor)?)
        }
        TAG_THREADS => OwnedSessionPayload::Threads(decode_threads(&mut cursor)?),
        TAG_CONTEXTS => OwnedSessionPayload::Contexts(decode_contexts(&mut cursor)?),
        TAG_COUNTERS => OwnedSessionPayload::Counters(decode_counters(&mut cursor)?),
        TAG_HISTOGRAMS => OwnedSessionPayload::Histograms(decode_histograms(&mut cursor)?),
        TAG_EVENTS => OwnedSessionPayload::Events(decode_events(&mut cursor)?),
        TAG_SNAPSHOT_END => OwnedSessionPayload::SnapshotEnd(decode_drops(&mut cursor)?),
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
    Ok(OwnedSessionEnvelope::new(header, payload))
}

fn encode_header(bytes: &mut Vec<u8>, header: EnvelopeHeader) {
    encode_u16(bytes, header.protocol_version().major());
    encode_u16(bytes, header.protocol_version().minor());
    encode_u64(bytes, header.chunk_index());
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
    Ok(EnvelopeHeader::new(protocol_version, decode_u64(cursor)?))
}

fn encode_snapshot_summary(
    bytes: &mut Vec<u8>,
    metadata: &SnapshotMetadata,
    captured_at_micros: u64,
    window: EventWindow,
    counts: RowCounts,
) -> io::Result<()> {
    encode_snapshot_metadata(bytes, metadata)?;
    encode_u64(bytes, captured_at_micros);
    encode_u64(bytes, window.since());
    encode_u64(bytes, window.next());
    encode_u64(bytes, window.missed());
    encode_usize(bytes, counts.threads())?;
    encode_usize(bytes, counts.contexts())?;
    encode_usize(bytes, counts.counters())?;
    encode_usize(bytes, counts.histograms())?;
    encode_usize(bytes, counts.events())?;
    Ok(())
}

fn decode_snapshot_summary(cursor: &mut Cursor<'_>) -> io::Result<OwnedSnapshotSummary> {
    let metadata = decode_snapshot_metadata(cursor)?;
    let captured_at_micros = decode_u64(cursor)?;
    let window = EventWindow::new(
        decode_u64(cursor)?,
        decode_u64(cursor)?,
        decode_u64(cursor)?,
    );
    let counts = RowCounts::new(
        decode_usize(cursor)?,
        decode_usize(cursor)?,
        decode_usize(cursor)?,
        decode_usize(cursor)?,
        decode_usize(cursor)?,
    );
    Ok(OwnedSnapshotSummary::new(
        metadata,
        captured_at_micros,
        window,
        counts,
    ))
}

fn encode_snapshot_metadata(bytes: &mut Vec<u8>, metadata: &SnapshotMetadata) -> io::Result<()> {
    encode_string(bytes, metadata.process_name())?;
    encode_optional_string(bytes, metadata.session_name())?;
    encode_u64(bytes, metadata.instance_id());
    encode_u64(bytes, metadata.wall_clock_anchor_micros());
    encode_string(bytes, metadata.producer_version())?;
    let capture = metadata.capture();
    encode_u8(bytes, encode_level(capture.level())?);
    encode_u64(bytes, usize_to_u64(capture.counter_capacity()));
    encode_u64(bytes, usize_to_u64(capture.event_capacity()));
    encode_u64(bytes, usize_to_u64(capture.context_capacity()));
    Ok(())
}

fn decode_snapshot_metadata(cursor: &mut Cursor<'_>) -> io::Result<SnapshotMetadata> {
    let process_name = decode_string(cursor)?;
    let session_name = decode_optional_string(cursor, "session name")?;
    let instance_id = decode_u64(cursor)?;
    let wall_clock_anchor_micros = decode_u64(cursor)?;
    let producer_version = decode_string(cursor)?;
    let capture = CaptureSettings::new(
        decode_level(decode_u8(cursor)?)?,
        decode_u64_as_usize(cursor)?,
        decode_u64_as_usize(cursor)?,
        decode_u64_as_usize(cursor)?,
    );
    let metadata = SnapshotMetadata::new(process_name)
        .with_instance_id(instance_id)
        .with_wall_clock_anchor_micros(wall_clock_anchor_micros)
        .with_producer_version(producer_version)
        .with_capture(capture);
    Ok(match session_name {
        Some(session_name) => metadata.with_session_name(session_name),
        None => metadata,
    })
}

fn encode_threads(bytes: &mut Vec<u8>, threads: &[ThreadSnapshot]) -> io::Result<()> {
    encode_len(bytes, threads.len())?;
    for thread in threads {
        encode_u64(bytes, thread.id());
        encode_optional_string(bytes, thread.name())?;
    }
    Ok(())
}

fn decode_threads(cursor: &mut Cursor<'_>) -> io::Result<Vec<ThreadSnapshot>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "thread rows")?;
    let mut threads = Vec::with_capacity(len);
    for _ in 0..len {
        let id = decode_u64(cursor)?;
        threads.push(ThreadSnapshot::new(
            id,
            decode_optional_string(cursor, "thread name")?,
        ));
    }
    Ok(threads)
}

fn encode_optional_string(bytes: &mut Vec<u8>, value: Option<&str>) -> io::Result<()> {
    match value {
        Some(value) => {
            encode_u8(bytes, 1);
            encode_string(bytes, value)
        }
        None => {
            encode_u8(bytes, 0);
            Ok(())
        }
    }
}

fn decode_optional_string(cursor: &mut Cursor<'_>, what: &str) -> io::Result<Option<String>> {
    match decode_u8(cursor)? {
        0 => Ok(None),
        1 => Ok(Some(decode_string(cursor)?)),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid {what} presence tag: {tag}"),
        )),
    }
}

fn encode_level(level: InstrumentationLevel) -> io::Result<u8> {
    match level {
        InstrumentationLevel::Off => Ok(0),
        InstrumentationLevel::Counters => Ok(1),
        InstrumentationLevel::Scoped => Ok(2),
        InstrumentationLevel::Trace => Ok(3),
        _ => Err(unsupported("instrumentation level")),
    }
}

fn decode_level(tag: u8) -> io::Result<InstrumentationLevel> {
    match tag {
        0 => Ok(InstrumentationLevel::Off),
        1 => Ok(InstrumentationLevel::Counters),
        2 => Ok(InstrumentationLevel::Scoped),
        3 => Ok(InstrumentationLevel::Trace),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown instrumentation level tag: {tag}"),
        )),
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn decode_u64_as_usize(cursor: &mut Cursor<'_>) -> io::Result<usize> {
    usize::try_from(decode_u64(cursor)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "u64 value does not fit usize"))
}

#[cfg(test)]
fn encode_contexts(bytes: &mut Vec<u8>, contexts: &[OwnedContextSnapshot]) -> io::Result<()> {
    encode_len(bytes, contexts.len())?;
    for context in contexts {
        encode_u64(bytes, context.id().get());
        encode_len(bytes, context.entries().len())?;
        for entry in context.entries() {
            encode_string(bytes, entry.key())?;
            encode_label_value(bytes, entry.value())?;
        }
    }
    Ok(())
}

fn encode_borrowed_contexts(
    bytes: &mut Vec<u8>,
    contexts: &[footfall_runtime::ContextSnapshot],
) -> io::Result<()> {
    encode_len(bytes, contexts.len())?;
    for context in contexts {
        encode_u64(bytes, context.id().get());
        encode_len(bytes, context.context().len())?;
        for entry in context.context().iter() {
            encode_string(bytes, entry.key().name())?;
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
            entries.push(OwnedContextEntry::new(
                decode_string(cursor)?,
                decode_label_value(cursor)?,
            ));
        }
        contexts.push(OwnedContextSnapshot::new(id, entries));
    }
    Ok(contexts)
}

fn encode_borrowed_label_value(
    bytes: &mut Vec<u8>,
    value: footfall_core::LabelValue,
) -> io::Result<()> {
    match value {
        footfall_core::LabelValue::Unsigned(value) => {
            encode_u8(bytes, 0);
            encode_u64(bytes, value);
        }
        footfall_core::LabelValue::Signed(value) => {
            encode_u8(bytes, 1);
            encode_i64(bytes, value);
        }
        footfall_core::LabelValue::Bool(value) => {
            encode_u8(bytes, 2);
            encode_u8(bytes, u8::from(value));
        }
        footfall_core::LabelValue::StaticStr(value) => {
            encode_u8(bytes, 3);
            encode_string(bytes, value)?;
        }
        _ => return Err(unsupported("label value")),
    }
    Ok(())
}

#[cfg(test)]
fn encode_counters(bytes: &mut Vec<u8>, counters: &[OwnedCounterSnapshot]) -> io::Result<()> {
    encode_len(bytes, counters.len())?;
    for counter in counters {
        encode_u64(bytes, counter.desc_id().get());
        encode_string(bytes, counter.name())?;
        encode_u8(bytes, encode_unit(counter.unit())?);
        encode_u64(bytes, counter.context_id().get());
        encode_u8(bytes, encode_counter_kind(counter.kind())?);
        encode_u64(bytes, counter.value());
        encode_u64(bytes, counter.updated_at_micros());
    }
    Ok(())
}

fn encode_borrowed_counters(
    bytes: &mut Vec<u8>,
    counters: &[footfall_runtime::CounterSnapshot],
) -> io::Result<()> {
    encode_len(bytes, counters.len())?;
    for counter in counters {
        encode_u64(bytes, counter.desc_id().get());
        encode_string(bytes, counter.name())?;
        encode_u8(bytes, encode_unit(counter.unit())?);
        encode_u64(bytes, counter.context_id().get());
        encode_u8(bytes, encode_counter_kind(counter.kind())?);
        encode_u64(bytes, counter.value());
        encode_u64(bytes, counter.updated_at_micros());
    }
    Ok(())
}

fn decode_counters(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedCounterSnapshot>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "counter rows")?;
    let mut counters = Vec::with_capacity(len);
    for _ in 0..len {
        counters.push(OwnedCounterSnapshot::new(
            CounterDescId::new(decode_u64(cursor)?),
            decode_string(cursor)?,
            decode_unit(decode_u8(cursor)?)?,
            ContextId::new(decode_u64(cursor)?),
            decode_counter_kind(decode_u8(cursor)?)?,
            decode_u64(cursor)?,
            decode_u64(cursor)?,
        ));
    }
    Ok(counters)
}

#[cfg(test)]
fn encode_histograms(bytes: &mut Vec<u8>, histograms: &[OwnedHistogramSnapshot]) -> io::Result<()> {
    encode_len(bytes, histograms.len())?;
    for histogram in histograms {
        encode_u64(bytes, histogram.desc_id().get());
        encode_string(bytes, histogram.name())?;
        encode_u8(bytes, encode_unit(histogram.unit())?);
        encode_u64(bytes, histogram.context_id().get());
        encode_histogram_body(
            bytes,
            histogram.count(),
            histogram.sum(),
            histogram.min(),
            histogram.max(),
            histogram.buckets(),
            histogram.updated_at_micros(),
        );
    }
    Ok(())
}

fn encode_borrowed_histograms(
    bytes: &mut Vec<u8>,
    histograms: &[footfall_runtime::HistogramSnapshot],
) -> io::Result<()> {
    encode_len(bytes, histograms.len())?;
    for histogram in histograms {
        encode_u64(bytes, histogram.desc_id().get());
        encode_string(bytes, histogram.name())?;
        encode_u8(bytes, encode_unit(histogram.unit())?);
        encode_u64(bytes, histogram.context_id().get());
        encode_histogram_body(
            bytes,
            histogram.count(),
            histogram.sum(),
            histogram.min(),
            histogram.max(),
            histogram.buckets(),
            histogram.updated_at_micros(),
        );
    }
    Ok(())
}

fn encode_histogram_body(
    bytes: &mut Vec<u8>,
    count: u64,
    sum: u64,
    min: u64,
    max: u64,
    buckets: &[u64; footfall_runtime::HISTOGRAM_BUCKET_COUNT],
    updated_at_micros: u64,
) {
    encode_u64(bytes, count);
    encode_u64(bytes, sum);
    encode_u64(bytes, min);
    encode_u64(bytes, max);
    for bucket in buckets {
        encode_u64(bytes, *bucket);
    }
    encode_u64(bytes, updated_at_micros);
}

fn decode_histograms(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedHistogramSnapshot>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "histogram rows")?;
    let mut histograms = Vec::with_capacity(len);
    for _ in 0..len {
        let desc_id = CounterDescId::new(decode_u64(cursor)?);
        let name = decode_string(cursor)?;
        let unit = decode_unit(decode_u8(cursor)?)?;
        let context_id = ContextId::new(decode_u64(cursor)?);
        let count = decode_u64(cursor)?;
        let sum = decode_u64(cursor)?;
        let min = decode_u64(cursor)?;
        let max = decode_u64(cursor)?;
        let mut buckets = [0_u64; footfall_runtime::HISTOGRAM_BUCKET_COUNT];
        for bucket in &mut buckets {
            *bucket = decode_u64(cursor)?;
        }
        histograms.push(OwnedHistogramSnapshot::new(
            desc_id,
            name,
            unit,
            context_id,
            count,
            sum,
            min,
            max,
            buckets,
            decode_u64(cursor)?,
        ));
    }
    Ok(histograms)
}

#[cfg(test)]
fn encode_events(bytes: &mut Vec<u8>, events: &[OwnedTimedObservation]) -> io::Result<()> {
    encode_len(bytes, events.len())?;
    for event in events {
        encode_u64(bytes, event.at_micros());
        encode_u64(bytes, event.sequence());
        encode_u64(bytes, event.thread_id());
        encode_observation(bytes, event.observation())?;
    }
    Ok(())
}

fn encode_borrowed_events(
    bytes: &mut Vec<u8>,
    events: &[footfall_runtime::TimedObservation],
) -> io::Result<()> {
    encode_len(bytes, events.len())?;
    for event in events {
        encode_u64(bytes, event.at_micros());
        encode_u64(bytes, event.sequence());
        encode_u64(bytes, event.thread_id());
        encode_borrowed_observation(bytes, event.observation())?;
    }
    Ok(())
}

fn decode_events(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedTimedObservation>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "event rows")?;
    let mut events = Vec::with_capacity(len);
    for _ in 0..len {
        events.push(OwnedTimedObservation::new(
            decode_u64(cursor)?,
            decode_u64(cursor)?,
            decode_u64(cursor)?,
            decode_observation(cursor)?,
        ));
    }
    Ok(events)
}

fn encode_borrowed_observation(
    bytes: &mut Vec<u8>,
    observation: &footfall_runtime::Observation,
) -> io::Result<()> {
    match observation {
        footfall_runtime::Observation::ScopeBegin {
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
        footfall_runtime::Observation::ScopeEnd { token } => {
            encode_u8(bytes, 1);
            encode_u64(bytes, token.get());
        }
        footfall_runtime::Observation::ResourceCreate {
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
        footfall_runtime::Observation::ResourceUpdate {
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
        footfall_runtime::Observation::ResourceDrop {
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
        footfall_runtime::Observation::FrameMarker {
            kind,
            phase,
            context_id,
        } => {
            encode_u8(bytes, 5);
            encode_u8(bytes, encode_marker_kind(*kind)?);
            encode_u8(bytes, encode_marker_phase(*phase));
            encode_u64(bytes, context_id.get());
        }
        footfall_runtime::Observation::Flow {
            flow_id,
            phase,
            context_id,
        } => {
            encode_u8(bytes, 6);
            encode_u64(bytes, flow_id.get());
            encode_u8(bytes, encode_flow_phase(*phase));
            encode_u64(bytes, context_id.get());
        }
        footfall_runtime::Observation::Message {
            desc_id,
            name,
            context_id,
            text,
            args,
        } => {
            encode_u8(bytes, 7);
            encode_u64(bytes, desc_id.get());
            encode_string(bytes, name)?;
            encode_u64(bytes, context_id.get());
            encode_string(bytes, text)?;
            encode_borrowed_args(bytes, *args)?;
        }
        _ => return Err(unsupported("observation")),
    }
    Ok(())
}

#[cfg(test)]
fn encode_args(bytes: &mut Vec<u8>, args: &[OwnedArgEntry]) -> io::Result<()> {
    let count = u8::try_from(args.len().min(MAX_ARGS_PER_OBSERVATION))
        .expect("bounded args count fits into u8");
    encode_u8(bytes, count);
    for entry in args.iter().take(usize::from(count)) {
        encode_string(bytes, entry.key())?;
        encode_label_value(bytes, entry.value())?;
    }
    Ok(())
}

fn encode_borrowed_args(bytes: &mut Vec<u8>, args: footfall_core::Args) -> io::Result<()> {
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
        args.push(OwnedArgEntry::new(
            decode_string(cursor)?,
            decode_label_value(cursor)?,
        ));
    }
    Ok(args)
}

fn encode_drops(bytes: &mut Vec<u8>, drops: Drops) {
    encode_u64(bytes, drops.refused_contexts());
    encode_u64(bytes, drops.unattributed());
    encode_u64(bytes, drops.counter_updates());
    encode_u64(bytes, drops.evicted_events());
}

fn decode_drops(cursor: &mut Cursor<'_>) -> io::Result<Drops> {
    Ok(Drops::new()
        .with_refused_contexts(decode_u64(cursor)?)
        .with_unattributed(decode_u64(cursor)?)
        .with_counter_updates(decode_u64(cursor)?)
        .with_evicted_events(decode_u64(cursor)?))
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
        _ => return Err(unsupported("label value")),
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
            encode_u8(bytes, encode_marker_kind(*kind)?);
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
        OwnedObservation::Message {
            desc_id,
            name,
            context_id,
            text,
            args,
        } => {
            encode_u8(bytes, 7);
            encode_u64(bytes, desc_id.get());
            encode_string(bytes, name)?;
            encode_u64(bytes, context_id.get());
            encode_string(bytes, text)?;
            encode_args(bytes, args)?;
        }
        _ => return Err(unsupported("observation")),
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
        7 => Ok(OwnedObservation::Message {
            desc_id: EventDescId::new(decode_u64(cursor)?),
            name: decode_string(cursor)?,
            context_id: ContextId::new(decode_u64(cursor)?),
            text: decode_string(cursor)?,
            args: decode_args(cursor)?,
        }),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown observation tag: {tag}"),
        )),
    }
}

fn encode_counter_kind(kind: CounterKind) -> io::Result<u8> {
    match kind {
        CounterKind::Counter => Ok(0),
        CounterKind::Gauge => Ok(1),
        _ => Err(unsupported("counter kind")),
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

fn encode_unit(unit: Unit) -> io::Result<u8> {
    match unit {
        Unit::Bytes => Ok(0),
        Unit::Count => Ok(1),
        Unit::Nanos => Ok(2),
        Unit::Micros => Ok(3),
        Unit::Ratio => Ok(4),
        Unit::Items => Ok(5),
        _ => Err(unsupported("unit")),
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

fn encode_marker_kind(kind: MarkerKind) -> io::Result<u8> {
    match kind {
        MarkerKind::Frame => Ok(0),
        MarkerKind::Transaction => Ok(1),
        MarkerKind::Pass => Ok(2),
        _ => Err(unsupported("marker kind")),
    }
}

/// Error for a value this protocol version has no encoding for.
fn unsupported(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{what} has no encoding in this session protocol version"),
    )
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
                .join("footfall")
        })
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = env::var_os("APPDATA") {
            Ok(PathBuf::from(app_data).join("footfall"))
        } else {
            home_directory().map(|home| home.join("AppData").join("Roaming").join("footfall"))
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(state_home) = env::var_os("XDG_STATE_HOME") {
            Ok(PathBuf::from(state_home).join("footfall"))
        } else {
            home_directory().map(|home| home.join(".local").join("state").join("footfall"))
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
        encode_borrowed_envelope, encode_envelope, encode_header, encode_u8, encode_u32,
        encode_u64, map_finish_error,
    };
    use footfall_core::{
        Context, ContextId, CounterDescId, EventDescId, FlowId, FlowPhase, LabelKey, MarkerKind,
        MarkerPhase, ScopeToken,
    };
    use footfall_runtime::{CounterKind, Snapshot, SnapshotMetadata, ThreadSnapshot};
    use footfall_session::{
        Chunking, Envelope, EnvelopeHeader, OwnedArgEntry, OwnedContextEntry, OwnedContextSnapshot,
        OwnedCounterSnapshot, OwnedLabelValue, OwnedObservation, OwnedSessionEnvelope,
        OwnedSessionPayload, OwnedSnapshotSummary, OwnedTimedObservation, PROTOCOL_VERSION,
        Payload, RowCounts, collect_snapshot,
    };
    use std::io::ErrorKind;
    use wtransport::{VarInt, error::StreamWriteError};

    static FRAME: LabelKey = LabelKey::new("frame");
    static REASON: LabelKey = LabelKey::new("reason");

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
        let counters = OwnedSessionEnvelope::new(
            EnvelopeHeader::new(PROTOCOL_VERSION, 2),
            OwnedSessionPayload::Counters(vec![OwnedCounterSnapshot::new(
                CounterDescId::new(3),
                String::from("ops"),
                footfall_core::Unit::Count,
                ContextId::new(1),
                CounterKind::Counter,
                7,
                14,
            )]),
        );
        let encoded = encode_envelope(&counters).expect("encoding should succeed");
        assert_eq!(
            to_hex(&encoded),
            "01000000020000000000000003010000000300000000000000030000006f707301010000\
             00000000000007000000000000000e00000000000000"
        );

        let events = OwnedSessionEnvelope::new(
            EnvelopeHeader::new(PROTOCOL_VERSION, 3),
            OwnedSessionPayload::Events(vec![OwnedTimedObservation::new(
                15,
                4,
                2,
                OwnedObservation::ScopeBegin {
                    desc_id: footfall_core::ScopeDescId::new(5),
                    name: String::from("record"),
                    context_id: ContextId::new(1),
                    token: ScopeToken::new(9),
                    args: vec![OwnedArgEntry::new(
                        String::from("items"),
                        OwnedLabelValue::Unsigned(3),
                    )],
                },
            )]),
        );
        let encoded = encode_envelope(&events).expect("encoding should succeed");
        assert_eq!(
            to_hex(&encoded),
            "01000000030000000000000005010000000f000000000000000400000000000000020000\
             0000000000000500000000000000060000007265636f7264010000000000000009000000\
             0000000001050000006974656d73000300000000000000"
        );

        // Context rows carry label keys by name. The production encoder for
        // borrowed runtime snapshots must produce the same bytes.
        let header = EnvelopeHeader::new(PROTOCOL_VERSION, 1);
        let contexts = OwnedSessionEnvelope::new(
            header,
            OwnedSessionPayload::Contexts(vec![OwnedContextSnapshot::new(
                ContextId::new(1),
                vec![
                    OwnedContextEntry::new(String::from("frame"), OwnedLabelValue::Unsigned(7)),
                    OwnedContextEntry::new(
                        String::from("reason"),
                        OwnedLabelValue::String(String::from("invalidate")),
                    ),
                ],
            )]),
        );
        let expected = "010000000100000000000000020100000001000000000000000200000005000000667261\
                        6d6500070000000000000006000000726561736f6e030a000000696e76616c6964617465";
        let encoded = encode_envelope(&contexts).expect("encoding should succeed");
        assert_eq!(to_hex(&encoded), expected.replace(' ', ""));

        let borrowed = [footfall_runtime::ContextSnapshot::new(
            ContextId::new(1),
            Context::root()
                .with(&FRAME, 7_u32)
                .with(&REASON, "invalidate"),
        )];
        let encoded = encode_borrowed_envelope(Envelope::new(header, Payload::Contexts(&borrowed)))
            .expect("encoding should succeed");
        assert_eq!(to_hex(&encoded), expected.replace(' ', ""));
    }

    /// Golden fixtures for the envelopes that frame a snapshot, built from
    /// the layouts in the wire-format document.
    #[test]
    fn snapshot_framing_envelopes_match_golden_fixtures() {
        let metadata = SnapshotMetadata::new("demo")
            .with_instance_id(0x0102_0304_0506_0708)
            .with_wall_clock_anchor_micros(9)
            .with_producer_version("0.1.0")
            .with_capture(footfall_session::CaptureSettings::new(
                footfall_core::InstrumentationLevel::Scoped,
                16,
                64,
                32,
            ));
        let start = OwnedSessionEnvelope::new(
            EnvelopeHeader::new(PROTOCOL_VERSION, 0),
            OwnedSessionPayload::SnapshotStart(OwnedSnapshotSummary::new(
                metadata,
                20,
                footfall_session::EventWindow::new(1, 3, 1),
                RowCounts::new(1, 2, 0, 0, 1),
            )),
        );
        assert_eq!(
            to_hex(&encode_envelope(&start).expect("encoding should succeed")),
            "010000000000000000000000000400000064656d6f000807060504030201090000000000\
             000005000000302e312e3002100000000000000040000000000000002000000000000000\
             140000000000000001000000000000000300000000000000010000000000000001000000\
             02000000000000000000000001000000"
        );

        let threads = OwnedSessionEnvelope::new(
            EnvelopeHeader::new(PROTOCOL_VERSION, 1),
            OwnedSessionPayload::Threads(vec![
                ThreadSnapshot::new(3, Some(String::from("render"))),
                ThreadSnapshot::new(4, None),
            ]),
        );
        assert_eq!(
            to_hex(&encode_envelope(&threads).expect("encoding should succeed")),
            "01000000010000000000000001020000000300000000000000010600000072656e646572\
             040000000000000000"
        );

        let end = OwnedSessionEnvelope::new(
            EnvelopeHeader::new(PROTOCOL_VERSION, 2),
            OwnedSessionPayload::SnapshotEnd(
                footfall_session::Drops::new()
                    .with_refused_contexts(1)
                    .with_unattributed(2)
                    .with_counter_updates(3)
                    .with_evicted_events(4),
            ),
        );
        assert_eq!(
            to_hex(&encode_envelope(&end).expect("encoding should succeed")),
            "010000000200000000000000060100000000000000020000000000000003000000000000\
             000400000000000000"
        );

        for envelope in [start, threads, end] {
            let bytes = encode_envelope(&envelope).expect("encoding should succeed");
            assert_eq!(
                decode_envelope(&bytes).expect("decoding should succeed"),
                envelope
            );
        }
    }

    #[test]
    fn codec_round_trips_owned_session_envelopes() {
        let mut histogram = footfall_runtime::HistogramSnapshot::new(
            CounterDescId::new(4),
            "frame_micros",
            footfall_core::Unit::Micros,
            ContextId::new(0),
        );
        histogram.record(8, 14);
        histogram.record(16, 15);
        let metadata = SnapshotMetadata::new("demo")
            .with_session_name("wt")
            .with_instance_id(0x1234_5678_9abc_def0)
            .with_wall_clock_anchor_micros(1_700_000_000_000_000)
            .with_producer_version("0.1.0")
            .with_capture(footfall_session::CaptureSettings::new(
                footfall_core::InstrumentationLevel::Scoped,
                16,
                64,
                32,
            ));
        let snapshot = Snapshot::builder(metadata)
            .captured_at_micros(20)
            .window(footfall_session::EventWindow::new(0, 3, 1))
            .thread(ThreadSnapshot::new(2, Some(String::from("worker"))))
            .context(footfall_runtime::ContextSnapshot::new(
                ContextId::new(0),
                Context::root()
                    .with(&FRAME, 9_u32)
                    .with(&REASON, "invalidate"),
            ))
            .counter(footfall_runtime::CounterSnapshot::new(
                CounterDescId::new(3),
                "ops",
                footfall_core::Unit::Count,
                ContextId::new(0),
                CounterKind::Counter,
                7,
                14,
            ))
            .histogram(histogram)
            .event(footfall_runtime::TimedObservation::new(
                15,
                0,
                2,
                footfall_runtime::Observation::Flow {
                    flow_id: FlowId::new(8),
                    phase: FlowPhase::Step,
                    context_id: ContextId::new(0),
                },
            ))
            .event(footfall_runtime::TimedObservation::new(
                16,
                1,
                2,
                footfall_runtime::Observation::Message {
                    desc_id: EventDescId::new(9),
                    name: "app_log",
                    context_id: ContextId::new(0),
                    text: String::from("cache warmed"),
                    args: footfall_core::Args::none().with("level", "info"),
                },
            ))
            .drops(
                footfall_runtime::Drops::new()
                    .with_refused_contexts(0)
                    .with_counter_updates(1)
                    .with_evicted_events(2),
            )
            .build();

        let envelopes = collect_snapshot(&snapshot, Chunking::new(8, 8, 8, 8));
        let round_tripped = envelopes
            .iter()
            .map(|envelope| {
                decode_envelope(&encode_envelope(envelope).expect("encoding should succeed"))
                    .expect("decoding should succeed")
            })
            .collect::<Vec<_>>();

        assert_eq!(round_tripped, envelopes);
        assert!(matches!(
            &round_tripped[1].payload(),
            OwnedSessionPayload::Threads(threads) if threads[0].name() == Some("worker")
        ));
        assert!(matches!(
            &round_tripped[4].payload(),
            OwnedSessionPayload::Histograms(histograms)
                if histograms[0].name() == "frame_micros" && histograms[0].buckets()[4] == 1
        ));
        assert!(matches!(
            &round_tripped[5].payload(),
            OwnedSessionPayload::Events(events)
                if matches!(&events[0].observation(), OwnedObservation::Flow {
                    flow_id,
                    phase: FlowPhase::Step,
                    context_id
                } if *flow_id == FlowId::new(8) && *context_id == ContextId::new(0))
        ));
    }

    #[test]
    fn cursor_detects_trailing_bytes() {
        let envelope = OwnedSessionEnvelope::new(
            EnvelopeHeader::new(PROTOCOL_VERSION, 0),
            OwnedSessionPayload::Events(vec![OwnedTimedObservation::new(
                2,
                0,
                1,
                OwnedObservation::FrameMarker {
                    kind: MarkerKind::Frame,
                    phase: MarkerPhase::Begin,
                    context_id: ContextId::new(0),
                },
            )]),
        );
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
        encode_header(&mut bytes, EnvelopeHeader::new(PROTOCOL_VERSION, 0));
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
        encode_header(&mut bytes, EnvelopeHeader::new(PROTOCOL_VERSION, 0));
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
        encode_header(&mut bytes, EnvelopeHeader::new(PROTOCOL_VERSION, 0));
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
        let envelope = OwnedSessionEnvelope::new(
            EnvelopeHeader::new(PROTOCOL_VERSION, 1),
            OwnedSessionPayload::Events(vec![OwnedTimedObservation::new(
                3,
                0,
                4,
                OwnedObservation::ScopeEnd {
                    token: ScopeToken::new(11),
                },
            )]),
        );
        let decoded =
            decode_envelope(&encode_envelope(&envelope).expect("encoding should succeed"))
                .expect("decoding should succeed");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn decoder_rejects_incompatible_codec_versions() {
        let envelope = OwnedSessionEnvelope::new(
            EnvelopeHeader::new(PROTOCOL_VERSION, 0),
            OwnedSessionPayload::SnapshotEnd(footfall_session::Drops::new()),
        );
        let encoded = encode_envelope(&envelope).expect("encoding should succeed");

        // The frame starts with little-endian u16 major and u16 minor.
        let mut different_major = encoded.clone();
        different_major[0..2].copy_from_slice(&(crate::CODEC_VERSION.major() + 1).to_le_bytes());
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
                since: None,
            }
            .session_id(),
            Some("wt_live")
        );
    }
}
