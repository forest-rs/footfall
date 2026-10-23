// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scope admission follows the level; already-issued tokens still close their scopes.
//!
//! This test owns its integration-test process's installed backend.

#![cfg(not(footfall_compile_out))]

use footfall_core::{
    Backend, ContextHandle, InstrumentationLevel, ScopeDesc, ScopeToken, scope, scope_end,
};
use footfall_runtime::{Observation, Runtime};

static OUTER: ScopeDesc = ScopeDesc::new("outer");
static INNER: ScopeDesc = ScopeDesc::new("inner");
static SUPPRESSED: ScopeDesc = ScopeDesc::new("suppressed");

#[test]
fn nested_scopes_close_after_capture_is_lowered() {
    let runtime = Runtime::builder()
        .level(InstrumentationLevel::Scoped)
        .counter_capacity(8)
        .event_capacity(32)
        .install_global()
        .expect("runtime should install once for this test binary");

    for lowered in [InstrumentationLevel::Off, InstrumentationLevel::Counters] {
        runtime.set_level(InstrumentationLevel::Scoped);
        let cursor = runtime.cursor();
        runtime.scope_end(ScopeToken::INVALID);
        let outer = scope(&OUTER, ContextHandle::ROOT);
        let inner = scope(&INNER, ContextHandle::ROOT);
        let outer_token = outer.token();
        let inner_token = inner.token();
        assert!(outer_token.is_valid());
        assert!(inner_token.is_valid());

        runtime.set_level(lowered);
        let suppressed = scope(&SUPPRESSED, ContextHandle::ROOT);
        assert_eq!(suppressed.token(), ScopeToken::INVALID);
        drop(suppressed);
        runtime.scope_end(ScopeToken::INVALID);
        scope_end(ScopeToken::INVALID);
        inner.end();
        drop(outer);

        let snapshot = runtime.snapshot_since(cursor);
        let events: Vec<_> = snapshot
            .events()
            .iter()
            .map(|event| match event.observation() {
                Observation::ScopeBegin { token, .. } => (true, *token),
                Observation::ScopeEnd { token } => (false, *token),
                other => panic!("unexpected observation: {other:?}"),
            })
            .collect();
        assert_eq!(
            events,
            [
                (true, outer_token),
                (true, inner_token),
                (false, inner_token),
                (false, outer_token),
            ],
            "lowering to {lowered:?} must not abandon admitted scopes"
        );
        assert_eq!(snapshot.window().missed(), 0);
    }
}
