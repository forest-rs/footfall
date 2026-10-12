// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Small proving-ground capture for Footfall.

use footfall::{
    CounterDesc, FlowId, LabelKey, RegisteredContext, ResourceId, ResourceKindDesc, ScopeDesc,
    counter_add, flow_begin, flow_end, flow_step, frame_begin, frame_end, pass_begin, pass_end,
    resource_create, resource_drop, resource_update, scope,
};
use footfall_export::to_chrome_trace_json;
use footfall_memory::{CacheMetrics, CacheStats, ScratchMetrics, TrackedScratchVec};
use footfall_runtime::Runtime;

static FRAME: LabelKey = LabelKey::new("frame");
static TRANSACTION: LabelKey = LabelKey::new("transaction");
static PASS: LabelKey = LabelKey::new("pass");
static NODE: LabelKey = LabelKey::new("node");
static RESOURCE: LabelKey = LabelKey::new("resource");
static CACHE: LabelKey = LabelKey::new("cache");
static REASON: LabelKey = LabelKey::new("reason");

static RECORD_FRAME: ScopeDesc = footfall::scope_desc!("record_frame", "basic::main");
static TESSELLATE_NODE: ScopeDesc = footfall::scope_desc!("tessellate_node", "basic::main");
static UPLOAD_ATLAS: ScopeDesc = footfall::scope_desc!("upload_atlas", "basic::main");

static DRAW_OPS: CounterDesc = footfall::counter_desc!(count, "draw_ops");
static UPLOAD_BYTES: CounterDesc = footfall::counter_desc!(bytes, "upload_bytes");
static CACHE_CURRENT: CounterDesc = footfall::counter_desc!(bytes, "cache_current_bytes");
static CACHE_HITS: CounterDesc = footfall::counter_desc!(count, "cache_hits");
static CACHE_MISSES: CounterDesc = footfall::counter_desc!(count, "cache_misses");
static CACHE_EVICTIONS: CounterDesc = footfall::counter_desc!(count, "cache_evictions");
static SCRATCH_CURRENT: CounterDesc = footfall::counter_desc!(bytes, "scratch_current_bytes");
static SCRATCH_PEAK: CounterDesc = footfall::counter_desc!(bytes, "scratch_peak_bytes");
static TESSELLATE_MICROS: CounterDesc =
    footfall::counter_desc!("tessellate_micros", footfall::Unit::Micros);
static APP_LOG: footfall::EventDesc = footfall::event_desc!("app_log");
static SCRATCH_GROW: CounterDesc = footfall::counter_desc!(count, "scratch_grow");

static ATLAS: ResourceKindDesc = footfall::resource_kind_desc!("atlas");
static ATLAS_ID: ResourceId = ResourceId::new(9);
static INVALIDATION_FLOW: FlowId = FlowId::new(1);

fn main() {
    let runtime = Runtime::builder()
        .process_name("basic")
        .session_name("recording_demo")
        .counter_capacity(128)
        .event_capacity(256)
        .install_global()
        .expect("global runtime should install once");

    // Build each semantic owner's context once, register it after the
    // runtime is installed, and emit with its handle.
    let frame = footfall::context!(FRAME = 7_u32, TRANSACTION = 1_u32);
    let record = frame.with(&PASS, 1_u32).with(&NODE, 42_u32);
    let cache = frame.with(&PASS, 1_u32).with(&CACHE, "glyph_atlas");
    let atlas = cache.with(&RESOURCE, ATLAS_ID);
    let invalidate = frame.with(&REASON, "layout_invalidation");

    let frame = RegisteredContext::new(&frame);
    let record = RegisteredContext::new(&record);
    let cache = RegisteredContext::new(&cache);
    let atlas = RegisteredContext::new(&atlas);
    let invalidate = RegisteredContext::new(&invalidate);
    let frame_context = frame.handle();
    let record_context = record.handle();
    let cache_context = cache.handle();
    let atlas_context = atlas.handle();
    let invalidate_context = invalidate.handle();

    let scratch_metrics = ScratchMetrics::new(&SCRATCH_CURRENT, &SCRATCH_PEAK, &SCRATCH_GROW);
    let cache_metrics =
        CacheMetrics::new(&CACHE_CURRENT, &CACHE_HITS, &CACHE_MISSES, &CACHE_EVICTIONS);

    frame_begin(frame_context);
    pass_begin(record_context);
    flow_begin(INVALIDATION_FLOW, invalidate_context);

    {
        let _frame_scope = scope(&RECORD_FRAME, frame_context);
        {
            let _tessellate_scope = footfall::scope_with(
                &TESSELLATE_NODE,
                record_context,
                &footfall::args!("reserved" => 32_u32),
            );
            let mut scratch = TrackedScratchVec::<u32>::new(scratch_metrics, record_context);
            scratch.reserve(32);
            for index in 0_u32..24 {
                scratch.push(index);
            }
            counter_add(&DRAW_OPS, record_context, scratch.len() as u64);
            for sample in [180_u64, 220, 210, 950] {
                footfall::histogram_record(&TESSELLATE_MICROS, record_context, sample);
            }
            footfall::message_with(
                &APP_LOG,
                record_context,
                "tessellation finished",
                &footfall::args!("level" => "info"),
            );
            flow_step(INVALIDATION_FLOW, record_context);
        }

        let mut cache = CacheStats::new(cache_metrics, cache_context);
        cache.record_miss();
        cache.observe_resident_bytes(16_384);
        resource_create(&ATLAS, ATLAS_ID, atlas_context, cache.current_bytes());

        {
            let _upload_scope = scope(&UPLOAD_ATLAS, atlas_context);
            counter_add(&UPLOAD_BYTES, atlas_context, 4_096);
            resource_update(&ATLAS, ATLAS_ID, atlas_context, 20_480);
            cache.record_hit();
            cache.record_eviction(12_288);
            flow_step(INVALIDATION_FLOW, atlas_context);
        }

        resource_drop(&ATLAS, ATLAS_ID, atlas_context);
        flow_end(INVALIDATION_FLOW, atlas_context);
    }
    pass_end(record_context);
    frame_end(frame_context);

    let snapshot = runtime.snapshot();
    println!("{}", to_chrome_trace_json(&snapshot));
}
