// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounded runtime collection for Spoor.

use spoor_core::{
    Backend, Context, ContextId, CounterDesc, CounterDescId, FlowId, FlowPhase,
    InstallBackendError, MarkerKind, MarkerPhase, ResourceId, ResourceKindDesc, ResourceKindId,
    ScopeDesc, ScopeDescId, ScopeToken, install_backend,
};
use std::{
    collections::{HashMap, VecDeque},
    string::String,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

/// Installs a runtime as the process-wide Spoor backend.
pub fn install_global(runtime: &'static Runtime) -> Result<(), InstallError> {
    RUNTIME
        .set(runtime)
        .map_err(|_| InstallError::RuntimeAlreadyInstalled)?;
    install_backend(BACKEND).map_err(InstallError::BackendAlreadyInstalled)
}

/// In-process bounded collector for counters and timestamped observation records.
#[derive(Debug)]
pub struct Runtime {
    created_at: Instant,
    process_name: String,
    session_name: Option<String>,
    state: Mutex<State>,
}

impl Runtime {
    /// Creates a runtime with explicit counter-key and event capacities.
    #[must_use]
    pub fn new(counter_capacity: usize, event_capacity: usize) -> Self {
        Self::named("spoor", counter_capacity, event_capacity)
    }

    /// Creates a runtime with an explicit process name.
    #[must_use]
    pub fn named(
        process_name: impl Into<String>,
        counter_capacity: usize,
        event_capacity: usize,
    ) -> Self {
        Self {
            created_at: Instant::now(),
            process_name: process_name.into(),
            session_name: None,
            state: Mutex::new(State::new(counter_capacity, event_capacity)),
        }
    }

    /// Sets an optional capture-session label for future snapshots.
    pub fn set_session_name(&mut self, session_name: impl Into<String>) {
        self.session_name = Some(session_name.into());
    }

    /// Clears the capture-session label for future snapshots.
    pub fn clear_session_name(&mut self) {
        self.session_name = None;
    }

    /// Returns a snapshot of the currently collected counters, contexts, and events.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let captured_at_micros = self.now_micros();
        let state = self
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        state.snapshot(self.metadata(state.session_sequence), captured_at_micros)
    }

    /// Drains the current capture state into a snapshot and starts a fresh session.
    #[must_use]
    pub fn drain_snapshot(&self) -> Snapshot {
        let captured_at_micros = self.now_micros();
        let mut state = self
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        let snapshot = state.snapshot(self.metadata(state.session_sequence), captured_at_micros);
        state.reset(captured_at_micros);
        snapshot
    }

    /// Clears the current capture state and starts a fresh session.
    pub fn reset(&self) {
        let captured_at_micros = self.now_micros();
        let mut state = self
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        state.reset(captured_at_micros);
    }

    fn metadata(&self, session_sequence: u64) -> SnapshotMetadata {
        SnapshotMetadata {
            process_name: self.process_name.clone(),
            session_name: self.session_name.clone(),
            session_sequence,
        }
    }

    fn now_micros(&self) -> u64 {
        duration_to_micros(self.created_at.elapsed())
    }
}

/// Error returned when runtime installation fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallError {
    /// A runtime has already been installed.
    RuntimeAlreadyInstalled,
    /// A backend has already been installed in `spoor_core`.
    BackendAlreadyInstalled(InstallBackendError),
}

/// Snapshot of counters, canonical contexts, and events retained by a [`Runtime`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    /// Snapshot metadata identifying the process and capture session.
    pub metadata: SnapshotMetadata,
    /// Snapshot start time relative to runtime creation.
    pub capture_start_micros: u64,
    /// Time since runtime creation when the snapshot was captured.
    pub captured_at_micros: u64,
    /// Canonical context table referenced by counters and events.
    pub contexts: Vec<ContextSnapshot>,
    /// Aggregated counters and gauges.
    pub counters: Vec<CounterSnapshot>,
    /// Bounded sequence of timestamped observation records.
    pub events: Vec<TimedObservation>,
    /// Count of observations dropped because a new context could not be interned.
    pub dropped_contexts: u64,
    /// Count of counter updates dropped due to counter-key capacity pressure.
    pub dropped_counters: u64,
    /// Count of events dropped due to event-capacity pressure.
    pub dropped_events: u64,
}

impl Snapshot {
    /// Resolves one canonical context by id.
    #[must_use]
    pub fn context(&self, context_id: ContextId) -> Option<&ContextSnapshot> {
        usize::try_from(context_id.get())
            .ok()
            .and_then(|index| self.contexts.get(index))
    }
}

/// Metadata describing the process and session that produced a snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SnapshotMetadata {
    /// Process name used by exporters.
    pub process_name: String,
    /// Optional capture-session label.
    pub session_name: Option<String>,
    /// Monotonic sequence number of the capture session.
    pub session_sequence: u64,
}

/// One canonical context carried by a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSnapshot {
    /// Canonical context id.
    pub id: ContextId,
    /// Explicit semantic context value.
    pub context: Context,
}

/// One aggregated counter or gauge value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterSnapshot {
    /// Counter descriptor id.
    pub desc_id: CounterDescId,
    /// Stable descriptor name.
    pub name: &'static str,
    /// Canonical semantic context id.
    pub context_id: ContextId,
    /// Whether the value is additive or gauge-like.
    pub kind: CounterKind,
    /// Aggregated value.
    pub value: u64,
    /// Last update time relative to runtime creation.
    pub updated_at_micros: u64,
}

/// Distinguishes additive counters from gauges.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CounterKind {
    /// Additive counter.
    Counter,
    /// Last-value gauge.
    Gauge,
}

/// Observation paired with a timestamp relative to runtime creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimedObservation {
    /// Observation time in microseconds since runtime creation.
    pub at_micros: u64,
    /// Observation payload.
    pub observation: Observation,
}

/// One retained observation record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Observation {
    /// Scope begin event.
    ScopeBegin {
        /// Descriptor id for the scope.
        desc_id: ScopeDescId,
        /// Stable descriptor name.
        name: &'static str,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Opaque scope token returned to the caller.
        token: ScopeToken,
    },
    /// Scope end event.
    ScopeEnd {
        /// Opaque scope token previously returned from `scope_begin`.
        token: ScopeToken,
    },
    /// Resource creation event.
    ResourceCreate {
        /// Resource kind id.
        kind_id: ResourceKindId,
        /// Stable resource kind name.
        kind_name: &'static str,
        /// Resource instance id.
        resource_id: ResourceId,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Logical resource size.
        size: u64,
    },
    /// Resource update event.
    ResourceUpdate {
        /// Resource kind id.
        kind_id: ResourceKindId,
        /// Stable resource kind name.
        kind_name: &'static str,
        /// Resource instance id.
        resource_id: ResourceId,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Logical resource size.
        size: u64,
    },
    /// Resource drop event.
    ResourceDrop {
        /// Resource kind id.
        kind_id: ResourceKindId,
        /// Stable resource kind name.
        kind_name: &'static str,
        /// Resource instance id.
        resource_id: ResourceId,
        /// Canonical semantic context id.
        context_id: ContextId,
    },
    /// Frame, transaction, or pass marker.
    FrameMarker {
        /// Marker kind.
        kind: MarkerKind,
        /// Marker phase.
        phase: MarkerPhase,
        /// Canonical semantic context id.
        context_id: ContextId,
    },
    /// Causal flow observation.
    Flow {
        /// Stable flow id linking related work.
        flow_id: FlowId,
        /// Flow phase.
        phase: FlowPhase,
        /// Canonical semantic context id.
        context_id: ContextId,
    },
}

impl Observation {
    /// Returns the canonical context id carried by this observation when present.
    #[must_use]
    pub fn context_id(&self) -> Option<ContextId> {
        match self {
            Self::ScopeBegin { context_id, .. }
            | Self::ResourceCreate { context_id, .. }
            | Self::ResourceUpdate { context_id, .. }
            | Self::ResourceDrop { context_id, .. }
            | Self::FrameMarker { context_id, .. }
            | Self::Flow { context_id, .. } => Some(*context_id),
            Self::ScopeEnd { .. } => None,
        }
    }
}

#[derive(Debug)]
struct State {
    counter_capacity: usize,
    event_capacity: usize,
    context_capacity: usize,
    capture_start_micros: u64,
    session_sequence: u64,
    next_scope_token: u64,
    next_context_id: u64,
    context_ids: HashMap<Context, ContextId>,
    contexts: Vec<ContextSnapshot>,
    counters: HashMap<CounterKey, CounterSnapshot>,
    events: VecDeque<TimedObservation>,
    dropped_contexts: u64,
    dropped_counters: u64,
    dropped_events: u64,
}

impl State {
    fn new(counter_capacity: usize, event_capacity: usize) -> Self {
        let context_capacity = counter_capacity
            .saturating_add(event_capacity)
            .saturating_add(1);
        let root_id = ContextId::new(0);
        let root_context = Context::root();
        let mut context_ids = HashMap::with_capacity(context_capacity);
        context_ids.insert(root_context, root_id);

        Self {
            counter_capacity,
            event_capacity,
            context_capacity,
            capture_start_micros: 0,
            session_sequence: 0,
            next_scope_token: 1,
            next_context_id: 1,
            context_ids,
            contexts: vec![ContextSnapshot {
                id: root_id,
                context: root_context,
            }],
            counters: HashMap::with_capacity(counter_capacity),
            events: VecDeque::with_capacity(event_capacity),
            dropped_contexts: 0,
            dropped_counters: 0,
            dropped_events: 0,
        }
    }

    fn snapshot(&self, metadata: SnapshotMetadata, captured_at_micros: u64) -> Snapshot {
        Snapshot {
            metadata,
            capture_start_micros: self.capture_start_micros,
            captured_at_micros,
            contexts: self.contexts.clone(),
            counters: self.counters.values().cloned().collect(),
            events: self.events.iter().cloned().collect(),
            dropped_contexts: self.dropped_contexts,
            dropped_counters: self.dropped_counters,
            dropped_events: self.dropped_events,
        }
    }

    fn reset(&mut self, capture_start_micros: u64) {
        self.capture_start_micros = capture_start_micros;
        self.session_sequence = self.session_sequence.saturating_add(1);
        self.next_scope_token = 1;
        self.next_context_id = 1;
        self.context_ids.clear();
        self.contexts.clear();
        let root_id = ContextId::new(0);
        let root_context = Context::root();
        self.context_ids.insert(root_context, root_id);
        self.contexts.push(ContextSnapshot {
            id: root_id,
            context: root_context,
        });
        self.counters.clear();
        self.events.clear();
        self.dropped_contexts = 0;
        self.dropped_counters = 0;
        self.dropped_events = 0;
    }

    fn counter_add(&mut self, desc: &CounterDesc, context: Context, value: u64, at_micros: u64) {
        let Some(context_id) = self.context_id(context) else {
            self.dropped_counters = self.dropped_counters.saturating_add(1);
            return;
        };

        let key = CounterKey::new(desc.id(), context_id, CounterKind::Counter);
        if let Some(snapshot) = self.counters.get_mut(&key) {
            snapshot.value = snapshot.value.saturating_add(value);
            snapshot.updated_at_micros = at_micros;
            return;
        }

        if self.counters.len() == self.counter_capacity {
            self.dropped_counters = self.dropped_counters.saturating_add(1);
            return;
        }

        self.counters.insert(
            key,
            CounterSnapshot {
                desc_id: desc.id(),
                name: desc.name(),
                context_id,
                kind: CounterKind::Counter,
                value,
                updated_at_micros: at_micros,
            },
        );
    }

    fn gauge_set(&mut self, desc: &CounterDesc, context: Context, value: u64, at_micros: u64) {
        let Some(context_id) = self.context_id(context) else {
            self.dropped_counters = self.dropped_counters.saturating_add(1);
            return;
        };

        let key = CounterKey::new(desc.id(), context_id, CounterKind::Gauge);
        if let Some(snapshot) = self.counters.get_mut(&key) {
            snapshot.value = value;
            snapshot.updated_at_micros = at_micros;
            return;
        }

        if self.counters.len() == self.counter_capacity {
            self.dropped_counters = self.dropped_counters.saturating_add(1);
            return;
        }

        self.counters.insert(
            key,
            CounterSnapshot {
                desc_id: desc.id(),
                name: desc.name(),
                context_id,
                kind: CounterKind::Gauge,
                value,
                updated_at_micros: at_micros,
            },
        );
    }

    fn next_scope_token(&mut self) -> ScopeToken {
        let token = ScopeToken::new(self.next_scope_token);
        self.next_scope_token = self.next_scope_token.saturating_add(1);
        token
    }

    fn push_event(&mut self, observation: Observation, at_micros: u64) {
        if self.events.len() == self.event_capacity {
            self.dropped_events = self.dropped_events.saturating_add(1);
            return;
        }

        self.events.push_back(TimedObservation {
            at_micros,
            observation,
        });
    }

    fn context_id(&mut self, context: Context) -> Option<ContextId> {
        if let Some(context_id) = self.context_ids.get(&context) {
            return Some(*context_id);
        }

        if self.contexts.len() == self.context_capacity {
            self.dropped_contexts = self.dropped_contexts.saturating_add(1);
            return None;
        }

        let context_id = ContextId::new(self.next_context_id);
        self.next_context_id = self.next_context_id.saturating_add(1);
        self.context_ids.insert(context, context_id);
        self.contexts.push(ContextSnapshot {
            id: context_id,
            context,
        });
        Some(context_id)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct CounterKey {
    desc_id: CounterDescId,
    context_id: ContextId,
    kind: CounterKind,
}

impl CounterKey {
    fn new(desc_id: CounterDescId, context_id: ContextId, kind: CounterKind) -> Self {
        Self {
            desc_id,
            context_id,
            kind,
        }
    }
}

static RUNTIME: OnceLock<&'static Runtime> = OnceLock::new();

const BACKEND: Backend = Backend {
    counter_add: backend_counter_add,
    gauge_set: backend_gauge_set,
    scope_begin: backend_scope_begin,
    scope_end: backend_scope_end,
    resource_create: backend_resource_create,
    resource_update: backend_resource_update,
    resource_drop: backend_resource_drop,
    frame_marker: backend_frame_marker,
    flow: backend_flow,
};

fn backend_counter_add(desc: &CounterDesc, context: Context, value: u64) {
    with_runtime(|runtime| {
        let at_micros = runtime.now_micros();
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        state.counter_add(desc, context, value, at_micros);
    });
}

fn backend_gauge_set(desc: &CounterDesc, context: Context, value: u64) {
    with_runtime(|runtime| {
        let at_micros = runtime.now_micros();
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        state.gauge_set(desc, context, value, at_micros);
    });
}

fn backend_scope_begin(desc: &ScopeDesc, context: Context) -> ScopeToken {
    with_runtime(|runtime| {
        let at_micros = runtime.now_micros();
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        let token = state.next_scope_token();
        let Some(context_id) = state.context_id(context) else {
            state.dropped_events = state.dropped_events.saturating_add(1);
            return token;
        };
        state.push_event(
            Observation::ScopeBegin {
                desc_id: desc.id(),
                name: desc.name(),
                context_id,
                token,
            },
            at_micros,
        );
        token
    })
    .unwrap_or_else(|| ScopeToken::new(desc.id().get()))
}

fn backend_scope_end(token: ScopeToken) {
    with_runtime(|runtime| {
        let at_micros = runtime.now_micros();
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        state.push_event(Observation::ScopeEnd { token }, at_micros);
    });
}

fn backend_resource_create(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    with_runtime(|runtime| {
        let at_micros = runtime.now_micros();
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        let Some(context_id) = state.context_id(context) else {
            state.dropped_events = state.dropped_events.saturating_add(1);
            return;
        };
        state.push_event(
            Observation::ResourceCreate {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context_id,
                size,
            },
            at_micros,
        );
    });
}

fn backend_resource_update(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    with_runtime(|runtime| {
        let at_micros = runtime.now_micros();
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        let Some(context_id) = state.context_id(context) else {
            state.dropped_events = state.dropped_events.saturating_add(1);
            return;
        };
        state.push_event(
            Observation::ResourceUpdate {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context_id,
                size,
            },
            at_micros,
        );
    });
}

fn backend_resource_drop(desc: &ResourceKindDesc, resource_id: ResourceId, context: Context) {
    with_runtime(|runtime| {
        let at_micros = runtime.now_micros();
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        let Some(context_id) = state.context_id(context) else {
            state.dropped_events = state.dropped_events.saturating_add(1);
            return;
        };
        state.push_event(
            Observation::ResourceDrop {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context_id,
            },
            at_micros,
        );
    });
}

fn backend_frame_marker(kind: MarkerKind, phase: MarkerPhase, context: Context) {
    with_runtime(|runtime| {
        let at_micros = runtime.now_micros();
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        let Some(context_id) = state.context_id(context) else {
            state.dropped_events = state.dropped_events.saturating_add(1);
            return;
        };
        state.push_event(
            Observation::FrameMarker {
                kind,
                phase,
                context_id,
            },
            at_micros,
        );
    });
}

fn backend_flow(flow_id: FlowId, phase: FlowPhase, context: Context) {
    with_runtime(|runtime| {
        let at_micros = runtime.now_micros();
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        let Some(context_id) = state.context_id(context) else {
            state.dropped_events = state.dropped_events.saturating_add(1);
            return;
        };
        state.push_event(
            Observation::Flow {
                flow_id,
                phase,
                context_id,
            },
            at_micros,
        );
    });
}

fn with_runtime<T>(f: impl FnOnce(&Runtime) -> T) -> Option<T> {
    RUNTIME.get().map(|runtime| f(runtime))
}

fn duration_to_micros(duration: Duration) -> u64 {
    let micros = duration.as_micros();
    u64::try_from(micros).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{CounterKind, Observation, Runtime};
    use spoor_core::{
        Context, ContextId, CounterDesc, CounterDescId, FlowId, FlowPhase, MarkerKind, MarkerPhase,
        ResourceId, ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId,
    };

    #[test]
    fn runtime_collects_context_table_and_timestamped_records() {
        let runtime = Runtime::new(16, 16);
        let counter = CounterDesc::count(CounterDescId::new(1), "ops");
        let resource = ResourceKindDesc::new(ResourceKindId::new(2), "atlas");
        let scope = ScopeDesc::new(ScopeDescId::new(3), "record");
        let context = Context::root().with(spoor_core::LabelKey::Pass, 1_u32);

        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        state.counter_add(&counter, context, 5, 10);
        state.gauge_set(&counter, context, 9, 11);
        let token = state.next_scope_token();
        let context_id = state.context_id(context).expect("context should intern");
        state.push_event(
            Observation::ScopeBegin {
                desc_id: scope.id(),
                name: scope.name(),
                context_id,
                token,
            },
            12,
        );
        state.push_event(Observation::ScopeEnd { token }, 13);
        state.push_event(
            Observation::ResourceCreate {
                kind_id: resource.id(),
                kind_name: resource.name(),
                resource_id: ResourceId::new(7),
                context_id,
                size: 64,
            },
            14,
        );
        state.push_event(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context_id,
            },
            15,
        );
        state.push_event(
            Observation::Flow {
                flow_id: FlowId::new(21),
                phase: FlowPhase::Begin,
                context_id,
            },
            16,
        );
        drop(state);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.metadata.process_name, "spoor");
        assert_eq!(snapshot.metadata.session_sequence, 0);
        assert_eq!(snapshot.contexts[0].id, ContextId::new(0));
        assert_eq!(snapshot.contexts[1].context, context);
        assert_eq!(snapshot.counters.len(), 2);
        assert!(snapshot.counters.iter().any(|counter| {
            counter.kind == CounterKind::Counter
                && counter.value == 5
                && counter.name == "ops"
                && counter.context_id == context_id
        }));
        assert_eq!(snapshot.events.len(), 5);
        assert_eq!(snapshot.events[0].at_micros, 12);
        assert_eq!(
            snapshot.events[0].observation.context_id(),
            Some(context_id)
        );
    }

    #[test]
    fn drain_snapshot_clears_state_and_advances_session_sequence() {
        let mut runtime = Runtime::named("demo", 8, 8);
        runtime.set_session_name("recording");
        let runtime = runtime;

        {
            let mut state = runtime
                .state
                .lock()
                .expect("runtime mutex should not be poisoned");
            let context_id = state
                .context_id(Context::root().with(spoor_core::LabelKey::Frame, 1_u32))
                .expect("context should intern");
            state.push_event(
                Observation::FrameMarker {
                    kind: MarkerKind::Frame,
                    phase: MarkerPhase::Begin,
                    context_id,
                },
                1,
            );
        }

        let drained = runtime.drain_snapshot();
        assert_eq!(drained.metadata.process_name, "demo");
        assert_eq!(drained.metadata.session_name.as_deref(), Some("recording"));
        assert_eq!(drained.metadata.session_sequence, 0);
        assert_eq!(drained.events.len(), 1);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.metadata.session_sequence, 1);
        assert_eq!(snapshot.events.len(), 0);
        assert_eq!(snapshot.contexts.len(), 1);
    }

    #[test]
    fn runtime_enforces_event_capacity() {
        let runtime = Runtime::new(4, 1);
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        let context_id = state
            .context_id(Context::root())
            .expect("root context should exist");
        state.push_event(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context_id,
            },
            1,
        );
        state.push_event(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::End,
                context_id,
            },
            2,
        );
        drop(state);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.dropped_events, 1);
    }
}
