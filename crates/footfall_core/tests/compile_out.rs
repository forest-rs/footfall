// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! With `--cfg footfall_compile_out`, no emission reaches the installed
//! backend.
//!
//! Run with `RUSTFLAGS="--cfg footfall_compile_out" cargo test -p footfall_core
//! --features std --test compile_out`. Without the cfg this file is empty.

#![cfg(all(footfall_compile_out, feature = "std"))]

use footfall_core::{
    Args, Backend, Context, CounterDesc, EventDesc, FlowId, FlowPhase, MarkerKind, MarkerPhase,
    ResourceId, ResourceKindDesc, ScopeDesc, ScopeToken, install_backend,
};
use std::sync::atomic::{AtomicUsize, Ordering};

static SCOPE: ScopeDesc = ScopeDesc::new("record");
static COUNTER: CounterDesc = CounterDesc::count("ops");
static RESOURCE: ResourceKindDesc = ResourceKindDesc::new("atlas");
static EVENT: EventDesc = EventDesc::new("log");

/// Counts every call that reaches it.
#[derive(Debug, Default)]
struct Counting {
    calls: AtomicUsize,
}

impl Counting {
    fn hit(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
}

impl Backend for Counting {
    fn counter_add(&self, _: &'static CounterDesc, _: &Context, _: u64) {
        self.hit();
    }
    fn gauge_set(&self, _: &'static CounterDesc, _: &Context, _: u64) {
        self.hit();
    }
    fn histogram_record(&self, _: &'static CounterDesc, _: &Context, _: u64) {
        self.hit();
    }
    fn scope_begin(&self, _: &'static ScopeDesc, _: &Context, _: &Args) -> ScopeToken {
        self.hit();
        ScopeToken::new(1)
    }
    fn scope_end(&self, _: ScopeToken) {
        self.hit();
    }
    fn resource_create(&self, _: &'static ResourceKindDesc, _: ResourceId, _: &Context, _: u64) {
        self.hit();
    }
    fn resource_update(&self, _: &'static ResourceKindDesc, _: ResourceId, _: &Context, _: u64) {
        self.hit();
    }
    fn resource_drop(&self, _: &'static ResourceKindDesc, _: ResourceId, _: &Context) {
        self.hit();
    }
    fn frame_marker(&self, _: MarkerKind, _: MarkerPhase, _: &Context) {
        self.hit();
    }
    fn flow(&self, _: FlowId, _: FlowPhase, _: &Context) {
        self.hit();
    }
    fn message(&self, _: &'static EventDesc, _: &Context, _: &str, _: &Args) {
        self.hit();
    }
}

static BACKEND: Counting = Counting {
    calls: AtomicUsize::new(0),
};

#[test]
fn compiled_out_emissions_never_reach_the_backend() {
    install_backend(&BACKEND).expect("backend should install once");
    let context = Context::root();
    let args = Args::none();

    footfall_core::counter_add(&COUNTER, &context, 1);
    footfall_core::gauge_set(&COUNTER, &context, 1);
    footfall_core::histogram_record(&COUNTER, &context, 1);
    let token = footfall_core::scope_begin_with(&SCOPE, &context, &args);
    assert_eq!(token, ScopeToken::INVALID, "no scope is recorded");
    footfall_core::scope_end(ScopeToken::new(7));
    drop(footfall_core::scope(&SCOPE, &context));
    footfall_core::resource_create(&RESOURCE, ResourceId::new(1), &context, 1);
    footfall_core::resource_update(&RESOURCE, ResourceId::new(1), &context, 2);
    footfall_core::resource_drop(&RESOURCE, ResourceId::new(1), &context);
    footfall_core::frame_begin(&context);
    footfall_core::pass_end(&context);
    footfall_core::flow_step(FlowId::new(3), &context);
    footfall_core::message_with(&EVENT, &context, "hello", &args);

    assert_eq!(BACKEND.calls.load(Ordering::Relaxed), 0);
}
