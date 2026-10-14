// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Footfall snapshots for consumers outside the instrumented process.
//!
//! A runtime snapshot belongs to the process that took it. This crate
//! defines the form it takes everywhere else: [`OwnedSnapshot`], which a
//! consumer gets the same way whether the snapshot was taken in process,
//! received from a live session, or read from a capture file, and which
//! `footfall_analysis` and `footfall_export` work on.
//!
//! - [`OwnedSnapshot`] is what consumers read. It converts from a runtime
//!   `Snapshot`, joins consecutive windows with
//!   [`append`](OwnedSnapshot::append), and folds in changes with
//!   [`apply`](OwnedSnapshot::apply).
//! - [`capture`] saves snapshots to a file and reads them back.
//! - Snapshots travel as versioned, chunked envelopes:
//!   [`publish_snapshot`] and [`publish_changes`] hand them to a
//!   [`Consumer`], [`codec`] encodes and decodes them, and
//!   [`OwnedSnapshot::from_envelopes`] reassembles them. Transports such as
//!   `footfall_session_wtransport` build on these; consumers rarely need
//!   them.
//!
//! ```
//! use footfall_core::{Backend as _, ContextHandle, ScopeDesc};
//! use footfall_runtime::Runtime;
//! use footfall_session::{OwnedSnapshot, capture::{CaptureReader, CaptureWriter}};
//!
//! static RENDER: ScopeDesc = ScopeDesc::new("render");
//! let runtime = Runtime::default();
//! let token = runtime.scope_begin(&RENDER, ContextHandle::ROOT, &Default::default());
//! runtime.scope_end(token);
//!
//! // In process: convert the runtime snapshot.
//! let live = OwnedSnapshot::from(runtime.snapshot());
//!
//! // Offline: the same snapshot after a round trip through a capture file.
//! let mut writer = CaptureWriter::new(Vec::new())?;
//! writer.write_owned(&live)?;
//! let bytes = writer.finish()?;
//! let saved = CaptureReader::new(bytes.as_slice())?.read_snapshot()?;
//! assert_eq!(saved.as_ref(), Some(&live));
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! The wire format and the contract consumers can rely on are documented in
//! the [session wire format] and [session consumer contract] guides.
//!
//! Without the `std` feature, on `alloc` alone, this crate still has
//! [`OwnedSnapshot`], publishing envelopes to a [`Consumer`], and
//! reassembling them; [`codec`] and [`capture`] need `std`.
//!
//! [session wire format]: https://github.com/forest-rs/footfall/blob/main/docs/session-wire-format.md
//! [session consumer contract]: https://github.com/forest-rs/footfall/blob/main/docs/session-consumer-contract.md

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod assembled;
#[cfg(feature = "std")]
pub mod capture;
mod clocks;
#[cfg(feature = "std")]
pub mod codec;
mod error;
mod snapshot;

pub use assembled::{AppendError, AssembleError, OwnedSnapshot};
// Types that snapshots carry as they are, re-exported so consumers can name
// and match them without depending on `footfall_core` or `footfall_runtime`.
pub use clocks::{ClockInfo, Clocks, Converted};
pub use error::SessionErrorCode;
pub use footfall_core::{
    ContextId, CounterDescId, EventDescId, FlowId, FlowPhase, InstrumentationLevel, MarkerKind,
    MarkerPhase, ResourceId, ResourceKindId, ScopeDescId, ScopeToken, Unit,
};
pub use footfall_runtime::{
    CaptureSettings, CounterKind, Drops, EventCursor, EventWindow, SnapshotMetadata, ThreadSnapshot,
};
pub use snapshot::{
    Chunking, Consumer, Envelope, EnvelopeHeader, OwnedArgEntry, OwnedClockTicks,
    OwnedContextEntry, OwnedContextSnapshot, OwnedCounterSnapshot, OwnedHistogramSnapshot,
    OwnedLabelValue, OwnedObservation, OwnedSessionEnvelope, OwnedSessionPayload,
    OwnedSnapshotSummary, OwnedSourceLocation, OwnedTimedObservation, OwnedTimelineWork, Payload,
    RowCounts, SnapshotKind, SnapshotSummary, collect_snapshot, publish_changes, publish_snapshot,
};

/// Current protocol version for Footfall session messages.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0);

/// Semantic protocol version for a stream of session messages.
///
/// [`PROTOCOL_VERSION`] is the version this crate speaks; each envelope
/// header carries the version it was produced at
/// (`EnvelopeHeader::protocol_version`), and [`ProtocolVersion::can_decode`]
/// says whether one is readable. Transports report a peer's version, for
/// example in discovery.
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

/// Data produced at a protocol version this crate cannot decode.
///
/// Capture readers and the codec return it inside the [`std::io::Error`]
/// they fail with, of kind `InvalidData`, so a tool can tell a capture from
/// an incompatible Footfall apart from a damaged one, and say which version
/// would read it:
///
/// ```
/// use footfall_session::IncompatibleVersion;
/// use footfall_session::capture::CaptureReader;
///
/// // A capture header from a protocol 2.0 producer.
/// let bytes = b"FFALLCAP\x02\x00\x00\x00";
/// let Err(error) = CaptureReader::new(&bytes[..]) else {
///     panic!("this reader speaks 1.0");
/// };
/// let incompatible = IncompatibleVersion::of(&error).expect("not damaged");
/// assert_eq!(incompatible.produced().major(), 2);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncompatibleVersion {
    produced: ProtocolVersion,
    supported: ProtocolVersion,
}

impl IncompatibleVersion {
    /// Returns the version the data was produced at.
    #[must_use]
    pub const fn produced(&self) -> ProtocolVersion {
        self.produced
    }

    /// Returns the version this crate speaks, [`PROTOCOL_VERSION`].
    #[must_use]
    pub const fn supported(&self) -> ProtocolVersion {
        self.supported
    }

    /// Returns the incompatibility `error` reports, if that is why it
    /// failed.
    #[cfg(feature = "std")]
    #[must_use]
    pub fn of(error: &std::io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }

    /// The error for data produced at `produced`.
    #[cfg(feature = "std")]
    pub(crate) fn error(produced: ProtocolVersion) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            Self {
                produced,
                supported: PROTOCOL_VERSION,
            },
        )
    }
}

impl core::fmt::Display for IncompatibleVersion {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "produced at session protocol {}, which this reader ({}) cannot decode; \
             a Footfall that speaks {} can",
            self.produced, self.supported, self.produced
        )
    }
}

impl core::error::Error for IncompatibleVersion {}

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
