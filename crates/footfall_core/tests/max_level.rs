// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The process-wide maximum level filters emissions before dispatch, except
//! the ones that close something already begun.

#![cfg(all(feature = "std", not(footfall_compile_out)))]

use footfall_core::{
    Args, Backend, ContextHandle, CounterDesc, InstrumentationLevel, MarkerKind, MarkerPhase,
    ScopeDesc, ScopeToken, install_backend, max_level, set_max_level,
};
use std::sync::Mutex;

static SCOPE: ScopeDesc = ScopeDesc::new("record");
static COUNTER: CounterDesc = CounterDesc::count("ops");

/// Records the name of every call that reaches it.
#[derive(Debug, Default)]
struct Recording {
    calls: Mutex<Vec<&'static str>>,
}

impl Recording {
    fn hit(&self, name: &'static str) {
        self.calls.lock().expect("calls lock").push(name);
    }

    fn take(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.calls.lock().expect("calls lock"))
    }
}

impl Backend for Recording {
    fn counter_add(&self, _: &'static CounterDesc, _: ContextHandle, _: u64) {
        self.hit("counter_add");
    }
    fn scope_begin(&self, _: &'static ScopeDesc, _: ContextHandle, _: &Args) -> ScopeToken {
        self.hit("scope_begin");
        ScopeToken::new(1)
    }
    fn scope_end(&self, _: ScopeToken) {
        self.hit("scope_end");
    }
    fn frame_marker(&self, _: MarkerKind, phase: MarkerPhase, _: ContextHandle) {
        self.hit(match phase {
            MarkerPhase::Begin => "frame_begin",
            MarkerPhase::End => "frame_end",
        });
    }
}

static BACKEND: Recording = Recording {
    calls: Mutex::new(Vec::new()),
};

#[test]
fn max_level_gates_emissions_before_dispatch() {
    let context = ContextHandle::ROOT;
    assert_eq!(
        max_level(),
        InstrumentationLevel::Off,
        "off until installed"
    );

    install_backend(&BACKEND).expect("backend should install once");
    assert_eq!(
        max_level(),
        InstrumentationLevel::Trace,
        "install raises it"
    );

    let token = footfall_core::scope_begin(&SCOPE, context);
    footfall_core::frame_begin(context);
    assert_eq!(BACKEND.take(), ["scope_begin", "frame_begin"]);

    // Lowering the level mid-scope filters new work but still closes what
    // was already begun.
    set_max_level(InstrumentationLevel::Counters);
    footfall_core::counter_add(&COUNTER, context, 1);
    let skipped = footfall_core::scope_begin(&SCOPE, context);
    assert_eq!(skipped, ScopeToken::INVALID);
    footfall_core::frame_begin(context);
    footfall_core::frame_end(context);
    footfall_core::scope_end(token);
    assert_eq!(BACKEND.take(), ["counter_add", "frame_end", "scope_end"]);

    set_max_level(InstrumentationLevel::Off);
    footfall_core::counter_add(&COUNTER, context, 1);
    assert!(BACKEND.take().is_empty());
}
