// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! External-view coverage that the facade exposes the full call-site surface,
//! including the `macro_rules!` macros.

use spoor::{CounterDesc, LabelKey, ResourceKindDesc, ScopeDesc, Unit};

static SCOPE: ScopeDesc = spoor::scope_desc!(1, "record_frame", "facade::tests");
static COUNTER: CounterDesc = spoor::counter_desc!(bytes, 2, "upload_bytes");
static RESOURCE: ResourceKindDesc = spoor::resource_kind_desc!(3, "atlas");

#[test]
fn facade_exposes_call_site_macros() {
    let context = spoor::context!(Frame = 7_u32, Pass = 1_u32);

    assert_eq!(SCOPE.name(), "record_frame");
    assert!(SCOPE.location().is_some());
    assert_eq!(COUNTER.unit(), Unit::Bytes);
    assert_eq!(RESOURCE.name(), "atlas");
    assert_eq!(
        context
            .get(LabelKey::Frame)
            .and_then(|value| value.as_unsigned()),
        Some(7)
    );
}

#[test]
fn facade_exposes_emission_surface() {
    let context = spoor::context!(Frame = 1_u32);

    // Without an installed backend these lower to the no-op path; the test
    // guards that the facade exposes the whole call-site vocabulary.
    spoor::counter_add(&COUNTER, context, 16);
    spoor::gauge_set(&COUNTER, context, 32);
    let token = spoor::scope_begin(&SCOPE, context);
    spoor::scope_end(token);
    let guard = spoor::scope(&SCOPE, context);
    drop(guard);
}
