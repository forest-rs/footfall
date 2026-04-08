# Spoor Architecture

Status: draft

## Overview

Spoor is a family of small Rust crates for instrumentation and observability. Its job is not to replace system profilers or become a generic logging framework. Its job is to provide a slim, durable instrumentation seam that can attach low-level observations to semantic ownership.

The key question Spoor should answer is not only "what happened?" but "what did it happen under?" A draw, upload, cache resize, scratch burst, or invalidation storm is much more useful when it is attributed to a pass, subtree, owner, cache, or resource kind instead of only a thread and call stack.

Phase 1 should stay narrow:

- a `no_std`-friendly core
- a thin top-level `spoor` facade
- explicit instrumentation levels
- counters, scopes, and resource lifecycle as the only core observation families
- export into existing ecosystems before any custom viewer work

Spoor explicitly does not own OS-wide tracing, full heap profiling on day 1, or a universal arbitrary-event schema.

## Fence

`spoor_core` owns ids, static descriptors, context values, instrumentation levels, and the tiny emission ABI; it explicitly does not own runtime buffering, export formats, clocks, allocator interception, or domain-specific viewer logic.

`spoor` owns the recommended user-facing surface and re-exports; it explicitly does not add a second architecture beside the core.

Later crates own runtime collection, memory helpers, export, and macros as separate seams; they do not redefine the core vocabulary.

## First Read

The architectural bet is simple:

- keep the call-site seam tiny and boring
- make counters first-class
- make semantic context explicit
- keep disabled and lightweight modes cheap enough to leave instrumentation in the codebase
- validate the model by exporting into existing tooling early

That leads to a phased shape:

1. `spoor_core` defines ids, descriptors, labels, levels, and inline emission functions.
2. `spoor` re-exports the calm API that most callers use.
3. `spoor_runtime` later provides bounded buffers, counter banks, clocks, and snapshots.
4. `spoor_memory` later standardizes scratch, cache, GPU-adjacent, and heap-accounting helpers.
5. `spoor_export` later projects captured data into an external timeline or trace ecosystem.
6. `spoor_macros` later provides ergonomic sugar but remains optional and thin.

The current workspace should start with only `spoor_core` and `spoor`. That keeps the first slice honest and prevents premature crate proliferation before the call-site seam is proven in real instrumentation.

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

#### `spoor_core`

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

#### `spoor`

Owns:

- re-exports of the calm public API
- default feature wiring across internal crates
- convenience modules that do not distort the core model

Does not own:

- a different descriptor system
- a different context model
- runtime-only policy in the hot path

#### `spoor_runtime` later

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

#### `spoor_memory` later

Owns:

- standard memory categories
- wrappers for scratch buffers, caches, GPU-adjacent accounting, and retained/transient reporting
- later allocator integration

#### `spoor_export` later

Owns:

- lowering Spoor snapshots or traces into an external model
- export-specific track, counter, and resource projection rules

#### `spoor_macros` later

Owns:

- source-location capture
- call-site sugar
- descriptor declaration helpers when they materially reduce boilerplate

### Core Vocabulary

Start with a deliberately small vocabulary:

- ids: `ContextId`, `ScopeDescId`, `CounterDescId`, `ResourceId`, `ResourceKindId`
- descriptors: `ScopeDesc`, `CounterDesc`, `ResourceKindDesc`
- units: `Bytes`, `Count`, `Nanos`, `Micros`, `Ratio`, `Items`, plus a small extension path
- operations: `counter_add`, `gauge_set`, `scope_begin`, `scope_end`, `resource_create`, `resource_update`, `resource_drop`, `frame_marker`

Reserve `EventDescId`, `EventDesc`, `SpanId`, flows, and point events for later phases once a real subsystem proves they are needed.

Anything beyond that should need a concrete proving-ground use case before joining the core.

### Initial Label Vocabulary

The core should begin with a small shared label-key set:

- `Frame`
- `Transaction`
- `Pass`
- `Subsystem`
- `Node`
- `Widget`
- `Layer`
- `Resource`
- `Cache`
- `Reason`

The rule is strict: new label keys must buy real aggregation value across at least one proving-ground subsystem. Product-specific concepts should enter as descriptor names or typed values first, not as a rapidly growing core key enum.

### Context Model

Callers should work with an explicit `Context` value, not hidden thread-local state.

- `Context` is a small owned caller-facing semantic token that is copied and passed explicitly
- `ContextId` is the canonical integer handle used inside runtime storage, snapshots, and exports
- a context is formed by extending a parent context with one typed label
- labels are key/value pairs where keys come from a shared small vocabulary and values are compact typed ids or integers

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

The boundary between `spoor_core` and `spoor_runtime` should be explicit and tiny.

- `spoor_core` defines a backend ABI as a fixed function table or equivalent static call surface
- the default backend is a built-in no-op implementation
- `spoor_runtime` installs a concrete backend during initialization
- hot-path emission performs at most one backend indirection after the instrumentation-level gate

This keeps runtime policy out of the call site while still allowing a bounded, replaceable collection layer.

### Runtime Model

The first runtime cut should stay modest:

- no-op sink
- counter bank
- bounded event ring
- dropped-event counters
- frame or transaction flush boundaries
- snapshot extraction for export

A good initial threading model is "local writes, merge later":

- per-thread append buffers where practical
- reduced or merged snapshots at a safe boundary
- avoid accidental shared contention in the hottest paths

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
static RECORD_PASS: ScopeDesc = ScopeDesc::new(ScopeDescId::new(1), "record_pass");
static UPLOAD_BYTES: CounterDesc = CounterDesc::bytes(CounterDescId::new(7), "upload_bytes");
static ATLAS: ResourceKindDesc =
    ResourceKindDesc::new(ResourceKindId::new(2), "atlas");

let ctx = Context::root()
    .with(LabelKey::Frame, frame_id.into())
    .with(LabelKey::Pass, pass_id.into())
    .with(LabelKey::Node, node_id.into());

let scope = spoor::scope_begin(&RECORD_PASS, ctx);
spoor::counter_add(&UPLOAD_BYTES, ctx, upload_len as u64);
spoor::resource_update(&ATLAS, atlas_id, ctx, atlas_bytes as u64);
spoor::scope_end(scope);
```

Macros can later compress that to a nicer surface, but this should remain close to the real implementation.

## Glossary

- Descriptor: Static metadata that defines the meaning of a scope, counter, or resource kind.
- Context: A compact caller-facing semantic attribution value built from parent plus typed label.
- ContextId: A canonical runtime handle for a context used in storage, snapshots, and export.
- Observation: A recorded counter change, scope boundary, resource lifecycle step, or frame marker.
- Sink: The runtime destination for emitted observations, such as a no-op sink, counter bank, or event buffer.
- Projection: Lowering captured Spoor data into an external trace or timeline model.

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

Spoor should be measured, not merely described as "lightweight."

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

The first implementation should match the current workspace and stay intentionally small.

### Phase 0

- land this design note
- settle naming for ids, descriptors, and labels
- settle the core/runtime backend ABI
- choose the first export target before runtime details harden

### Phase 1

- implement `spoor_core`
- implement a thin `spoor` facade
- keep `frame_marker` as a core primitive alongside counters, scopes, and resources
- add docs and tests for ids, descriptors, levels, context composition, and the no-op backend path

### Phase 2

- add `spoor_runtime`
- provide a counter bank, bounded event ring, and dropped-data accounting
- prove basic snapshot extraction against a tiny example

### Phase 3

- instrument one real imaging path
- instrument scratch buffers and one retained cache
- add frame and pass markers

### Phase 4

- add `spoor_export`
- inspect traces in an existing tool and adjust the model based on real use

### Phase 5

- expand `spoor_memory`
- consider allocator instrumentation only after the wrapper-based path has already paid off

## Next Steps Checklist

- Create `crates/spoor_core` as the real foundation crate.
- Create `crates/spoor` as a thin facade over `spoor_core`.
- Keep `runtime`, `memory`, `export`, and `macros` out of the workspace until phase 2+ work begins.
- Define `Context` and `ContextId` separately so the call-site model stays explicit while the runtime storage model stays compact.
- Define the initial label-key vocabulary and value encoding rules.
- Define the core/backend ABI before implementing runtime buffering.
- Define the minimal descriptor constructors and unit set.
- Start with Chrome Trace Event JSON export and validate that scopes, counters, resources, and frame markers map cleanly in Perfetto.
- Add benchmarks in a separate top-level crate once the first emission seam exists.
- Instrument one real subsystem before broadening the crate family.
