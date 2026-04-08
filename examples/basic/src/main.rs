// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal Spoor example binary.

use spoor::{
    Context, CounterDesc, CounterDescId, LabelKey, MarkerKind, MarkerPhase, ResourceId,
    ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId, counter_add, frame_marker,
    resource_update, scope_begin, scope_end,
};

static RECORD_PASS: ScopeDesc = ScopeDesc::new(ScopeDescId::new(1), "record_pass");
static UPLOAD_BYTES: CounterDesc = CounterDesc::bytes(CounterDescId::new(2), "upload_bytes");
static ATLAS: ResourceKindDesc = ResourceKindDesc::new(ResourceKindId::new(3), "atlas");

fn main() {
    let context = Context::root()
        .with(LabelKey::Frame, 7_u32)
        .with(LabelKey::Pass, 1_u32)
        .with(LabelKey::Node, 42_u32);

    frame_marker(MarkerKind::Frame, MarkerPhase::Begin, context);
    let scope = scope_begin(&RECORD_PASS, context);
    counter_add(&UPLOAD_BYTES, context, 4096);
    resource_update(&ATLAS, ResourceId::new(9), context, 8192);
    scope_end(scope);
    frame_marker(MarkerKind::Frame, MarkerPhase::End, context);
}
