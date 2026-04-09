// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Live Tracy example for Spoor.

use spoor::{
    Context, CounterDesc, CounterDescId, LabelKey, MarkerKind, MarkerPhase, ResourceId,
    ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId, SourceLocation, counter_add,
    frame_marker, resource_create, resource_drop, resource_update, scope_begin, scope_end,
};
use spoor_fanout::{Fanout, install_global as install_fanout};
use spoor_memory::{CacheMetrics, CacheStats, ScratchMetrics, TrackedScratchVec};
use spoor_runtime::{Runtime, backend as runtime_backend, register_global as register_runtime};
use spoor_session::{Chunking, Consumer, Envelope, Payload, publish_snapshot};
use spoor_tracy::{Tracy, backend as tracy_backend, register_global as register_tracy};
use std::{thread::sleep, time::Duration};

static RECORD_FRAME: ScopeDesc = ScopeDesc::with_location(
    ScopeDescId::new(1),
    "record_frame",
    SourceLocation::new("tracy_live::run_frame", file!(), 95),
);
static TESSELLATE_NODE: ScopeDesc = ScopeDesc::with_location(
    ScopeDescId::new(2),
    "tessellate_node",
    SourceLocation::new("tracy_live::run_frame", file!(), 96),
);
static UPLOAD_ATLAS: ScopeDesc = ScopeDesc::with_location(
    ScopeDescId::new(3),
    "upload_atlas",
    SourceLocation::new("tracy_live::run_frame", file!(), 121),
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

const FRAME_COUNT: u32 = 300;

fn main() {
    let mut runtime = Runtime::named("tracy_live", 512, 2048);
    runtime.set_session_name("tracy_live");
    let runtime = Box::leak(Box::new(runtime));
    let tracy = Box::leak(Box::new(Tracy::new()));
    register_runtime(runtime).expect("global runtime should register once");
    register_tracy(tracy).expect("global Tracy backend should register once");
    let fanout = Box::leak(Box::new(Fanout::new(runtime_backend(), tracy_backend())));
    install_fanout(fanout).expect("fanout backend should install once");
    tracy.set_current_thread_name("main");
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

    println!("running Tracy live example for {FRAME_COUNT} frames");

    for frame in 0..FRAME_COUNT {
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

    let snapshot = runtime.drain_snapshot();
    let mut consumer = SummaryConsumer::default();
    publish_snapshot(&snapshot, Chunking::default(), &mut consumer)
        .expect("session publication should succeed");
    println!(
        "captured {} session envelopes, {} counters, {} events",
        consumer.envelopes, consumer.counters, consumer.events
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

#[derive(Default)]
struct SummaryConsumer {
    envelopes: u64,
    counters: usize,
    events: usize,
}

impl Consumer for SummaryConsumer {
    type Error = core::convert::Infallible;

    fn consume(&mut self, envelope: Envelope<'_>) -> Result<(), Self::Error> {
        self.envelopes = self.envelopes.saturating_add(1);
        match envelope.payload {
            Payload::Counters(counters) => {
                self.counters = self.counters.saturating_add(counters.len());
            }
            Payload::Events(events) => {
                self.events = self.events.saturating_add(events.len());
            }
            Payload::SnapshotStart(_) | Payload::Contexts(_) | Payload::SnapshotEnd(_) => {}
        }
        Ok(())
    }
}
