// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Small proving-ground capture for Footfall.

use footfall::{
    CounterDesc, FlowId, LabelKey, ResourceId, ResourceKindDesc, ScopeDesc, counter_add,
    flow_begin, flow_end, flow_step, frame_begin, frame_end, pass_begin, pass_end, resource_create,
    resource_drop, resource_update, scope,
};
use footfall_export::to_chrome_trace_json;
use footfall_memory::{CacheMetrics, CacheStats, ScratchMetrics, TrackedScratchVec};
use footfall_runtime::Runtime;

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
    let runtime = Runtime::named("basic", 128, 256)
        .with_session_name("recording_demo")
        .install_global()
        .expect("global runtime should install once");

    let frame_context = footfall::context!(Frame = 7_u32, Transaction = 1_u32);
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

    frame_begin(frame_context);
    pass_begin(record_context);
    flow_begin(INVALIDATION_FLOW, invalidate_context);

    {
        let _frame_scope = scope(&RECORD_FRAME, frame_context);
        {
            let _tessellate_scope = footfall::scope_with(
                &TESSELLATE_NODE,
                record_context,
                footfall::args!("reserved" => 32_u32),
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
                footfall::args!("level" => "info"),
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

    let snapshot = runtime.drain_snapshot();
    println!("{}", to_chrome_trace_json(&snapshot));
}
