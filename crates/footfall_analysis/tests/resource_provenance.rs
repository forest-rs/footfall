// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Creation provenance remains distinct from later mutation, release, and ownership.

use footfall_analysis::{ResourceAction, Trace};
use footfall_core::{
    Args, Backend as _, ContextHandle, EventDesc, ResourceId, ResourceKindDesc, ScopeDesc,
};
use footfall_runtime::Runtime;
use footfall_session::{
    OwnedSnapshot,
    capture::{CaptureReader, CaptureWriter},
};
use std::sync::Arc;

static EDIT: ScopeDesc = ScopeDesc::new("edit");
static DROP: ScopeDesc = ScopeDesc::new("release");
static BUFFER: ResourceKindDesc = ResourceKindDesc::new("buffer");
static MESSAGE: EventDesc = EventDesc::new("event");
static DOMAIN: footfall_core::LabelKey = footfall_core::LabelKey::new("domain");

#[test]
fn lifecycle_events_preserve_their_own_scopes_across_threads_and_capture() {
    let runtime = Arc::new(Runtime::default());
    let first_context = ContextHandle::new(1);
    let second_context = ContextHandle::new(2);
    runtime.context_registered(
        first_context,
        &footfall_core::Context::root().with(&DOMAIN, "first"),
    );
    runtime.context_registered(
        second_context,
        &footfall_core::Context::root().with(&DOMAIN, "second"),
    );
    let edit = runtime.scope_begin(
        &EDIT,
        ContextHandle::ROOT,
        &Args::none().with("edit", 1_u64),
    );
    runtime.resource_create(&BUFFER, ResourceId::new(1), first_context, 64);
    runtime.scope_end(edit);
    let edit = runtime.scope_begin(
        &EDIT,
        ContextHandle::ROOT,
        &Args::none().with("edit", 2_u64),
    );
    runtime.resource_update(&BUFFER, ResourceId::new(1), second_context, 128);
    runtime.scope_end(edit);
    let other = Arc::clone(&runtime);
    let release = move || {
        let scope = other.scope_begin(&DROP, ContextHandle::ROOT, &Args::none());
        other.resource_drop(&BUFFER, ResourceId::new(1), ContextHandle::ROOT);
        other.scope_end(scope);
    };
    // WASI has no std thread spawning. Keep the lifecycle/capture assertions
    // there; native targets additionally verify the cross-thread provenance.
    #[cfg(target_family = "wasm")]
    release();
    #[cfg(not(target_family = "wasm"))]
    std::thread::spawn(release).join().expect("release thread");
    let mut writer = CaptureWriter::new(Vec::new()).expect("writer");
    writer
        .write_snapshot(&runtime.snapshot())
        .expect("snapshot");
    let bytes = writer.finish().expect("capture");
    let captured =
        Trace::from_capture(CaptureReader::new(bytes.as_slice()).expect("reader")).expect("trace");
    let trace = captured.trace();
    let resource = &trace.resources()[0];
    let observations = resource.observations();
    assert_eq!(
        observations
            .iter()
            .map(|event| event.action())
            .collect::<Vec<_>>(),
        [
            ResourceAction::Create,
            ResourceAction::Update,
            ResourceAction::Drop
        ]
    );
    let origin = resource.creation().expect("observed creation");
    assert_eq!(
        origin.context_id().get(),
        1,
        "origin context is not overwritten by later events"
    );
    assert_eq!(
        observations[1].context_id().get(),
        2,
        "updates retain their own context"
    );
    let creating = trace
        .span(origin.span().expect("creating scope"))
        .expect("retained scope");
    assert_eq!(creating.arg("edit"), Some(&1_u64.into()));
    let updating = trace
        .span(observations[1].span().expect("updating scope"))
        .expect("scope");
    assert_eq!(updating.arg("edit"), Some(&2_u64.into()));
    #[cfg(not(target_family = "wasm"))]
    assert_ne!(
        origin.thread_id(),
        observations[2].thread_id(),
        "release occurred on another thread"
    );
    assert_eq!(
        trace
            .span(observations[2].span().expect("release scope"))
            .expect("scope")
            .name(),
        "release"
    );
    assert!(
        resource.alive_at(updating.end()),
        "a buffer can survive its creating edit and later edits"
    );
    assert!(
        !resource.alive_at(observations[2].at()),
        "drop closes the observed lifetime"
    );
}

#[test]
fn missing_origins_are_not_inferred_from_later_scopes_or_missing_begins() {
    let runtime = Runtime::builder().event_capacity(2).build();
    runtime.resource_create(&BUFFER, ResourceId::new(1), ContextHandle::ROOT, 1);
    let scope = runtime.scope_begin(&EDIT, ContextHandle::ROOT, &Args::none());
    runtime.resource_update(&BUFFER, ResourceId::new(1), ContextHandle::ROOT, 2);
    runtime.scope_end(scope);
    let trace = Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot()));
    let resource = &trace.resources()[0];
    assert!(
        resource.creation().is_none(),
        "an update cannot substitute for a missing creation"
    );
    assert!(resource.created().is_none(), "creation time is unknown");

    let runtime = Runtime::builder().event_capacity(3).build();
    let scope = runtime.scope_begin(&EDIT, ContextHandle::ROOT, &Args::none());
    runtime.resource_create(&BUFFER, ResourceId::new(1), ContextHandle::ROOT, 1);
    runtime.scope_end(scope);
    runtime.message(
        &MESSAGE,
        ContextHandle::ROOT,
        "evict the begin",
        &Args::none(),
    );
    let trace = Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot()));
    assert!(
        trace.resources()[0]
            .creation()
            .expect("creation retained")
            .span()
            .is_none(),
        "a missing begin cannot establish an enclosing scope"
    );
}

#[test]
fn a_reused_resource_id_has_distinct_creation_provenance() {
    let runtime = Runtime::default();
    for edit in [1_u64, 2] {
        let scope =
            runtime.scope_begin(&EDIT, ContextHandle::ROOT, &Args::none().with("edit", edit));
        runtime.resource_create(&BUFFER, ResourceId::new(1), ContextHandle::ROOT, edit);
        runtime.resource_drop(&BUFFER, ResourceId::new(1), ContextHandle::ROOT);
        runtime.scope_end(scope);
    }
    let trace = Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot()));
    assert_eq!(trace.resources().len(), 2);
    let a = trace.resources()[0].creation().expect("first lifetime");
    let b = trace.resources()[1].creation().expect("second lifetime");
    assert_ne!(
        a.sequence(),
        b.sequence(),
        "event identity distinguishes reused ids"
    );
    assert_ne!(a.span(), b.span(), "origins belong to different edits");
}
