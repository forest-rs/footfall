// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Long-lived scopes retain their issued tokens across capture changes and event eviction.

#![cfg(not(footfall_compile_out))]

use footfall_core::{
    Args, Backend, ContextHandle, InstrumentationLevel, ScopeDesc, ScopeToken, scope,
};
use footfall_runtime::{Observation, Runtime, Snapshot};

static ROOT: ScopeDesc = ScopeDesc::new("root");
static CHILD: ScopeDesc = ScopeDesc::new("child");
static SUPPRESSED: ScopeDesc = ScopeDesc::new("suppressed");

fn events(snapshot: &Snapshot) -> Vec<(bool, ScopeToken)> {
    snapshot
        .events()
        .iter()
        .map(|event| match event.observation() {
            Observation::ScopeBegin { token, .. } => (true, *token),
            Observation::ScopeEnd { token } => (false, *token),
            other => panic!("unexpected observation: {other:?}"),
        })
        .collect()
}

#[test]
fn a_root_remains_open_across_snapshots_and_capture_resume() {
    // Only this test uses the process-wide backend; the eviction test calls its own runtime.
    let runtime = Runtime::builder()
        .level(InstrumentationLevel::Scoped)
        .counter_capacity(8)
        .event_capacity(16)
        .install_global()
        .expect("runtime should install once for this test binary");

    for lowered in [InstrumentationLevel::Off, InstrumentationLevel::Counters] {
        runtime.set_level(InstrumentationLevel::Scoped);
        let start = runtime.cursor();
        let root = scope(&ROOT, ContextHandle::ROOT);
        let root_token = root.token();
        assert!(root_token.is_valid());
        let begun = runtime.snapshot_since(start);
        assert_eq!(events(&begun), [(true, root_token)]);

        runtime.set_level(lowered);
        let suppressed = scope(&SUPPRESSED, ContextHandle::ROOT);
        assert_eq!(suppressed.token(), ScopeToken::INVALID);
        let paused = runtime.snapshot_since(begun.next_cursor());
        assert!(
            paused.events().is_empty(),
            "lowering must not close the root"
        );

        runtime.set_level(InstrumentationLevel::Scoped);
        drop(suppressed);
        let child = scope(&CHILD, ContextHandle::ROOT);
        let child_token = child.token();
        assert!(child_token.is_valid());
        drop(child);
        let resumed = runtime.snapshot_since(paused.next_cursor());
        assert_eq!(
            events(&resumed),
            [(true, child_token), (false, child_token)],
            "resume must not restart the root or end the suppressed scope"
        );

        runtime.set_level(lowered);
        drop(root);
        let ended = runtime.snapshot_since(resumed.next_cursor());
        assert_eq!(events(&ended), [(false, root_token)]);
        let whole = runtime.snapshot_since(start);
        assert_eq!(
            events(&whole),
            [
                (true, root_token),
                (true, child_token),
                (false, child_token),
                (false, root_token),
            ]
        );
        assert_eq!(whole.window().missed(), 0);
    }
}

#[test]
fn an_evicted_root_begin_does_not_suppress_its_later_end() {
    let runtime = Runtime::builder()
        .level(InstrumentationLevel::Scoped)
        .counter_capacity(8)
        .event_capacity(2)
        .build();
    let root = runtime.scope_begin(&ROOT, ContextHandle::ROOT, &Args::none());
    assert!(root.is_valid());
    let early = runtime.snapshot();
    assert_eq!(events(&early), [(true, root)]);

    let child = runtime.scope_begin(&CHILD, ContextHandle::ROOT, &Args::none());
    runtime.scope_end(child);
    let evicted = runtime.snapshot();
    assert_eq!(events(&evicted), [(true, child), (false, child)]);
    assert_eq!(evicted.window().missed(), 1);

    runtime.set_level(InstrumentationLevel::Off);
    runtime.scope_end(root);
    let latest = runtime.snapshot_since(evicted.next_cursor());
    assert_eq!(events(&latest), [(false, root)]);
    assert_eq!(latest.window().missed(), 0);

    // An earlier reader still owns the real begin; a late reader gets loss evidence.
    assert_eq!(events(&early), [(true, root)]);
    let late = runtime.snapshot();
    assert_eq!(events(&late), [(false, child), (false, root)]);
    assert_eq!(late.window().missed(), 2);
}
