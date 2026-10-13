// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Frames the program numbers, and the streams their numbers belong to.

use core::fmt;

/// A sequence of frames the program numbers, such as one window's or one
/// surface's. Frame numbers are unique within a stream; two streams may
/// both have a frame 42.
///
/// Streams are the program's to assign. Give each window or surface its own
/// id, and a new generation when the frame driver or surface behind it is
/// recreated and its numbering starts again. A program with one stream of
/// frames can ignore streams: plain frame numbers belong to
/// [`FrameStream::DEFAULT`].
///
/// A stream is not a display: a window keeps its stream when it moves to
/// another display.
///
/// ```
/// use footfall_core::{FrameNumber, FrameStream};
///
/// let main_window = FrameStream::new(1);
/// let frame = main_window.frame(1234);
/// assert_eq!(frame.number(), 1234);
/// assert_ne!(frame, FrameNumber::from(1234), "the default stream's 1234");
///
/// // The window's surface was recreated and numbering began again.
/// let recreated = main_window.with_generation(1);
/// assert_ne!(recreated.frame(1), main_window.frame(1));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FrameStream {
    id: u32,
    generation: u32,
}

impl FrameStream {
    /// The stream that plain frame numbers belong to: id `0`, generation `0`.
    pub const DEFAULT: Self = Self::new(0);

    /// Creates the stream with the program's id `id`, generation `0`.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self { id, generation: 0 }
    }

    /// Returns the same stream in generation `generation`, for when its
    /// numbering starts again.
    #[must_use]
    pub const fn with_generation(self, generation: u32) -> Self {
        Self { generation, ..self }
    }

    /// Returns the program's id for the stream.
    #[must_use]
    pub const fn id(self) -> u32 {
        self.id
    }

    /// Returns the stream's generation.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }

    /// Returns the frame numbered `number` in this stream.
    #[must_use]
    pub const fn frame(self, number: u64) -> FrameNumber {
        FrameNumber::new(self, number)
    }
}

impl fmt::Display for FrameStream {
    /// Writes `2` for stream 2, and `2.1` for its generation 1.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.generation == 0 {
            write!(f, "{}", self.id)
        } else {
            write!(f, "{}.{}", self.id, self.generation)
        }
    }
}

/// A frame the program numbers: its number within its [`FrameStream`].
///
/// Markers, timeline work, and frame points that carry the same frame
/// number belong to the same frame. A plain `u64` converts to a frame of
/// the default stream.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FrameNumber {
    stream: FrameStream,
    number: u64,
}

impl FrameNumber {
    /// Creates the frame numbered `number` in `stream`.
    #[must_use]
    pub const fn new(stream: FrameStream, number: u64) -> Self {
        Self { stream, number }
    }

    /// Returns the stream the frame belongs to.
    #[must_use]
    pub const fn stream(self) -> FrameStream {
        self.stream
    }

    /// Returns the frame's number within its stream.
    #[must_use]
    pub const fn number(self) -> u64 {
        self.number
    }

    /// Returns the frame numbered one before this one in the same stream,
    /// or `None` for frame `0`.
    #[must_use]
    pub const fn previous(self) -> Option<Self> {
        match self.number.checked_sub(1) {
            Some(number) => Some(Self::new(self.stream, number)),
            None => None,
        }
    }
}

impl From<u64> for FrameNumber {
    /// Returns frame `number` of [`FrameStream::DEFAULT`].
    fn from(number: u64) -> Self {
        Self::new(FrameStream::DEFAULT, number)
    }
}

impl fmt::Display for FrameNumber {
    /// Writes `1234` for a frame of the default stream, and `2:1234` or
    /// `2.1:1234` for one of another stream.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.stream == FrameStream::DEFAULT {
            write!(f, "{}", self.number)
        } else {
            write!(f, "{}:{}", self.stream, self.number)
        }
    }
}
