// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `scope!` keeps its scope open until the end of the enclosing block, and
//! the installed runtime can be reached again.

use footfall_core::{ContextHandle, EventDesc, ScopeDesc, message};
use footfall_runtime::{Observation, Runtime};

static WORK: ScopeDesc = ScopeDesc::new("work");
static LOG: EventDesc = EventDesc::new("log");

fn work() {
    footfall_core::scope!(&WORK, ContextHandle::ROOT);
    message(&LOG, ContextHandle::ROOT, "inside");
}

#[test]
fn scope_macro_covers_the_rest_of_the_block() {
    let runtime = Runtime::builder()
        .install_global()
        .expect("runtime should install once for this test binary");
    assert!(
        Runtime::global().is_some_and(|global| core::ptr::eq(global, runtime)),
        "the installed runtime can be reached"
    );
    let error = Runtime::default()
        .install_global()
        .expect_err("a backend is already installed");
    assert!(format!("{error:?}").contains("already installed"));

    work();
    let kinds: Vec<_> = runtime
        .snapshot()
        .events()
        .iter()
        .map(|event| match event.observation() {
            Observation::ScopeBegin { .. } => "begin",
            Observation::Message { .. } => "message",
            Observation::ScopeEnd { .. } => "end",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["begin", "message", "end"]);
}
