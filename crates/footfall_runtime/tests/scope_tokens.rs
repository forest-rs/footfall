// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scope tokens issued before a backend is installed.
//!
//! This lives in its own integration test binary because it installs the
//! process-wide backend.

use footfall_core::{ContextHandle, ScopeDesc, ScopeToken, scope, scope_begin, scope_end};
use footfall_runtime::{Observation, Runtime};

static EARLY: ScopeDesc = ScopeDesc::new("early");
static LATER: ScopeDesc = ScopeDesc::new("later");

#[test]
fn guard_started_before_install_does_not_end_a_later_scope() {
    let early = scope(&EARLY, ContextHandle::ROOT);
    assert_eq!(early.token(), ScopeToken::INVALID);

    let runtime = Runtime::builder()
        .counter_capacity(16)
        .event_capacity(16)
        .install_global()
        .expect("runtime should install once");

    // The runtime issues token 1 here. Before token 0 was reserved, the
    // early guard carried its descriptor id (also 1) and would end this
    // scope when dropped.
    let later = scope_begin(&LATER, ContextHandle::ROOT);
    assert!(later.is_valid());
    drop(early);
    scope_end(later);

    let snapshot = runtime.snapshot();
    let events = snapshot.events();
    let ends: Vec<_> = events
        .iter()
        .filter_map(|event| match event.observation() {
            Observation::ScopeEnd { token } => Some(*token),
            _ => None,
        })
        .collect();
    assert_eq!(ends, [later], "only the later scope should end");
    assert_eq!(events.len(), 2);
}
