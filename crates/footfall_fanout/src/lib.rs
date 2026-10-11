// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Optional fan-out backend adapter for Footfall.
//!
//! `footfall_core` intentionally installs one process-wide backend. This crate keeps that seam intact
//! while letting two already-registered backends observe the same emissions.
//!
//! Phase 1 keeps the shape narrow on purpose: one primary backend and one secondary backend.
//! That covers the practical case of bounded runtime capture plus a live Tracy session without
//! forcing extra indirection into `footfall_core`.

use footfall_core::{
    Args, Backend, Context, CounterDesc, EventDesc, FlowId, FlowPhase, InstallBackendError,
    MarkerKind, MarkerPhase, ResourceId, ResourceKindDesc, ScopeDesc, ScopeToken, install_backend,
};
use std::{
    collections::HashMap,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

/// Installs a fan-out adapter as the process-wide Footfall backend.
pub fn install_global(fanout: &'static Fanout) -> Result<(), InstallError> {
    FANOUT
        .set(fanout)
        .map_err(|_| InstallError::FanoutAlreadyInstalled)?;
    install_backend(BACKEND).map_err(InstallError::BackendAlreadyInstalled)
}

/// Error returned when fan-out installation fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallError {
    /// A fan-out backend has already been installed.
    FanoutAlreadyInstalled,
    /// Another Footfall backend has already been installed.
    BackendAlreadyInstalled(InstallBackendError),
}

/// Pairwise backend fan-out state.
#[derive(Debug)]
pub struct Fanout {
    primary: Backend,
    secondary: Backend,
    next_scope_token: AtomicU64,
    scopes: Mutex<HashMap<u64, ScopePair>>,
}

impl Fanout {
    /// Creates a fan-out adapter over two already-registered backends.
    #[must_use]
    pub fn new(primary: Backend, secondary: Backend) -> Self {
        Self {
            primary,
            secondary,
            next_scope_token: AtomicU64::new(1),
            scopes: Mutex::new(HashMap::new()),
        }
    }

    fn begin_scope(&self, desc: &ScopeDesc, context: Context, args: Args) -> ScopeToken {
        let primary = (self.primary.scope_begin)(desc, context, args);
        let secondary = (self.secondary.scope_begin)(desc, context, args);
        let token = ScopeToken::new(self.next_scope_token.fetch_add(1, Ordering::Relaxed));
        self.scopes
            .lock()
            .expect("fanout scope mutex should not be poisoned")
            .insert(token.get(), ScopePair { primary, secondary });
        token
    }

    fn end_scope(&self, token: ScopeToken) {
        if !token.is_valid() {
            return;
        }
        let pair = self
            .scopes
            .lock()
            .expect("fanout scope mutex should not be poisoned")
            .remove(&token.get());
        if let Some(pair) = pair {
            (self.secondary.scope_end)(pair.secondary);
            (self.primary.scope_end)(pair.primary);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ScopePair {
    primary: ScopeToken,
    secondary: ScopeToken,
}

static FANOUT: OnceLock<&'static Fanout> = OnceLock::new();

const BACKEND: Backend = Backend {
    counter_add: backend_counter_add,
    gauge_set: backend_gauge_set,
    histogram_record: backend_histogram_record,
    scope_begin: backend_scope_begin,
    scope_end: backend_scope_end,
    resource_create: backend_resource_create,
    resource_update: backend_resource_update,
    resource_drop: backend_resource_drop,
    frame_marker: backend_frame_marker,
    flow: backend_flow,
    message: backend_message,
};

fn backend_counter_add(desc: &CounterDesc, context: Context, value: u64) {
    let _ = with_fanout(|fanout| {
        (fanout.primary.counter_add)(desc, context, value);
        (fanout.secondary.counter_add)(desc, context, value);
    });
}

fn backend_histogram_record(desc: &CounterDesc, context: Context, value: u64) {
    let _ = with_fanout(|fanout| {
        (fanout.primary.histogram_record)(desc, context, value);
        (fanout.secondary.histogram_record)(desc, context, value);
    });
}

fn backend_gauge_set(desc: &CounterDesc, context: Context, value: u64) {
    let _ = with_fanout(|fanout| {
        (fanout.primary.gauge_set)(desc, context, value);
        (fanout.secondary.gauge_set)(desc, context, value);
    });
}

fn backend_scope_begin(desc: &ScopeDesc, context: Context, args: Args) -> ScopeToken {
    with_fanout(|fanout| fanout.begin_scope(desc, context, args)).unwrap_or_default()
}

fn backend_scope_end(token: ScopeToken) {
    let _ = with_fanout(|fanout| fanout.end_scope(token));
}

fn backend_resource_create(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    let _ = with_fanout(|fanout| {
        (fanout.primary.resource_create)(desc, resource_id, context, size);
        (fanout.secondary.resource_create)(desc, resource_id, context, size);
    });
}

fn backend_resource_update(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    let _ = with_fanout(|fanout| {
        (fanout.primary.resource_update)(desc, resource_id, context, size);
        (fanout.secondary.resource_update)(desc, resource_id, context, size);
    });
}

fn backend_resource_drop(desc: &ResourceKindDesc, resource_id: ResourceId, context: Context) {
    let _ = with_fanout(|fanout| {
        (fanout.primary.resource_drop)(desc, resource_id, context);
        (fanout.secondary.resource_drop)(desc, resource_id, context);
    });
}

fn backend_frame_marker(kind: MarkerKind, phase: MarkerPhase, context: Context) {
    let _ = with_fanout(|fanout| {
        (fanout.primary.frame_marker)(kind, phase, context);
        (fanout.secondary.frame_marker)(kind, phase, context);
    });
}

fn backend_message(desc: &EventDesc, context: Context, text: &str, args: Args) {
    let _ = with_fanout(|fanout| {
        (fanout.primary.message)(desc, context, text, args);
        (fanout.secondary.message)(desc, context, text, args);
    });
}

fn backend_flow(flow_id: FlowId, phase: FlowPhase, context: Context) {
    let _ = with_fanout(|fanout| {
        (fanout.primary.flow)(flow_id, phase, context);
        (fanout.secondary.flow)(flow_id, phase, context);
    });
}

fn with_fanout<T>(f: impl FnOnce(&Fanout) -> T) -> Option<T> {
    FANOUT.get().map(|fanout| f(fanout))
}

#[cfg(test)]
mod tests {
    use super::Fanout;
    use footfall_core::{
        Backend, Context, CounterDesc, CounterDescId, LabelKey, MarkerKind, MarkerPhase, ScopeDesc,
        ScopeDescId, ScopeToken,
    };
    use std::sync::{Mutex, OnceLock};

    static COUNTER: CounterDesc = CounterDesc::count(CounterDescId::new(1), "ops");
    static SCOPE: ScopeDesc = ScopeDesc::new(ScopeDescId::new(2), "record");
    static LOG: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();

    #[test]
    fn fanout_ignores_invalid_scope_token() {
        let fanout = Fanout::new(Backend::NOOP, Backend::NOOP);
        let token = fanout.begin_scope(&SCOPE, Context::root(), footfall_core::Args::none());
        assert!(token.is_valid(), "fanout tokens should start at 1");

        fanout.end_scope(ScopeToken::INVALID);
        assert_eq!(
            fanout
                .scopes
                .lock()
                .expect("fanout scope mutex should not be poisoned")
                .len(),
            1,
            "ending the invalid token must not touch open scopes"
        );

        fanout.end_scope(token);
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
        LOG.get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .expect("log mutex should not be poisoned")
            .clear();

        let fanout = Box::leak(Box::new(Fanout::new(
            primary_backend(),
            secondary_backend(),
        )));
        super::FANOUT
            .set(fanout)
            .expect("fanout test should install only once");
        let context = Context::root().with(LabelKey::Frame, 1_u32);

        super::backend_counter_add(&COUNTER, context, 3);
        let token = super::backend_scope_begin(&SCOPE, context, footfall_core::Args::none());
        super::backend_scope_end(token);
        super::backend_frame_marker(MarkerKind::Frame, MarkerPhase::Begin, context);

        let log = LOG
            .get()
            .expect("log should be initialized")
            .lock()
            .expect("log mutex should not be poisoned")
            .clone();
        assert_eq!(
            log,
            vec![
                "primary counter_add",
                "secondary counter_add",
                "primary scope_begin",
                "secondary scope_begin",
                "secondary scope_end",
                "primary scope_end",
                "primary frame_marker",
                "secondary frame_marker",
            ]
        );
    }

    fn primary_backend() -> Backend {
        Backend {
            counter_add: primary_counter_add,
            gauge_set: primary_gauge_set,
            histogram_record: primary_histogram_record,
            scope_begin: primary_scope_begin,
            scope_end: primary_scope_end,
            resource_create: primary_resource_create,
            resource_update: primary_resource_update,
            resource_drop: primary_resource_drop,
            frame_marker: primary_frame_marker,
            flow: primary_flow,
            message: primary_message,
        }
    }

    fn secondary_backend() -> Backend {
        Backend {
            counter_add: secondary_counter_add,
            gauge_set: secondary_gauge_set,
            histogram_record: secondary_histogram_record,
            scope_begin: secondary_scope_begin,
            scope_end: secondary_scope_end,
            resource_create: secondary_resource_create,
            resource_update: secondary_resource_update,
            resource_drop: secondary_resource_drop,
            frame_marker: secondary_frame_marker,
            flow: secondary_flow,
            message: secondary_message,
        }
    }

    fn record(event: &'static str) {
        LOG.get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .expect("log mutex should not be poisoned")
            .push(event);
    }

    fn primary_counter_add(_desc: &CounterDesc, _context: Context, _value: u64) {
        record("primary counter_add");
    }

    fn primary_histogram_record(_desc: &CounterDesc, _context: Context, _value: u64) {
        record("primary histogram_record");
    }

    fn primary_gauge_set(_desc: &CounterDesc, _context: Context, _value: u64) {
        record("primary gauge_set");
    }

    fn primary_scope_begin(
        _desc: &ScopeDesc,
        _context: Context,
        _args: footfall_core::Args,
    ) -> ScopeToken {
        record("primary scope_begin");
        ScopeToken::new(11)
    }

    fn primary_scope_end(_token: ScopeToken) {
        record("primary scope_end");
    }

    fn primary_resource_create(
        _desc: &footfall_core::ResourceKindDesc,
        _resource_id: footfall_core::ResourceId,
        _context: Context,
        _size: u64,
    ) {
        record("primary resource_create");
    }

    fn primary_resource_update(
        _desc: &footfall_core::ResourceKindDesc,
        _resource_id: footfall_core::ResourceId,
        _context: Context,
        _size: u64,
    ) {
        record("primary resource_update");
    }

    fn primary_resource_drop(
        _desc: &footfall_core::ResourceKindDesc,
        _resource_id: footfall_core::ResourceId,
        _context: Context,
    ) {
        record("primary resource_drop");
    }

    fn primary_frame_marker(_kind: MarkerKind, _phase: MarkerPhase, _context: Context) {
        record("primary frame_marker");
    }

    fn primary_message(
        _desc: &footfall_core::EventDesc,
        _context: Context,
        _text: &str,
        _args: footfall_core::Args,
    ) {
        record("primary message");
    }

    fn primary_flow(
        _flow_id: footfall_core::FlowId,
        _phase: footfall_core::FlowPhase,
        _context: Context,
    ) {
        record("primary flow");
    }

    fn secondary_counter_add(_desc: &CounterDesc, _context: Context, _value: u64) {
        record("secondary counter_add");
    }

    fn secondary_histogram_record(_desc: &CounterDesc, _context: Context, _value: u64) {
        record("secondary histogram_record");
    }

    fn secondary_gauge_set(_desc: &CounterDesc, _context: Context, _value: u64) {
        record("secondary gauge_set");
    }

    fn secondary_scope_begin(
        _desc: &ScopeDesc,
        _context: Context,
        _args: footfall_core::Args,
    ) -> ScopeToken {
        record("secondary scope_begin");
        ScopeToken::new(22)
    }

    fn secondary_scope_end(_token: ScopeToken) {
        record("secondary scope_end");
    }

    fn secondary_resource_create(
        _desc: &footfall_core::ResourceKindDesc,
        _resource_id: footfall_core::ResourceId,
        _context: Context,
        _size: u64,
    ) {
        record("secondary resource_create");
    }

    fn secondary_resource_update(
        _desc: &footfall_core::ResourceKindDesc,
        _resource_id: footfall_core::ResourceId,
        _context: Context,
        _size: u64,
    ) {
        record("secondary resource_update");
    }

    fn secondary_resource_drop(
        _desc: &footfall_core::ResourceKindDesc,
        _resource_id: footfall_core::ResourceId,
        _context: Context,
    ) {
        record("secondary resource_drop");
    }

    fn secondary_frame_marker(_kind: MarkerKind, _phase: MarkerPhase, _context: Context) {
        record("secondary frame_marker");
    }

    fn secondary_message(
        _desc: &footfall_core::EventDesc,
        _context: Context,
        _text: &str,
        _args: footfall_core::Args,
    ) {
        record("secondary message");
    }

    fn secondary_flow(
        _flow_id: footfall_core::FlowId,
        _phase: footfall_core::FlowPhase,
        _context: Context,
    ) {
        record("secondary flow");
    }
}
