// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Answering every request for a set of runtimes.

use crate::client::SessionClient;
use crate::identity::{CertificateHash, SessionIdentity, local_identity_paths};
use crate::server::{
    ServedRequest, SessionAnnouncement, SessionRequest, SessionRequestKind, SessionServer,
    SessionServerConfig, read_request,
};
use footfall_runtime::Runtime;
use footfall_session::{Chunking, SessionErrorCode};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::task::{JoinHandle, JoinSet};

/// A runtime offered to consumers under a session id.
#[derive(Clone, Debug)]
pub struct ServedSession {
    id: String,
    runtime: &'static Runtime,
    chunking: Chunking,
}

impl ServedSession {
    /// Offers `runtime` under the session id `id`.
    ///
    /// Discovery announces it with the runtime's process name and session
    /// name, and snapshots are published in chunks of
    /// [`DEFAULT_CHUNKING`](Self::DEFAULT_CHUNKING).
    #[must_use]
    pub fn new(id: impl Into<String>, runtime: &'static Runtime) -> Self {
        Self {
            id: id.into(),
            runtime,
            chunking: Self::DEFAULT_CHUNKING,
        }
    }

    /// Rows per envelope by default: at most 1,024 contexts, counters, or
    /// histograms and 8,192 events.
    pub const DEFAULT_CHUNKING: Chunking = Chunking::new(1024, 1024, 1024, 8192);

    /// Sets how snapshots are split into envelopes.
    #[must_use]
    pub const fn with_chunking(mut self, chunking: Chunking) -> Self {
        self.chunking = chunking;
        self
    }

    /// Returns the session id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the served runtime.
    #[must_use]
    pub const fn runtime(&self) -> &'static Runtime {
        self.runtime
    }

    /// Returns the announcement discovery sends for this session.
    #[must_use]
    pub fn announcement(&self) -> SessionAnnouncement {
        SessionAnnouncement::new(
            self.id.clone(),
            self.runtime.process_name(),
            self.runtime.session_name(),
        )
    }
}

impl SessionServer {
    /// Answers every request for `sessions` until the returned future is
    /// dropped, handling each request on its own task.
    ///
    /// Discovery lists the sessions; snapshot requests read the named
    /// session's runtime from the request's cursor; subscriptions stream
    /// snapshots until the client goes away; anything else gets an error
    /// response with the matching [`SessionErrorCode`]. Failures of one
    /// request, such as a client that disconnects, do not affect others.
    ///
    /// Call it from within a Tokio runtime.
    pub async fn serve(self, sessions: Vec<ServedSession>) {
        let sessions: Arc<[ServedSession]> = sessions.into();
        let path: Arc<str> = self.path.into();
        // Request tasks live in this set, so dropping or aborting `serve`
        // also ends every request in flight, subscriptions included.
        let mut requests = JoinSet::new();
        loop {
            tokio::select! {
                incoming = self.endpoint.accept() => {
                    let sessions = Arc::clone(&sessions);
                    let path = Arc::clone(&path);
                    requests.spawn(async move {
                        if let Ok(request) = read_request(incoming, &path).await {
                            // An error here means the client went away
                            // mid-answer, which only concerns that client.
                            let _ = answer(request, &sessions).await;
                        }
                    });
                }
                Some(_) = requests.join_next(), if !requests.is_empty() => {}
            }
        }
    }
}

async fn answer(request: SessionRequest, sessions: &[ServedSession]) -> io::Result<ServedRequest> {
    let find = |id: &str| sessions.iter().find(|session| session.id == id);
    match request.kind().clone() {
        SessionRequestKind::Discover => {
            let announcements: Vec<_> = sessions.iter().map(ServedSession::announcement).collect();
            request.respond_discovery(&announcements).await
        }
        SessionRequestKind::Snapshot { session_id, since } => match find(&session_id) {
            Some(session) => {
                let snapshot = match since {
                    Some(since) => session.runtime.snapshot_since(since),
                    None => session.runtime.snapshot(),
                };
                request.respond_snapshot(snapshot, session.chunking).await
            }
            None => {
                request
                    .respond_error(SessionErrorCode::NotFound, &unknown(&session_id, sessions))
                    .await
            }
        },
        SessionRequestKind::Subscribe { session_id, .. } => match find(&session_id) {
            Some(session) => {
                request
                    .respond_subscription(session.runtime, session.chunking)
                    .await
            }
            None => {
                request
                    .respond_error(SessionErrorCode::NotFound, &unknown(&session_id, sessions))
                    .await
            }
        },
    }
}

fn unknown(session_id: &str, sessions: &[ServedSession]) -> String {
    let offered: Vec<_> = sessions.iter().map(|session| session.id.as_str()).collect();
    format!("unknown session `{session_id}`; this server offers {offered:?}")
}

/// Serves runtimes to consumers on this machine until dropped.
///
/// This is the one-call way to make a process attachable:
///
/// ```no_run
/// # async fn start() -> std::io::Result<()> {
/// use footfall_runtime::Runtime;
/// use footfall_session_wtransport::{LocalSessionServer, ServedSession};
///
/// let runtime = Runtime::builder().process_name("my_app").install_global().unwrap();
/// let server = LocalSessionServer::start(vec![ServedSession::new("main", runtime)]).await?;
/// println!("serving at {}", server.url());
/// # Ok(())
/// # }
/// ```
///
/// It loads or creates the local identity, binds the default local port, or
/// an ephemeral one when another producer already holds it, and registers
/// its URL in the local registry, so [`SessionClient::local_servers`] finds
/// every producer on the machine. Dropping it stops serving and removes the
/// registration.
#[derive(Debug)]
pub struct LocalSessionServer {
    url: String,
    certificate_hash: CertificateHash,
    registration: PathBuf,
    task: JoinHandle<()>,
}

impl LocalSessionServer {
    /// Starts serving `sessions`. Call it from within a Tokio runtime.
    pub async fn start(sessions: Vec<ServedSession>) -> io::Result<Self> {
        let identity = SessionIdentity::load_or_create_localhost().await?;
        let server = match SessionServer::bind(&identity, SessionServerConfig::local_default()) {
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                SessionServer::bind(&identity, SessionServerConfig::default())?
            }
            bound => bound?,
        };
        let url = server.url()?;
        let port = server.local_addr()?.port();

        let directory = local_registry_directory()?;
        fs::create_dir_all(&directory).await?;
        let registration = directory.join(port.to_string());
        fs::write(&registration, &url).await?;

        let task = tokio::spawn(server.serve(sessions));
        Ok(Self {
            url,
            certificate_hash: identity.certificate_hash(),
            registration,
            task,
        })
    }

    /// Returns the URL consumers connect to.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the hash consumers pin.
    #[must_use]
    pub const fn certificate_hash(&self) -> CertificateHash {
        self.certificate_hash
    }

    /// Returns a client for this server.
    #[must_use]
    pub fn client(&self) -> SessionClient {
        SessionClient::new(self.url.clone(), self.certificate_hash)
    }
}

impl Drop for LocalSessionServer {
    fn drop(&mut self) {
        self.task.abort();
        // Best effort: a registration left behind by a crash is skipped by
        // consumers that cannot connect to it.
        let _ = std::fs::remove_file(&self.registration);
    }
}

/// Directory where local servers register their URLs, one file per port.
pub(crate) fn local_registry_directory() -> io::Result<PathBuf> {
    Ok(local_identity_paths()?.directory.join("servers"))
}
