// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! External-view coverage that the facade exposes the full call-site surface,
//! including the `macro_rules!` macros.

use footfall::{CounterDesc, LabelKey, ResourceKindDesc, ScopeDesc, Unit};

static SCOPE: ScopeDesc = footfall::scope_desc!("record_frame", "facade::tests");
static COUNTER: CounterDesc = footfall::counter_desc!(bytes, "upload_bytes");
static RESOURCE: ResourceKindDesc = footfall::resource_kind_desc!("atlas");

#[test]
fn facade_exposes_call_site_macros() {
    let context = footfall::context!(Frame = 7_u32, Pass = 1_u32);

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
    let context = footfall::context!(Frame = 1_u32);

    // Without an installed backend these lower to the no-op path; the test
    // guards that the facade exposes the whole call-site vocabulary.
    footfall::counter_add(&COUNTER, context, 16);
    footfall::gauge_set(&COUNTER, context, 32);
    let token = footfall::scope_begin(&SCOPE, context);
    footfall::scope_end(token);
    let guard = footfall::scope(&SCOPE, context);
    drop(guard);
}
