// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

#[cfg(feature = "std")]
use crate::InstrumentationLevel;
#[cfg(not(footfall_compile_out))]
use crate::max_level;
use crate::{
    Args, Context, ContextHandle, CounterDesc, EventDesc, FlowId, ResourceId, ResourceKindDesc,
    ScopeDesc,
};
#[cfg(feature = "std")]
use std::sync::OnceLock;

/// Forwards one emission to the installed backend when `$enabled` holds.
///
/// `$enabled` is the cheap pre-dispatch check, normally against
/// [`max_level`]; when it fails the call returns `$skipped` without loading
/// the backend. With `--cfg footfall_compile_out` the whole call is replaced
/// by evaluating the arguments and discarding them, so instrumented code
/// compiles to nothing.
macro_rules! dispatch {
    (when $enabled:expr => $method:ident($($arg:expr),* $(,)?) else $skipped:expr) => {{
        #[cfg(not(footfall_compile_out))]
        {
            if $enabled {
                backend().$method($($arg),*)
            } else {
                $skipped
            }
        }
        #[cfg(footfall_compile_out)]
        {
            let _ = ($($arg,)*);
            $skipped
        }
    }};
    (when $enabled:expr => $method:ident($($arg:expr),* $(,)?)) => {
        dispatch!(when $enabled => $method($($arg),*) else ())
    };
}

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
    #[cfg_attr(footfall_compile_out, inline(always))]
    #[cfg_attr(not(footfall_compile_out), inline)]
    pub fn new(desc: &'static ScopeDesc, context: ContextHandle) -> Self {
        Self {
            token: scope_begin(desc, context),
            active: true,
        }
    }

    /// Begins a new guarded scope carrying per-instance arguments.
    #[cfg_attr(footfall_compile_out, inline(always))]
    #[cfg_attr(not(footfall_compile_out), inline)]
    pub fn with_args(desc: &'static ScopeDesc, context: ContextHandle, args: &Args) -> Self {
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
    #[cfg_attr(footfall_compile_out, inline(always))]
    #[cfg_attr(not(footfall_compile_out), inline)]
    pub fn end(mut self) {
        self.end_if_active();
    }

    #[cfg_attr(footfall_compile_out, inline(always))]
    #[cfg_attr(not(footfall_compile_out), inline)]
    fn end_if_active(&mut self) {
        if self.active {
            scope_end(self.token);
            self.active = false;
        }
    }
}

impl Drop for ScopeGuard {
    #[cfg_attr(footfall_compile_out, inline(always))]
    #[cfg_attr(not(footfall_compile_out), inline)]
    fn drop(&mut self) {
        self.end_if_active();
    }
}

/// Coarse marker kind used to structure a trace.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
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
/// # Contexts
///
/// Emissions carry a [`ContextHandle`]. The context behind it arrives once,
/// through [`Backend::context_registered`]; [`ContextHandle::ROOT`] is the
/// empty root context and is never registered. A backend may receive handles
/// it was never told about, for example ones registered before it was
/// installed, and should attribute those to the root context.
///
/// # Scope tokens
///
/// [`Backend::scope_begin`] returns a token that is later passed to
/// [`Backend::scope_end`]. Backends must issue tokens starting at `1`, return
/// [`ScopeToken::INVALID`] when no scope was recorded, and ignore
/// [`ScopeToken::INVALID`] in `scope_end`.
pub trait Backend: Send + Sync {
    /// Learns the context behind a newly registered handle.
    ///
    /// Called once per [`register_context`](crate::register_context), whatever
    /// the instrumentation level; every later emission carries only the
    /// handle. Backends that attribute observations keep the context until
    /// [`Backend::context_released`].
    fn context_registered(&self, handle: ContextHandle, context: &Context) {
        let _ = (handle, context);
    }

    /// Forgets a handle released with [`release_context`](crate::release_context).
    ///
    /// Observations already recorded with the handle may still need it, so a
    /// backend may keep resolving it until it no longer holds any of them.
    fn context_released(&self, handle: ContextHandle) {
        let _ = handle;
    }

    /// Adds `value` to a counter under a semantic context.
    fn counter_add(&self, desc: &'static CounterDesc, context: ContextHandle, value: u64) {
        let _ = (desc, context, value);
    }

    /// Sets a gauge under a semantic context.
    fn gauge_set(&self, desc: &'static CounterDesc, context: ContextHandle, value: u64) {
        let _ = (desc, context, value);
    }

    /// Records one histogram sample under a semantic context.
    fn histogram_record(&self, desc: &'static CounterDesc, context: ContextHandle, value: u64) {
        let _ = (desc, context, value);
    }

    /// Begins a scope and returns the token that ends it.
    ///
    /// The default records nothing and returns [`ScopeToken::INVALID`].
    fn scope_begin(
        &self,
        desc: &'static ScopeDesc,
        context: ContextHandle,
        args: &Args,
    ) -> ScopeToken {
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
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
        size: u64,
    ) {
        let _ = (desc, resource_id, context, size);
    }

    /// Observes a size update of a logical resource.
    fn resource_update(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
        size: u64,
    ) {
        let _ = (desc, resource_id, context, size);
    }

    /// Observes the drop of a logical resource.
    fn resource_drop(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
    ) {
        let _ = (desc, resource_id, context);
    }

    /// Observes a frame, transaction, or pass boundary.
    fn frame_marker(&self, kind: MarkerKind, phase: MarkerPhase, context: ContextHandle) {
        let _ = (kind, phase, context);
    }

    /// Observes one phase of a causal flow.
    fn flow(&self, flow_id: FlowId, phase: FlowPhase, context: ContextHandle) {
        let _ = (flow_id, phase, context);
    }

    /// Observes a message event.
    ///
    /// The text is borrowed for the duration of the call; backends that
    /// retain messages must copy it.
    fn message(&self, desc: &'static EventDesc, context: ContextHandle, text: &str, args: &Args) {
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
#[non_exhaustive]
pub struct InstallBackendError;

#[cfg(feature = "std")]
impl core::fmt::Display for InstallBackendError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("a footfall backend is already installed")
    }
}

#[cfg(feature = "std")]
impl core::error::Error for InstallBackendError {}

/// Adds to a counter descriptor under a semantic context.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn counter_add(desc: &'static CounterDesc, context: ContextHandle, value: u64) {
    dispatch!(when max_level().captures_counters() => counter_add(desc, context, value));
}

/// Sets a gauge descriptor under a semantic context.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn gauge_set(desc: &'static CounterDesc, context: ContextHandle, value: u64) {
    dispatch!(when max_level().captures_counters() => gauge_set(desc, context, value));
}

/// Records one sample into a histogram descriptor under a semantic context.
///
/// Histograms aggregate a value distribution per descriptor and context, so
/// consumers can derive percentiles instead of only sums and last values.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn histogram_record(desc: &'static CounterDesc, context: ContextHandle, value: u64) {
    dispatch!(when max_level().captures_counters() => histogram_record(desc, context, value));
}

/// Begins a scope under a semantic context.
#[must_use]
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn scope_begin(desc: &'static ScopeDesc, context: ContextHandle) -> ScopeToken {
    // Dispatch directly rather than through `scope_begin_with`, so the empty
    // `Args` is only built once the level check has passed.
    dispatch!(when max_level().captures_scopes() => scope_begin(desc, context, &Args::none()) else ScopeToken::INVALID)
}

/// Begins a scope carrying per-instance arguments.
#[must_use]
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn scope_begin_with(
    desc: &'static ScopeDesc,
    context: ContextHandle,
    args: &Args,
) -> ScopeToken {
    dispatch!(when max_level().captures_scopes() => scope_begin(desc, context, args) else ScopeToken::INVALID)
}

/// Ends a previously-begun scope.
///
/// Ending [`ScopeToken::INVALID`] does nothing.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn scope_end(token: ScopeToken) {
    if token.is_valid() {
        dispatch!(when true => scope_end(token));
    }
}

/// Begins a scope and returns a guard that ends it on drop.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn scope(desc: &'static ScopeDesc, context: ContextHandle) -> ScopeGuard {
    ScopeGuard::new(desc, context)
}

/// Begins a guarded scope carrying per-instance arguments.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn scope_with(desc: &'static ScopeDesc, context: ContextHandle, args: &Args) -> ScopeGuard {
    ScopeGuard::with_args(desc, context, args)
}

/// Emits a resource-creation observation.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn resource_create(
    desc: &'static ResourceKindDesc,
    resource_id: ResourceId,
    context: ContextHandle,
    size: u64,
) {
    dispatch!(when max_level().captures_resources() => resource_create(desc, resource_id, context, size));
}

/// Emits a resource-update observation.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn resource_update(
    desc: &'static ResourceKindDesc,
    resource_id: ResourceId,
    context: ContextHandle,
    size: u64,
) {
    dispatch!(when max_level().captures_resources() => resource_update(desc, resource_id, context, size));
}

/// Emits a resource-drop observation.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn resource_drop(
    desc: &'static ResourceKindDesc,
    resource_id: ResourceId,
    context: ContextHandle,
) {
    dispatch!(when max_level().captures_resources() => resource_drop(desc, resource_id, context));
}

/// Emits a frame, transaction, or pass boundary marker.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn frame_marker(kind: MarkerKind, phase: MarkerPhase, context: ContextHandle) {
    dispatch!(when matches!(phase, MarkerPhase::End) || max_level().captures_markers() => frame_marker(kind, phase, context));
}

/// Emits a frame begin marker.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn frame_begin(context: ContextHandle) {
    frame_marker(MarkerKind::Frame, MarkerPhase::Begin, context);
}

/// Emits a frame end marker.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn frame_end(context: ContextHandle) {
    frame_marker(MarkerKind::Frame, MarkerPhase::End, context);
}

/// Emits a transaction begin marker.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn transaction_begin(context: ContextHandle) {
    frame_marker(MarkerKind::Transaction, MarkerPhase::Begin, context);
}

/// Emits a transaction end marker.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn transaction_end(context: ContextHandle) {
    frame_marker(MarkerKind::Transaction, MarkerPhase::End, context);
}

/// Emits a pass begin marker.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn pass_begin(context: ContextHandle) {
    frame_marker(MarkerKind::Pass, MarkerPhase::Begin, context);
}

/// Emits a pass end marker.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn pass_end(context: ContextHandle) {
    frame_marker(MarkerKind::Pass, MarkerPhase::End, context);
}

/// Starts a causal flow under a semantic context.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn flow_begin(flow_id: FlowId, context: ContextHandle) {
    dispatch!(when max_level().captures_flows() => flow(flow_id, FlowPhase::Begin, context));
}

/// Continues a causal flow under a semantic context.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn flow_step(flow_id: FlowId, context: ContextHandle) {
    dispatch!(when max_level().captures_flows() => flow(flow_id, FlowPhase::Step, context));
}

/// Ends a causal flow under a semantic context.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn flow_end(flow_id: FlowId, context: ContextHandle) {
    dispatch!(when max_level().captures_flows() => flow(flow_id, FlowPhase::End, context));
}

/// Emits a message event under a semantic context.
///
/// The text is borrowed for the duration of the call; backends that retain
/// messages copy and may truncate it. Message formatting cost is only paid by
/// the caller when a backend is installed and the level captures messages, so
/// prefer emitting through a facility that already formatted the text, such
/// as a log or tracing bridge.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn message(desc: &'static EventDesc, context: ContextHandle, text: &str) {
    dispatch!(when max_level().captures_messages() => message(desc, context, text, &Args::none()));
}

/// Emits a message event carrying per-instance arguments.
#[cfg_attr(footfall_compile_out, inline(always))]
#[cfg_attr(not(footfall_compile_out), inline)]
pub fn message_with(desc: &'static EventDesc, context: ContextHandle, text: &str, args: &Args) {
    dispatch!(when max_level().captures_messages() => message(desc, context, text, args));
}

/// Installs the process-wide backend.
///
/// Only the first installation succeeds; the backend stays installed for the
/// rest of the process. Emissions made before installation go to
/// [`NoopBackend`].
///
/// A successful installation raises the process-wide maximum level to
/// [`InstrumentationLevel::Trace`] so the backend sees every emission; lower
/// it afterwards with [`set_max_level`](crate::set_max_level) to filter
/// emissions before they reach the backend.
#[cfg(feature = "std")]
pub fn install_backend(backend: &'static dyn Backend) -> Result<(), InstallBackendError> {
    BACKEND.set(backend).map_err(|_| InstallBackendError)?;
    crate::set_max_level(InstrumentationLevel::Trace);
    Ok(())
}

#[cfg(not(footfall_compile_out))]
pub(crate) fn backend() -> &'static dyn Backend {
    #[cfg(feature = "std")]
    if let Some(backend) = BACKEND.get() {
        return *backend;
    }

    &NoopBackend
}

/// Returns `true` once a backend has been installed.
#[cfg(feature = "std")]
pub(crate) fn backend_installed() -> bool {
    BACKEND.get().is_some()
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
    use crate::{ContextHandle, CounterDesc, FlowId, ResourceId, ResourceKindDesc, ScopeDesc};

    static RECORD_PASS: ScopeDesc = ScopeDesc::new("record_pass");
    static OPS: CounterDesc = CounterDesc::count("ops");
    static ATLAS: ResourceKindDesc = ResourceKindDesc::new("atlas");

    #[test]
    fn noop_scope_begin_returns_invalid_token() {
        let token = scope_begin(&RECORD_PASS, ContextHandle::ROOT);
        assert_eq!(token, ScopeToken::INVALID);
        assert!(!token.is_valid());
        scope_end(token);
    }

    #[test]
    fn scope_guard_exposes_token_and_can_end_early() {
        let guard = scope(&RECORD_PASS, ContextHandle::ROOT);
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
        let context = ContextHandle::ROOT;

        counter_add(&OPS, context, 5);
        resource_drop(&ATLAS, ResourceId::new(7), context);
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
