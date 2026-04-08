// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Small proving-ground capture for Spoor.

use spoor::{
    Context, CounterDesc, CounterDescId, FlowId, LabelKey, MarkerKind, MarkerPhase, ResourceId,
    ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId, counter_add, flow_begin, flow_end,
    flow_step, frame_marker, resource_create, resource_drop, resource_update, scope_begin,
    scope_end,
};
use spoor_export::to_chrome_trace_json;
use spoor_memory::{CacheMetrics, CacheStats, ScratchMetrics, TrackedScratchVec};
use spoor_runtime::{Runtime, install_global};

static RECORD_FRAME: ScopeDesc = ScopeDesc::new(ScopeDescId::new(1), "record_frame");
static TESSELLATE_NODE: ScopeDesc = ScopeDesc::new(ScopeDescId::new(2), "tessellate_node");
static UPLOAD_ATLAS: ScopeDesc = ScopeDesc::new(ScopeDescId::new(3), "upload_atlas");

static DRAW_OPS: CounterDesc = CounterDesc::count(CounterDescId::new(10), "draw_ops");
static UPLOAD_BYTES: CounterDesc = CounterDesc::bytes(CounterDescId::new(11), "upload_bytes");
static CACHE_CURRENT: CounterDesc =
    CounterDesc::bytes(CounterDescId::new(12), "cache_current_bytes");
static CACHE_HITS: CounterDesc = CounterDesc::count(CounterDescId::new(13), "cache_hits");
static CACHE_MISSES: CounterDesc = CounterDesc::count(CounterDescId::new(14), "cache_misses");
static CACHE_EVICTIONS: CounterDesc = CounterDesc::count(CounterDescId::new(15), "cache_evictions");
static SCRATCH_CURRENT: CounterDesc =
    CounterDesc::bytes(CounterDescId::new(16), "scratch_current_bytes");
static SCRATCH_PEAK: CounterDesc = CounterDesc::bytes(CounterDescId::new(17), "scratch_peak_bytes");
static SCRATCH_GROW: CounterDesc = CounterDesc::count(CounterDescId::new(18), "scratch_grow");

static ATLAS: ResourceKindDesc = ResourceKindDesc::new(ResourceKindId::new(20), "atlas");
static ATLAS_ID: ResourceId = ResourceId::new(9);
static INVALIDATION_FLOW: FlowId = FlowId::new(1);

fn main() {
    let mut runtime = Runtime::named("basic", 128, 256);
    runtime.set_session_name("recording_demo");
    let runtime = Box::leak(Box::new(runtime));
    install_global(runtime).expect("global runtime should install once");

    let frame_context = Context::root()
        .with(LabelKey::Frame, 7_u32)
        .with(LabelKey::Transaction, 1_u32);
    let record_context = frame_context
        .with(LabelKey::Pass, 1_u32)
        .with(LabelKey::Node, 42_u32);
    let cache_context = frame_context
        .with(LabelKey::Pass, 1_u32)
        .with(LabelKey::Cache, "glyph_atlas");
    let atlas_context = cache_context.with(LabelKey::Resource, ATLAS_ID);
    let invalidate_context = frame_context.with(LabelKey::Reason, "layout_invalidation");

    let scratch_metrics = ScratchMetrics {
        current_bytes: &SCRATCH_CURRENT,
        peak_bytes: &SCRATCH_PEAK,
        grow_count: &SCRATCH_GROW,
    };
    let cache_metrics = CacheMetrics {
        current_bytes: &CACHE_CURRENT,
        hit_count: &CACHE_HITS,
        miss_count: &CACHE_MISSES,
        eviction_count: &CACHE_EVICTIONS,
    };

    frame_marker(MarkerKind::Frame, MarkerPhase::Begin, frame_context);
    frame_marker(MarkerKind::Pass, MarkerPhase::Begin, record_context);
    flow_begin(INVALIDATION_FLOW, invalidate_context);

    let frame_scope = scope_begin(&RECORD_FRAME, frame_context);
    let tessellate_scope = scope_begin(&TESSELLATE_NODE, record_context);
    let mut scratch = TrackedScratchVec::<u32>::new(scratch_metrics, record_context);
    scratch.reserve(32);
    for index in 0_u32..24 {
        scratch.push(index);
    }
    counter_add(&DRAW_OPS, record_context, scratch.len() as u64);
    flow_step(INVALIDATION_FLOW, record_context);
    scope_end(tessellate_scope);

    let mut cache = CacheStats::new(cache_metrics, cache_context);
    cache.record_miss();
    cache.observe_resident_bytes(16_384);
    resource_create(&ATLAS, ATLAS_ID, atlas_context, cache.current_bytes());

    let upload_scope = scope_begin(&UPLOAD_ATLAS, atlas_context);
    counter_add(&UPLOAD_BYTES, atlas_context, 4_096);
    resource_update(&ATLAS, ATLAS_ID, atlas_context, 20_480);
    cache.record_hit();
    cache.record_eviction(12_288);
    flow_step(INVALIDATION_FLOW, atlas_context);
    scope_end(upload_scope);

    resource_drop(&ATLAS, ATLAS_ID, atlas_context);
    flow_end(INVALIDATION_FLOW, atlas_context);
    scope_end(frame_scope);
    frame_marker(MarkerKind::Pass, MarkerPhase::End, record_context);
    frame_marker(MarkerKind::Frame, MarkerPhase::End, frame_context);

    let snapshot = runtime.drain_snapshot();
    println!("{}", to_chrome_trace_json(&snapshot));
}
