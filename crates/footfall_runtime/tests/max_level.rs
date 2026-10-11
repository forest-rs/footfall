// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A runtime installed as the process backend drives the core maximum level.

use footfall_core::{InstrumentationLevel, max_level};
use footfall_runtime::Runtime;

#[test]
fn installed_runtime_level_sets_the_core_max_level() {
    let runtime = Runtime::with_level(8, 8, InstrumentationLevel::Counters)
        .install_global()
        .expect("runtime should install once for this test binary");
    assert_eq!(max_level(), InstrumentationLevel::Counters);

    runtime.set_level(InstrumentationLevel::Off);
    assert_eq!(max_level(), InstrumentationLevel::Off);

    runtime.set_level(InstrumentationLevel::Trace);
    assert_eq!(max_level(), InstrumentationLevel::Trace);

    // A runtime that is not the installed backend leaves the level alone.
    Runtime::new(8, 8).set_level(InstrumentationLevel::Off);
    assert_eq!(max_level(), InstrumentationLevel::Trace);
}
