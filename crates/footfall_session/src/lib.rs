// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transport-neutral session envelopes for Footfall.
//!
//! This crate does not participate in hot-path emission. Without the `std`
//! feature it only defines the protocol version and error codes; snapshot
//! publication, the codec, and capture files need `std`.

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "std")]
mod assembled;
#[cfg(feature = "std")]
pub mod capture;
#[cfg(feature = "std")]
pub mod codec;
mod error;
#[cfg(feature = "std")]
mod snapshot;

#[cfg(feature = "std")]
pub use assembled::{AppendError, AssembleError, OwnedSnapshot};
// Types that snapshots carry as they are, re-exported so consumers can name
// and match them without depending on `footfall_core` or `footfall_runtime`.
pub use error::SessionErrorCode;
pub use footfall_core::{
    ContextId, CounterDescId, EventDescId, FlowId, FlowPhase, InstrumentationLevel, MarkerKind,
    MarkerPhase, ResourceId, ResourceKindId, ScopeDescId, ScopeToken, Unit,
};
#[cfg(feature = "std")]
pub use footfall_runtime::{
    CaptureSettings, CounterKind, Drops, EventCursor, EventWindow, SnapshotMetadata, ThreadSnapshot,
};
#[cfg(feature = "std")]
pub use snapshot::{
    Chunking, Consumer, Envelope, EnvelopeHeader, OwnedArgEntry, OwnedContextEntry,
    OwnedContextSnapshot, OwnedCounterSnapshot, OwnedHistogramSnapshot, OwnedLabelValue,
    OwnedObservation, OwnedSessionEnvelope, OwnedSessionPayload, OwnedSnapshotSummary,
    OwnedTimedObservation, Payload, RowCounts, SnapshotKind, SnapshotSummary, collect_snapshot,
    publish_changes, publish_snapshot,
};

/// Current protocol version for Footfall session messages.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0);

/// Semantic protocol version for a stream of session messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolVersion {
    major: u16,
    minor: u16,
}

impl core::fmt::Display for ProtocolVersion {
    /// Writes the version as `major.minor`.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
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
