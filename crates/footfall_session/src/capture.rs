// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Capture files: snapshots saved for later reading.
//!
//! A capture file is the header `FFALLCAP` followed by the `u16` protocol
//! major and minor version, little-endian, and then snapshot publications
//! back to back: each envelope as its byte length, a LEB128 integer,
//! followed by the envelope encoded as in the [wire-format document]. A file can hold any
//! number of snapshots, from one runtime or several, and a reader rebuilds
//! each one exactly as a live consumer would have received it.
//!
//! ```
//! use footfall_runtime::Runtime;
//! use footfall_session::capture::{CaptureReader, CaptureWriter};
//!
//! let runtime = Runtime::default();
//! let mut writer = CaptureWriter::new(Vec::new())?;
//! writer.write_snapshot(&runtime.snapshot())?;
//! let bytes = writer.finish()?;
//!
//! let snapshots: Vec<_> = CaptureReader::new(bytes.as_slice())?.collect::<Result<_, _>>()?;
//! assert_eq!(snapshots[0].metadata().instance_id(), runtime.instance_id());
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! [wire-format document]: https://github.com/forest-rs/footfall/blob/main/docs/session-wire-format.md

use alloc::{format, vec, vec::Vec};
use std::io::{self, BufReader, Read, Write};

use footfall_runtime::Snapshot;

use crate::{
    Chunking, Consumer, Envelope, OwnedSessionEnvelope, OwnedSessionPayload, OwnedSnapshot,
    PROTOCOL_VERSION, ProtocolVersion,
    codec::{
        MAX_SESSION_ENVELOPE_BYTES, VarintDecoder, decode_envelope, encode_envelope,
        encode_owned_envelope, encode_varint,
    },
    publish_snapshot,
};

/// The first bytes of every capture file.
pub const CAPTURE_MAGIC: &[u8; 8] = b"FFALLCAP";

/// Writes snapshots to a capture file.
#[derive(Debug)]
pub struct CaptureWriter<W: Write> {
    inner: W,
    chunking: Chunking,
}

impl<W: Write> CaptureWriter<W> {
    /// Starts a capture, writing the header to `inner`.
    pub fn new(inner: W) -> io::Result<Self> {
        let mut writer = Self {
            inner,
            chunking: Chunking::default(),
        };
        writer.inner.write_all(CAPTURE_MAGIC)?;
        writer
            .inner
            .write_all(&PROTOCOL_VERSION.major().to_le_bytes())?;
        writer
            .inner
            .write_all(&PROTOCOL_VERSION.minor().to_le_bytes())?;
        Ok(writer)
    }

    /// Appends a runtime snapshot.
    pub fn write_snapshot(&mut self, snapshot: &Snapshot) -> io::Result<()> {
        let mut frames = FrameWriter {
            inner: &mut self.inner,
        };
        publish_snapshot(snapshot, self.chunking, &mut frames)
    }

    /// Appends a snapshot received from a session or read from another
    /// capture.
    pub fn write_owned(&mut self, snapshot: &OwnedSnapshot) -> io::Result<()> {
        for envelope in snapshot.to_envelopes(self.chunking) {
            write_frame(&mut self.inner, &encode_owned_envelope(&envelope)?)?;
        }
        Ok(())
    }

    /// Flushes what has been written so far, so a reader of a capture still
    /// being written sees every complete snapshot.
    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    /// Flushes the capture and returns the underlying writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.inner.flush()?;
        Ok(self.inner)
    }
}

struct FrameWriter<'a, W: Write> {
    inner: &'a mut W,
}

impl<W: Write> Consumer for FrameWriter<'_, W> {
    type Error = io::Error;

    fn consume(&mut self, envelope: Envelope<'_>) -> io::Result<()> {
        write_frame(self.inner, &encode_envelope(envelope)?)
    }
}

fn write_frame(writer: &mut impl Write, frame: &[u8]) -> io::Result<()> {
    let mut len = Vec::with_capacity(10);
    encode_varint(&mut len, frame.len() as u64);
    writer.write_all(&len)?;
    writer.write_all(frame)
}

/// Reads snapshots from a capture file, one at a time.
///
/// As an iterator it yields each snapshot in file order, or the error that
/// stopped reading. Errors are [`io::ErrorKind::InvalidData`] for a file
/// that is not a capture, was written by an incompatible protocol version,
/// or holds a malformed publication, and
/// [`io::ErrorKind::UnexpectedEof`] for a file that ends partway through a
/// snapshot, as one being written might.
#[derive(Debug)]
pub struct CaptureReader<R: Read> {
    inner: BufReader<R>,
    protocol_version: ProtocolVersion,
    failed: bool,
    /// Snapshots read so far, and the byte offset reached, for errors.
    snapshots: u64,
    offset: u64,
}

impl<R: Read> CaptureReader<R> {
    /// Opens a capture, reading and checking its header. Reads are
    /// buffered, so `inner` can be a plain file.
    pub fn new(inner: R) -> io::Result<Self> {
        let mut inner = BufReader::new(inner);
        let mut magic = [0_u8; 8];
        inner.read_exact(&mut magic)?;
        if &magic != CAPTURE_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a Footfall capture file",
            ));
        }
        let mut version = [0_u8; 4];
        inner.read_exact(&mut version)?;
        let protocol_version = ProtocolVersion::new(
            u16::from_le_bytes([version[0], version[1]]),
            u16::from_le_bytes([version[2], version[3]]),
        );
        if !PROTOCOL_VERSION.can_decode(protocol_version) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "capture was written with session protocol {}.{}, which this reader ({}.{}) cannot decode",
                    protocol_version.major(),
                    protocol_version.minor(),
                    PROTOCOL_VERSION.major(),
                    PROTOCOL_VERSION.minor()
                ),
            ));
        }
        Ok(Self {
            inner,
            protocol_version,
            failed: false,
            snapshots: 0,
            offset: 12,
        })
    }

    /// Returns the protocol version the capture was written with.
    #[must_use]
    pub const fn protocol_version(&self) -> ProtocolVersion {
        self.protocol_version
    }

    /// Reads the next snapshot, or `None` at the end of the file.
    pub fn read_snapshot(&mut self) -> io::Result<Option<OwnedSnapshot>> {
        let mut envelopes = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            if self.inner.read(&mut byte)? == 0 {
                if envelopes.is_empty() {
                    return Ok(None);
                }
                return Err(self.truncated());
            }
            let mut decoder = VarintDecoder::new();
            let mut len_bytes = 1_u64;
            let len = loop {
                if let Some(len) = decoder.push(byte[0])? {
                    break usize::try_from(len).unwrap_or(usize::MAX);
                }
                self.read_exact(&mut byte)?;
                len_bytes += 1;
            };
            if len > MAX_SESSION_ENVELOPE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "snapshot {} at byte {}: an envelope of {len} bytes exceeds the limit",
                        self.snapshots, self.offset
                    ),
                ));
            }
            let mut frame = vec![0_u8; len];
            self.read_exact(&mut frame)?;
            self.offset += len_bytes + u64::try_from(len).unwrap_or(u64::MAX);
            let envelope = decode_envelope(&frame).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "snapshot {} at byte {}: {error}",
                        self.snapshots, self.offset
                    ),
                )
            })?;
            let ends = matches!(envelope.payload(), OwnedSessionPayload::SnapshotEnd(_));
            envelopes.push(envelope);
            if ends {
                let snapshot = assemble(envelopes)?;
                self.snapshots += 1;
                return Ok(Some(snapshot));
            }
        }
    }

    /// Reads exactly `buffer.len()` bytes, reporting a short read as a
    /// truncated capture.
    fn read_exact(&mut self, buffer: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact(buffer).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                self.truncated()
            } else {
                error
            }
        })
    }

    fn truncated(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "capture ends partway through snapshot {} after byte {}; the {} snapshots before it are complete",
                self.snapshots, self.offset, self.snapshots
            ),
        )
    }
}

fn assemble(envelopes: Vec<OwnedSessionEnvelope>) -> io::Result<OwnedSnapshot> {
    OwnedSnapshot::from_envelopes(envelopes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

impl<R: Read> Iterator for CaptureReader<R> {
    type Item = io::Result<OwnedSnapshot>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.read_snapshot() {
            Ok(snapshot) => snapshot.map(Ok),
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, vec::Vec};
    use std::io::ErrorKind;

    use footfall_core::{Backend as _, ContextHandle, MarkerKind, MarkerPhase};
    use footfall_runtime::Runtime;

    use super::{CaptureReader, CaptureWriter};
    use crate::OwnedSnapshot;

    fn mark(runtime: &Runtime) {
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
    }

    #[test]
    fn captures_round_trip_runtime_and_owned_snapshots() {
        let runtime = Runtime::default();
        mark(&runtime);
        let first = runtime.snapshot();
        mark(&runtime);
        let second = runtime.snapshot_since(first.next_cursor());

        let mut writer = CaptureWriter::new(Vec::new()).expect("header writes");
        writer.write_snapshot(&first).expect("snapshot writes");
        writer
            .write_owned(&OwnedSnapshot::from(&second))
            .expect("owned snapshot writes");
        let bytes = writer.finish().expect("capture finishes");

        let read: Vec<_> = CaptureReader::new(bytes.as_slice())
            .expect("header reads")
            .collect::<Result<_, _>>()
            .expect("snapshots read");
        assert_eq!(
            read,
            [OwnedSnapshot::from(&first), OwnedSnapshot::from(&second)]
        );
    }

    #[test]
    fn readers_reject_other_files_and_report_truncation() {
        let error = CaptureReader::new(&b"not a capture"[..]).expect_err("wrong magic");
        assert_eq!(error.kind(), ErrorKind::InvalidData);

        let runtime = Runtime::default();
        mark(&runtime);
        let mut writer = CaptureWriter::new(Vec::new()).expect("header writes");
        writer
            .write_snapshot(&runtime.snapshot())
            .expect("snapshot writes");
        let bytes = writer.finish().expect("capture finishes");

        let truncated = &bytes[..bytes.len() - 3];
        let mut reader = CaptureReader::new(truncated).expect("header reads");
        let error = reader
            .next()
            .expect("an item")
            .expect_err("the snapshot is cut short");
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
        assert!(
            error.to_string().contains("partway through snapshot 0"),
            "{error}"
        );
        assert!(reader.next().is_none(), "reading stops after an error");
    }
}
