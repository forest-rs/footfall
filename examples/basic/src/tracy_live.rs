// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Live Tracy example for Spoor.

use spoor::{
    Context, CounterDesc, CounterDescId, LabelKey, MarkerKind, MarkerPhase, ResourceId,
    ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId, counter_add, frame_marker,
    resource_create, resource_drop, resource_update, scope_begin, scope_end,
};
use spoor_memory::{CacheMetrics, CacheStats, ScratchMetrics, TrackedScratchVec};
use spoor_tracy::{Tracy, install_global};
use std::{thread::sleep, time::Duration};

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

const FRAME_COUNT: u32 = 300;

fn main() {
    let tracy = Box::leak(Box::new(Tracy::new()));
    install_global(tracy).expect("global Tracy backend should install once");
    tracy.set_current_thread_name("main");
    let pid = std::process::id();

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

    println!(
        "Tracy example pid={pid} running={} connected={}",
        tracy.is_running(),
        tracy.is_connected()
    );

    println!(
        "running Tracy live example for {FRAME_COUNT} frames (connected: {})",
        tracy.is_connected()
    );

    let mut reported_connected = tracy.is_connected();
    for frame in 0..FRAME_COUNT {
        let connected = tracy.is_connected();
        if connected && !reported_connected {
            println!("Tracy connected on frame {frame}");
        }
        reported_connected = connected;
        run_frame(frame, scratch_metrics, cache_metrics);
    }

    resource_drop(
        &ATLAS,
        ATLAS_ID,
        Context::root()
            .with(LabelKey::Frame, FRAME_COUNT)
            .with(LabelKey::Pass, 1_u32)
            .with(LabelKey::Cache, "glyph_atlas")
            .with(LabelKey::Resource, ATLAS_ID),
    );
}

fn run_frame(frame: u32, scratch_metrics: ScratchMetrics, cache_metrics: CacheMetrics) {
    let frame_context = Context::root()
        .with(LabelKey::Frame, frame)
        .with(LabelKey::Transaction, frame / 30);
    let record_context = frame_context
        .with(LabelKey::Pass, 1_u32)
        .with(LabelKey::Node, 42_u32 + (frame % 6));
    let cache_context = frame_context
        .with(LabelKey::Pass, 1_u32)
        .with(LabelKey::Cache, "glyph_atlas");
    let atlas_context = cache_context.with(LabelKey::Resource, ATLAS_ID);

    frame_marker(MarkerKind::Frame, MarkerPhase::Begin, frame_context);

    let frame_scope = scope_begin(&RECORD_FRAME, frame_context);
    let tessellate_scope = scope_begin(&TESSELLATE_NODE, record_context);
    let mut scratch = TrackedScratchVec::<u32>::new(scratch_metrics, record_context);
    let scratch_items = 12_u32 + (frame % 36);
    scratch.reserve(scratch_items as usize);
    for index in 0..scratch_items {
        scratch.push(index);
    }
    counter_add(&DRAW_OPS, record_context, u64::from(scratch_items));
    sleep(Duration::from_millis(u64::from(2 + (frame % 3))));
    scope_end(tessellate_scope);

    let mut cache = CacheStats::new(cache_metrics, cache_context);
    let resident_bytes = 8_192_u64 + u64::from(frame % 8) * 2_048;
    if frame == 0 {
        cache.record_miss();
        cache.observe_resident_bytes(resident_bytes);
        resource_create(&ATLAS, ATLAS_ID, atlas_context, resident_bytes);
    } else if frame.is_multiple_of(11) {
        cache.record_miss();
        cache.record_eviction(resident_bytes.saturating_sub(2_048));
    } else {
        cache.record_hit();
        cache.observe_resident_bytes(resident_bytes);
    }

    let upload_scope = scope_begin(&UPLOAD_ATLAS, atlas_context);
    let upload_bytes = 1_024_u64 * u64::from(1 + (frame % 6));
    counter_add(&UPLOAD_BYTES, atlas_context, upload_bytes);
    resource_update(
        &ATLAS,
        ATLAS_ID,
        atlas_context,
        resident_bytes + upload_bytes,
    );
    sleep(Duration::from_millis(u64::from(3 + (frame % 5))));
    scope_end(upload_scope);

    scope_end(frame_scope);
    frame_marker(MarkerKind::Frame, MarkerPhase::End, frame_context);
    sleep(Duration::from_millis(8));
}
