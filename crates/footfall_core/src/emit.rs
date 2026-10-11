// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{
    Args, Context, CounterDesc, EventDesc, FlowId, ResourceId, ResourceKindDesc, ScopeDesc,
};
#[cfg(feature = "std")]
use std::sync::OnceLock;

/// Opaque token returned by [`scope_begin`] and consumed by [`scope_end`].
///
/// The raw value `0` is reserved as [`ScopeToken::INVALID`]. It is returned
/// when no scope was recorded, for example before a backend is installed or
/// while the capture level excludes scopes, and backends ignore it in
/// `scope_end`. Backends must issue tokens starting at `1`. The
/// [`Default`] token is [`ScopeToken::INVALID`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ScopeToken {
    raw: u64,
}

impl ScopeToken {
    /// Token returned when no scope was recorded; ending it is a no-op.
    pub const INVALID: Self = Self { raw: 0 };

    /// Creates a scope token from its raw representation.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self { raw }
    }

    /// Returns the raw representation of this scope token.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.raw
    }

    /// Returns `true` unless this is [`ScopeToken::INVALID`].
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.raw != Self::INVALID.raw
    }
}

/// Drop-based scope guard returned by [`scope`].
///
/// Dropping the guard emits the matching [`scope_end`] call. Use
/// [`ScopeGuard::end`] when a scope must end before the guard leaves lexical
/// scope.
#[derive(Debug)]
#[must_use = "dropping a ScopeGuard immediately ends the scope"]
pub struct ScopeGuard {
    token: ScopeToken,
    active: bool,
}

impl ScopeGuard {
    /// Begins a new guarded scope.
    pub fn new(desc: &ScopeDesc, context: Context) -> Self {
        Self::with_args(desc, context, Args::none())
    }

    /// Begins a new guarded scope carrying per-instance arguments.
    pub fn with_args(desc: &ScopeDesc, context: Context, args: Args) -> Self {
        Self {
            token: scope_begin_with(desc, context, args),
            active: true,
        }
    }

    /// Returns the underlying scope token.
    #[must_use]
    pub const fn token(&self) -> ScopeToken {
        self.token
    }

    /// Ends the guarded scope before the guard is dropped.
    pub fn end(mut self) {
        self.end_if_active();
    }

    fn end_if_active(&mut self) {
        if self.active {
            scope_end(self.token);
            self.active = false;
        }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        self.end_if_active();
    }
}

/// Coarse marker kind used to structure a trace.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MarkerKind {
    /// Marks a frame boundary.
    Frame,
    /// Marks a transaction boundary.
    Transaction,
    /// Marks a pass boundary.
    Pass,
}

/// Boundary phase for a coarse trace marker.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MarkerPhase {
    /// Marks the start of a boundary.
    Begin,
    /// Marks the end of a boundary.
    End,
}

/// Phase of a causal flow link.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FlowPhase {
    /// Starts a causal link.
    Begin,
    /// Continues a causal link across work.
    Step,
    /// Ends a causal link.
    End,
}

/// Sink for every Footfall emission primitive.
///
/// The emission functions in this crate dispatch to the process-wide backend
/// installed with [`install_backend`], or to [`NoopBackend`] until one is
/// installed. Backends carry their own state, so one value can be installed
/// directly or composed inside another backend, for example a fan-out adapter.
///
/// Every method has a no-op default, so a backend overrides only the
/// primitives it cares about, and new primitives can be added without
/// breaking existing backends.
///
/// # Scope tokens
///
/// [`Backend::scope_begin`] returns a token that is later passed to
/// [`Backend::scope_end`]. Backends must issue tokens starting at `1`, return
/// [`ScopeToken::INVALID`] when no scope was recorded, and ignore
/// [`ScopeToken::INVALID`] in `scope_end`.
pub trait Backend: Send + Sync {
    /// Adds `value` to a counter under a semantic context.
    fn counter_add(&self, desc: &CounterDesc, context: Context, value: u64) {
        let _ = (desc, context, value);
    }

    /// Sets a gauge under a semantic context.
    fn gauge_set(&self, desc: &CounterDesc, context: Context, value: u64) {
        let _ = (desc, context, value);
    }

    /// Records one histogram sample under a semantic context.
    fn histogram_record(&self, desc: &CounterDesc, context: Context, value: u64) {
        let _ = (desc, context, value);
    }

    /// Begins a scope and returns the token that ends it.
    ///
    /// The default records nothing and returns [`ScopeToken::INVALID`].
    fn scope_begin(&self, desc: &ScopeDesc, context: Context, args: Args) -> ScopeToken {
        let _ = (desc, context, args);
        ScopeToken::INVALID
    }

    /// Ends a scope previously begun by [`Backend::scope_begin`].
    fn scope_end(&self, token: ScopeToken) {
        let _ = token;
    }

    /// Observes the creation of a logical resource.
    fn resource_create(
        &self,
        desc: &ResourceKindDesc,
        resource_id: ResourceId,
        context: Context,
        size: u64,
    ) {
        let _ = (desc, resource_id, context, size);
    }

    /// Observes a size update of a logical resource.
    fn resource_update(
        &self,
        desc: &ResourceKindDesc,
        resource_id: ResourceId,
        context: Context,
        size: u64,
    ) {
        let _ = (desc, resource_id, context, size);
    }

    /// Observes the drop of a logical resource.
    fn resource_drop(&self, desc: &ResourceKindDesc, resource_id: ResourceId, context: Context) {
        let _ = (desc, resource_id, context);
    }

    /// Observes a frame, transaction, or pass boundary.
    fn frame_marker(&self, kind: MarkerKind, phase: MarkerPhase, context: Context) {
        let _ = (kind, phase, context);
    }

    /// Observes one phase of a causal flow.
    fn flow(&self, flow_id: FlowId, phase: FlowPhase, context: Context) {
        let _ = (flow_id, phase, context);
    }

    /// Observes a message event.
    ///
    /// The text is borrowed for the duration of the call; backends that
    /// retain messages must copy it.
    fn message(&self, desc: &EventDesc, context: Context, text: &str, args: Args) {
        let _ = (desc, context, text, args);
    }
}

/// Backend that ignores every emission.
///
/// This is the backend in effect until one is installed with
/// [`install_backend`]. It is also useful as a placeholder when composing
/// backends.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopBackend;

impl Backend for NoopBackend {}

/// Returned when a backend is already installed.
#[cfg(feature = "std")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstallBackendError;

/// Adds to a counter descriptor under a semantic context.
pub fn counter_add(desc: &CounterDesc, context: Context, value: u64) {
    backend().counter_add(desc, context, value);
}

/// Sets a gauge descriptor under a semantic context.
pub fn gauge_set(desc: &CounterDesc, context: Context, value: u64) {
    backend().gauge_set(desc, context, value);
}

/// Records one sample into a histogram descriptor under a semantic context.
///
/// Histograms aggregate a value distribution per descriptor and context, so
/// consumers can derive percentiles instead of only sums and last values.
pub fn histogram_record(desc: &CounterDesc, context: Context, value: u64) {
    backend().histogram_record(desc, context, value);
}

/// Begins a scope under a semantic context.
#[must_use]
pub fn scope_begin(desc: &ScopeDesc, context: Context) -> ScopeToken {
    scope_begin_with(desc, context, Args::none())
}

/// Begins a scope carrying per-instance arguments.
#[must_use]
pub fn scope_begin_with(desc: &ScopeDesc, context: Context, args: Args) -> ScopeToken {
    backend().scope_begin(desc, context, args)
}

/// Ends a previously-begun scope.
///
/// Ending [`ScopeToken::INVALID`] does nothing.
pub fn scope_end(token: ScopeToken) {
    if token.is_valid() {
        backend().scope_end(token);
    }
}

/// Begins a scope and returns a guard that ends it on drop.
pub fn scope(desc: &ScopeDesc, context: Context) -> ScopeGuard {
    ScopeGuard::new(desc, context)
}

/// Begins a guarded scope carrying per-instance arguments.
pub fn scope_with(desc: &ScopeDesc, context: Context, args: Args) -> ScopeGuard {
    ScopeGuard::with_args(desc, context, args)
}

/// Emits a resource-creation observation.
pub fn resource_create(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    backend().resource_create(desc, resource_id, context, size);
}

/// Emits a resource-update observation.
pub fn resource_update(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    backend().resource_update(desc, resource_id, context, size);
}

/// Emits a resource-drop observation.
pub fn resource_drop(desc: &ResourceKindDesc, resource_id: ResourceId, context: Context) {
    backend().resource_drop(desc, resource_id, context);
}

/// Emits a frame, transaction, or pass boundary marker.
pub fn frame_marker(kind: MarkerKind, phase: MarkerPhase, context: Context) {
    backend().frame_marker(kind, phase, context);
}

/// Emits a frame begin marker.
pub fn frame_begin(context: Context) {
    frame_marker(MarkerKind::Frame, MarkerPhase::Begin, context);
}

/// Emits a frame end marker.
pub fn frame_end(context: Context) {
    frame_marker(MarkerKind::Frame, MarkerPhase::End, context);
}

/// Emits a transaction begin marker.
pub fn transaction_begin(context: Context) {
    frame_marker(MarkerKind::Transaction, MarkerPhase::Begin, context);
}

/// Emits a transaction end marker.
pub fn transaction_end(context: Context) {
    frame_marker(MarkerKind::Transaction, MarkerPhase::End, context);
}

/// Emits a pass begin marker.
pub fn pass_begin(context: Context) {
    frame_marker(MarkerKind::Pass, MarkerPhase::Begin, context);
}

/// Emits a pass end marker.
pub fn pass_end(context: Context) {
    frame_marker(MarkerKind::Pass, MarkerPhase::End, context);
}

/// Starts a causal flow under a semantic context.
pub fn flow_begin(flow_id: FlowId, context: Context) {
    backend().flow(flow_id, FlowPhase::Begin, context);
}

/// Continues a causal flow under a semantic context.
pub fn flow_step(flow_id: FlowId, context: Context) {
    backend().flow(flow_id, FlowPhase::Step, context);
}

/// Ends a causal flow under a semantic context.
pub fn flow_end(flow_id: FlowId, context: Context) {
    backend().flow(flow_id, FlowPhase::End, context);
}

/// Emits a message event under a semantic context.
///
/// The text is borrowed for the duration of the call; backends that retain
/// messages copy and may truncate it. Message formatting cost is only paid by
/// the caller when a backend is installed and the level captures messages, so
/// prefer emitting through a facility that already formatted the text, such
/// as a log or tracing bridge.
pub fn message(desc: &EventDesc, context: Context, text: &str) {
    backend().message(desc, context, text, Args::none());
}

/// Emits a message event carrying per-instance arguments.
pub fn message_with(desc: &EventDesc, context: Context, text: &str, args: Args) {
    backend().message(desc, context, text, args);
}

/// Installs the process-wide backend.
///
/// Only the first installation succeeds; the backend stays installed for the
/// rest of the process. Emissions made before installation go to
/// [`NoopBackend`].
#[cfg(feature = "std")]
pub fn install_backend(backend: &'static dyn Backend) -> Result<(), InstallBackendError> {
    BACKEND.set(backend).map_err(|_| InstallBackendError)
}

fn backend() -> &'static dyn Backend {
    #[cfg(feature = "std")]
    if let Some(backend) = BACKEND.get() {
        return *backend;
    }

    &NoopBackend
}

#[cfg(feature = "std")]
static BACKEND: OnceLock<&'static dyn Backend> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::{
        MarkerKind, MarkerPhase, ScopeToken, counter_add, flow_begin, flow_end, flow_step,
        frame_begin, frame_end, frame_marker, pass_begin, pass_end, resource_drop, scope,
        scope_begin, scope_end, transaction_begin, transaction_end,
    };
    use crate::{
        Context, CounterDesc, CounterDescId, FlowId, ResourceId, ResourceKindDesc, ResourceKindId,
        ScopeDesc, ScopeDescId,
    };

    #[test]
    fn noop_scope_begin_returns_invalid_token() {
        let desc = ScopeDesc::new(ScopeDescId::new(9), "record_pass");
        let token = scope_begin(&desc, Context::root());
        assert_eq!(token, ScopeToken::INVALID);
        assert!(!token.is_valid());
        scope_end(token);
    }

    #[test]
    fn scope_guard_exposes_token_and_can_end_early() {
        let desc = ScopeDesc::new(ScopeDescId::new(10), "record_pass");
        let guard = scope(&desc, Context::root());
        assert_eq!(guard.token(), ScopeToken::INVALID);
        guard.end();
    }

    #[test]
    fn default_token_is_invalid_and_issued_tokens_are_valid() {
        assert_eq!(ScopeToken::default(), ScopeToken::INVALID);
        assert!(ScopeToken::new(1).is_valid());
    }

    #[test]
    fn noop_emission_surface_accepts_all_primitives() {
        let counter = CounterDesc::count(CounterDescId::new(1), "ops");
        let resource = ResourceKindDesc::new(ResourceKindId::new(2), "atlas");
        let context = Context::root();

        counter_add(&counter, context, 5);
        resource_drop(&resource, ResourceId::new(7), context);
        frame_marker(MarkerKind::Frame, MarkerPhase::Begin, context);
        frame_begin(context);
        frame_end(context);
        transaction_begin(context);
        transaction_end(context);
        pass_begin(context);
        pass_end(context);
        flow_begin(FlowId::new(12), context);
        flow_step(FlowId::new(12), context);
        flow_end(FlowId::new(12), context);
    }
}
