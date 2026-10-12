// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Discovering and reading sessions.

use super::*;
use footfall_session::{AssembleError, OwnedSnapshot, SessionErrorCode};
use std::{future::Future, time::Duration};
use wtransport::error::{ConnectingError, ConnectionError};

/// Returns the default localhost WebTransport server URL for Footfall sessions.
#[must_use]
pub fn default_local_url() -> String {
    format!("https://127.0.0.1:{DEFAULT_LOCAL_PORT}{DEFAULT_PATH}")
}

/// Reads sessions from one session server.
///
/// ```no_run
/// # async fn read() -> Result<(), footfall_session_wtransport::ClientError> {
/// use footfall_session_wtransport::SessionClient;
///
/// let client = SessionClient::local().await?;
/// let discovery = client.discover().await?;
/// let session = &discovery.sessions()[0];
///
/// let first = client.fetch(session.id(), None).await?;
/// // Later: only the events recorded since the first read.
/// let next = client.fetch(session.id(), Some(first.next_cursor())).await?;
/// # let _ = next;
/// # Ok(())
/// # }
/// ```
///
/// Every request opens its own connection and must finish within the
/// client's [`timeout`](Self::timeout), so a missing or stalled server is
/// reported as [`ClientError::TimedOut`] rather than waiting indefinitely.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionClient {
    url: String,
    certificate_hash: CertificateHash,
    timeout: Duration,
}

impl SessionClient {
    /// How long a request may take by default, from connecting to reading
    /// the last byte of the response.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

    /// Creates a client for the server at `url` whose certificate has
    /// `certificate_hash`.
    #[must_use]
    pub fn new(url: impl Into<String>, certificate_hash: CertificateHash) -> Self {
        Self {
            url: url.into(),
            certificate_hash,
            timeout: Self::DEFAULT_TIMEOUT,
        }
    }

    /// Creates a client for the default local server, pinned to the local
    /// identity's certificate; see [`SessionIdentity::localhost_certificate_path`].
    ///
    /// The certificate is read now, so create a new client after the local
    /// identity is renewed.
    pub async fn local() -> Result<Self, ClientError> {
        let certificate_hash = SessionIdentity::load_localhost_certificate_hash()
            .await
            .map_err(|source| ClientError::LocalIdentity {
                path: SessionIdentity::localhost_certificate_path().ok(),
                source,
            })?;
        Ok(Self::new(default_local_url(), certificate_hash))
    }

    /// Returns a client for every session server registered on this
    /// machine, default port first; see [`LocalSessionServer`].
    ///
    /// A registration can outlive a producer that crashed, so a client in
    /// the list may fail with [`ClientError::TimedOut`]; skip it.
    pub async fn local_servers() -> Result<Vec<Self>, ClientError> {
        let certificate_hash = SessionIdentity::load_localhost_certificate_hash()
            .await
            .map_err(|source| ClientError::LocalIdentity {
                path: SessionIdentity::localhost_certificate_path().ok(),
                source,
            })?;
        let directory = local_registry_directory().map_err(ClientError::Io)?;
        let mut entries = match fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(ClientError::Io(error)),
        };
        let mut servers = Vec::new();
        while let Some(entry) = entries.next_entry().await.map_err(ClientError::Io)? {
            let Some(port) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u16>().ok())
            else {
                continue;
            };
            let url = fs::read_to_string(entry.path())
                .await
                .map_err(ClientError::Io)?;
            servers.push((port != DEFAULT_LOCAL_PORT, port, url.trim().to_owned()));
        }
        servers.sort();
        Ok(servers
            .into_iter()
            .map(|(_, _, url)| Self::new(url, certificate_hash))
            .collect())
    }

    /// Sets how long each request may take.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Returns the server URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the pinned certificate hash.
    #[must_use]
    pub const fn certificate_hash(&self) -> CertificateHash {
        self.certificate_hash
    }

    /// Returns how long each request may take.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Lists the sessions the server offers.
    pub async fn discover(&self) -> Result<Discovery, ClientError> {
        self.request(|connection| async move {
            let (mut send, mut recv) = open_stream(&connection).await?;
            send.write_all(DISCOVER_MAGIC).await.map_err(stream_error)?;
            finish_request_stream(&mut send).await.map_err(io_error)?;

            match read_u8_stream(&mut recv).await.map_err(io_error)? {
                RESPONSE_DISCOVERY => {
                    let server = ProtocolVersion::new(
                        read_u16(&mut recv).await.map_err(io_error)?,
                        read_u16(&mut recv).await.map_err(io_error)?,
                    );
                    if !CODEC_VERSION.can_decode(server) {
                        return Err(ClientError::Incompatible {
                            server,
                            client: CODEC_VERSION,
                        });
                    }
                    let count = read_u64(&mut recv).await.map_err(io_error)?;
                    validate_wire_count(count, MAX_DISCOVERED_SESSIONS, "session announcements")
                        .map_err(io_error)?;
                    let mut sessions = Vec::new();
                    for _ in 0..count {
                        sessions.push(
                            decode_session_announcement_stream(&mut recv)
                                .await
                                .map_err(io_error)?,
                        );
                    }
                    Ok(Discovery {
                        protocol_version: server,
                        sessions,
                    })
                }
                RESPONSE_ERROR => Err(read_server_error(&mut recv).await),
                tag => Err(ClientError::Malformed {
                    reason: format!("unexpected discovery response tag {tag}"),
                }),
            }
        })
        .await
    }

    /// Reads one snapshot of the session `session_id`.
    ///
    /// With `since`, the snapshot's events are those recorded after a
    /// previous read: pass that snapshot's
    /// [`next_cursor`](OwnedSnapshot::next_cursor). Without it, they are
    /// every event the producer retains. Contexts, counters, and histograms
    /// are always current.
    pub async fn fetch(
        &self,
        session_id: &str,
        since: Option<EventCursor>,
    ) -> Result<OwnedSnapshot, ClientError> {
        self.request(|connection| async move {
            let (mut send, mut recv) = open_stream(&connection).await?;
            let mut request = Vec::with_capacity(SNAPSHOT_MAGIC.len() + session_id.len() + 21);
            request.extend_from_slice(SNAPSHOT_MAGIC);
            encode_string(&mut request, session_id).map_err(io_error)?;
            encode_cursor(&mut request, since);
            send.write_all(&request).await.map_err(stream_error)?;
            finish_request_stream(&mut send).await.map_err(io_error)?;

            match read_u8_stream(&mut recv).await.map_err(io_error)? {
                RESPONSE_SNAPSHOT => read_snapshot(&mut recv).await,
                RESPONSE_ERROR => Err(read_server_error(&mut recv).await),
                tag => Err(ClientError::Malformed {
                    reason: format!("unexpected snapshot response tag {tag}"),
                }),
            }
        })
        .await
    }

    /// Subscribes to the session `session_id`: the server sends a snapshot
    /// now and another every `interval`, each with the events recorded since
    /// the previous one.
    ///
    /// `since` is where the first snapshot's events start, as for
    /// [`fetch`](Self::fetch). Read snapshots with [`Subscription::next`];
    /// dropping the subscription ends it. Servers wait at least 10 ms between
    /// snapshots.
    pub async fn subscribe(
        &self,
        session_id: &str,
        since: Option<EventCursor>,
        interval: Duration,
    ) -> Result<Subscription, ClientError> {
        let interval_millis = u32::try_from(interval.as_millis()).unwrap_or(u32::MAX);
        let attempt = async {
            let config = ClientConfig::builder()
                .with_bind_default()
                .with_server_certificate_hashes([self.certificate_hash.to_digest()])
                .build();
            let endpoint = Endpoint::client(config).map_err(ClientError::Io)?;
            let connection = endpoint
                .connect(&self.url)
                .await
                .map_err(|error| connecting_error(&self.url, error))?;
            let (mut send, recv) = open_stream(&connection).await?;
            let mut request = Vec::with_capacity(SUBSCRIBE_MAGIC.len() + session_id.len() + 25);
            request.extend_from_slice(SUBSCRIBE_MAGIC);
            encode_string(&mut request, session_id).map_err(io_error)?;
            encode_cursor(&mut request, since);
            encode_u32(&mut request, interval_millis);
            send.write_all(&request).await.map_err(stream_error)?;
            finish_request_stream(&mut send).await.map_err(io_error)?;
            Ok(Subscription {
                url: self.url.clone(),
                patience: self.timeout.saturating_add(interval),
                _endpoint: endpoint,
                _connection: connection,
                recv,
            })
        };
        tokio::time::timeout(self.timeout, attempt)
            .await
            .map_err(|_| ClientError::TimedOut {
                url: self.url.clone(),
                after: self.timeout,
            })?
    }

    /// Connects and runs one request within the timeout.
    async fn request<T, F, Fut>(&self, run: F) -> Result<T, ClientError>
    where
        F: FnOnce(Connection) -> Fut,
        Fut: Future<Output = Result<T, ClientError>>,
    {
        let attempt = async {
            let config = ClientConfig::builder()
                .with_bind_default()
                .with_server_certificate_hashes([self.certificate_hash.to_digest()])
                .build();
            let endpoint = Endpoint::client(config).map_err(ClientError::Io)?;
            let connection = endpoint
                .connect(&self.url)
                .await
                .map_err(|error| connecting_error(&self.url, error))?;
            run(connection).await
        };
        tokio::time::timeout(self.timeout, attempt)
            .await
            .map_err(|_| ClientError::TimedOut {
                url: self.url.clone(),
                after: self.timeout,
            })?
    }
}

/// A stream of snapshots from [`SessionClient::subscribe`].
pub struct Subscription {
    url: String,
    patience: Duration,
    _endpoint: Endpoint<wtransport::endpoint::endpoint_side::Client>,
    _connection: Connection,
    recv: RecvStream,
}

impl fmt::Debug for Subscription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Subscription")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl Subscription {
    /// Waits for the next snapshot.
    ///
    /// Each snapshot holds the events recorded since the previous one, with
    /// current contexts, counters, and histograms; its window reports events
    /// the producer evicted before sending them. Fails with
    /// [`ClientError::TimedOut`] when no snapshot arrives within the
    /// interval plus the client's timeout, and with [`ClientError::Server`]
    /// when the server ends the subscription with an error, such as an
    /// unknown session id.
    pub async fn next(&mut self) -> Result<OwnedSnapshot, ClientError> {
        let read = async {
            match read_u8_stream(&mut self.recv).await.map_err(io_error)? {
                RESPONSE_SNAPSHOT => read_snapshot(&mut self.recv).await,
                RESPONSE_ERROR => Err(read_server_error(&mut self.recv).await),
                tag => Err(ClientError::Malformed {
                    reason: format!("unexpected subscription response tag {tag}"),
                }),
            }
        };
        tokio::time::timeout(self.patience, read)
            .await
            .map_err(|_| ClientError::TimedOut {
                url: self.url.clone(),
                after: self.patience,
            })?
    }
}

/// The sessions a server offers, from [`SessionClient::discover`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Discovery {
    protocol_version: ProtocolVersion,
    sessions: Vec<SessionAnnouncement>,
}

impl Discovery {
    /// Returns the session protocol version the server speaks.
    #[must_use]
    pub const fn protocol_version(&self) -> ProtocolVersion {
        self.protocol_version
    }

    /// Returns the sessions the server offers.
    #[must_use]
    pub fn sessions(&self) -> &[SessionAnnouncement] {
        &self.sessions
    }

    /// Returns the session with the given id, if the server offers it.
    #[must_use]
    pub fn session(&self, id: &str) -> Option<&SessionAnnouncement> {
        self.sessions.iter().find(|session| session.id() == id)
    }
}

/// Why a [`SessionClient`] request failed.
///
/// Each variant names the failure a caller can act on; the `Display` text
/// says what to check.
#[derive(Debug)]
#[non_exhaustive]
pub enum ClientError {
    /// The URL is not a valid WebTransport URL, or its host does not resolve.
    InvalidUrl {
        /// The URL.
        url: String,
        /// Why it is invalid.
        reason: String,
    },
    /// The connection failed before a session was established.
    Unreachable {
        /// The URL.
        url: String,
        /// The transport's description of the failure.
        reason: String,
    },
    /// No complete response arrived within the client's timeout, usually
    /// because no server is listening at the URL.
    TimedOut {
        /// The URL.
        url: String,
        /// The timeout that elapsed.
        after: Duration,
    },
    /// The server's certificate does not match the pinned hash.
    CertificateRejected {
        /// The URL.
        url: String,
        /// The transport's description of the rejection.
        reason: String,
    },
    /// The server refused the WebTransport session, usually because the
    /// URL's path is not the server's session path.
    SessionRejected {
        /// The URL.
        url: String,
    },
    /// The server answered the request with an error.
    Server {
        /// What kind of error.
        code: SessionErrorCode,
        /// The server's description.
        message: String,
    },
    /// The server speaks a session protocol version this client cannot
    /// decode.
    Incompatible {
        /// The server's protocol version.
        server: ProtocolVersion,
        /// The newest version this client decodes.
        client: ProtocolVersion,
    },
    /// The response broke the wire format or the publication contract.
    Malformed {
        /// What was wrong.
        reason: String,
    },
    /// The local identity's certificate could not be read, usually because
    /// no local producer has created it yet.
    LocalIdentity {
        /// Where the certificate was expected, when that could be worked out.
        path: Option<PathBuf>,
        /// The underlying error.
        source: io::Error,
    },
    /// Reading or writing the connection failed.
    Io(io::Error),
}

impl ClientError {
    /// Returns the server's error code, for [`ClientError::Server`].
    #[must_use]
    pub const fn server_code(&self) -> Option<SessionErrorCode> {
        match self {
            Self::Server { code, .. } => Some(*code),
            _ => None,
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl { url, reason } => write!(formatter, "invalid URL {url}: {reason}"),
            Self::Unreachable { url, reason } => {
                write!(formatter, "could not connect to {url}: {reason}")
            }
            Self::TimedOut { url, after } => write!(
                formatter,
                "no complete response from {url} within {after:?}; check that a producer is serving sessions there"
            ),
            Self::CertificateRejected { url, reason } => write!(
                formatter,
                "the certificate of {url} does not match the pinned hash ({reason}); re-read the server's certificate hash"
            ),
            Self::SessionRejected { url } => write!(
                formatter,
                "{url} refused the session; check that the URL path is the server's session path"
            ),
            Self::Server { code, message } => write!(formatter, "server error ({code}): {message}"),
            Self::Incompatible { server, client } => write!(
                formatter,
                "the server speaks session protocol {}.{}, which this client ({}.{}) cannot decode",
                server.major(),
                server.minor(),
                client.major(),
                client.minor()
            ),
            Self::Malformed { reason } => write!(formatter, "malformed response: {reason}"),
            Self::LocalIdentity { path, source } => match path {
                Some(path) => write!(
                    formatter,
                    "cannot read the local session certificate at {}: {source}; start a local producer first",
                    path.display()
                ),
                None => write!(
                    formatter,
                    "cannot locate the local session certificate: {source}"
                ),
            },
            Self::Io(source) => write!(formatter, "session connection failed: {source}"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LocalIdentity { source, .. } | Self::Io(source) => Some(source),
            _ => None,
        }
    }
}

impl From<AssembleError> for ClientError {
    fn from(error: AssembleError) -> Self {
        Self::Malformed {
            reason: error.to_string(),
        }
    }
}

async fn open_stream(connection: &Connection) -> Result<(SendStream, RecvStream), ClientError> {
    connection
        .open_bi()
        .await
        .map_err(|error| ClientError::Io(io::Error::other(error.to_string())))?
        .await
        .map_err(|error| ClientError::Io(io::Error::other(error.to_string())))
}

async fn read_snapshot(recv: &mut RecvStream) -> Result<OwnedSnapshot, ClientError> {
    let count = read_u64(recv).await.map_err(io_error)?;
    validate_wire_count(count, MAX_SESSION_ENVELOPES, "session envelopes").map_err(io_error)?;
    let mut envelopes = Vec::new();
    for _ in 0..count {
        let len = usize::try_from(read_u32(recv).await.map_err(io_error)?).map_err(|_| {
            ClientError::Malformed {
                reason: String::from("envelope length does not fit in memory"),
            }
        })?;
        validate_wire_len(len, MAX_SESSION_ENVELOPE_BYTES, "session envelope frame")
            .map_err(io_error)?;
        let mut frame = vec![0_u8; len];
        recv.read_exact(&mut frame)
            .await
            .map_err(|error| ClientError::Io(io::Error::other(error.to_string())))?;
        if let Some(server) = frame_version(&frame)
            && !CODEC_VERSION.can_decode(server)
        {
            return Err(ClientError::Incompatible {
                server,
                client: CODEC_VERSION,
            });
        }
        envelopes.push(decode_envelope(&frame).map_err(io_error)?);
    }
    Ok(OwnedSnapshot::from_envelopes(envelopes)?)
}

/// Reads the protocol version at the start of an encoded envelope.
fn frame_version(frame: &[u8]) -> Option<ProtocolVersion> {
    let major = u16::from_le_bytes(frame.get(0..2)?.try_into().ok()?);
    let minor = u16::from_le_bytes(frame.get(2..4)?.try_into().ok()?);
    Some(ProtocolVersion::new(major, minor))
}

async fn read_server_error(recv: &mut RecvStream) -> ClientError {
    let code = match read_u8_stream(recv).await {
        Ok(code) => decode_error_code(code),
        Err(error) => return io_error(error),
    };
    match read_stream_string(recv).await {
        Ok(message) => ClientError::Server { code, message },
        Err(error) => io_error(error),
    }
}

fn connecting_error(url: &str, error: ConnectingError) -> ClientError {
    let url = String::from(url);
    match error {
        ConnectingError::InvalidUrl(reason) => ClientError::InvalidUrl { url, reason },
        ConnectingError::DnsLookup(source) => ClientError::InvalidUrl {
            url,
            reason: source.to_string(),
        },
        ConnectingError::DnsNotFound => ClientError::InvalidUrl {
            url,
            reason: String::from("the host does not resolve"),
        },
        ConnectingError::SessionRejected => ClientError::SessionRejected { url },
        // The transport reports certificate failures only as text.
        ConnectingError::ConnectionError(ConnectionError::QuicProto(error))
            if error.to_string().contains("certificate") =>
        {
            ClientError::CertificateRejected {
                url,
                reason: error.to_string(),
            }
        }
        other => ClientError::Unreachable {
            url,
            reason: other.to_string(),
        },
    }
}

/// Classifies a framing error: bad data is a malformed response, anything
/// else a connection failure.
fn io_error(error: io::Error) -> ClientError {
    if error.kind() == io::ErrorKind::InvalidData {
        ClientError::Malformed {
            reason: error.to_string(),
        }
    } else {
        ClientError::Io(error)
    }
}

fn stream_error(error: StreamWriteError) -> ClientError {
    ClientError::Io(io::Error::other(error.to_string()))
}
