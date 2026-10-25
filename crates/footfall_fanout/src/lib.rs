// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fan-out backend adapter for Footfall.
//!
//! `footfall_core` installs one process-wide backend. [`Fanout`] is itself a
//! [`Backend`] that forwards every emission to two child backends, so a bounded
//! runtime capture and a live Tracy session can observe the same emissions.
//!
//! The shape is deliberately pairwise. Fan-outs nest, so more than two sinks
//! compose as a tree of [`Fanout`] values:
//!
//! ```
//! use footfall_core::NoopBackend;
//! use footfall_fanout::Fanout;
//!
//! static FIRST: NoopBackend = NoopBackend;
//! static SECOND: NoopBackend = NoopBackend;
//! static THIRD: NoopBackend = NoopBackend;
//!
//! let inner: &'static Fanout = Box::leak(Box::new(Fanout::new(&SECOND, &THIRD)));
//! let outer = Fanout::new(&FIRST, inner);
//! # let _ = outer;
//! ```

use footfall_core::{
    Args, Backend, ClockReading, Context, ContextHandle, CounterDesc, EventDesc, FlowId, FlowPhase,
    FrameNumber, FrameOpportunity, FramePlan, FramePoint, InstallBackendError, MarkerKind,
    MarkerPhase, ResourceId, ResourceKindDesc, ScopeDesc, ScopeToken, TimelineSpan,
    install_backend,
};
use hashbrown::HashMap;
use std::{
    boxed::Box,
    fmt,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// Backend that forwards every emission to two child backends.
///
/// Emissions reach the primary backend first. Scope ends and context releases
/// unwind in reverse order: the secondary backend sees them before the primary
/// one. Context handles are allocated by `footfall_core`, so both children
/// share them without translation.
pub struct Fanout {
    primary: &'static dyn Backend,
    secondary: &'static dyn Backend,
    next_scope_token: AtomicU64,
    scopes: Mutex<HashMap<u64, ScopePair>>,
}

impl Fanout {
    /// Creates a fan-out adapter over two backends.
    #[must_use]
    pub fn new(primary: &'static dyn Backend, secondary: &'static dyn Backend) -> Self {
        Self {
            primary,
            secondary,
            next_scope_token: AtomicU64::new(1),
            scopes: Mutex::new(HashMap::new()),
        }
    }

    /// Installs this fan-out as the process-wide backend and keeps it alive for the process.
    pub fn install_global(self) -> Result<&'static Self, InstallBackendError> {
        let fanout: &'static Self = Box::leak(Box::new(self));
        install_backend(fanout)?;
        Ok(fanout)
    }
}

impl fmt::Debug for Fanout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Fanout")
            .field("next_scope_token", &self.next_scope_token)
            .finish_non_exhaustive()
    }
}

impl Backend for Fanout {
    fn counter_checkpoint(&self, desc: &'static EventDesc, context: ContextHandle) {
        self.primary.counter_checkpoint(desc, context);
        self.secondary.counter_checkpoint(desc, context);
    }

    fn context_registered(&self, handle: ContextHandle, context: &Context) {
        self.primary.context_registered(handle, context);
        self.secondary.context_registered(handle, context);
    }

    fn context_released(&self, handle: ContextHandle) {
        self.secondary.context_released(handle);
        self.primary.context_released(handle);
    }

    fn counter_add(&self, desc: &'static CounterDesc, context: ContextHandle, value: u64) {
        self.primary.counter_add(desc, context, value);
        self.secondary.counter_add(desc, context, value);
    }

    fn gauge_set(&self, desc: &'static CounterDesc, context: ContextHandle, value: u64) {
        self.primary.gauge_set(desc, context, value);
        self.secondary.gauge_set(desc, context, value);
    }

    fn histogram_record(&self, desc: &'static CounterDesc, context: ContextHandle, value: u64) {
        self.primary.histogram_record(desc, context, value);
        self.secondary.histogram_record(desc, context, value);
    }

    fn scope_begin(
        &self,
        desc: &'static ScopeDesc,
        context: ContextHandle,
        args: &Args,
    ) -> ScopeToken {
        let primary = self.primary.scope_begin(desc, context, args);
        let secondary = self.secondary.scope_begin(desc, context, args);
        let token = ScopeToken::new(self.next_scope_token.fetch_add(1, Ordering::Relaxed));
        self.scopes
            .lock()
            .expect("fanout scope mutex should not be poisoned")
            .insert(token.get(), ScopePair { primary, secondary });
        token
    }

    fn scope_end(&self, token: ScopeToken) {
        if !token.is_valid() {
            return;
        }
        let pair = self
            .scopes
            .lock()
            .expect("fanout scope mutex should not be poisoned")
            .remove(&token.get());
        if let Some(pair) = pair {
            self.secondary.scope_end(pair.secondary);
            self.primary.scope_end(pair.primary);
        }
    }

    fn resource_create(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
        size: u64,
    ) {
        self.primary
            .resource_create(desc, resource_id, context, size);
        self.secondary
            .resource_create(desc, resource_id, context, size);
    }

    fn resource_update(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
        size: u64,
    ) {
        self.primary
            .resource_update(desc, resource_id, context, size);
        self.secondary
            .resource_update(desc, resource_id, context, size);
    }

    fn resource_drop(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
    ) {
        self.primary.resource_drop(desc, resource_id, context);
        self.secondary.resource_drop(desc, resource_id, context);
    }

    fn frame_marker(&self, kind: MarkerKind, phase: MarkerPhase, context: ContextHandle) {
        self.primary.frame_marker(kind, phase, context);
        self.secondary.frame_marker(kind, phase, context);
    }

    fn numbered_marker(
        &self,
        kind: MarkerKind,
        phase: MarkerPhase,
        number: FrameNumber,
        context: ContextHandle,
    ) {
        self.primary.numbered_marker(kind, phase, number, context);
        self.secondary.numbered_marker(kind, phase, number, context);
    }

    fn frame_point(&self, point: &FramePoint) {
        self.primary.frame_point(point);
        self.secondary.frame_point(point);
    }

    fn frame_opportunity(&self, opportunity: &FrameOpportunity) {
        self.primary.frame_opportunity(opportunity);
        self.secondary.frame_opportunity(opportunity);
    }

    fn frame_plan(&self, plan: &FramePlan) {
        self.primary.frame_plan(plan);
        self.secondary.frame_plan(plan);
    }

    fn timeline_span(&self, span: &TimelineSpan) {
        self.primary.timeline_span(span);
        self.secondary.timeline_span(span);
    }

    fn clock_sync(&self, reading: ClockReading) {
        self.primary.clock_sync(reading);
        self.secondary.clock_sync(reading);
    }

    fn clock_pair(&self, first: ClockReading, second: ClockReading) {
        self.primary.clock_pair(first, second);
        self.secondary.clock_pair(first, second);
    }

    fn flow(&self, flow_id: FlowId, phase: FlowPhase, context: ContextHandle) {
        self.primary.flow(flow_id, phase, context);
        self.secondary.flow(flow_id, phase, context);
    }

    fn message(&self, desc: &'static EventDesc, context: ContextHandle, text: &str, args: &Args) {
        self.primary.message(desc, context, text, args);
        self.secondary.message(desc, context, text, args);
    }
}

#[derive(Clone, Copy, Debug)]
struct ScopePair {
    primary: ScopeToken,
    secondary: ScopeToken,
}

#[cfg(test)]
mod tests {
    use super::Fanout;
    use footfall_core::{
        Args, Backend, Context, ContextHandle, CounterDesc, LabelKey, MarkerKind, MarkerPhase,
        NoopBackend, ScopeDesc, ScopeToken,
    };
    use std::sync::Mutex;

    static FRAME: LabelKey = LabelKey::new("frame");

    static COUNTER: CounterDesc = CounterDesc::count("ops");
    static SCOPE: ScopeDesc = ScopeDesc::new("record");
    static NOOP: NoopBackend = NoopBackend;

    /// Records the calls it receives under a fixed name.
    struct Recorder {
        name: &'static str,
        token: u64,
        log: &'static Mutex<Vec<String>>,
    }

    impl Recorder {
        fn leak(name: &'static str, token: u64, log: &'static Mutex<Vec<String>>) -> &'static Self {
            Box::leak(Box::new(Self { name, token, log }))
        }

        fn record(&self, call: &str) {
            self.log
                .lock()
                .expect("log mutex should not be poisoned")
                .push(format!("{} {call}", self.name));
        }
    }

    impl Backend for Recorder {
        fn counter_checkpoint(&self, _desc: &'static footfall_core::EventDesc, _context: ContextHandle) {
            self.record("counter_checkpoint");
        }
        fn context_registered(&self, handle: ContextHandle, _context: &Context) {
            self.record(&format!("context_registered {}", handle.get()));
        }

        fn context_released(&self, handle: ContextHandle) {
            self.record(&format!("context_released {}", handle.get()));
        }

        fn counter_add(&self, _desc: &'static CounterDesc, _context: ContextHandle, _value: u64) {
            self.record("counter_add");
        }

        fn scope_begin(
            &self,
            _desc: &'static ScopeDesc,
            _context: ContextHandle,
            _args: &Args,
        ) -> ScopeToken {
            self.record("scope_begin");
            ScopeToken::new(self.token)
        }

        fn scope_end(&self, token: ScopeToken) {
            self.record(&format!("scope_end {}", token.get()));
        }

        fn frame_marker(&self, _kind: MarkerKind, _phase: MarkerPhase, _context: ContextHandle) {
            self.record("frame_marker");
        }
    }

    fn leak_log() -> &'static Mutex<Vec<String>> {
        Box::leak(Box::new(Mutex::new(Vec::new())))
    }

    fn take(log: &Mutex<Vec<String>>) -> Vec<String> {
        core::mem::take(&mut *log.lock().expect("log mutex should not be poisoned"))
    }

    #[test]
    fn fanout_ignores_invalid_scope_token() {
        let fanout = Fanout::new(&NOOP, &NOOP);
        let token = fanout.scope_begin(&SCOPE, ContextHandle::ROOT, &Args::none());
        assert!(token.is_valid(), "fanout tokens should start at 1");

        fanout.scope_end(ScopeToken::INVALID);
        assert_eq!(
            fanout
                .scopes
                .lock()
                .expect("fanout scope mutex should not be poisoned")
                .len(),
            1,
            "ending the invalid token must not touch open scopes"
        );

        fanout.scope_end(token);
        assert!(
            fanout
                .scopes
                .lock()
                .expect("fanout scope mutex should not be poisoned")
                .is_empty()
        );
    }

    #[test]
    fn fanout_forwards_scopes_counters_and_markers() {
        let log = leak_log();
        let fanout = Fanout::new(
            Recorder::leak("primary", 11, log),
            Recorder::leak("secondary", 22, log),
        );
        let context = ContextHandle::new(4);
        fanout.context_registered(context, &Context::root().with(&FRAME, 1_u32));

        fanout.counter_add(&COUNTER, context, 3);
        static CHECKPOINT: footfall_core::EventDesc = footfall_core::EventDesc::new("checkpoint");
        fanout.counter_checkpoint(&CHECKPOINT, context);
        let token = fanout.scope_begin(&SCOPE, context, &Args::none());
        fanout.scope_end(token);
        fanout.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, context);
        fanout.context_released(context);

        assert_eq!(
            take(log),
            [
                "primary context_registered 4",
                "secondary context_registered 4",
                "primary counter_add",
                "secondary counter_add",
                "primary counter_checkpoint",
                "secondary counter_checkpoint",
                "primary scope_begin",
                "secondary scope_begin",
                "secondary scope_end 22",
                "primary scope_end 11",
                "primary frame_marker",
                "secondary frame_marker",
                "secondary context_released 4",
                "primary context_released 4",
            ]
        );
    }

    #[test]
    fn nested_fanouts_translate_tokens_per_level() {
        let log = leak_log();
        let inner: &'static Fanout = Box::leak(Box::new(Fanout::new(
            Recorder::leak("b", 7, log),
            Recorder::leak("c", 8, log),
        )));
        let outer = Fanout::new(Recorder::leak("a", 5, log), inner);

        let first = outer.scope_begin(&SCOPE, ContextHandle::ROOT, &Args::none());
        let second = outer.scope_begin(&SCOPE, ContextHandle::ROOT, &Args::none());
        assert_ne!(first, second, "each level issues its own tokens");
        outer.scope_end(second);
        outer.scope_end(first);

        assert_eq!(
            take(log),
            [
                "a scope_begin",
                "b scope_begin",
                "c scope_begin",
                "a scope_begin",
                "b scope_begin",
                "c scope_begin",
                "c scope_end 8",
                "b scope_end 7",
                "a scope_end 5",
                "c scope_end 8",
                "b scope_end 7",
                "a scope_end 5",
            ]
        );
        assert!(
            inner
                .scopes
                .lock()
                .expect("fanout scope mutex should not be poisoned")
                .is_empty()
        );
    }
}
