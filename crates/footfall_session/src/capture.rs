// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Capture files: snapshots saved for later reading.
//!
//! A capture file is the header `FFALLCAP` followed by the `u16` protocol
//! major and minor version, little-endian, and then snapshot publications
//! back to back: each envelope as its byte length, a LEB128 integer,
//! followed by the envelope encoded as in the [wire-format document]. A
//! file can hold any number of snapshots, from one runtime or several, and a
//! reader rebuilds each one exactly as a live consumer would have received
//! it.
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
//! A program can record itself with [`CaptureRecorder`], which writes what
//! its runtime recorded since the last write each time it is asked, and a
//! tool reads a whole capture as one snapshot with
//! [`CaptureReader::join`]:
//!
//! ```
//! use footfall_runtime::Runtime;
//! use footfall_session::capture::{CaptureReader, CaptureRecorder};
//!
//! let runtime = Runtime::default();
//! let mut recorder = CaptureRecorder::new(&runtime, Vec::new())?;
//! // Each frame, or every few: write what was recorded since the last write.
//! recorder.write()?;
//! let finished = recorder.finish()?;
//! assert_eq!(finished.missed(), 0);
//! let bytes = finished.into_inner();
//!
//! let joined = CaptureReader::new(bytes.as_slice())?.join()?;
//! assert_eq!(joined.snapshot().metadata().instance_id(), runtime.instance_id());
//! assert!(joined.complete());
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! [wire-format document]: https://github.com/forest-rs/footfall/blob/main/docs/session-wire-format.md

use alloc::{format, vec, vec::Vec};
use std::io::{self, BufReader, Read, Write};

use footfall_runtime::{EventCursor, EventWindow, Runtime, Snapshot};

use crate::{
    Chunking, Consumer, Envelope, IncompatibleVersion, OwnedSessionEnvelope, OwnedSessionPayload,
    OwnedSnapshot, PROTOCOL_VERSION, ProtocolVersion,
    codec::{
        CodecError, MAX_SESSION_ENVELOPE_BYTES, VarintDecoder, decode_envelope, encode_envelope,
        encode_owned_envelope, encode_varint,
    },
    publish_snapshot,
};

/// The first bytes of every capture file.
pub const CAPTURE_MAGIC: &[u8; 8] = b"FFALLCAP";

/// How capture files chunk snapshots. Each events envelope writes a
/// descriptor's name, and a scope's source location, once, so envelopes of
/// up to 8192 events, as the WebTransport server sends, keep that cost
/// small per event.
const CAPTURE_CHUNKING: Chunking = Chunking::new(256, 256, 256, 8192);

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
            chunking: CAPTURE_CHUNKING,
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

/// Records a runtime to a capture as it runs.
///
/// Each [`write`](Self::write) appends a snapshot of what the runtime
/// recorded since the previous one, so the file holds every event once and
/// reads back the way a live consumer would have seen it. The first write
/// holds everything the runtime still retains. Write often enough that the
/// runtime's event buffer does not evict events between writes; the
/// [`EventWindow`] each write returns, and [`missed`](Self::missed), say if
/// it did.
#[derive(Debug)]
pub struct CaptureRecorder<'a, W: Write> {
    runtime: &'a Runtime,
    writer: CaptureWriter<W>,
    cursor: EventCursor,
    snapshots: u64,
    events: u64,
    missed: u64,
}

impl<'a, W: Write> CaptureRecorder<'a, W> {
    /// Starts a capture of `runtime`, writing the header to `inner`.
    pub fn new(runtime: &'a Runtime, inner: W) -> io::Result<Self> {
        Ok(Self {
            runtime,
            writer: CaptureWriter::new(inner)?,
            // A cursor of no instance reads from the start.
            cursor: EventCursor::default(),
            snapshots: 0,
            events: 0,
            missed: 0,
        })
    }

    /// Appends a snapshot of what was recorded since the last write, and
    /// flushes it, so a reader of the file sees it at once.
    ///
    /// Returns the snapshot's window: the event sequence numbers it covers,
    /// and how many in that range the runtime evicted before they could be
    /// written.
    pub fn write(&mut self) -> io::Result<EventWindow> {
        let snapshot = self.runtime.snapshot_since(self.cursor);
        self.writer.write_snapshot(&snapshot)?;
        self.writer.flush()?;
        self.cursor = snapshot.next_cursor();
        let window = snapshot.window();
        self.snapshots += 1;
        self.events += snapshot.events().len() as u64;
        self.missed += window.missed();
        Ok(window)
    }

    /// Returns how many snapshots have been written.
    #[must_use]
    pub const fn snapshots(&self) -> u64 {
        self.snapshots
    }

    /// Returns how many events have been written.
    #[must_use]
    pub const fn events(&self) -> u64 {
        self.events
    }

    /// Returns how many events the runtime evicted before they could be
    /// written. Write more often, or give the runtime a larger event
    /// capacity, to keep it zero.
    #[must_use]
    pub const fn missed(&self) -> u64 {
        self.missed
    }

    /// Writes what was recorded since the last write, flushes the capture,
    /// and returns the underlying writer with the capture's totals, this
    /// last write included.
    pub fn finish(mut self) -> io::Result<FinishedCapture<W>> {
        self.write()?;
        Ok(FinishedCapture {
            inner: self.writer.finish()?,
            snapshots: self.snapshots,
            events: self.events,
            missed: self.missed,
        })
    }
}

/// A capture a [`CaptureRecorder`] finished: the underlying writer, and
/// what was written to it.
#[derive(Debug)]
pub struct FinishedCapture<W> {
    inner: W,
    snapshots: u64,
    events: u64,
    missed: u64,
}

impl<W> FinishedCapture<W> {
    /// Returns how many snapshots were written.
    #[must_use]
    pub const fn snapshots(&self) -> u64 {
        self.snapshots
    }

    /// Returns how many events were written.
    #[must_use]
    pub const fn events(&self) -> u64 {
        self.events
    }

    /// Returns how many events the runtime evicted before they could be
    /// written.
    #[must_use]
    pub const fn missed(&self) -> u64 {
        self.missed
    }

    /// Returns the underlying writer.
    #[must_use]
    pub fn into_inner(self) -> W {
        self.inner
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
            return Err(IncompatibleVersion::error(protocol_version));
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
            let envelope = decode_envelope(&frame).map_err(|error| match error {
                // An incompatibility stays one, for the reader to tell apart.
                CodecError::Incompatible(_) => io::Error::from(error),
                error => io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "snapshot {} at byte {}: {error}",
                        self.snapshots, self.offset
                    ),
                ),
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

impl<R: Read> CaptureReader<R> {
    /// Reads the rest of the capture as one snapshot: each snapshot appended
    /// to the first, as [`OwnedSnapshot::append`] joins a subscription's
    /// windows.
    ///
    /// Snapshots from another runtime instance than the first, such as a
    /// producer that restarted while it was recorded, cannot be joined and
    /// are counted in [`JoinedCapture::skipped`]. A capture cut short, as one
    /// still being written can be, keeps its complete snapshots and says so
    /// in [`JoinedCapture::truncated`].
    ///
    /// Fails for a capture with no snapshots, and for any error other than a
    /// truncated end.
    pub fn join(self) -> io::Result<JoinedCapture> {
        let mut snapshot: Option<OwnedSnapshot> = None;
        let mut skipped = 0;
        let mut truncated = None;
        for next in self {
            let next = match next {
                Ok(next) => next,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    truncated = Some(error);
                    break;
                }
                Err(error) => return Err(error),
            };
            match &mut snapshot {
                None => snapshot = Some(next),
                Some(joined) => {
                    if joined.append(next).is_err() {
                        skipped += 1;
                    }
                }
            }
        }
        let Some(snapshot) = snapshot else {
            return Err(truncated.unwrap_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "the capture holds no snapshots")
            }));
        };
        Ok(JoinedCapture {
            snapshot,
            skipped,
            truncated,
        })
    }
}

/// A whole capture joined into one snapshot, as [`CaptureReader::join`]
/// returns it, with what could not be joined.
#[derive(Debug)]
pub struct JoinedCapture {
    snapshot: OwnedSnapshot,
    skipped: u64,
    truncated: Option<io::Error>,
}

impl JoinedCapture {
    /// Returns the joined snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &OwnedSnapshot {
        &self.snapshot
    }

    /// Returns the joined snapshot, leaving the rest.
    #[must_use]
    pub fn into_snapshot(self) -> OwnedSnapshot {
        self.snapshot
    }

    /// Returns how many snapshots could not be joined to the first: from
    /// another runtime instance, or ending before what came before them.
    #[must_use]
    pub const fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Returns the error that cut the capture short, when it ends partway
    /// through a snapshot. The snapshots before it are joined.
    #[must_use]
    pub const fn truncated(&self) -> Option<&io::Error> {
        self.truncated.as_ref()
    }

    /// Returns whether every snapshot in the capture was joined: none
    /// skipped, and the capture not cut short.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.skipped == 0 && self.truncated.is_none()
    }
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

    use super::{CaptureReader, CaptureRecorder, CaptureWriter};
    use crate::{IncompatibleVersion, OwnedSnapshot, ProtocolVersion};

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
    fn runtime_snapshots_write_frame_opportunities_and_scheduled_points() {
        use footfall_core::{
            BeginsAt, ClockDesc, FrameOpportunity, FramePoint, FramePointKind, FrameStream,
            TickPeriod,
        };
        static HOST: ClockDesc = ClockDesc::new("host");
        let runtime = Runtime::default();
        let stream = FrameStream::new(2).with_generation(1);
        runtime.frame_opportunity(
            &FrameOpportunity::at(stream, &HOST, TickPeriod::NANOSECOND, 1_000).with_interval(100),
        );
        runtime.frame_point(&FramePoint::at(
            stream.frame(4),
            FramePointKind::Scheduled(BeginsAt::Opportunity),
            &HOST,
            TickPeriod::NANOSECOND,
            1_050,
        ));
        runtime.frame_opportunity(&FrameOpportunity::at(
            stream,
            &HOST,
            TickPeriod::NANOSECOND,
            1_100,
        ));
        let snapshot = runtime.snapshot();

        let mut writer = CaptureWriter::new(Vec::new()).expect("header writes");
        writer.write_snapshot(&snapshot).expect("snapshot writes");
        let bytes = writer.finish().expect("capture finishes");
        let read: Vec<_> = CaptureReader::new(bytes.as_slice())
            .expect("header reads")
            .collect::<Result<_, _>>()
            .expect("snapshots read");
        assert_eq!(read, [OwnedSnapshot::from(&snapshot)]);
        assert_eq!(read[0].events().len(), 3);
    }

    #[test]
    fn runtime_snapshots_write_frame_plans() {
        use footfall_core::{Args, FramePlan, FrameStream, HoldReason, TickPeriod};
        let runtime = Runtime::default();
        let stream = FrameStream::new(2).with_generation(1);
        runtime.frame_plan(
            &FramePlan::new(stream.frame(4), TickPeriod::new(125, 3))
                .with_interval(200_000)
                .with_depth(2)
                .with_latency_allowance(200_000)
                .with_hold(200_000, HoldReason::DrainQueue)
                .with_args(Args::none().with("safety_margin_ns", 1_500_u64)),
        );
        runtime.frame_plan(&FramePlan::new(stream.frame(5), TickPeriod::NANOSECOND).synchronous());
        let snapshot = runtime.snapshot();

        let mut writer = CaptureWriter::new(Vec::new()).expect("header writes");
        writer.write_snapshot(&snapshot).expect("snapshot writes");
        let bytes = writer.finish().expect("capture finishes");
        let read: Vec<_> = CaptureReader::new(bytes.as_slice())
            .expect("header reads")
            .collect::<Result<_, _>>()
            .expect("snapshots read");
        assert_eq!(read, [OwnedSnapshot::from(&snapshot)]);
        assert_eq!(read[0].events().len(), 2);
    }

    #[test]
    fn recorders_write_each_event_once_and_say_what_was_evicted() {
        let runtime = Runtime::builder().event_capacity(4).build();
        mark(&runtime);
        mark(&runtime);
        let mut recorder = CaptureRecorder::new(&runtime, Vec::new()).expect("header writes");
        let first = recorder.write().expect("first write");
        assert_eq!((first.since(), first.next(), first.missed()), (0, 2, 0));
        mark(&runtime);
        let second = recorder.write().expect("second write");
        assert_eq!((second.since(), second.next(), second.missed()), (2, 3, 0));
        // Six events between writes, into a buffer of four: two are lost.
        for _ in 0..6 {
            mark(&runtime);
        }
        let third = recorder.write().expect("third write");
        assert_eq!((third.since(), third.next(), third.missed()), (3, 9, 2));
        assert_eq!(
            (recorder.snapshots(), recorder.events(), recorder.missed()),
            (3, 7, 2)
        );
        mark(&runtime);
        let finished = recorder.finish().expect("capture finishes");
        assert_eq!(
            (finished.snapshots(), finished.events(), finished.missed()),
            (4, 8, 2),
            "the totals count the last write, which finishing makes"
        );
        let bytes = finished.into_inner();

        let joined = CaptureReader::new(bytes.as_slice())
            .expect("header reads")
            .join()
            .expect("the capture joins");
        assert!(joined.complete());
        let snapshot = joined.snapshot();
        assert_eq!(snapshot.events().len(), 8, "each event once");
        assert_eq!(
            (snapshot.window().since(), snapshot.window().next()),
            (0, 10)
        );
        assert_eq!(
            snapshot.window().missed(),
            2,
            "the evicted events stay missed"
        );
    }

    #[test]
    fn joining_skips_other_instances_and_keeps_what_precedes_a_cut() {
        let runtime = Runtime::default();
        let restarted = Runtime::default();
        mark(&runtime);
        mark(&restarted);
        let mut writer = CaptureWriter::new(Vec::new()).expect("header writes");
        writer
            .write_snapshot(&runtime.snapshot())
            .expect("snapshot writes");
        writer
            .write_snapshot(&restarted.snapshot())
            .expect("snapshot writes");
        mark(&runtime);
        writer
            .write_snapshot(&runtime.snapshot())
            .expect("snapshot writes");
        let bytes = writer.finish().expect("capture finishes");

        let joined = CaptureReader::new(bytes.as_slice())
            .expect("header reads")
            .join()
            .expect("the capture joins");
        assert_eq!(joined.skipped(), 1, "the restarted runtime's snapshot");
        assert!(joined.truncated().is_none());
        assert!(!joined.complete());
        assert_eq!(joined.snapshot().events().len(), 2);
        assert_eq!(
            joined.snapshot().metadata().instance_id(),
            runtime.instance_id()
        );

        let cut = CaptureReader::new(&bytes[..bytes.len() - 3])
            .expect("header reads")
            .join()
            .expect("the complete snapshots join");
        assert_eq!(
            cut.truncated().map(std::io::Error::kind),
            Some(ErrorKind::UnexpectedEof)
        );
        assert_eq!(cut.snapshot().events().len(), 1, "the first snapshot");

        let empty = CaptureWriter::new(Vec::new())
            .expect("header writes")
            .finish()
            .expect("capture finishes");
        let error = CaptureReader::new(empty.as_slice())
            .expect("header reads")
            .join()
            .expect_err("nothing to join");
        assert_eq!(error.kind(), ErrorKind::InvalidData);
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

    /// A capture of one snapshot, with its header's protocol version, and
    /// its first envelope's, set to `header` and `envelope`.
    fn capture_at(header: (u16, u16), envelope: (u16, u16)) -> Vec<u8> {
        let runtime = Runtime::default();
        mark(&runtime);
        let mut writer = CaptureWriter::new(Vec::new()).expect("header writes");
        writer
            .write_snapshot(&runtime.snapshot())
            .expect("snapshot writes");
        let mut bytes = writer.finish().expect("capture finishes");
        bytes[8..10].copy_from_slice(&header.0.to_le_bytes());
        bytes[10..12].copy_from_slice(&header.1.to_le_bytes());
        // The first envelope follows its LEB128 length.
        let start = 12
            + bytes[12..]
                .iter()
                .position(|byte| byte & 0x80 == 0)
                .expect("a length")
            + 1;
        bytes[start..start + 2].copy_from_slice(&envelope.0.to_le_bytes());
        bytes[start + 2..start + 4].copy_from_slice(&envelope.1.to_le_bytes());
        bytes
    }

    #[test]
    fn captures_from_incompatible_versions_say_so_and_are_never_misread() {
        // Today's capture reads, and so does one an older minor wrote.
        for version in [(1, 2), (1, 1), (1, 0)] {
            let capture = capture_at(version, version);
            let snapshots: Vec<_> = CaptureReader::new(capture.as_slice())
                .expect("header reads")
                .collect::<Result<_, _>>()
                .expect("snapshots read");
            assert_eq!(snapshots.len(), 1);
        }

        // A newer minor or another major is refused at the header, saying
        // which version wrote it.
        for header in [(1, 3), (2, 0), (0, 9)] {
            let bytes = capture_at(header, (1, 0));
            let error = CaptureReader::new(bytes.as_slice()).expect_err("refused");
            assert_eq!(error.kind(), ErrorKind::InvalidData);
            let incompatible = IncompatibleVersion::of(&error).expect("an incompatibility");
            assert_eq!(
                incompatible.produced(),
                ProtocolVersion::new(header.0, header.1)
            );
            assert_eq!(incompatible.supported(), crate::PROTOCOL_VERSION);
            assert!(error.to_string().contains("cannot decode"), "{error}");
        }

        // An unsupported envelope in a supported file is refused too.
        let bytes = capture_at((1, 2), (1, 3));
        let error = CaptureReader::new(bytes.as_slice())
            .expect("the header is current")
            .next()
            .expect("a snapshot")
            .expect_err("its envelope is refused");
        assert_eq!(
            IncompatibleVersion::of(&error).map(IncompatibleVersion::produced),
            Some(ProtocolVersion::new(1, 3))
        );

        // A damaged capture is not an incompatible one.
        let error =
            CaptureReader::new(&b"FFALLCAX\x01\x00\x00\x00"[..]).expect_err("not a capture");
        assert!(IncompatibleVersion::of(&error).is_none());
    }
}
