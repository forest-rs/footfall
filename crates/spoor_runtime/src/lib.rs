// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounded runtime collection for Spoor.

use spoor_core::{
    Backend, Context, ContextId, CounterDesc, CounterDescId, FlowId, FlowPhase,
    InstallBackendError, InstrumentationLevel, MarkerKind, MarkerPhase, ResourceId,
    ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId, ScopeToken, install_backend,
};
use std::{
    boxed::Box,
    collections::{HashMap, VecDeque},
    string::String,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const DEFAULT_SHARD_COUNT: usize = 16;

/// Installs a runtime as the process-wide Spoor backend.
pub fn install_global(runtime: &'static Runtime) -> Result<(), InstallError> {
    register_global(runtime)?;
    install_backend(BACKEND).map_err(InstallError::BackendAlreadyInstalled)
}

/// Registers a runtime instance for backend use without installing it into `spoor_core`.
pub fn register_global(runtime: &'static Runtime) -> Result<(), InstallError> {
    RUNTIME
        .set(runtime)
        .map_err(|_| InstallError::RuntimeAlreadyInstalled)
}

/// Returns the runtime backend function table for fan-out composition.
#[must_use]
pub const fn backend() -> Backend {
    BACKEND
}

/// In-process bounded collector for counters and timestamped observation records.
#[derive(Debug)]
pub struct Runtime {
    created_at: Instant,
    process_name: String,
    level: AtomicU8,
    counter_capacity: usize,
    event_capacity: usize,
    next_scope_token: AtomicU64,
    counter_count: AtomicUsize,
    event_count: AtomicUsize,
    dropped_counters: AtomicU64,
    dropped_events: AtomicU64,
    session: Mutex<SessionState>,
    contexts: Mutex<ContextState>,
    counter_shards: Vec<Mutex<CounterShard>>,
    event_shards: Vec<Mutex<EventShard>>,
}

impl Runtime {
    /// Creates a runtime with explicit counter-key and event capacities.
    #[must_use]
    pub fn new(counter_capacity: usize, event_capacity: usize) -> Self {
        Self::named("spoor", counter_capacity, event_capacity)
    }

    /// Creates a runtime with explicit capacities and capture level.
    #[must_use]
    pub fn with_level(
        counter_capacity: usize,
        event_capacity: usize,
        level: InstrumentationLevel,
    ) -> Self {
        Self::named_with_level("spoor", counter_capacity, event_capacity, level)
    }

    /// Creates a runtime with an explicit process name.
    #[must_use]
    pub fn named(
        process_name: impl Into<String>,
        counter_capacity: usize,
        event_capacity: usize,
    ) -> Self {
        Self::named_with_level(
            process_name,
            counter_capacity,
            event_capacity,
            InstrumentationLevel::Trace,
        )
    }

    /// Creates a runtime with explicit process name, capacities, and capture level.
    #[must_use]
    pub fn named_with_level(
        process_name: impl Into<String>,
        counter_capacity: usize,
        event_capacity: usize,
        level: InstrumentationLevel,
    ) -> Self {
        Self {
            created_at: Instant::now(),
            process_name: process_name.into(),
            level: AtomicU8::new(level_to_u8(level)),
            counter_capacity,
            event_capacity,
            next_scope_token: AtomicU64::new(1),
            counter_count: AtomicUsize::new(0),
            event_count: AtomicUsize::new(0),
            dropped_counters: AtomicU64::new(0),
            dropped_events: AtomicU64::new(0),
            session: Mutex::new(SessionState::default()),
            contexts: Mutex::new(ContextState::new(context_capacity(
                counter_capacity,
                event_capacity,
            ))),
            counter_shards: build_counter_shards(DEFAULT_SHARD_COUNT),
            event_shards: build_event_shards(DEFAULT_SHARD_COUNT),
        }
    }

    /// Adds a capture-session label and returns the runtime.
    #[must_use]
    pub fn with_session_name(self, session_name: impl Into<String>) -> Self {
        self.set_session_name(session_name);
        self
    }

    /// Creates and installs a process-wide runtime with explicit capacities.
    pub fn install_global_with_capacity(
        counter_capacity: usize,
        event_capacity: usize,
    ) -> Result<&'static Self, InstallError> {
        Self::new(counter_capacity, event_capacity).install_global()
    }

    /// Creates and installs a named process-wide runtime.
    pub fn install_global_named(
        process_name: impl Into<String>,
        counter_capacity: usize,
        event_capacity: usize,
    ) -> Result<&'static Self, InstallError> {
        Self::named(process_name, counter_capacity, event_capacity).install_global()
    }

    /// Creates and installs a named process-wide runtime with an explicit capture level.
    pub fn install_global_named_with_level(
        process_name: impl Into<String>,
        counter_capacity: usize,
        event_capacity: usize,
        level: InstrumentationLevel,
    ) -> Result<&'static Self, InstallError> {
        Self::named_with_level(process_name, counter_capacity, event_capacity, level)
            .install_global()
    }

    /// Installs this runtime as the process-wide backend and keeps it alive for the process.
    pub fn install_global(self) -> Result<&'static Self, InstallError> {
        let runtime = Box::leak(Box::new(self));
        install_global(runtime)?;
        Ok(runtime)
    }

    /// Returns the current runtime capture level.
    #[must_use]
    pub fn level(&self) -> InstrumentationLevel {
        level_from_u8(self.level.load(Ordering::Relaxed))
    }

    /// Sets the runtime capture level used by subsequent backend emissions.
    pub fn set_level(&self, level: InstrumentationLevel) {
        self.level.store(level_to_u8(level), Ordering::Relaxed);
    }

    /// Sets an optional capture-session label for future snapshots.
    ///
    /// The label persists across session resets until cleared, and can be
    /// changed after the runtime is installed as the process-wide backend.
    pub fn set_session_name(&self, session_name: impl Into<String>) {
        self.session
            .lock()
            .expect("runtime session mutex should not be poisoned")
            .session_name = Some(session_name.into());
    }

    /// Clears the capture-session label for future snapshots.
    pub fn clear_session_name(&self) {
        self.session
            .lock()
            .expect("runtime session mutex should not be poisoned")
            .session_name = None;
    }

    /// Returns a snapshot of the currently collected counters, contexts, and events.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let captured_at_micros = self.now_micros();
        self.snapshot_at(captured_at_micros)
    }

    /// Drains the current capture state into a snapshot and starts a fresh session.
    #[must_use]
    pub fn drain_snapshot(&self) -> Snapshot {
        let captured_at_micros = self.now_micros();
        let snapshot = self.snapshot_at(captured_at_micros);
        self.reset_at(captured_at_micros);
        snapshot
    }

    /// Clears the current capture state and starts a fresh session.
    pub fn reset(&self) {
        let captured_at_micros = self.now_micros();
        self.reset_at(captured_at_micros);
    }

    fn metadata(&self, session: &SessionState) -> SnapshotMetadata {
        SnapshotMetadata {
            process_name: self.process_name.clone(),
            session_name: session.session_name.clone(),
            session_sequence: session.session_sequence,
        }
    }

    fn now_micros(&self) -> u64 {
        duration_to_micros(self.created_at.elapsed())
    }

    fn snapshot_at(&self, captured_at_micros: u64) -> Snapshot {
        let session = self
            .session
            .lock()
            .expect("runtime session mutex should not be poisoned");
        let contexts = self
            .contexts
            .lock()
            .expect("runtime context mutex should not be poisoned");
        let mut counters = Vec::with_capacity(self.counter_count.load(Ordering::Relaxed));
        for shard in &self.counter_shards {
            counters.extend(
                shard
                    .lock()
                    .expect("runtime counter shard should not be poisoned")
                    .counters
                    .values()
                    .cloned(),
            );
        }

        let mut events = Vec::with_capacity(self.event_count.load(Ordering::Relaxed));
        for shard in &self.event_shards {
            events.extend(
                shard
                    .lock()
                    .expect("runtime event shard should not be poisoned")
                    .events
                    .iter()
                    .cloned(),
            );
        }
        events.sort_by_key(event_sort_key);

        Snapshot {
            metadata: self.metadata(&session),
            capture_start_micros: session.capture_start_micros,
            captured_at_micros,
            contexts: contexts.contexts.clone(),
            counters,
            events,
            dropped_contexts: contexts.dropped_contexts,
            dropped_counters: self.dropped_counters.load(Ordering::Relaxed),
            dropped_events: self.dropped_events.load(Ordering::Relaxed),
        }
    }

    fn reset_at(&self, capture_start_micros: u64) {
        let mut session = self
            .session
            .lock()
            .expect("runtime session mutex should not be poisoned");
        session.capture_start_micros = capture_start_micros;
        session.session_sequence = session.session_sequence.saturating_add(1);

        // Scope tokens stay monotonic across session resets so a scope that
        // straddles a drain cannot collide with tokens issued afterwards.
        self.counter_count.store(0, Ordering::Relaxed);
        self.event_count.store(0, Ordering::Relaxed);
        self.dropped_counters.store(0, Ordering::Relaxed);
        self.dropped_events.store(0, Ordering::Relaxed);

        self.contexts
            .lock()
            .expect("runtime context mutex should not be poisoned")
            .reset();
        for shard in &self.counter_shards {
            shard
                .lock()
                .expect("runtime counter shard should not be poisoned")
                .counters
                .clear();
        }
        for shard in &self.event_shards {
            shard
                .lock()
                .expect("runtime event shard should not be poisoned")
                .events
                .clear();
        }
    }

    fn counter_add_at(&self, desc: &CounterDesc, context: Context, value: u64, at_micros: u64) {
        self.counter_update_at(desc, context, value, at_micros, CounterKind::Counter);
    }

    fn gauge_set_at(&self, desc: &CounterDesc, context: Context, value: u64, at_micros: u64) {
        self.counter_update_at(desc, context, value, at_micros, CounterKind::Gauge);
    }

    fn counter_update_at(
        &self,
        desc: &CounterDesc,
        context: Context,
        value: u64,
        at_micros: u64,
        kind: CounterKind,
    ) {
        let Some(context_id) = self.context_id(context) else {
            self.dropped_counters.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let key = CounterKey::new(desc.id(), context_id, kind);
        let shard_index = self.counter_shard_index(key);
        let mut shard = self.counter_shards[shard_index]
            .lock()
            .expect("runtime counter shard should not be poisoned");
        if let Some(snapshot) = shard.counters.get_mut(&key) {
            snapshot.value = match kind {
                CounterKind::Counter => snapshot.value.saturating_add(value),
                CounterKind::Gauge => value,
            };
            snapshot.updated_at_micros = at_micros;
            return;
        }

        if !self.reserve_counter_slot() {
            self.dropped_counters.fetch_add(1, Ordering::Relaxed);
            return;
        }

        shard.counters.insert(
            key,
            CounterSnapshot {
                desc_id: desc.id(),
                name: desc.name(),
                context_id,
                kind,
                value,
                updated_at_micros: at_micros,
            },
        );
    }

    fn next_scope_token(&self) -> ScopeToken {
        ScopeToken::new(self.next_scope_token.fetch_add(1, Ordering::Relaxed))
    }

    fn push_event_at(&self, observation: Observation, at_micros: u64) {
        if !self.reserve_event_slot() {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let shard_index = self.event_shard_index(&observation);
        self.event_shards[shard_index]
            .lock()
            .expect("runtime event shard should not be poisoned")
            .events
            .push_back(TimedObservation {
                at_micros,
                observation,
            });
    }

    fn context_id(&self, context: Context) -> Option<ContextId> {
        self.contexts
            .lock()
            .expect("runtime context mutex should not be poisoned")
            .context_id(context)
    }

    fn reserve_counter_slot(&self) -> bool {
        reserve_slot(&self.counter_count, self.counter_capacity)
    }

    fn reserve_event_slot(&self) -> bool {
        reserve_slot(&self.event_count, self.event_capacity)
    }

    fn counter_shard_index(&self, key: CounterKey) -> usize {
        shard_index(
            key.desc_id
                .get()
                .wrapping_mul(31)
                .wrapping_add(key.context_id.get())
                .wrapping_add(counter_kind_discriminant(key.kind)),
            self.counter_shards.len(),
        )
    }

    fn event_shard_index(&self, observation: &Observation) -> usize {
        let raw = observation
            .context_id()
            .map_or_else(|| scope_token_for_observation(observation), ContextId::get);
        shard_index(raw, self.event_shards.len())
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

#[derive(Clone, Debug, Default)]
struct SessionState {
    capture_start_micros: u64,
    session_sequence: u64,
    session_name: Option<String>,
}

#[derive(Debug)]
struct ContextState {
    capacity: usize,
    next_context_id: u64,
    context_ids: HashMap<Context, ContextId>,
    contexts: Vec<ContextSnapshot>,
    dropped_contexts: u64,
}

impl ContextState {
    fn new(capacity: usize) -> Self {
        let root_id = ContextId::new(0);
        let root_context = Context::root();
        let mut context_ids = HashMap::with_capacity(capacity);
        context_ids.insert(root_context, root_id);

        Self {
            capacity,
            next_context_id: 1,
            context_ids,
            contexts: vec![ContextSnapshot {
                id: root_id,
                context: root_context,
            }],
            dropped_contexts: 0,
        }
    }

    fn reset(&mut self) {
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
        self.dropped_contexts = 0;
    }

    fn context_id(&mut self, context: Context) -> Option<ContextId> {
        if let Some(context_id) = self.context_ids.get(&context) {
            return Some(*context_id);
        }

        if self.contexts.len() == self.capacity {
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

#[derive(Debug, Default)]
struct CounterShard {
    counters: HashMap<CounterKey, CounterSnapshot>,
}

#[derive(Debug, Default)]
struct EventShard {
    events: VecDeque<TimedObservation>,
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
        if !runtime.level().captures_counters() {
            return;
        }
        let at_micros = runtime.now_micros();
        runtime.counter_add_at(desc, context, value, at_micros);
    });
}

fn backend_gauge_set(desc: &CounterDesc, context: Context, value: u64) {
    with_runtime(|runtime| {
        if !runtime.level().captures_counters() {
            return;
        }
        let at_micros = runtime.now_micros();
        runtime.gauge_set_at(desc, context, value, at_micros);
    });
}

fn backend_scope_begin(desc: &ScopeDesc, context: Context) -> ScopeToken {
    with_runtime(|runtime| {
        if !runtime.level().captures_scopes() {
            return ScopeToken::new(desc.id().get());
        }
        let at_micros = runtime.now_micros();
        let token = runtime.next_scope_token();
        let Some(context_id) = runtime.context_id(context) else {
            runtime.dropped_events.fetch_add(1, Ordering::Relaxed);
            return token;
        };
        runtime.push_event_at(
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
        if !runtime.level().captures_scopes() {
            return;
        }
        let at_micros = runtime.now_micros();
        runtime.push_event_at(Observation::ScopeEnd { token }, at_micros);
    });
}

fn backend_resource_create(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    with_runtime(|runtime| {
        if !runtime.level().captures_resources() {
            return;
        }
        let at_micros = runtime.now_micros();
        let Some(context_id) = runtime.context_id(context) else {
            runtime.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        runtime.push_event_at(
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
        if !runtime.level().captures_resources() {
            return;
        }
        let at_micros = runtime.now_micros();
        let Some(context_id) = runtime.context_id(context) else {
            runtime.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        runtime.push_event_at(
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
        if !runtime.level().captures_resources() {
            return;
        }
        let at_micros = runtime.now_micros();
        let Some(context_id) = runtime.context_id(context) else {
            runtime.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        runtime.push_event_at(
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
        if !runtime.level().captures_markers() {
            return;
        }
        let at_micros = runtime.now_micros();
        let Some(context_id) = runtime.context_id(context) else {
            runtime.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        runtime.push_event_at(
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
        if !runtime.level().captures_flows() {
            return;
        }
        let at_micros = runtime.now_micros();
        let Some(context_id) = runtime.context_id(context) else {
            runtime.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        runtime.push_event_at(
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

fn build_counter_shards(count: usize) -> Vec<Mutex<CounterShard>> {
    (0..count.max(1))
        .map(|_| Mutex::new(CounterShard::default()))
        .collect()
}

fn build_event_shards(count: usize) -> Vec<Mutex<EventShard>> {
    (0..count.max(1))
        .map(|_| Mutex::new(EventShard::default()))
        .collect()
}

const fn context_capacity(counter_capacity: usize, event_capacity: usize) -> usize {
    counter_capacity
        .saturating_add(event_capacity)
        .saturating_add(1)
}

fn reserve_slot(count: &AtomicUsize, capacity: usize) -> bool {
    let previous = count.fetch_add(1, Ordering::Relaxed);
    if previous < capacity {
        true
    } else {
        count.fetch_sub(1, Ordering::Relaxed);
        false
    }
}

fn shard_index(raw: u64, shard_count: usize) -> usize {
    let mixed = raw ^ raw.rotate_left(17) ^ raw.rotate_right(11);
    usize::try_from(mixed).unwrap_or(0) % shard_count.max(1)
}

const fn counter_kind_discriminant(kind: CounterKind) -> u64 {
    match kind {
        CounterKind::Counter => 0,
        CounterKind::Gauge => 1,
    }
}

fn scope_token_for_observation(observation: &Observation) -> u64 {
    match observation {
        Observation::ScopeEnd { token } => token.get(),
        Observation::ScopeBegin { token, .. } => token.get(),
        Observation::ResourceCreate { resource_id, .. }
        | Observation::ResourceUpdate { resource_id, .. }
        | Observation::ResourceDrop { resource_id, .. } => resource_id.get(),
        Observation::FrameMarker { .. } | Observation::Flow { .. } => 0,
    }
}

fn event_sort_key(event: &TimedObservation) -> (u64, u8) {
    (event.at_micros, observation_order(&event.observation))
}

const fn observation_order(observation: &Observation) -> u8 {
    match observation {
        Observation::ScopeBegin { .. } => 0,
        Observation::ResourceCreate { .. } => 1,
        Observation::ResourceUpdate { .. } => 2,
        Observation::FrameMarker { .. } => 3,
        Observation::Flow { .. } => 4,
        Observation::ResourceDrop { .. } => 5,
        Observation::ScopeEnd { .. } => 6,
    }
}

fn duration_to_micros(duration: Duration) -> u64 {
    let micros = duration.as_micros();
    u64::try_from(micros).unwrap_or(u64::MAX)
}

const fn level_to_u8(level: InstrumentationLevel) -> u8 {
    match level {
        InstrumentationLevel::Off => 0,
        InstrumentationLevel::Counters => 1,
        InstrumentationLevel::Scoped => 2,
        InstrumentationLevel::Trace => 3,
    }
}

const fn level_from_u8(level: u8) -> InstrumentationLevel {
    match level {
        0 => InstrumentationLevel::Off,
        1 => InstrumentationLevel::Counters,
        2 => InstrumentationLevel::Scoped,
        _ => InstrumentationLevel::Trace,
    }
}

#[cfg(test)]
mod tests {
    use super::{CounterKind, Observation, Runtime};
    use spoor_core::{
        Context, ContextId, CounterDesc, CounterDescId, FlowId, FlowPhase, InstrumentationLevel,
        MarkerKind, MarkerPhase, ResourceId, ResourceKindDesc, ResourceKindId, ScopeDesc,
        ScopeDescId,
    };

    #[test]
    fn runtime_collects_context_table_and_timestamped_records() {
        let runtime = Runtime::new(16, 16);
        let counter = CounterDesc::count(CounterDescId::new(1), "ops");
        let resource = ResourceKindDesc::new(ResourceKindId::new(2), "atlas");
        let scope = ScopeDesc::new(ScopeDescId::new(3), "record");
        let context = Context::root().with(spoor_core::LabelKey::Pass, 1_u32);

        runtime.counter_add_at(&counter, context, 5, 10);
        runtime.gauge_set_at(&counter, context, 9, 11);
        let token = runtime.next_scope_token();
        let context_id = runtime.context_id(context).expect("context should intern");
        runtime.push_event_at(
            Observation::ScopeBegin {
                desc_id: scope.id(),
                name: scope.name(),
                context_id,
                token,
            },
            12,
        );
        runtime.push_event_at(Observation::ScopeEnd { token }, 13);
        runtime.push_event_at(
            Observation::ResourceCreate {
                kind_id: resource.id(),
                kind_name: resource.name(),
                resource_id: ResourceId::new(7),
                context_id,
                size: 64,
            },
            14,
        );
        runtime.push_event_at(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context_id,
            },
            15,
        );
        runtime.push_event_at(
            Observation::Flow {
                flow_id: FlowId::new(21),
                phase: FlowPhase::Begin,
                context_id,
            },
            16,
        );

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
        let runtime = Runtime::named("demo", 8, 8);
        runtime.set_session_name("recording");

        let context_id = runtime
            .context_id(Context::root().with(spoor_core::LabelKey::Frame, 1_u32))
            .expect("context should intern");
        runtime.push_event_at(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context_id,
            },
            1,
        );

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
    fn scope_tokens_stay_unique_across_session_resets() {
        let runtime = Runtime::new(8, 8);
        let before_reset = runtime.next_scope_token();
        let _ = runtime.drain_snapshot();
        let after_reset = runtime.next_scope_token();
        assert_ne!(
            before_reset, after_reset,
            "a scope straddling a drain must not collide with new tokens"
        );
        assert!(after_reset.get() > before_reset.get());
    }

    #[test]
    fn session_name_changes_through_shared_reference() {
        let runtime = Runtime::named("demo", 8, 8).with_session_name("first");
        let shared: &Runtime = &runtime;

        assert_eq!(
            shared.snapshot().metadata.session_name.as_deref(),
            Some("first")
        );

        shared.set_session_name("second");
        let drained = shared.drain_snapshot();
        assert_eq!(drained.metadata.session_name.as_deref(), Some("second"));

        // The label persists across the session reset until cleared.
        assert_eq!(
            shared.snapshot().metadata.session_name.as_deref(),
            Some("second")
        );
        shared.clear_session_name();
        assert_eq!(shared.snapshot().metadata.session_name, None);
    }

    #[test]
    fn runtime_enforces_event_capacity() {
        let runtime = Runtime::new(4, 1);
        let context_id = runtime
            .context_id(Context::root())
            .expect("root context should exist");
        runtime.push_event_at(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context_id,
            },
            1,
        );
        runtime.push_event_at(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::End,
                context_id,
            },
            2,
        );

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.dropped_events, 1);
    }

    #[test]
    fn runtime_enforces_counter_capacity_across_shards() {
        let runtime = Runtime::new(1, 4);
        let context = Context::root().with(spoor_core::LabelKey::Frame, 1_u32);
        let first = CounterDesc::count(CounterDescId::new(31), "first");
        let second = CounterDesc::count(CounterDescId::new(32), "second");

        runtime.counter_add_at(&first, context, 1, 1);
        runtime.counter_add_at(&second, context, 1, 2);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.dropped_counters, 1);
    }

    #[test]
    fn backend_respects_runtime_capture_level() {
        static COUNTER: CounterDesc = CounterDesc::count(CounterDescId::new(11), "ops");
        static SCOPE: ScopeDesc = ScopeDesc::new(ScopeDescId::new(12), "record");
        static RESOURCE: ResourceKindDesc = ResourceKindDesc::new(ResourceKindId::new(13), "atlas");

        let runtime = Box::leak(Box::new(Runtime::with_level(
            16,
            16,
            InstrumentationLevel::Counters,
        )));
        super::RUNTIME
            .set(runtime)
            .expect("runtime backend test should install only once");

        let context = Context::root().with(spoor_core::LabelKey::Frame, 3_u32);
        super::backend_counter_add(&COUNTER, context, 1);
        let token = super::backend_scope_begin(&SCOPE, context);
        super::backend_scope_end(token);
        super::backend_resource_create(&RESOURCE, ResourceId::new(7), context, 64);
        super::backend_frame_marker(MarkerKind::Frame, MarkerPhase::Begin, context);
        super::backend_flow(FlowId::new(9), FlowPhase::Begin, context);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.events.len(), 0);

        runtime.set_level(InstrumentationLevel::Trace);
        runtime.reset();
        super::backend_counter_add(&COUNTER, context, 1);
        let token = super::backend_scope_begin(&SCOPE, context);
        super::backend_scope_end(token);
        super::backend_resource_create(&RESOURCE, ResourceId::new(7), context, 64);
        super::backend_frame_marker(MarkerKind::Frame, MarkerPhase::Begin, context);
        super::backend_flow(FlowId::new(9), FlowPhase::Begin, context);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.events.len(), 5);
    }
}
