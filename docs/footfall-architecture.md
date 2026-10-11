# Footfall Architecture

Status: working design, aligned with the current workspace

## Overview

Footfall is a family of small Rust crates for instrumentation and observability. Its job is not to replace system profilers or become a generic logging framework. Its job is to provide a slim, durable instrumentation seam that can attach low-level observations to semantic ownership.

The key question Footfall should answer is not only "what happened?" but "what did it happen under?" A draw, upload, cache resize, scratch burst, or invalidation storm is much more useful when it is attributed to a pass, subtree, owner, cache, or resource kind instead of only a thread and call stack.

The first durable slice stays narrow:

- a `no_std`-friendly core
- a thin top-level `footfall` facade
- explicit instrumentation levels
- counters, scopes, and resource lifecycle as the only core observation families
- export into existing ecosystems before any custom viewer work

Footfall explicitly does not own OS-wide tracing, full heap profiling on day 1, or a universal arbitrary-event schema.

## Fence

`footfall_core` owns ids, static descriptors, context values, instrumentation levels, and the tiny emission ABI; it explicitly does not own runtime buffering, export formats, clocks, allocator interception, or domain-specific viewer logic.

`footfall` owns the recommended user-facing surface and re-exports; it explicitly does not add a second architecture beside the core.

`footfall_runtime` owns bounded in-process collection, timestamping, context
interning, and snapshot extraction; it explicitly does not own descriptor
semantics, transport protocols, exporters, allocator interception, or UI policy.

Downstream crates own memory helpers, export, live backends, and transport as
separate seams; they do not redefine the core vocabulary.

## First Read

The architectural bet is simple:

- keep the call-site seam tiny and boring
- make counters first-class
- make semantic context explicit
- keep disabled and lightweight modes cheap enough to leave instrumentation in the codebase
- validate the model by exporting into existing tooling early

That led to the current crate family:

1. `footfall_core` defines ids, descriptors, labels, levels, and inline emission functions.
2. `footfall` re-exports the calm API that most callers use.
3. `footfall_runtime` provides bounded buffers, counter banks, clocks, and snapshots.
4. `footfall_memory` standardizes scratch and cache helpers without pulling dependencies into core crates.
5. `footfall_export` projects captured data into Chrome Trace Event JSON.
6. `footfall_session` defines a versioned, transport-neutral consumer contract.
7. `footfall_session_wtransport` provides a localhost attach path for discovery and snapshot fetch.
8. `footfall_tracy` and `footfall_fanout` prove live backend composition without changing core.
9. `footfall_macros` remains intentionally absent until the raw call-site seam has more real use.

The current workspace has moved beyond the original two-crate starting point.
That is acceptable because the extra crates are downstream seams rather than a
second architecture. The remaining discipline is to avoid adding more crates or
surface area until benchmarks and one real subsystem prove the model.

## Goals

- Near-zero disabled overhead with no hot-path allocation.
- Explicit instrumentation levels from `Off` through richer trace capture.
- Semantic attribution through compact, composable context.
- First-class counters and gauges, not trace-only thinking.
- Bounded runtime memory with explicit dropped-data accounting.
- Early export to an existing trace or timeline format.
- `no_std`-friendly foundations and replaceable runtime backends.

## Non-Goals

- Replacing ETW, DTrace, LTTng, or other system tracing stacks.
- Starting with a custom GUI viewer.
- Making arbitrary dynamic event bags the central API.
- Requiring allocator interception for initial usefulness.
- Instrumenting every subsystem before the seam proves itself.
- Pulling large dependencies into foundational crates.

## Invariants

- Core emission never performs hidden allocation.
- Hot-path emission never requires dynamic string formatting.
- Core descriptors are static and stable for the life of the process.
- Rich trace data may be dropped under pressure; memory growth must stay bounded.
- Counter-only collection remains useful without a trace buffer.
- Context is composable and explicit, even when scoped helpers exist.
- Domain vocabulary lives in conventions and descriptors, not in hard-coded core enums for every product concept.

## Alternatives Considered

### Option A: Monolithic tracing crate

One crate owns core types, runtime collection, exporters, macros, and memory instrumentation.

Why reject it:

- bloats the dependency surface of low-level code
- makes `no_std` support harder
- couples hot-path emission to runtime and export concerns
- encourages "just add one more feature" architecture drift

### Option B: TLS-scoped tracing first

Make scoped entry/exit and thread-local context the primary model, then layer explicit ids later.

Why reject it:

- makes cost less obvious in hot paths
- weakens portability to async, batch, or explicit ownership-heavy code
- turns the real semantic model into an implicit side channel

### Option C: Explicit core context plus optional scoped helpers

Use explicit `Context` values and static descriptors as the real substrate. Add ergonomic scoped helpers and macros later.

Why choose it:

- preserves a predictable hot-path cost model
- works in `no_std`-friendly and performance-sensitive code
- supports higher-level ergonomic wrappers without making them foundational
- keeps the architecture aligned with tracepoint-style prior art

## Chosen Design

### Crate Boundaries

#### `footfall_core`

Owns:

- integer-backed id newtypes
- static descriptor types
- label keys and label values
- explicit context values and optional canonical context ids
- instrumentation level model
- inline emission functions, the backend ABI, and compile-time no-op paths

Does not own:

- clocks or timestamps beyond abstract hooks if needed
- ring buffers, sinks, snapshots, or merging
- exporters
- allocator hooks
- viewer concerns

#### `footfall`

Owns:

- re-exports of the calm public API
- default feature wiring across internal crates
- convenience modules that do not distort the core model

Does not own:

- a different descriptor system
- a different context model
- runtime-only policy in the hot path

#### `footfall_runtime`

Owns:

- counter banks
- bounded append buffers
- dropped-event accounting
- sink selection and snapshot extraction
- per-thread or low-contention write paths

Does not own:

- descriptor semantics
- macro-only APIs
- domain taxonomy outside shared conventions

#### `footfall_memory`

Owns:

- standard memory categories
- wrappers for scratch buffers and caches
- later GPU-adjacent accounting and retained/transient reporting
- later allocator integration

#### `footfall_export`

Owns:

- lowering Footfall snapshots or traces into an external model
- export-specific track, counter, and resource projection rules

#### `footfall_session`

Owns:

- versioned snapshot envelope types
- chunk ordering
- owned transport-friendly payloads
- dropped-data summaries for consumers

Does not own:

- hot-path emission
- network protocol details
- viewer-side caching or UI policy

#### `footfall_session_wtransport`

Owns:

- localhost WebTransport discovery and snapshot fetch
- concrete wire encoding for session envelopes
- certificate-pinned local demo identity

Does not own:

- the transport-neutral envelope model
- runtime collection
- UI behavior

#### `footfall_tracy`

Owns:

- live projection from Footfall backend calls into Tracy
- Tracy-specific scope, plot, frame, and message mapping

Does not own:

- canonical snapshots
- the session contract
- flow semantics Tracy cannot represent cleanly

#### `footfall_fanout`

Owns:

- pairwise backend composition, nestable for more than two sinks

Does not own:

- a general plugin bus
- dynamic sink registration after installation
- extra indirection in `footfall_core`

#### `footfall_macros` later

Owns:

- source-location capture
- call-site sugar
- descriptor declaration helpers when they materially reduce boilerplate

### Core Vocabulary

Start with a deliberately small vocabulary:

- ids: `ContextId`, `ScopeDescId`, `CounterDescId`, `ResourceId`, `ResourceKindId`, `EventDescId`
- descriptors: `ScopeDesc`, `CounterDesc`, `ResourceKindDesc`, `EventDesc`

Descriptors are `static` items whose identity is their address. Their ids are
derived from that address, so independent crates cannot collide by picking the
same number. Descriptor types are deliberately not `const`-promotable: using a
`const` where a `&'static` descriptor is required is a compile error, because
each use of a `const` would otherwise be a different descriptor.
- units: `Bytes`, `Count`, `Nanos`, `Micros`, `Ratio`, `Items`, plus a small extension path
- operations: `counter_add`, `gauge_set`, `scope_begin`, `scope_end`, `resource_create`, `resource_update`, `resource_drop`, `frame_marker`, and causal flow helpers

`EventDesc` backs message events. Reserve `SpanId` and other point events for
later phases once a real subsystem proves they are needed.

Anything beyond that should need a concrete proving-ground use case before joining the core.

### Label Keys

Footfall core ships no label vocabulary. Applications and libraries declare
the keys they attribute work by as static `LabelKey` items, for example
`static FRAME: LabelKey = LabelKey::new("frame");`. Unlike descriptors, a
key's identity is its name, which is also all that consumers outside the
process see, so a key means the same thing everywhere. A library should
prefix its key names with its own name.

The discipline moves from the core enum to each subsystem: a key should buy
real aggregation value. Product-specific concepts should enter as descriptor
names or typed values first, not as a rapidly growing set of keys.

### Context Model

Callers should work with an explicit `Context` value, not hidden thread-local state.

- `Context` is a small owned caller-facing semantic token that is copied and passed explicitly
- `ContextId` is the canonical integer handle used inside runtime storage, snapshots, and exports
- a context is formed by extending a parent context with one typed label
- labels are key/value pairs where keys are static `LabelKey` items declared by the instrumented code and values are compact typed ids or integers

This implies a logical shape like:

- parent context
- label key
- label value

The runtime may canonicalize a `Context` into a `ContextId` through interning, indexing, or another bounded mechanism. The important boundary is that callers do not pay for heap allocation or string assembly when constructing semantic context in hot code.

Scoped helpers are still useful, but they are secondary:

- explicit context is the core model
- RAII-style helpers are ergonomic adapters over that model
- no API should require thread-local context to be the only way to express attribution

### Instrumentation Levels

Define levels up front:

- `Off`: compile away or become an effectively free no-op
- `Counters`: counters and gauges only
- `Scoped`: counters plus begin/end scope recording
- `Trace`: full scope, resource, and richer event capture

Both compile-time and runtime control matter:

- compile time decides whether the machinery exists at all
- runtime decides how much of that machinery is currently active

That split keeps the disabled path honest while still allowing rich diagnostics in chosen sessions.

### Emission Seam

The emission seam should be inline function calls over static descriptors and compact ids. Macros may improve ergonomics, but they must lower to ordinary, obvious core operations.

Properties of the hot path:

- no dynamic string formatting
- no hidden allocation
- minimal branching
- no lock on the common append path if it can be avoided
- graceful overflow with explicit drop accounting

The preferred mental model is tracepoints plus counters, not builder-style structured logging.

### Core/Runtime Handshake

The boundary between `footfall_core` and `footfall_runtime` should be explicit and tiny.

- `footfall_core` defines the `Backend` trait, with a no-op default for every primitive so new primitives do not break existing backends
- backends carry their own state; the process-wide backend is one `&'static dyn Backend`
- the default backend is the built-in `NoopBackend`
- `footfall_runtime` installs a concrete backend during initialization, or hands a `&'static Runtime` to a composing backend such as `footfall_fanout`
- hot-path emission performs at most one backend indirection after the instrumentation-level gate

This keeps runtime policy out of the call site while still allowing a bounded, replaceable collection layer.

### Runtime Model

The runtime cut stays modest:

- no-op sink
- sharded counter banks
- bounded sharded event retention
- dropped-event counters
- frame or transaction flush boundaries
- snapshot extraction for export

The current threading model is "shared context interning, sharded writes, merge
on snapshot":

- contexts are canonicalized through one small interner
- counters and events are written into independent shards
- snapshots merge shards at a safe boundary
- future work can replace shards with per-thread buffers if benchmarks show it

### Memory Model

Memory observation should begin with wrappers around memory we already own, not a global allocator hook.

Shared phase-1 categories:

- `Scratch`
- `Cache`
- `Gpu`
- `GpuAux`
- `Heap`

Useful standard metrics:

- current bytes
- peak bytes
- allocated bytes total
- freed bytes total
- net delta
- churn bytes total
- allocation count
- grow or realloc count
- hit count
- miss count
- eviction count

### Resource Model

Resources are first-class because many rendering and systems problems are resource problems.

Phase 1 should use caller-supplied `ResourceId` values. Runtime-issued ids would introduce hidden registries and lifecycle coupling too early, while caller-supplied ids preserve the boring seam and match existing ownership models in rendering and cache code.

Phase-1 resource observations should support:

- create
- update
- drop
- logical size where relevant
- owner or context association

This enables lifetime views, memory-by-kind summaries, upload ownership reports, and retained-versus-transient comparisons without needing a bespoke viewer first.

## Minimal Public Shape

The exact names can change, but the core should feel roughly like this:

```rust
static RECORD_PASS: ScopeDesc = ScopeDesc::new("record_pass");
static UPLOAD_BYTES: CounterDesc = CounterDesc::bytes("upload_bytes");
static ATLAS: ResourceKindDesc =
    ResourceKindDesc::new("atlas");

static FRAME: LabelKey = LabelKey::new("frame");
static PASS: LabelKey = LabelKey::new("pass");
static NODE: LabelKey = LabelKey::new("node");

let ctx = Context::root()
    .with(&FRAME, frame_id)
    .with(&PASS, pass_id)
    .with(&NODE, node_id);

let scope = footfall::scope_begin(&RECORD_PASS, &ctx);
footfall::counter_add(&UPLOAD_BYTES, &ctx, upload_len as u64);
footfall::resource_update(&ATLAS, atlas_id, &ctx, atlas_bytes as u64);
footfall::scope_end(scope);
```

Macros can later compress that to a nicer surface, but this should remain close to the real implementation.

## Glossary

- Descriptor: Static metadata that defines the meaning of a scope, counter, or resource kind.
- Context: A compact caller-facing semantic attribution value built from parent plus typed label.
- ContextId: A canonical runtime handle for a context used in storage, snapshots, and export.
- Observation: A recorded counter change, scope boundary, resource lifecycle step, or frame marker.
- Sink: The runtime destination for emitted observations, such as a no-op sink, counter bank, or event buffer.
- Projection: Lowering captured Footfall data into an external trace or timeline model.

## Proving Grounds

Start where semantic attribution is already meaningful and the feedback loop is short:

1. Imaging draw recording.
2. Scratch buffers and retained caches.
3. GPU resource lifecycle and upload accounting.
4. Frame, transaction, and pass markers.

These domains should produce the first real answers to questions such as:

- which subtree emitted the most draw work
- where scratch churn spikes during recording
- which owner drove upload volume
- which cache grows without sufficient hit value

## Export Strategy

Export early to an existing ecosystem. That gives immediate leverage and tests whether the internal model is actually coherent.

The first target should be Chrome Trace Event JSON, inspected in Perfetto. It is simple enough to generate in an early crate, easy to inspect immediately, and close enough to the eventual timeline model that it will stress whether scopes, counters, markers, and resource tracks are shaped correctly.

The first export target should support:

- nested scopes
- counters over time
- frame or pass boundaries
- resource lifecycle or equivalent timeline tracks

A custom viewer should wait until the exported traces expose stable pain points that generic tools cannot express well enough. A richer Perfetto-native path or other binary export can come later if the JSON bridge proves too lossy or too expensive.

## Operational Definition of Low Overhead

Footfall should be measured, not merely described as "lightweight."

The Criterion benchmarks and current numbers live in
[Performance](performance.md).

At minimum, benchmarks should report:

- disabled-call overhead
- counter update overhead
- scope begin/end overhead
- context extension or reuse overhead
- trace append overhead
- export cost

For phase 1, "low overhead" means:

- disabled mode leaves no allocation and only trivial residual work
- counters mode is cheap enough for broad always-on use in selected subsystems
- trace mode is bounded and explicit about what it drops under pressure

## Risks And Gotchas

- Context can become string soup if labels and values are not disciplined.
- Runtime buffering can quietly become the real architecture if core seams are fuzzy.
- Macros can metastasize into a shadow API if they hide too much.
- Allocator instrumentation can distort the design if introduced before wrapper-based memory wins are proven.
- Too many early crates create ceremony without evidence; too few late crates create coupling. The current workspace should resist extra splits until the first export path and one real subsystem are instrumented.

## First Slice

The first implementation now matches a small but complete crate family. The
phase list below records what is done and what remains prototype-grade.

### Phase 0

- Done: design note landed.
- Done: naming for ids, descriptors, and labels is settled enough for first users.
- Done: the core/runtime backend seam is the `Backend` trait, installed once as `&'static dyn Backend`.
- Done: Chrome Trace Event JSON is the first export target.

### Phase 1

- Done: `footfall_core` is the foundation crate.
- Done: `footfall` is a thin facade over `footfall_core`.
- Done: `frame_marker` is a core primitive alongside counters, scopes, resources, and flows.
- Done: ids, descriptors, levels, context composition, and the no-op backend path have focused tests.

### Phase 2

- Done: `footfall_runtime` exists.
- Done: it provides a counter bank, bounded event retention, and dropped-data accounting.
- Done: examples prove snapshot extraction.
- Done: runtime capture levels are enforced by collection behavior.
- Done: counter and event storage are sharded to reduce the single-lock write path.

### Phase 3

- Done in examples: scratch buffers, one cache shape, resource lifecycle, frame markers, and pass markers are exercised.
- Still needed: instrument one real Forest subsystem outside the demo crate.

### Phase 4

- Done: `footfall_export` emits Chrome Trace Event JSON.
- Still needed: inspect traces from a real subsystem and adjust context/export rules from that feedback.

### Phase 5

- Started: `footfall_memory` has scratch and cache helpers.
- Still needed: retained/transient memory conventions, GPU-adjacent accounting, and allocator decision points.

### Phase 6

- Started: `footfall_session` and `footfall_session_wtransport` support local attach.
- Done: the concrete WebTransport wire format is documented, bounded, and covered
  by rejection tests for oversized payload shapes.
- Still needed: multi-process discovery policy and external-consumer compatibility
  fixtures.

### Phase 7

- Started: `footfall_benchmarks` measures disabled calls, counter updates, scope
  pairs, context construction, trace append, snapshots, and export.
- Started: measurements now include a parallel counter write case.
- Still needed: use results to refine context interning and snapshot merge costs.

## Prototype Boundaries

The crate family is usable for experiments and early external integration, but
these areas are still prototype-grade:

- Runtime write costs: counter and event writes are sharded, but context
  interning and snapshot merge/export remain the measured hotspots.
- Protocol surface: the WebTransport wire format is bounded and documented, but
  external-consumer compatibility fixtures and discovery policy still need work.
- Measurement: the benchmark crate exists, but results still need to drive the
  next runtime changes before low-overhead claims are treated as proven.
- Real proving ground: examples demonstrate the model, but one production
  subsystem must prove the context vocabulary and exported views.

## Next Steps Checklist

- Use benchmark results to reduce context interning and snapshot merge/export
  costs.
- Add external-consumer compatibility fixtures for the session wire format.
- Instrument one real Forest subsystem before broadening the crate family again.
