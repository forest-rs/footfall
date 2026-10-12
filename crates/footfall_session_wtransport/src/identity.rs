// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Self-signed localhost identity and certificate pinning.

use super::*;

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

    pub(crate) fn from_digest(digest: &Sha256Digest) -> Self {
        Self(*digest.as_ref())
    }

    pub(crate) fn to_digest(self) -> Sha256Digest {
        Sha256Digest::new(self.0)
    }
}

impl std::str::FromStr for CertificateHash {
    type Err = ParseCertificateHashError;

    /// Parses 64 hex digits, in either case, as printed by `Display`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        if text.len() != 64 || !text.is_ascii() {
            return Err(ParseCertificateHashError);
        }
        let mut bytes = [0_u8; 32];
        let (pairs, _) = text.as_bytes().as_chunks::<2>();
        for (byte, pair) in bytes.iter_mut().zip(pairs) {
            let pair = core::str::from_utf8(pair).map_err(|_| ParseCertificateHashError)?;
            *byte = u8::from_str_radix(pair, 16).map_err(|_| ParseCertificateHashError)?;
        }
        Ok(Self(bytes))
    }
}

/// The text was not a certificate hash: 64 hex digits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ParseCertificateHashError;

impl fmt::Display for ParseCertificateHashError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a certificate hash is 64 hex digits")
    }
}

impl std::error::Error for ParseCertificateHashError {}

impl fmt::Display for CertificateHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Self-signed localhost certificates are valid for 14 days (the WebTransport
/// certificate-hash rule). A persisted identity is regenerated once it has
/// less than this many whole days of validity left, so an attach session
/// started right after load does not race certificate expiry.
pub(crate) const IDENTITY_RENEWAL_MARGIN_DAYS: i64 = 2;

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

    pub(crate) fn clone_identity(&self) -> Identity {
        self.identity.clone_identity()
    }

    pub(crate) fn from_identity(identity: Identity) -> io::Result<Self> {
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
    pub(crate) fn needs_renewal(&self) -> bool {
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

    /// Returns where the local identity's certificate is stored, as PEM.
    ///
    /// It is `footfall/wtransport/localhost-cert.pem` under the user's state
    /// directory: `~/Library/Application Support` on macOS, `%APPDATA%` on
    /// Windows, and `$XDG_STATE_HOME`, or `~/.local/state`, elsewhere.
    /// Consumers that cannot use [`Self::load_localhost_certificate_hash`],
    /// such as a browser, can hash the DER certificate in this file with
    /// SHA-256 to get the pin.
    pub fn localhost_certificate_path() -> io::Result<PathBuf> {
        Ok(local_identity_paths()?.cert)
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

#[derive(Debug)]
pub(crate) struct IdentityPaths {
    directory: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

pub(crate) fn local_identity_paths() -> io::Result<IdentityPaths> {
    let directory = local_state_directory()?.join("wtransport");
    Ok(IdentityPaths {
        cert: directory.join("localhost-cert.pem"),
        key: directory.join("localhost-key.pem"),
        directory,
    })
}

pub(crate) fn local_state_directory() -> io::Result<PathBuf> {
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

pub(crate) fn home_directory() -> io::Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
}
