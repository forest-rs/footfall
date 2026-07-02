// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transport-neutral session envelopes for Spoor.
//!
//! This crate does not participate in hot-path emission. Its live-session
//! control vocabulary is `no_std` with `alloc`; runtime snapshot publication is
//! available through the `std` feature.

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod live;
#[cfg(feature = "std")]
mod snapshot;

pub use live::{
    SessionError, SessionErrorCode, SessionMessage, SessionMethod, SessionRequest,
    SessionRequestId, SessionResponse, SessionResult, SessionStreamEnd, SessionStreamId,
    SessionStreamItem, SessionStreamStart, SessionStreamStatus,
};
#[cfg(feature = "std")]
pub use snapshot::{
    Chunking, Consumer, DroppedSummary, Envelope, EnvelopeHeader, OwnedArgEntry, OwnedContextEntry,
    OwnedContextSnapshot, OwnedCounterSnapshot, OwnedLabelValue, OwnedObservation,
    OwnedSessionEnvelope, OwnedSessionPayload, OwnedSnapshotMetadata, OwnedSnapshotSummary,
    OwnedTimedObservation, Payload, SnapshotSummary, collect_snapshot, publish_snapshot,
};

/// Current protocol version for Spoor session messages.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0);

/// Semantic protocol version for a stream of session messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolVersion {
    major: u16,
    minor: u16,
}

impl ProtocolVersion {
    /// Creates a new protocol version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Returns the major protocol version.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the minor protocol version.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Returns true when a decoder supporting `self` can decode data produced
    /// at version `produced`.
    ///
    /// Minor versions are additive: a decoder understands everything produced
    /// at the same major version with an equal or older minor version. A
    /// different major version or a newer minor version is not decodable.
    #[must_use]
    pub const fn can_decode(self, produced: Self) -> bool {
        self.major == produced.major && produced.minor <= self.minor
    }
}

#[cfg(test)]
mod tests {
    use super::ProtocolVersion;

    #[test]
    fn minor_versions_are_additive_for_decoders() {
        let supported = ProtocolVersion::new(1, 2);
        assert!(supported.can_decode(ProtocolVersion::new(1, 0)));
        assert!(supported.can_decode(ProtocolVersion::new(1, 2)));
        assert!(!supported.can_decode(ProtocolVersion::new(1, 3)));
        assert!(!supported.can_decode(ProtocolVersion::new(2, 0)));
        assert!(!supported.can_decode(ProtocolVersion::new(0, 2)));
    }
}
