// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A small fake renderer, instrumented the way a real one would be.
//!
//! Each frame updates, lays out, rasterizes tiles on a pool of worker
//! threads, shapes text through a glyph cache, and composites. Costs vary
//! from frame to frame, and every so often something goes wrong the way it
//! does in real programs: a font change invalidates layout, a shader
//! compiles in the middle of a frame, or an asset streams in. That gives a
//! trace viewer or `footfall_top` something worth looking at.
//!
//! The instrumentation follows the guide: descriptors are `static`, each
//! long-lived owner (the window, each pass, the glyph cache, each worker)
//! registers its context once, frames are marked with `frame_begin_numbered`
//! and `frame_end_numbered`, and the work runs under the owner's handle.

use footfall::{
    CounterDesc, EventDesc, FlowId, LabelKey, RegisteredContext, ResourceId, ResourceKindDesc,
    ScopeDesc, args, context, counter_add, flow_begin, flow_end, flow_step, frame_begin_numbered,
    frame_end_numbered, gauge_set, histogram_record, message_with, resource_create, resource_drop,
    resource_update,
};
use footfall_memory::{CacheMetrics, CacheStats};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

static WINDOW: LabelKey = LabelKey::new("window");
static PASS: LabelKey = LabelKey::new("pass");
static CACHE: LabelKey = LabelKey::new("cache");
static WORKER: LabelKey = LabelKey::new("worker");

static FRAME: ScopeDesc = footfall::scope_desc!("frame", "demo::Renderer::frame");
static UPDATE: ScopeDesc = footfall::scope_desc!("update", "demo::Renderer::frame");
static LAYOUT: ScopeDesc = footfall::scope_desc!("layout", "demo::Renderer::layout");
static PAINT: ScopeDesc = footfall::scope_desc!("paint", "demo::Renderer::paint");
static COMPILE_SHADER: ScopeDesc = footfall::scope_desc!("compile_shader", "demo::Renderer::paint");
static RASTER_TILE: ScopeDesc = footfall::scope_desc!("raster_tile", "demo::worker");
static SHAPE_TEXT: ScopeDesc = footfall::scope_desc!("shape_text", "demo::Renderer::text");
static UPLOAD_GLYPHS: ScopeDesc = footfall::scope_desc!("upload_glyphs", "demo::Renderer::text");
static COMPOSITE: ScopeDesc = footfall::scope_desc!("composite", "demo::Renderer::frame");

static DRAW_CALLS: CounterDesc = footfall::counter_desc!(count, "draw_calls");
static TILES: CounterDesc = footfall::counter_desc!(count, "tiles_rastered");
static UPLOAD_BYTES: CounterDesc = footfall::counter_desc!(bytes, "upload_bytes");
static LAYOUT_NODES: CounterDesc = footfall::counter_desc!(count, "layout_nodes");
static LAYERS: CounterDesc = footfall::counter_desc!(count, "layers");
static TILE_MICROS: CounterDesc = footfall::counter_desc!("tile_micros", footfall::Unit::Micros);
static GLYPH_BYTES: CounterDesc = footfall::counter_desc!(bytes, "glyph_cache_bytes");
static GLYPH_HITS: CounterDesc = footfall::counter_desc!(count, "glyph_hits");
static GLYPH_MISSES: CounterDesc = footfall::counter_desc!(count, "glyph_misses");
static GLYPH_EVICTIONS: CounterDesc = footfall::counter_desc!(count, "glyph_evictions");

static LOG: EventDesc = footfall::event_desc!("renderer");
static ATLAS_PAGE: ResourceKindDesc = footfall::resource_kind_desc!("atlas_page");

/// Tiles rasterized per frame.
const TILES_PER_FRAME: u64 = 16;
/// Bytes in one glyph atlas page.
const ATLAS_PAGE_BYTES: u64 = 256 * 1024;
/// Atlas pages the glyph cache keeps before evicting the oldest.
const MAX_ATLAS_PAGES: usize = 4;

/// The demo renderer. Create it after installing a backend, so its contexts
/// are registered with that backend.
pub struct Renderer {
    rng: Rng,
    frame: u64,
    window: RegisteredContext,
    layout: RegisteredContext,
    paint: RegisteredContext,
    composite: RegisteredContext,
    text: RegisteredContext,
    glyphs: CacheStats,
    atlas_pages: Vec<ResourceId>,
    next_page: u64,
    next_flow: u64,
    workers: Vec<Worker>,
    finished: mpsc::Receiver<()>,
}

impl Renderer {
    /// Starts a renderer with `worker_count` raster threads.
    #[must_use]
    pub fn new(worker_count: usize) -> Self {
        let window_context = context!(WINDOW = "main");
        let window = RegisteredContext::new(&window_context);
        let pass = |name: &'static str| RegisteredContext::new(&window_context.with(&PASS, name));
        let layout = pass("layout");
        let paint = pass("paint");
        let composite = pass("composite");
        let text = RegisteredContext::new(&window_context.with(&CACHE, "glyphs"));
        let glyphs = CacheStats::new(
            CacheMetrics::new(&GLYPH_BYTES, &GLYPH_HITS, &GLYPH_MISSES, &GLYPH_EVICTIONS),
            text.handle(),
        );

        let (done, finished) = mpsc::channel();
        let workers = (0..worker_count)
            .map(|index| Worker::start(index, paint.context(), done.clone()))
            .collect();
        Self {
            rng: Rng(0x9e37_79b9_7f4a_7c15),
            frame: 0,
            window,
            layout,
            paint,
            composite,
            text,
            glyphs,
            atlas_pages: Vec::new(),
            next_page: 1,
            next_flow: 1,
            workers,
            finished,
        }
    }

    /// Renders frames at `fps` until `stop` is set or `frames` have been
    /// rendered, sleeping between frames to keep the pace.
    pub fn run(&mut self, fps: u32, frames: Option<u64>, stop: &AtomicBool) {
        let interval = Duration::from_secs(1) / fps.max(1);
        let start = Instant::now();
        let mut rendered = 0_u64;
        while !stop.load(Ordering::Relaxed) && frames.is_none_or(|frames| rendered < frames) {
            self.frame();
            rendered += 1;
            // Keep the pace, but never try to catch up after a slow frame.
            let due = start + interval * u32::try_from(rendered).unwrap_or(u32::MAX);
            match due.checked_duration_since(Instant::now()) {
                Some(wait) => thread::sleep(wait),
                None => thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    /// Renders one frame.
    pub fn frame(&mut self) {
        self.frame += 1;
        let window = self.window.handle();
        // The renderer's own frame number, so later observations of this
        // frame, such as when it was shown, can find it.
        frame_begin_numbered(self.frame, window);
        {
            footfall::scope!(&FRAME, window);
            {
                footfall::scope!(&UPDATE, window);
                busy(self.rng.micros(150, 500));
            }
            let invalidated = self.layout();
            self.paint(invalidated);
            self.text();
            {
                footfall::scope!(&COMPOSITE, self.composite.handle());
                gauge_set(&LAYERS, self.composite.handle(), 3 + self.rng.below(4));
                sleep(self.rng.micros(400, 900));
            }
        }
        frame_end_numbered(self.frame, window);
    }

    /// Lays out the window. Every few hundred frames a font change
    /// invalidates everything, which starts a flow that paint finishes.
    fn layout(&mut self) -> Option<FlowId> {
        let layout = self.layout.handle();
        let storm = self.frame.is_multiple_of(240);
        let flow = storm.then(|| {
            let flow = FlowId::new(self.next_flow);
            self.next_flow += 1;
            message_with(
                &LOG,
                layout,
                "font changed; relayout everything",
                &args!("level" => "warn"),
            );
            flow_begin(flow, layout);
            flow
        });
        let nodes = if storm {
            12_000
        } else {
            800 + self.rng.below(400)
        };
        footfall::scope!(&LAYOUT, layout, args!("nodes" => nodes));
        counter_add(&LAYOUT_NODES, layout, nodes);
        if let Some(flow) = flow {
            flow_step(flow, layout);
        }
        sleep(if storm {
            self.rng.micros(22_000, 34_000)
        } else {
            self.rng.micros(900, 2_200)
        });
        flow
    }

    /// Paints: fans tiles out to the workers and waits for them. Now and
    /// then a shader compiles first.
    fn paint(&mut self, invalidated: Option<FlowId>) {
        let paint = self.paint.handle();
        footfall::scope!(&PAINT, paint);
        if let Some(flow) = invalidated {
            flow_end(flow, paint);
        }
        if self.frame % 500 == 77 {
            footfall::scope!(&COMPILE_SHADER, paint);
            message_with(
                &LOG,
                paint,
                "compiling shader variant",
                &args!("variant" => self.frame / 500),
            );
            sleep(self.rng.micros(9_000, 15_000));
        }
        for tile in 0..TILES_PER_FRAME {
            let worker = &self.workers[usize::try_from(tile).unwrap_or(0) % self.workers.len()];
            worker.send(self.rng.micros(250, 1_300));
        }
        for _ in 0..TILES_PER_FRAME {
            let _ = self.finished.recv();
        }
        counter_add(&DRAW_CALLS, paint, 40 + self.rng.below(60));
    }

    /// Shapes text through the glyph cache. Misses upload glyphs to an
    /// atlas page; a full atlas evicts its oldest page.
    fn text(&mut self) {
        let text = self.text.handle();
        footfall::scope!(&SHAPE_TEXT, text);
        sleep(self.rng.micros(200, 600));
        let lookups = 20 + self.rng.below(20);
        let mut misses = 0;
        for _ in 0..lookups {
            // Mostly warm, with bursts of new glyphs.
            if self.rng.below(100) < if self.frame % 90 < 3 { 60 } else { 4 } {
                self.glyphs.record_miss();
                misses += 1;
            } else {
                self.glyphs.record_hit();
            }
        }
        if misses == 0 {
            return;
        }
        footfall::scope!(&UPLOAD_GLYPHS, text, args!("glyphs" => misses));
        let bytes = misses * 2_048;
        counter_add(&UPLOAD_BYTES, text, bytes);
        if self.atlas_pages.is_empty() || misses > 8 {
            if self.atlas_pages.len() == MAX_ATLAS_PAGES {
                let oldest = self.atlas_pages.remove(0);
                resource_drop(&ATLAS_PAGE, oldest, text);
                self.glyphs
                    .record_eviction(ATLAS_PAGE_BYTES * (MAX_ATLAS_PAGES as u64 - 1));
            }
            let page = ResourceId::new(self.next_page);
            self.next_page += 1;
            resource_create(&ATLAS_PAGE, page, text, ATLAS_PAGE_BYTES);
            self.atlas_pages.push(page);
        }
        if let Some(&page) = self.atlas_pages.last() {
            resource_update(&ATLAS_PAGE, page, text, ATLAS_PAGE_BYTES);
        }
        self.glyphs
            .observe_resident_bytes(ATLAS_PAGE_BYTES * self.atlas_pages.len() as u64);
        sleep(Duration::from_micros(bytes / 16));
    }
}

impl core::fmt::Debug for Renderer {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Renderer")
            .field("frame", &self.frame)
            .field("workers", &self.workers.len())
            .finish_non_exhaustive()
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        for worker in self.workers.drain(..) {
            worker.stop();
        }
    }
}

/// One raster thread, with its own context under the paint pass.
struct Worker {
    jobs: mpsc::Sender<Duration>,
    thread: JoinHandle<()>,
}

impl Worker {
    fn start(index: usize, paint: &footfall::Context, done: mpsc::Sender<()>) -> Self {
        let (jobs, incoming) = mpsc::channel::<Duration>();
        let context = paint.with(&WORKER, index as u64);
        let thread = thread::Builder::new()
            .name(format!("raster-{index}"))
            .spawn(move || {
                let worker = RegisteredContext::new(&context);
                for cost in incoming {
                    let began = Instant::now();
                    {
                        footfall::scope!(&RASTER_TILE, worker.handle());
                        sleep(cost);
                    }
                    let took = u64::try_from(began.elapsed().as_micros()).unwrap_or(u64::MAX);
                    histogram_record(&TILE_MICROS, worker.handle(), took);
                    counter_add(&TILES, worker.handle(), 1);
                    if done.send(()).is_err() {
                        break;
                    }
                }
            })
            .expect("worker threads should start");
        Self { jobs, thread }
    }

    fn send(&self, cost: Duration) {
        self.jobs
            .send(cost)
            .expect("workers run until the renderer drops");
    }

    fn stop(self) {
        drop(self.jobs);
        let _ = self.thread.join();
    }
}

/// Simulates work that blocks, such as waiting on the GPU.
fn sleep(duration: Duration) {
    thread::sleep(duration);
}

/// Simulates work that keeps the CPU busy.
fn busy(duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        std::hint::spin_loop();
    }
}

/// A small deterministic generator, so every run looks alike.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }

    fn micros(&mut self, low: u64, high: u64) -> Duration {
        Duration::from_micros(low + self.below(high - low))
    }
}
