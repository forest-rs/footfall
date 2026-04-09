// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use spoor::{
    Context, CounterDesc, CounterDescId, LabelKey, MarkerKind, MarkerPhase, ResourceId,
    ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId, SourceLocation, counter_add,
    frame_marker, resource_create, resource_drop, resource_update, scope_begin, scope_end,
};
use spoor_memory::{CacheMetrics, CacheStats, ScratchMetrics, TrackedScratchVec};
use spoor_runtime::{Runtime, Snapshot, install_global};

static RECORD_FRAME: ScopeDesc = ScopeDesc::with_location(
    ScopeDescId::new(1),
    "record_frame",
    SourceLocation::new("session_demo::DemoCapture::capture_frame", file!(), 96),
);
static TESSELLATE_NODE: ScopeDesc = ScopeDesc::with_location(
    ScopeDescId::new(2),
    "tessellate_node",
    SourceLocation::new("session_demo::DemoCapture::capture_frame", file!(), 97),
);
static UPLOAD_ATLAS: ScopeDesc = ScopeDesc::with_location(
    ScopeDescId::new(3),
    "upload_atlas",
    SourceLocation::new("session_demo::DemoCapture::capture_frame", file!(), 122),
);

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

pub(crate) struct DemoCapture {
    runtime: &'static Runtime,
}

impl DemoCapture {
    pub(crate) fn install(process_name: &str, session_name: &str) -> Self {
        let mut runtime = Runtime::named(process_name, 128, 256);
        runtime.set_session_name(session_name);
        let runtime = Box::leak(Box::new(runtime));
        install_global(runtime).expect("global runtime should install once");
        Self { runtime }
    }

    pub(crate) fn capture_frame(&self, frame: u32) -> Snapshot {
        let frame_context = Context::root()
            .with(LabelKey::Frame, frame)
            .with(LabelKey::Transaction, 1_u32 + frame / 8);
        let record_context = frame_context
            .with(LabelKey::Pass, 1_u32)
            .with(LabelKey::Node, 77_u32 + (frame % 5));
        let cache_context = frame_context
            .with(LabelKey::Pass, 1_u32)
            .with(LabelKey::Cache, "glyph_atlas");
        let atlas_context = cache_context.with(LabelKey::Resource, ATLAS_ID);

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
        let frame_scope = scope_begin(&RECORD_FRAME, frame_context);
        let tessellate_scope = scope_begin(&TESSELLATE_NODE, record_context);

        let mut scratch = TrackedScratchVec::<u32>::new(scratch_metrics, record_context);
        let scratch_items = 16_u32 + (frame % 20);
        scratch.reserve(scratch_items as usize);
        for index in 0..scratch_items {
            scratch.push(index);
        }
        counter_add(&DRAW_OPS, record_context, u64::from(scratch_items));
        scope_end(tessellate_scope);

        let mut cache = CacheStats::new(cache_metrics, cache_context);
        let resident_bytes = 8_192_u64 + u64::from(frame % 8) * 2_048;
        if frame.is_multiple_of(5) {
            cache.record_miss();
        } else {
            cache.record_hit();
        }
        if frame.is_multiple_of(7) {
            cache.record_eviction(resident_bytes.saturating_sub(2_048));
        } else {
            cache.observe_resident_bytes(resident_bytes);
        }

        resource_create(&ATLAS, ATLAS_ID, atlas_context, resident_bytes);
        let upload_scope = scope_begin(&UPLOAD_ATLAS, atlas_context);
        let upload_bytes = 1_024_u64 * u64::from(1 + (frame % 4));
        counter_add(&UPLOAD_BYTES, atlas_context, upload_bytes);
        resource_update(
            &ATLAS,
            ATLAS_ID,
            atlas_context,
            resident_bytes + upload_bytes,
        );
        scope_end(upload_scope);
        resource_drop(&ATLAS, ATLAS_ID, atlas_context);

        scope_end(frame_scope);
        frame_marker(MarkerKind::Frame, MarkerPhase::End, frame_context);

        self.runtime.drain_snapshot()
    }
}
