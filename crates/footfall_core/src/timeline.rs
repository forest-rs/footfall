// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Work on timelines that are not CPU threads.

use crate::{Args, ClockDesc, ContextHandle, FlowId, ScopeDesc, TickPeriod, TimelineDesc};

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
    frame: Option<u64>,
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

    /// Says which frame the work belongs to, by the program's number for it;
    /// see [`frame_begin_numbered`](crate::frame_begin_numbered).
    #[must_use]
    pub const fn in_frame(mut self, number: u64) -> Self {
        self.frame = Some(number);
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
    pub const fn frame(&self) -> Option<u64> {
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
