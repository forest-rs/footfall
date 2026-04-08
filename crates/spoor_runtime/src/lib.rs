// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounded runtime collection for Spoor.

use spoor_core::{
    Backend, Context, CounterDesc, CounterDescId, InstallBackendError, MarkerKind, MarkerPhase,
    ResourceId, ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId, ScopeToken,
    install_backend,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, OnceLock},
    time::Instant,
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
    state: Mutex<State>,
}

impl Runtime {
    /// Creates a runtime with explicit counter-key and event capacities.
    #[must_use]
    pub fn new(counter_capacity: usize, event_capacity: usize) -> Self {
        Self {
            created_at: Instant::now(),
            state: Mutex::new(State::new(counter_capacity, event_capacity)),
        }
    }

    /// Returns a snapshot of the currently collected counters and events.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let state = self
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        Snapshot {
            captured_at_micros: self.now_micros(),
            counters: state.counters.values().cloned().collect(),
            events: state.events.iter().cloned().collect(),
            dropped_counters: state.dropped_counters,
            dropped_events: state.dropped_events,
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

/// Snapshot of counters and events retained by a [`Runtime`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    /// Time since runtime creation when the snapshot was captured.
    pub captured_at_micros: u64,
    /// Aggregated counters and gauges.
    pub counters: Vec<CounterSnapshot>,
    /// Bounded sequence of timestamped observation records.
    pub events: Vec<TimedObservation>,
    /// Count of counter updates dropped due to counter-key capacity pressure.
    pub dropped_counters: u64,
    /// Count of events dropped due to event-capacity pressure.
    pub dropped_events: u64,
}

/// One aggregated counter or gauge value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterSnapshot {
    /// Counter descriptor id.
    pub desc_id: CounterDescId,
    /// Stable descriptor name.
    pub name: &'static str,
    /// Semantic context for this aggregate.
    pub context: Context,
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
        /// Explicit semantic context.
        context: Context,
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
        /// Semantic context.
        context: Context,
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
        /// Semantic context.
        context: Context,
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
        /// Semantic context.
        context: Context,
    },
    /// Frame, transaction, or pass marker.
    FrameMarker {
        /// Marker kind.
        kind: MarkerKind,
        /// Marker phase.
        phase: MarkerPhase,
        /// Semantic context.
        context: Context,
    },
}

impl Observation {
    /// Returns the semantic context carried by this observation when present.
    #[must_use]
    pub fn context(&self) -> Option<Context> {
        match self {
            Self::ScopeBegin { context, .. }
            | Self::ResourceCreate { context, .. }
            | Self::ResourceUpdate { context, .. }
            | Self::ResourceDrop { context, .. }
            | Self::FrameMarker { context, .. } => Some(*context),
            Self::ScopeEnd { .. } => None,
        }
    }
}

#[derive(Debug)]
struct State {
    counter_capacity: usize,
    event_capacity: usize,
    next_scope_token: u64,
    counters: HashMap<CounterKey, CounterSnapshot>,
    events: VecDeque<TimedObservation>,
    dropped_counters: u64,
    dropped_events: u64,
}

impl State {
    fn new(counter_capacity: usize, event_capacity: usize) -> Self {
        Self {
            counter_capacity,
            event_capacity,
            next_scope_token: 1,
            counters: HashMap::with_capacity(counter_capacity),
            events: VecDeque::with_capacity(event_capacity),
            dropped_counters: 0,
            dropped_events: 0,
        }
    }

    fn counter_add(&mut self, desc: &CounterDesc, context: Context, value: u64, at_micros: u64) {
        let key = CounterKey::new(desc.id(), context, CounterKind::Counter);
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
                context,
                kind: CounterKind::Counter,
                value,
                updated_at_micros: at_micros,
            },
        );
    }

    fn gauge_set(&mut self, desc: &CounterDesc, context: Context, value: u64, at_micros: u64) {
        let key = CounterKey::new(desc.id(), context, CounterKind::Gauge);
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
                context,
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
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct CounterKey {
    desc_id: CounterDescId,
    context: Context,
    kind: CounterKind,
}

impl CounterKey {
    fn new(desc_id: CounterDescId, context: Context, kind: CounterKind) -> Self {
        Self {
            desc_id,
            context,
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
        state.push_event(
            Observation::ScopeBegin {
                desc_id: desc.id(),
                name: desc.name(),
                context,
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
        state.push_event(
            Observation::ResourceCreate {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context,
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
        state.push_event(
            Observation::ResourceUpdate {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context,
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
        state.push_event(
            Observation::ResourceDrop {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context,
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
        state.push_event(
            Observation::FrameMarker {
                kind,
                phase,
                context,
            },
            at_micros,
        );
    });
}

fn with_runtime<T>(f: impl FnOnce(&Runtime) -> T) -> Option<T> {
    RUNTIME.get().map(|runtime| f(runtime))
}

fn duration_to_micros(duration: std::time::Duration) -> u64 {
    let micros = duration.as_micros();
    u64::try_from(micros).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{CounterKind, Observation, Runtime};
    use spoor_core::{
        Context, CounterDesc, CounterDescId, MarkerKind, MarkerPhase, ResourceId, ResourceKindDesc,
        ResourceKindId, ScopeDesc, ScopeDescId,
    };

    #[test]
    fn runtime_collects_named_timestamped_counters_and_events() {
        let runtime = Runtime::new(16, 16);
        let counter = CounterDesc::count(CounterDescId::new(1), "ops");
        let resource = ResourceKindDesc::new(ResourceKindId::new(2), "atlas");
        let scope = ScopeDesc::new(ScopeDescId::new(3), "record");
        let context = Context::root();

        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        state.counter_add(&counter, context, 5, 10);
        state.gauge_set(&counter, context, 9, 11);
        let token = state.next_scope_token();
        state.push_event(
            Observation::ScopeBegin {
                desc_id: scope.id(),
                name: scope.name(),
                context,
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
                context,
                size: 64,
            },
            14,
        );
        state.push_event(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context,
            },
            15,
        );
        drop(state);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 2);
        assert!(
            snapshot
                .counters
                .iter()
                .any(|counter| counter.kind == CounterKind::Counter
                    && counter.value == 5
                    && counter.name == "ops"
                    && counter.updated_at_micros == 10)
        );
        assert_eq!(snapshot.events.len(), 4);
        assert_eq!(snapshot.events[0].at_micros, 12);
    }

    #[test]
    fn runtime_enforces_event_capacity() {
        let runtime = Runtime::new(4, 1);
        let mut state = runtime
            .state
            .lock()
            .expect("runtime mutex should not be poisoned");
        state.push_event(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context: Context::root(),
            },
            1,
        );
        state.push_event(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::End,
                context: Context::root(),
            },
            2,
        );
        drop(state);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.dropped_events, 1);
    }
}
