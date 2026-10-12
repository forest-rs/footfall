// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use footfall::{
    Context, CounterDesc, EventDesc, FlowId, LabelKey, RegisteredContext, ResourceId,
    ResourceKindDesc, ScopeDesc, counter_add, flow_begin, flow_end, flow_step, frame_begin,
    frame_end, histogram_record, message_with, pass_begin, pass_end, resource_create,
    resource_drop, resource_update, scope, scope_with,
};
use footfall_memory::{CacheMetrics, CacheStats, ScratchMetrics, TrackedScratchVec};
use footfall_runtime::Runtime;
use footfall_tracing::FootfallLayer;
use tracing_subscriber::layer::SubscriberExt as _;

static FRAME: LabelKey = LabelKey::new("frame");
static TRANSACTION: LabelKey = LabelKey::new("transaction");
static PASS: LabelKey = LabelKey::new("pass");
static NODE: LabelKey = LabelKey::new("node");
static RESOURCE: LabelKey = LabelKey::new("resource");
static CACHE: LabelKey = LabelKey::new("cache");
static REASON: LabelKey = LabelKey::new("reason");

static RECORD_FRAME: ScopeDesc =
    footfall::scope_desc!("record_frame", "session_demo::DemoCapture::record_frame");
static TESSELLATE_NODE: ScopeDesc =
    footfall::scope_desc!("tessellate_node", "session_demo::DemoCapture::record_frame",);
static UPLOAD_ATLAS: ScopeDesc =
    footfall::scope_desc!("upload_atlas", "session_demo::DemoCapture::record_frame");

static DRAW_OPS: CounterDesc = footfall::counter_desc!(count, "draw_ops");
static UPLOAD_BYTES: CounterDesc = footfall::counter_desc!(bytes, "upload_bytes");
static CACHE_CURRENT: CounterDesc = footfall::counter_desc!(bytes, "cache_current_bytes");
static CACHE_HITS: CounterDesc = footfall::counter_desc!(count, "cache_hits");
static CACHE_MISSES: CounterDesc = footfall::counter_desc!(count, "cache_misses");
static CACHE_EVICTIONS: CounterDesc = footfall::counter_desc!(count, "cache_evictions");
static SCRATCH_CURRENT: CounterDesc = footfall::counter_desc!(bytes, "scratch_current_bytes");
static SCRATCH_PEAK: CounterDesc = footfall::counter_desc!(bytes, "scratch_peak_bytes");
static SCRATCH_GROW: CounterDesc = footfall::counter_desc!(count, "scratch_grow");
static TESSELLATE_MICROS: CounterDesc =
    footfall::counter_desc!("tessellate_micros", footfall::Unit::Micros);
static APP_LOG: EventDesc = footfall::event_desc!("app_log");

static ATLAS: ResourceKindDesc = footfall::resource_kind_desc!("atlas");
static ATLAS_ID: ResourceId = ResourceId::new(9);
static INVALIDATION_FLOW: FlowId = FlowId::new(1);

pub(crate) struct DemoCapture {
    runtime: &'static Runtime,
}

impl DemoCapture {
    pub(crate) fn install(process_name: &str, session_name: &str) -> Self {
        let runtime = Runtime::builder()
            .process_name(process_name)
            .session_name(session_name)
            .counter_capacity(128)
            .event_capacity(256)
            .install_global()
            .expect("global runtime should install once");
        Self { runtime }
    }

    pub(crate) const fn runtime(&self) -> &'static Runtime {
        self.runtime
    }

    /// Records one frame of demo work.
    pub(crate) fn record_frame(&self, frame: u32) {
        let subscriber = tracing_subscriber::registry().with(FootfallLayer::default());
        tracing::subscriber::with_default(subscriber, || self.record_frame_with_tracing(frame));
    }

    fn record_frame_with_tracing(&self, frame: u32) {
        // Register this frame's contexts; they are released when this
        // function returns, and the runtime keeps them until the frame's
        // events are evicted.
        let frame_labels = Context::root()
            .with(&FRAME, frame)
            .with(&TRANSACTION, 1_u32 + frame / 8);
        let record_labels = frame_labels
            .with(&PASS, 1_u32)
            .with(&NODE, 77_u32 + (frame % 5));
        let cache_labels = frame_labels.with(&PASS, 1_u32).with(&CACHE, "glyph_atlas");
        let frame_registration = RegisteredContext::new(&frame_labels);
        let record_registration = RegisteredContext::new(&record_labels);
        let cache_registration = RegisteredContext::new(&cache_labels);
        let atlas_registration = RegisteredContext::new(&cache_labels.with(&RESOURCE, ATLAS_ID));
        let invalidate_registration =
            RegisteredContext::new(&frame_labels.with(&REASON, "layout_invalidation"));
        let frame_context = frame_registration.handle();
        let record_context = record_registration.handle();
        let cache_context = cache_registration.handle();
        let atlas_context = atlas_registration.handle();

        let scratch_metrics = ScratchMetrics::new(&SCRATCH_CURRENT, &SCRATCH_PEAK, &SCRATCH_GROW);
        let cache_metrics =
            CacheMetrics::new(&CACHE_CURRENT, &CACHE_HITS, &CACHE_MISSES, &CACHE_EVICTIONS);

        frame_begin(frame_context);
        pass_begin(record_context);
        flow_begin(INVALIDATION_FLOW, invalidate_registration.handle());
        {
            let _frame_scope = scope(&RECORD_FRAME, frame_context);
            let tracing_frame = tracing::info_span!(
                "session_frame",
                frame = u64::from(frame),
                transaction = u64::from(1_u32 + frame / 8)
            );
            let _entered_frame = tracing_frame.enter();

            {
                let scratch_items = 16_u32 + (frame % 20);
                let _tessellate_scope = scope_with(
                    &TESSELLATE_NODE,
                    record_context,
                    &footfall::args!("scratch_items" => scratch_items),
                );
                let tracing_tessellate =
                    tracing::debug_span!("session_tessellate", paths = u64::from(scratch_items));
                let _entered_tessellate = tracing_tessellate.enter();

                let mut scratch = TrackedScratchVec::<u32>::new(scratch_metrics, record_context);
                scratch.reserve(scratch_items as usize);
                for index in 0..scratch_items {
                    scratch.push(index);
                }
                counter_add(&DRAW_OPS, record_context, u64::from(scratch_items));
                for sample in tessellate_samples(frame) {
                    histogram_record(&TESSELLATE_MICROS, record_context, sample);
                }
                message_with(
                    &APP_LOG,
                    record_context,
                    "tessellation finished",
                    &footfall::args!("items" => scratch_items, "level" => "info"),
                );
                tracing::debug!(
                    scratch_items = u64::from(scratch_items),
                    bytes = u64::from(scratch_items) * 4,
                    "scratch populated"
                );
                flow_step(INVALIDATION_FLOW, record_context);
            }

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
            {
                let _upload_scope = scope(&UPLOAD_ATLAS, atlas_context);
                let upload_bytes = 1_024_u64 * u64::from(1 + (frame % 4));
                counter_add(&UPLOAD_BYTES, atlas_context, upload_bytes);
                resource_update(
                    &ATLAS,
                    ATLAS_ID,
                    atlas_context,
                    resident_bytes + upload_bytes,
                );
                tracing::info!(upload_bytes, "atlas uploaded");
                flow_step(INVALIDATION_FLOW, atlas_context);
            }
            resource_drop(&ATLAS, ATLAS_ID, atlas_context);
            flow_end(INVALIDATION_FLOW, atlas_context);
        }
        pass_end(record_context);
        frame_end(frame_context);
    }
}

fn tessellate_samples(frame: u32) -> [u64; 4] {
    let base = 180_u64 + u64::from(frame % 7) * 10;
    [base, base + 25, base + 40, base + 320]
}
