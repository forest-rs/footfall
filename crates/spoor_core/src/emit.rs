// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Context, CounterDesc, FlowId, ResourceId, ResourceKindDesc, ScopeDesc, ScopeDescId};
#[cfg(feature = "std")]
use std::sync::OnceLock;

/// Opaque token returned by [`scope_begin`] and consumed by [`scope_end`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ScopeToken {
    raw: u64,
}

impl ScopeToken {
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
        Self {
            token: scope_begin(desc, context),
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

/// Function table implemented by a concrete runtime backend.
#[derive(Clone, Copy, Debug)]
pub struct Backend {
    /// Counter delta emission.
    pub counter_add: fn(&CounterDesc, Context, u64),
    /// Gauge update emission.
    pub gauge_set: fn(&CounterDesc, Context, u64),
    /// Scope begin emission.
    pub scope_begin: fn(&ScopeDesc, Context) -> ScopeToken,
    /// Scope end emission.
    pub scope_end: fn(ScopeToken),
    /// Resource creation emission.
    pub resource_create: fn(&ResourceKindDesc, ResourceId, Context, u64),
    /// Resource update emission.
    pub resource_update: fn(&ResourceKindDesc, ResourceId, Context, u64),
    /// Resource drop emission.
    pub resource_drop: fn(&ResourceKindDesc, ResourceId, Context),
    /// Frame, transaction, or pass boundary emission.
    pub frame_marker: fn(MarkerKind, MarkerPhase, Context),
    /// Causal flow emission.
    pub flow: fn(FlowId, FlowPhase, Context),
}

impl Backend {
    /// Built-in no-op backend used until a runtime is installed.
    pub const NOOP: Self = Self {
        counter_add: noop_counter_add,
        gauge_set: noop_gauge_set,
        scope_begin: noop_scope_begin,
        scope_end: noop_scope_end,
        resource_create: noop_resource_create,
        resource_update: noop_resource_update,
        resource_drop: noop_resource_drop,
        frame_marker: noop_frame_marker,
        flow: noop_flow,
    };
}

/// Returned when a backend is already installed.
#[cfg(feature = "std")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstallBackendError;

/// Adds to a counter descriptor under a semantic context.
pub fn counter_add(desc: &CounterDesc, context: Context, value: u64) {
    (backend().counter_add)(desc, context, value);
}

/// Sets a gauge descriptor under a semantic context.
pub fn gauge_set(desc: &CounterDesc, context: Context, value: u64) {
    (backend().gauge_set)(desc, context, value);
}

/// Begins a scope under a semantic context.
#[must_use]
pub fn scope_begin(desc: &ScopeDesc, context: Context) -> ScopeToken {
    (backend().scope_begin)(desc, context)
}

/// Ends a previously-begun scope.
pub fn scope_end(token: ScopeToken) {
    (backend().scope_end)(token);
}

/// Begins a scope and returns a guard that ends it on drop.
pub fn scope(desc: &ScopeDesc, context: Context) -> ScopeGuard {
    ScopeGuard::new(desc, context)
}

/// Emits a resource-creation observation.
pub fn resource_create(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    (backend().resource_create)(desc, resource_id, context, size);
}

/// Emits a resource-update observation.
pub fn resource_update(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    (backend().resource_update)(desc, resource_id, context, size);
}

/// Emits a resource-drop observation.
pub fn resource_drop(desc: &ResourceKindDesc, resource_id: ResourceId, context: Context) {
    (backend().resource_drop)(desc, resource_id, context);
}

/// Emits a frame, transaction, or pass boundary marker.
pub fn frame_marker(kind: MarkerKind, phase: MarkerPhase, context: Context) {
    (backend().frame_marker)(kind, phase, context);
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
    (backend().flow)(flow_id, FlowPhase::Begin, context);
}

/// Continues a causal flow under a semantic context.
pub fn flow_step(flow_id: FlowId, context: Context) {
    (backend().flow)(flow_id, FlowPhase::Step, context);
}

/// Ends a causal flow under a semantic context.
pub fn flow_end(flow_id: FlowId, context: Context) {
    (backend().flow)(flow_id, FlowPhase::End, context);
}

/// Installs a process-wide runtime backend.
#[cfg(feature = "std")]
pub fn install_backend(backend: Backend) -> Result<(), InstallBackendError> {
    BACKEND.set(backend).map_err(|_| InstallBackendError)
}

fn backend() -> &'static Backend {
    #[cfg(feature = "std")]
    if let Some(backend) = BACKEND.get() {
        return backend;
    }

    &Backend::NOOP
}

#[cfg(feature = "std")]
static BACKEND: OnceLock<Backend> = OnceLock::new();

fn noop_counter_add(_desc: &CounterDesc, _context: Context, _value: u64) {}

fn noop_gauge_set(_desc: &CounterDesc, _context: Context, _value: u64) {}

fn noop_scope_begin(desc: &ScopeDesc, _context: Context) -> ScopeToken {
    ScopeToken::new(scope_desc_raw(desc.id()))
}

fn noop_scope_end(_token: ScopeToken) {}

fn noop_resource_create(
    _desc: &ResourceKindDesc,
    _resource_id: ResourceId,
    _context: Context,
    _size: u64,
) {
}

fn noop_resource_update(
    _desc: &ResourceKindDesc,
    _resource_id: ResourceId,
    _context: Context,
    _size: u64,
) {
}

fn noop_resource_drop(_desc: &ResourceKindDesc, _resource_id: ResourceId, _context: Context) {}

fn noop_frame_marker(_kind: MarkerKind, _phase: MarkerPhase, _context: Context) {}

fn noop_flow(_flow_id: FlowId, _phase: FlowPhase, _context: Context) {}

const fn scope_desc_raw(id: ScopeDescId) -> u64 {
    id.get()
}

#[cfg(test)]
mod tests {
    use super::{
        MarkerKind, MarkerPhase, counter_add, flow_begin, flow_end, flow_step, frame_begin,
        frame_end, frame_marker, pass_begin, pass_end, resource_drop, scope, scope_begin,
        scope_end, transaction_begin, transaction_end,
    };
    use crate::{
        Context, CounterDesc, CounterDescId, FlowId, ResourceId, ResourceKindDesc, ResourceKindId,
        ScopeDesc, ScopeDescId,
    };

    #[test]
    fn noop_scope_token_round_trips_descriptor_id() {
        let desc = ScopeDesc::new(ScopeDescId::new(9), "record_pass");
        let token = scope_begin(&desc, Context::root());
        assert_eq!(token.get(), 9);
        scope_end(token);
    }

    #[test]
    fn scope_guard_exposes_token_and_can_end_early() {
        let desc = ScopeDesc::new(ScopeDescId::new(10), "record_pass");
        let guard = scope(&desc, Context::root());
        assert_eq!(guard.token().get(), 10);
        guard.end();
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
