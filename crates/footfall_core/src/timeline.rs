// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Work on timelines that are not CPU threads, and what frame schedulers
//! and displays report about frames.

use crate::{
    Args, ClockDesc, ClockReading, ContextHandle, FlowId, FrameNumber, ScopeDesc, TickPeriod,
    TimelineDesc,
};

/// One finished piece of work on a timeline that is not a CPU thread, such
/// as a render pass on a GPU queue, timed by that timeline's clock.
///
/// Work like this is known only after it happened: a GPU's timestamps are
/// read back frames later. Record each span once its times are known with
/// [`timeline_span`](crate::timeline_span). Relate its clock to the
/// runtime's with [`clock_sync`](crate::clock_sync) or
/// [`clock_pair`](crate::clock_pair) so analysis can place it.
///
/// ```
/// use footfall_core::{
///     ClockDesc, ContextHandle, FlowId, ScopeDesc, TickPeriod, TimelineDesc, TimelineSpan,
/// };
///
/// static GRAPHICS: TimelineDesc = TimelineDesc::gpu("graphics");
/// static GPU_CLOCK: ClockDesc = ClockDesc::new("gpu");
/// static SHADOWS: ScopeDesc = ScopeDesc::new("shadows");
///
/// // Read back from the GPU's timestamp queries for frame 1234.
/// let (begin, end) = (8_000_000_u64, 8_400_000_u64);
/// footfall_core::timeline_span(
///     &TimelineSpan::new(
///         &GRAPHICS, &SHADOWS, ContextHandle::ROOT,
///         &GPU_CLOCK, TickPeriod::NANOSECOND, begin, end,
///     )
///     .in_frame(1234)
///     .after_flow(FlowId::new(1234)),
/// );
/// ```
#[derive(Clone, Copy, Debug)]
pub struct TimelineSpan {
    timeline: &'static TimelineDesc,
    scope: &'static ScopeDesc,
    context: ContextHandle,
    clock: &'static ClockDesc,
    period: TickPeriod,
    begin: u64,
    end: u64,
    frame: Option<FrameNumber>,
    flow: Option<FlowId>,
    args: Args,
}

impl TimelineSpan {
    /// Describes work of the scope `scope` on `timeline`, attributed to
    /// `context`, that ran from `begin` to `end` ticks of `clock`, each tick
    /// lasting `period`.
    #[must_use]
    pub const fn new(
        timeline: &'static TimelineDesc,
        scope: &'static ScopeDesc,
        context: ContextHandle,
        clock: &'static ClockDesc,
        period: TickPeriod,
        begin: u64,
        end: u64,
    ) -> Self {
        Self {
            timeline,
            scope,
            context,
            clock,
            period,
            begin,
            end,
            frame: None,
            flow: None,
            args: Args::none(),
        }
    }

    /// Says which frame the work belongs to, by the program's number for it:
    /// a plain `u64`, or a [`FrameNumber`] in one of several frame streams;
    /// see [`frame_begin_numbered`](crate::frame_begin_numbered).
    #[must_use]
    pub fn in_frame(mut self, number: impl Into<FrameNumber>) -> Self {
        self.frame = Some(number.into());
        self
    }

    /// Says the work ends the causal flow `flow`, such as one begun by the
    /// CPU scope that submitted it with [`flow_begin`](crate::flow_begin).
    #[must_use]
    pub const fn after_flow(mut self, flow: FlowId) -> Self {
        self.flow = Some(flow);
        self
    }

    /// Attaches per-instance arguments, such as a pass's draw count.
    #[must_use]
    pub const fn with_args(mut self, args: Args) -> Self {
        self.args = args;
        self
    }

    /// Returns the timeline.
    #[must_use]
    pub const fn timeline(&self) -> &'static TimelineDesc {
        self.timeline
    }

    /// Returns the scope descriptor naming the work.
    #[must_use]
    pub const fn scope(&self) -> &'static ScopeDesc {
        self.scope
    }

    /// Returns the context the work is attributed to.
    #[must_use]
    pub const fn context(&self) -> ContextHandle {
        self.context
    }

    /// Returns the clock the times are in.
    #[must_use]
    pub const fn clock(&self) -> &'static ClockDesc {
        self.clock
    }

    /// Returns how long one tick of the clock lasts.
    #[must_use]
    pub const fn period(&self) -> TickPeriod {
        self.period
    }

    /// Returns when the work began, in clock ticks.
    #[must_use]
    pub const fn begin(&self) -> u64 {
        self.begin
    }

    /// Returns when the work ended, in clock ticks.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.end
    }

    /// Returns the frame the work belongs to, if given.
    #[must_use]
    pub const fn frame(&self) -> Option<FrameNumber> {
        self.frame
    }

    /// Returns the flow the work ends, if given.
    #[must_use]
    pub const fn flow(&self) -> Option<FlowId> {
        self.flow
    }

    /// Returns the per-instance arguments.
    #[must_use]
    pub const fn args(&self) -> &Args {
        &self.args
    }
}

/// What a [`FramePoint`] says about a frame.
///
/// A frame scheduler knows three times before a frame is shown, each later
/// and better informed than the one before: the platform's
/// [`Predicted`](Self::Predicted) time when the frame began, the scheduler's
/// [`Target`](Self::Target) when it planned the frame, and the
/// [`Expected`](Self::Expected) time when the frame was submitted. Comparing
/// each with [`Presented`](Self::Presented) shows how far off it was.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum FramePointKind {
    /// The latest time the frame's work could be submitted and still be
    /// shown at its target.
    Deadline,
    /// When the frame was meant to be shown, as the frame scheduler planned
    /// it.
    Target,
    /// When the frame was shown.
    Presented,
    /// The frame was never shown, and why. It usually carries no time.
    Dropped(DropReason),
    /// When the platform predicted, as the frame began, that a frame begun
    /// then would be shown, such as a display link's target timestamp.
    Predicted,
    /// When the frame was expected to be shown, as of submitting it, such
    /// as the presentation time a compositor or swapchain reports at submit.
    Expected,
}

/// Why a frame was never shown, as [`FramePointKind::Dropped`] carries it.
///
/// Only some drops are misses. A frame that missed its deadline, or found
/// no surface to draw into, leaves the previous frame on screen for another
/// refresh, which reads as a stutter. A frame the program discarded or
/// superseded, or that a later frame replaced on its way to the display,
/// left nothing missing on screen; it only cost the work spent on it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum DropReason {
    /// Why is not known.
    Unknown,
    /// The program chose not to show it, for no more specific reason.
    Discarded,
    /// Newer work replaced it before it was submitted, such as a frame
    /// planned before input that made it stale.
    Superseded,
    /// It was still waiting to be submitted when its deadline passed.
    MissedDeadline,
    /// The surface, drawable, or swapchain image it needed was unavailable.
    SurfaceUnavailable,
    /// The display it was for was unavailable, hidden, or gone.
    OutputUnavailable,
    /// It was submitted, but a later frame replaced it before the display
    /// showed it, as a mailbox swapchain does.
    Replaced,
}

impl DropReason {
    /// Returns `true` for a drop the user may see as a stutter: the frame
    /// missed its deadline, found no surface, or was dropped for a reason
    /// not known.
    #[must_use]
    pub const fn is_miss(self) -> bool {
        matches!(
            self,
            Self::Unknown | Self::MissedDeadline | Self::SurfaceUnavailable
        )
    }
}

/// A time a frame scheduler or display knows about a frame the program
/// numbers, such as its deadline, its target, or when it was shown,
/// recorded with [`frame_point`](crate::frame_point) whenever it becomes
/// known, often frames later.
///
/// Times are ticks of a clock other than the backend's, typically the host
/// clock the frame scheduler uses; relate it with
/// [`clock_sync`](crate::clock_sync).
///
/// ```
/// use footfall_core::{ClockDesc, FramePoint, FramePointKind, TickPeriod};
///
/// static HOST: ClockDesc = ClockDesc::new("host");
/// let period = TickPeriod::new(125, 3);
///
/// // When planning frame 1234.
/// footfall_core::frame_point(&FramePoint::at(1234, FramePointKind::Target, &HOST, period, 400_000));
/// // Frames later, when the display reports it.
/// footfall_core::frame_point(&FramePoint::at(1234, FramePointKind::Presented, &HOST, period, 400_000));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramePoint {
    number: FrameNumber,
    kind: FramePointKind,
    time: Option<ClockReading>,
}

impl FramePoint {
    /// Says the frame numbered `number`, a plain `u64` or a [`FrameNumber`]
    /// in one of several frame streams, reached `kind` at `ticks` of
    /// `clock`.
    #[must_use]
    pub fn at(
        number: impl Into<FrameNumber>,
        kind: FramePointKind,
        clock: &'static ClockDesc,
        period: TickPeriod,
        ticks: u64,
    ) -> Self {
        Self {
            number: number.into(),
            kind,
            time: Some(ClockReading::new(clock, ticks, period)),
        }
    }

    /// Says the frame numbered `number` was never shown, and why.
    #[must_use]
    pub fn dropped(number: impl Into<FrameNumber>, reason: DropReason) -> Self {
        Self {
            number: number.into(),
            kind: FramePointKind::Dropped(reason),
            time: None,
        }
    }

    /// Returns the program's number for the frame, in its stream.
    #[must_use]
    pub const fn number(&self) -> FrameNumber {
        self.number
    }

    /// Returns what the point says about the frame.
    #[must_use]
    pub const fn kind(&self) -> FramePointKind {
        self.kind
    }

    /// Returns when, as a reading of another clock, or `None` for a dropped
    /// frame.
    #[must_use]
    pub const fn time(&self) -> Option<ClockReading> {
        self.time
    }
}
