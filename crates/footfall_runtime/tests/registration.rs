// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Context registration through the process-wide backend.
//!
//! This lives in its own integration test binary because it installs the
//! process-wide backend.

use footfall_core::{
    ContextHandle, CounterDesc, LabelKey, LazyContext, RegisteredContext, context, counter_add,
};
use footfall_runtime::Runtime;

static PASS: LabelKey = LabelKey::new("pass");
static OPS: CounterDesc = CounterDesc::count("ops");
static OPAQUE: LazyContext = LazyContext::new(|| context!(PASS = "opaque"));

#[test]
fn contexts_register_through_the_installed_backend() {
    assert_eq!(
        OPAQUE.handle(),
        ContextHandle::ROOT,
        "a lazy context touched before installation stays unregistered"
    );

    let runtime = Runtime::new(16, 16)
        .install_global()
        .expect("runtime should install once");

    let opaque = OPAQUE.handle();
    assert!(
        !opaque.is_root(),
        "the first use after installation registers"
    );
    assert_eq!(OPAQUE.handle(), opaque, "later uses reuse the handle");

    let transparent = RegisteredContext::new(&context!(PASS = "transparent"));
    counter_add(&OPS, opaque, 1);
    counter_add(&OPS, transparent.handle(), 2);

    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.dropped_contexts, 0);
    for counter in &snapshot.counters {
        let context = snapshot
            .context(counter.context_id)
            .expect("every counter context should resolve");
        let expected = if counter.value == 1 {
            "opaque"
        } else {
            "transparent"
        };
        assert_eq!(
            context
                .context
                .get(&PASS)
                .and_then(|value| value.as_static_str()),
            Some(expected)
        );
    }

    let released = transparent.handle();
    drop(transparent);
    let drained = runtime.drain_snapshot();
    assert!(drained.context(released.id()).is_some());
    assert!(runtime.snapshot().context(released.id()).is_none());
    assert!(runtime.snapshot().context(opaque.id()).is_some());
}
