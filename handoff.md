# Footfall — Detailed Handoff for a Family of Instrumentation and Observability Crates

## Purpose

`footfall` is a family of Rust crates for low-overhead instrumentation, semantic tracing, counters, memory observation, and downstream projection into profiling/debugging/visualization tools.

The goal is **not** to build “yet another profiler” in the narrow sense. The goal is to build a **slim instrumentation seam** with a **semantic context model** that can support:

* profiling and performance analysis
* memory and allocation churn analysis
* scene / draw / layout / cache / GPU inspection
* invalidation and causality debugging
* trace export into existing ecosystems
* future interactive visual tooling

This should fit our normal tenets:

* durable and hard to outgrow
* modular rather than monolithic
* explicit over implicit
* introspection-first
* calm public APIs
* `no_std`-friendly in the core where possible
* low allocation and low overhead in hot paths
* replaceable backends and sinks

The core differentiator is that Footfall should make it possible to say not merely **that** work happened, but **what semantic owner or context it happened under**.

Examples:

* this draw op came from this layer subtree
* this upload came from this resource owner
* this cache growth happened during this pass
* this scratch churn occurred while tessellating this display node
* this invalidation propagated from this cause through these downstream scopes

That semantic layer is the gap in most existing tools. Existing tools are strong at call stacks, threads, processes, allocators, and sometimes GPU scopes. Far fewer are strong at domain-specific ownership in UI / rendering / scene / runtime systems.

---

## Prior Art and Positioning

Footfall should borrow aggressively, but not imitate blindly.

Relevant traditions:

* **DTrace / USDT** — static probes, dynamic enablement, small call-site seam
* **ETW / EventPipe / EventSource** — typed events, activities, counters, session control
* **LTTng-UST** — low-overhead userspace tracing with tracepoints
* **Chrome trace / Perfetto** — slices, counters, tracks, flows, export/import ecosystem
* **Tracy** — strong developer-facing interactive UX; explicit zones, plots, source locations
* **jemalloc profiling** — sampled heap profiling, allocation profiling lessons
* **SBCL / Common Lisp allocation profiling** — bytes-consed / churn mindset

Practical synthesis:

* Steal the **tracepoint discipline** from DTrace / ETW / LTTng / Chrome trace macros.
* Steal the **trace model** from Perfetto: slices, counters, tracks, resources, flows where appropriate.
* Steal the **interactive developer instincts** from Tracy.
* Steal the **allocation profiling and sampling lessons** from jemalloc and SBCL.
* Add a **semantic context model** for UI / rendering / scene / cache / GPU / invalidation ownership.

That last item is what makes this ours.

---

## High-Level Goals

Footfall should make these possible:

1. **Very slim instrumentation seam**

   * negligible cost when disabled
   * low fixed cost in lightweight modes
   * richer capture only when explicitly enabled

2. **Strong semantic attribution**

   * work, counters, and resources are associated with structured context
   * context is not just strings; it is compact, composable, and stable enough to aggregate on

3. **Support multiple observation styles**

   * counters/gauges
   * scopes / spans / slices
   * resource lifecycle
   * memory snapshots and churn
   * optional sampled or full heap profiling later

4. **Bounded, predictable runtime cost**

   * no allocation in hot emission paths
   * no formatting in hot emission paths
   * static descriptors rather than ad hoc strings
   * bounded buffers and explicit dropped-event accounting

5. **Good downstream interoperability**

   * export to existing trace/timeline ecosystems early
   * do not require a bespoke viewer for initial value

6. **Clear crate seams**

   * small, `no_std`-friendly core
   * runtime pieces outside the core
   * memory helpers separate from the base instrumentation substrate
   * exporters/projection bridges separate again

---

## Non-Goals

Footfall should **not** initially be:

* a giant monolithic tracing framework
* a replacement for every OS/system tracing facility
* structured logging with better branding
* a custom visualization app before the data model has proven itself
* a universal heap-profiler-first system
* a dependency magnet dragged through every low-level crate

Footfall should complement other tools, not try to swallow everything at once.

---

## Design Principles

### 1. The emission seam must be boring

Call sites should lower to tiny inline operations over static descriptors and compact ids.

Avoid:

* dynamic string formatting
* trait-object observers in hot paths
* hidden allocation
* locks in common hot paths if avoidable
* heavyweight generic event builders

### 2. Counters matter more than people first expect

Many useful observations do not require per-event recording.

Examples:

* scratch peak bytes
* cache current bytes
* cache hit/miss/evict counts
* upload bytes per frame
* generated vertices/path segments
* damaged rect count

A good Footfall design should make counters first-class, not force everything into per-event traces.

### 3. Semantic context is the real differentiator

Context is not garnish. It is the connective tissue between low-level observation and meaningful analysis.

Examples of contextual dimensions:

* frame / transaction / pass
* scene / layer / widget / node
* slot / index / child ordinal
* object / entity / resource owner
* invalidation cause
* source location / span
* subsystem
* cache name or resource kind

### 4. Instrumentation levels should be explicit

There should be clear modes, for example:

* `off`
* `counters`
* `scoped`
* `trace`

The goal is to let code carry lightweight instrumentation all the time without fear, and only turn on richer capture when debugging requires it.

### 5. Allocation profiling belongs later and should be isolated

Global allocator integration is valuable but expensive and noisy.

It should be:

* feature-gated
* optional
* layered in after the core and first proving grounds are solid
* available in aggregate, sampled, and maybe full modes later

### 6. Export early, custom UI later

We should get value quickly by exporting into known timeline/trace ecosystems. That helps validate the data model before we invest in bespoke UX.

---

## Proposed Crate Family

This section proposes a crate family. Names can shift, but the split matters.

### `footfall_core`

The tiny core substrate.

Responsibilities:

* core id types
* static descriptor types
* context representation primitives
* instrumentation level model
* tiny emission primitives
* compile-time disablement
* no-op backend path
* zero/near-zero-cost disabled behavior

Constraints:

* `no_std` + `alloc` if possible, ideally `no_std`-first
* no exporters
* no runtime-heavy buffering machinery
* no allocator hooks
* no UI assumptions

### `footfall_runtime`

Practical collection/runtime support.

Responsibilities:

* sink registration / selection
* per-thread or low-contention event append paths
* counter banks / aggregation
* ring buffers
* dropped-event accounting
* frame snapshots / flush points
* timestamps if not part of the core abstraction

Constraints:

* should still avoid hot-path allocation
* bounded memory
* clear overflow/drop semantics
* replaceable sink strategy

### `footfall_memory`

Memory-specific helpers and wrappers.

Responsibilities:

* tracked scratch buffers / scratch stats wrappers
* cache observation helpers
* GPU / GPU-adjacent memory accounting helpers
* retained-vs-transient memory metric support
* later: allocator integration

This crate should make it easy for subsystems to expose memory data without each one inventing ad hoc counters and taxonomies.

### `footfall_export`

Projection/export bridge.

Responsibilities:

* export to an existing trace/timeline ecosystem
* possibly snapshot export for counters/resources/memory
* trace-model lowering from Footfall’s internal representation

This should appear early because it gives us leverage without requiring a custom viewer.

### `footfall_macros`

Optional ergonomic macro front-end.

Responsibilities:

* lightweight call-site macros
* descriptor definition helpers if useful
* source location capture helpers where appropriate

This crate should remain thin and not become the real architecture.

### Possible later crates

#### `footfall_alloc`

Allocator instrumentation, if we decide it deserves its own isolation boundary.

Responsibilities:

* global allocator wrapper
* aggregate alloc/free/churn counts
* optional sampled attribution
* maybe later full event emission for debugging

#### `footfall_view` or domain-specific viewers later

Not phase 1. This is where a custom viewer could eventually live if we outgrow export-only workflows.

---

## Proposed Core Vocabulary

We should start with a very small set of observation primitives.

### IDs

At minimum:

* `ContextId`
* `ScopeDescId`
* `EventDescId`
* `CounterDescId`
* `ResourceId`
* `ResourceKindId`
* `SpanId`
* `SubsystemId` (optional early, likely useful)
* `FrameId` / `TransactionId` if explicit ids are helpful

These should be integer-backed newtypes with calm, obvious APIs.

### Descriptors

Descriptors should be static metadata known at compile time.

At minimum:

* `ScopeDesc`
* `CounterDesc`
* `EventDesc`
* `ResourceKindDesc`

Likely descriptor fields:

* numeric id
* stable name
* unit / kind where relevant
* maybe subsystem/category
* maybe source-location metadata if captured statically

Avoid dynamic names in hot paths.

### Units / metric kinds

For counters/gauges, a basic units vocabulary is useful.

Examples:

* bytes
* count
* nanoseconds / microseconds
* ratio / percentage
* items
* vertices
* paths
* glyphs
* rects

This helps exporters and later tools present data consistently.

### Primitive observation operations

Initial surface should be no more than:

* `counter_add`
* `gauge_set`
* `scope_begin`
* `scope_end`
* `resource_create`
* `resource_update`
* `resource_drop`
* `frame_marker`

Optional later additions:

* flows / causal links
* point/instant events
* sampled events
* memory snapshots as a more specialized first-class operation

But we should resist growing the primitive set too early.

---

## Context Model

This is the heart of the design.

### Why context matters

Call stacks are useful, but they do not line up well with how people understand rendering or UI systems.

People want answers like:

* which widget subtree was expensive?
* which display node caused this scratch growth?
* which invalidation cause led to this pass doing too much work?
* which cache is associated with this phase or owner?

That requires semantic context.

### Context requirements

The context model should be:

* compact
* composable
* stable enough to aggregate on
* representable without heap allocation in hot paths
* passable explicitly where needed
* optionally support scoped entry/exit ergonomics

### Candidate shape

A context can be modeled as a parent-linked chain of typed labels.

Conceptually:

* parent context
* label key
* typed value

This allows a compact immutable context id that refers to a chain or interned structure.

Potential context dimensions:

* `frame`
* `transaction`
* `pass`
* `subsystem`
* `scene`
* `layer`
* `widget`
* `node`
* `slot`
* `index`
* `entity`
* `resource`
* `cache`
* `span`
* `invalidation_reason`

We should keep the representation generic enough to add domain labels later without redesigning the whole substrate.

### Explicit context vs scoped context

Footfall should likely support both:

1. **Explicit context passing**

   * good for hot paths
   * clearer cost model
   * easier to reason about in performance-sensitive code

2. **Scoped / RAII-style context helpers**

   * good for higher-level code
   * ergonomic for nested operations
   * convenient when building traces around larger blocks

But explicit context should be the real core representation.

### Typed values vs strings

Avoid string soup.

Where possible, context values should be compact ids or typed numerics rather than ad hoc strings.

Examples:

* `slot: u32`
* `index: u32`
* `entity: u64`
* `resource: ResourceId`
* `span: SpanId`
* `pass: PassKind`
* `subsystem: SubsystemId`

Strings may still exist for some labels, but they should not dominate the hot-path representation.

---

## Instrumentation Levels

We should make levels explicit and documented.

A likely first model:

### `Off`

* compiles away or becomes no-op with near-zero residue
* no timestamps
* no event writes
* no hot-path allocation

### `Counters`

* counters and gauges only
* no per-event traces
* suitable for always-on use in many builds
* should be very low overhead

### `Scoped`

* begin/end scopes plus counters
* useful for timeline-style analysis without full trace richness

### `Trace`

* full event/scope/resource capture
* richer context and metadata
* intended for diagnostic sessions

We may ultimately want build-time and runtime control layered together:

* compile-time feature to include/exclude instrumentation machinery at all
* runtime level to decide how much capture is active

That combination gives us better control over the disabled path.

---

## Emission Seam Design

This is a critical design area.

### Requirements

Hot-path emission should:

* use static descriptors
* avoid dynamic formatting
* avoid allocation
* be inlineable
* branch minimally
* not lock in the common case if avoidable
* degrade gracefully when buffers are full

### Suggested shape

Use macros for ergonomics, but make the real implementation inline functions over descriptor references and compact ids.

Call sites should feel something like:

* enter a scope
* add to a counter
* set a gauge
* create/update/drop a resource

We do not need a giant builder API.

### Source location capture

Source location can be useful, especially for developer tooling, but it is not free.

We should support optional source-location metadata in descriptors/macros rather than force it everywhere.

There should be a clean path for:

* no source location
* static source location captured by macro
* maybe richer span/source mapping later

### Sink selection

The hot path should not be polluted by a large abstraction tower.

The practical implementation could use:

* a no-op backend
* a static sink table or function-pointer table
* per-thread event buffers
* a counter bank

What matters is that the common call path remains tiny and predictable.

### Overflow / dropped data

Buffers will fill sometimes. That is fine.

Footfall should:

* drop gracefully when necessary
* maintain explicit dropped-event counters
* prefer losing some rich trace data over unbounded memory growth or hidden allocation

---

## Runtime Collection Design

`footfall_runtime` should be intentionally modest in its first cut.

### Phase-1 runtime pieces

* no-op sink
* counter aggregation bank
* bounded ring buffer for scopes/events/resources
* frame markers / flush boundaries
* snapshot extraction for export

### Requirements

* no unbounded growth
* no hidden hot-path allocation
* preferably per-thread writes or otherwise low contention
* bounded backpressure story
* simple enough to trust and debug

### Threading model

We do not need to solve all threading complexity on day 1.

A good start:

* per-thread event append buffers or local sinks where practical
* merge/snapshot later
* counters may be per-thread then reduced, or handled via a lightweight shared structure depending on cost and ergonomics

The main requirement is that we do not accidentally insert lock contention into the hottest paths.

---

## Memory Observation Model

Footfall needs a disciplined memory story, but not an overcomplicated one.

### Memory categories

A simple initial taxonomy:

* `Scratch`
* `Cache`
* `Gpu`
* `GpuAux`
* `Heap`

We may refine later, but this is enough to start.

### Why categories matter

These categories let us distinguish:

* transient working memory
* retained caches
* logical GPU resources
* CPU-side GPU-adjacent support memory
* generic heap churn

This distinction is important because many memory pathologies are only obvious when you separate retained growth from transient churn.

### Standard memory metrics

Useful standard observations include:

* current bytes
* peak bytes
* bytes allocated total
* bytes freed total
* net delta
  n- bytes churned total
* allocation count
* grow count / realloc count
* cache hit count
* cache miss count
* cache eviction count

Not every subsystem needs every metric, but the vocabulary should be coherent.

### Wrappers/helpers first

Before allocator interception, instrument with wrappers/helpers for things we control:

* scratch vectors/arenas/work buffers
* caches and atlases
* upload staging buffers
* GPU resource allocators/pools

That gets us substantial value without the complexity of a global allocator hook.

### Heap profiling later

Allocator integration should likely be phased:

1. aggregate totals only
2. sampled attribution
3. optional full tracing

And it should be isolated enough not to distort the simpler, lower-overhead paths.

---

## Resource Model

Footfall should treat resources as first-class because a lot of rendering/system debugging is really resource debugging.

Examples:

* buffers
* textures
* atlases
* render targets
* caches
* upload batches
* maybe scene/display resources

At minimum, we want:

* create
* update
* drop
* current size / logical size where relevant
* context / owner association

This makes it possible to build:

* resource lifetime views
* upload ownership summaries
* memory-by-resource-kind reports
* retained-vs-transient comparisons

---

## First Proving Grounds

We should not instrument everything at once.

The first proving grounds should be places where the value is immediate and the semantics are already meaningful.

### 1. Imaging draw recording

This is likely the best first domain target.

Why:

* draw recording already has meaningful semantic boundaries
* draw ops, clips, groups, text runs, image draws, etc. are naturally inspectable
* we can attach context and see value quickly

Potential instrumentation:

* op counts by subtree / owner
* scope timing for recording passes
* scratch usage during tessellation or shaping
* resource references and uploads
* group/clip nesting

### 2. Scratch buffers and retained caches

This is the easiest early memory win.

Track:

* peak bytes
* grow counts
* current resident bytes
* hit/miss/eviction for caches
* bytes churned per frame/pass

This gives immediate leverage for performance and memory debugging.

### 3. GPU resource lifecycle and upload accounting

Track:

* logical creation/update/drop of buffers/textures/atlases
* upload bytes
* update frequency
* resource ownership context

Backend truth about physical GPU memory may vary, but our own logical resource accounting will still be useful.

### 4. Frame / transaction / pass markers

Without these, traces are mushy.

Need standard boundaries for:

* frame begin/end
* transaction begin/end
* pass begin/end

This provides structure for both counters and timelines.

---

## Export / Projection Strategy

We should target an existing ecosystem early.

### Why export early matters

* validates the data model
* avoids building a custom UI too soon
* lets engineers get value quickly
* grounds the design against real visualization constraints

### Desired early output forms

Footfall should be able to project into something that supports:

* nested slices/scopes
* counters/gauges over time
* resource events or equivalent tracks
* frame/pass markers

### Future custom views

Later, once the data model is proven, we can build more domain-specific projections, such as:

* subtree cost heatmaps
* invalidation propagation views
* cache residency summaries
* upload ownership maps
* “why was this pixel drawn?”-style ancestry inspection

But those are phase 2+ concerns.

---

## Suggested Incremental Plan

### Phase 0 — Design note and API discipline

Write a crisp design note before implementation.

It should cover:

* goals and non-goals
* crate split
* prior art influences
* core observation primitives
* context model
* instrumentation levels
* runtime/sink assumptions
* first proving grounds
* what “low overhead” means operationally

This handoff can seed that design note.

### Phase 1 — `footfall_core`

Build the small core:

* id types
* descriptor types
* instrumentation levels
* context representation
* tiny emission surface
* compile-time disablement / no-op path

Success criteria:

* calm public API
* small surface area
* no hot-path allocation
* disabled mode compiles away cleanly
* no_std-friendly core

### Phase 2 — `footfall_runtime`

Build a minimal runtime:

* counter bank
* bounded ring buffer
* dropped-event accounting
* snapshot or flush capability
* basic sink wiring

Success criteria:

* can record real traces/counters from a small example
* bounded memory
* understandable behavior under pressure

### Phase 3 — initial domain instrumentation

Instrument:

* imaging draw recording
* selected scratch buffers
* selected caches
* GPU resource creation/update/drop
* upload bytes
* frame/pass markers

Success criteria:

* can answer at least one real performance/debugging question from actual code
* data is semantically meaningful
* overhead stays sane

### Phase 4 — export bridge

Build the first export/projection bridge.

Success criteria:

* can inspect real traces in an existing toolchain or format
* engineers learn something useful from a trace immediately

### Phase 5 — memory helpers expansion

Broaden `footfall_memory` wrappers:

* more cache types
* scratch helper types
* staged GPU memory/accounting helpers
* retained-vs-transient reports

### Phase 6 — allocator integration

Add allocator instrumentation carefully:

* aggregate totals first
* sampled attribution second
* optional full tracing later

Success criteria:

* valuable heap-churn insights
* controlled overhead
* isolation from core design remains intact

---

## Questions to Resolve Early

These are design decisions that materially affect implementation shape.

### 1. What is the context representation?

Do we commit to:

* explicit context ids
* scoped push/pop helpers
* both

Recommendation: both, with explicit context ids as the real core.

### 2. How static are descriptors?

Recommendation: strongly static. Dynamic names/strings should be discouraged in hot paths.

### 3. How much should counters dominate phase 1?

Recommendation: a lot. Counters and gauges should be first-class and sufficient for many early use cases.

### 4. What level of `no_std` do we want?

Recommendation: `footfall_core` should be `no_std`-friendly. Runtime/export/view layers can be looser.

### 5. What external trace/projection format do we target first?

Recommendation: choose early so the internal event model is checked against real projection needs.

### 6. What should be phase-1 source location support?

Recommendation: optional static source-location capture via macros/descriptors, not mandatory at every emission point.

### 7. How much should Footfall know about domain labels up front?

Recommendation: core remains generic, but we should define a small shared vocabulary and conventions for common labels used in rendering/UI domains.

---

## API Style Guidance

The public API should feel calm and direct.

Desired qualities:

* short function names where the meaning is obvious
* integer-backed ids and static descriptors
* minimal generic cleverness
* easy to use in hot code without fear
* easy to ignore if a crate does not need richer instrumentation

Avoid:

* giant builder types
* excessive trait gymnastics
* trying to encode all possible future tracing semantics into phase 1
* forcing a logging mindset onto users

The mental model should be closer to tracepoints + counters + resource/accounting seams than to “structured logging framework.”

---

## Testing and Validation

Footfall needs validation at several levels.

### Correctness tests

* ids and descriptors behave predictably
* contexts compose and resolve correctly
* sink behavior is correct under normal and overflow conditions
* dropped-event accounting is correct
* exported traces are well-formed

### Overhead tests

We should explicitly measure:

* disabled overhead
* counters-only overhead
* scoped overhead
* trace overhead

This matters both for honesty and for design discipline.

### Wind-tunnel style benchmarks

Benchmarks should live outside core crates in separate workspace benches/examples as per our normal preferences.

Measure things like:

* event append throughput
* counter update cost
* scope enter/exit cost
* context construction or reuse cost
* scratch wrapper overhead
* export cost

### Real-world validation

Instrument one or two meaningful subsystems and make sure the traces answer real questions. Synthetic microbenchmarks alone will not prove the design.

---

## Documentation Expectations

Because this will become infrastructure, docs matter.

At minimum we should document:

* crate purpose and boundaries
* instrumentation levels
* when to use counters vs scopes vs resources
* context conventions
* memory category conventions
* sink/runtime behavior
* overhead expectations and tradeoffs
* examples for always-on lightweight instrumentation and richer diagnostic tracing

Public APIs should be documented from the start.

---

## Example Usage Scenarios We Want to Enable

These are the kinds of workflows Footfall should eventually support.

### Rendering / imaging

* See which subtree emitted the most draw ops
* attribute scratch churn during tessellation or shaping
* correlate upload bytes with resource owners
* identify expensive recording passes

### Cache tuning

* see current resident bytes per cache
* compare hit/miss/eviction behavior across runs
* detect caches that retain lots of memory without buying enough hits

### Memory analysis

* transient vs retained memory over time
* bytes churned per frame or pass
* later: sampled heap churn attributed to semantic contexts

### Invalidation / causality debugging

* understand which invalidation causes propagate to which work scopes
* correlate invalidation storms with downstream recording/upload behavior

### Resource debugging

* track create/update/drop lifecycles of logical GPU resources
* inspect update frequency and ownership
* understand atlas or target growth under real workloads

---

## What Not to Do in the First Implementation

Do not start with:

* full allocator interception
* custom GUI viewer as the primary path to value
* fully generic “event with arbitrary argument bag” APIs everywhere
* pervasive dynamic string usage
* instrumentation in every subsystem at once
* too many crate splits before the first real path is proven

The first implementation should be small, opinionated, and grounded.

---

## Suggested First Deliverables

A good first handoff-to-implementation target would be:

1. `footfall_core` skeleton with docs and tests
2. `footfall_runtime` minimal counter bank + ring buffer
3. one or two macros in `footfall_macros`
4. a tiny example that emits counters/scopes/resources
5. instrumentation of one real subsystem in imaging
6. one export path to an existing trace/timeline ecosystem
7. basic write-up of conventions for context labels and memory categories

That is enough to create a feedback loop.

---

## Closing Direction

Footfall should be treated as a **family of small, composable observability crates** whose foundation is a low-overhead instrumentation seam and whose real power comes from semantic context.

If we get the core right, this becomes useful far beyond “profiling”:

* profiling
* tracing
* memory accounting
* resource inspection
* visualization
* invalidation debugging
* future domain-specific tooling

The main architectural discipline is simple:

* keep the seam tiny
* keep the descriptors static
* let counters do a lot of work
* make context meaningful
* export early
* defer allocator complexity
* grow from real proving grounds rather than abstract completeness

That should give us something durable and meaningfully different from generic tracing infrastructure while still standing on strong prior art.
