// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Serving discovery and snapshot requests.

use super::*;

/// Discoverable metadata for one local Footfall WebTransport session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionAnnouncement {
    /// Stable session id unique within one session server.
    pub(crate) id: String,
    /// Process name associated with the published session.
    pub(crate) process_name: String,
    /// Optional capture-session label.
    pub(crate) session_name: Option<String>,
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
        /// [`next_cursor`](footfall_session::OwnedSnapshot::next_cursor), or `None` for everything
        /// retained. Pass it to `Runtime::snapshot_since`.
        since: Option<EventCursor>,
    },
    /// Subscribe to one named session: a snapshot now, and another every
    /// `interval` with the events recorded since the last.
    Subscribe {
        /// Session id advertised by discovery.
        session_id: String,
        /// Where the first snapshot's events start, as for `Snapshot`.
        since: Option<EventCursor>,
        /// How long to wait between snapshots.
        interval: Duration,
    },
}

impl SessionRequestKind {
    /// Returns the requested session id when this is a snapshot fetch.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Discover => None,
            Self::Snapshot { session_id, .. } | Self::Subscribe { session_id, .. } => {
                Some(session_id.as_str())
            }
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
    /// Subscription that ended when the client went away.
    Subscription {
        /// Number of snapshots written to the client.
        snapshot_count: usize,
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
    pub(crate) endpoint: Endpoint<wtransport::endpoint::endpoint_side::Server>,
    pub(crate) path: String,
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
    ///
    /// The request is read before this returns, so a slow client delays the
    /// next accept; [`serve`](Self::serve) handles each request on its own
    /// task instead.
    pub async fn accept_request(&self) -> io::Result<SessionRequest> {
        read_request(self.endpoint.accept().await, &self.path).await
    }
}

/// Completes one incoming session and reads its request.
pub(crate) async fn read_request(
    incoming_session: wtransport::endpoint::IncomingSession,
    expected_path: &str,
) -> io::Result<SessionRequest> {
    let session_request = incoming_session
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    let authority = session_request.authority().to_owned();
    let path = session_request.path().to_owned();
    if path != expected_path {
        // Answer 404 so the client learns the path is wrong rather than
        // seeing the connection drop.
        session_request.not_found().await;
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unexpected session path: {path}"),
        ));
    }

    let connection = session_request
        .accept()
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    let (mut send, mut recv) = connection
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
        let since = read_cursor(&mut recv).await?;
        SessionRequestKind::Snapshot { session_id, since }
    } else if &request == SUBSCRIBE_MAGIC {
        let session_id = read_stream_string(&mut recv).await?;
        let since = read_cursor(&mut recv).await?;
        let interval = Duration::from_millis(u64::from(read_u32(&mut recv).await?));
        SessionRequestKind::Subscribe {
            session_id,
            since,
            interval,
        }
    } else {
        let message = format!(
            "unknown request {:?}; this server speaks session protocol {}.{}",
            String::from_utf8_lossy(&request),
            CODEC_VERSION.major(),
            CODEC_VERSION.minor()
        );
        write_error(&mut send, SessionErrorCode::Unsupported, &message).await?;
        return Err(io::Error::new(io::ErrorKind::InvalidData, message));
    };

    Ok(SessionRequest {
        authority,
        path,
        _connection: connection,
        send,
        kind,
    })
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
            return self
                .respond_error(
                    SessionErrorCode::InvalidRequest,
                    "expected a discovery request",
                )
                .await;
        }
        if u64::try_from(sessions.len()).unwrap_or(u64::MAX) > MAX_DISCOVERED_SESSIONS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many session announcements",
            ));
        }

        write_u8_stream(&mut self.send, RESPONSE_DISCOVERY).await?;
        write_u16(&mut self.send, CODEC_VERSION.major()).await?;
        write_u16(&mut self.send, CODEC_VERSION.minor()).await?;
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
            return self
                .respond_error(
                    SessionErrorCode::InvalidRequest,
                    "expected a snapshot request",
                )
                .await;
        }

        let envelope_count = write_snapshot(&mut self.send, &snapshot, chunking).await?;
        finish_response_stream(&mut self.send).await?;

        Ok(ServedRequest {
            authority: self.authority,
            path: self.path,
            kind: ServedRequestKind::Snapshot { envelope_count },
        })
    }

    /// Responds to a subscription with a snapshot of `runtime` now and
    /// another every `interval`, each holding the events recorded since the
    /// previous one, until the client goes away.
    ///
    /// The first snapshot starts at the request's cursor. A client that
    /// reads slowly delays the next snapshot rather than queueing them, and
    /// events evicted meanwhile are reported in the next snapshot's window.
    pub async fn respond_subscription(
        mut self,
        runtime: &Runtime,
        chunking: Chunking,
    ) -> io::Result<ServedRequest> {
        let SessionRequestKind::Subscribe {
            since, interval, ..
        } = self.kind
        else {
            return self
                .respond_error(
                    SessionErrorCode::InvalidRequest,
                    "expected a subscription request",
                )
                .await;
        };

        let interval = interval.max(MIN_SUBSCRIPTION_INTERVAL);
        let mut cursor = since;
        let mut snapshot_count = 0_usize;
        loop {
            let snapshot = match cursor {
                Some(cursor) => runtime.snapshot_since(cursor),
                None => runtime.snapshot(),
            };
            cursor = Some(snapshot.next_cursor());
            if write_snapshot(&mut self.send, &snapshot, chunking)
                .await
                .is_err()
            {
                break;
            }
            snapshot_count += 1;
            tokio::time::sleep(interval).await;
        }

        Ok(ServedRequest {
            authority: self.authority,
            path: self.path,
            kind: ServedRequestKind::Subscription { snapshot_count },
        })
    }

    /// Responds with an error, which the client receives as
    /// `ClientError::Server` with this code and message.
    ///
    /// Use [`SessionErrorCode::NotFound`] for an unknown session id and
    /// [`SessionErrorCode::Unsupported`] for a request kind the server does
    /// not handle.
    pub async fn respond_error(
        mut self,
        code: SessionErrorCode,
        message: &str,
    ) -> io::Result<ServedRequest> {
        write_error(&mut self.send, code, message).await?;
        Ok(ServedRequest {
            authority: self.authority,
            path: self.path,
            kind: ServedRequestKind::Error,
        })
    }
}

/// Writes one snapshot response: the tag, the envelope count, and the
/// envelope frames. Returns the envelope count.
async fn write_snapshot(
    send: &mut SendStream,
    snapshot: &Snapshot,
    chunking: Chunking,
) -> io::Result<usize> {
    let mut consumer = EncodingConsumer::default();
    publish_snapshot(snapshot, chunking, &mut consumer)?;
    write_u8_stream(send, RESPONSE_SNAPSHOT).await?;
    write_u64(
        send,
        u64::try_from(consumer.frames.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many envelopes"))?,
    )
    .await?;
    for encoded in &consumer.frames {
        write_u32(
            send,
            u32::try_from(encoded.len()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "envelope exceeds u32 length")
            })?,
        )
        .await?;
        send.write_all(encoded)
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
    }
    Ok(consumer.frames.len())
}

async fn write_error(
    send: &mut SendStream,
    code: SessionErrorCode,
    message: &str,
) -> io::Result<()> {
    write_u8_stream(send, RESPONSE_ERROR).await?;
    write_u8_stream(send, encode_error_code(code)).await?;
    write_stream_string(send, message).await?;
    finish_response_stream(send).await
}

#[derive(Default)]
pub(crate) struct EncodingConsumer {
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
        let encoded = encode_envelope(envelope)?;
        validate_wire_len(
            encoded.len(),
            MAX_SESSION_ENVELOPE_BYTES,
            "session envelope frame",
        )?;
        self.frames.push(encoded);
        Ok(())
    }
}
