# Instrumenting an App

This is the shortest path for using Footfall from an application outside this repo.

## Goal

A host application should be able to:

- emit counters, scopes, markers, and resource lifecycle events
- attach semantic context to those observations
- collect them locally with bounded runtime cost
- expose them to downstream tooling without coupling hot paths to transport code

## Recommended Stack

For the current happy path, use:

- `footfall` for descriptors, contexts, and emission calls; this is the only
  Footfall dependency an instrumented library needs
- `footfall_runtime` for bounded local collection
- `footfall_export` to write a capture as Chrome Trace JSON for Perfetto
- `footfall_tracing` if the app or its dependencies already use `tracing`
- `footfall_tracy` for live viewing in Tracy, combined with the runtime
  through `footfall_fanout`
- `footfall_memory` when you want scratch or cache helpers
- `footfall_session_wtransport` if you want a local attach path for consumers

## Minimal Setup

1. Define static descriptors close to the subsystem being instrumented.
2. Install one `footfall_runtime::Runtime` once at process startup.
3. Register a `Context` once per semantic owner and emit with its handle.
4. Keep transport and UI concerns out of the hot path.

Example shape:

```rust
use footfall::{
    CounterDesc, InstrumentationLevel, LabelKey, LazyContext, RegisteredContext, ScopeDesc,
    counter_add, scope,
};
use footfall_runtime::Runtime;

static RECORD_SCOPE: ScopeDesc = footfall::scope_desc!("record_frame");
static DRAW_OPS: CounterDesc = footfall::counter_desc!(count, "draw_ops");

// Footfall ships no label vocabulary; declare the keys your app attributes by.
static SUBSYSTEM: LabelKey = LabelKey::new("subsystem");
static PASS: LabelKey = LabelKey::new("pass");

// A context for the whole subsystem, registered on first use after install.
static RENDERER: LazyContext = LazyContext::new(|| footfall::context!(SUBSYSTEM = "renderer"));

fn install_footfall() -> &'static Runtime {
    Runtime::named_with_level("my_app", 512, 4096, InstrumentationLevel::Scoped)
        .install_global()
        .expect("runtime should install once")
}

/// One semantic owner: the opaque pass, with its context registered once.
struct OpaquePass {
    context: RegisteredContext,
}

impl OpaquePass {
    fn new() -> Self {
        Self {
            context: RegisteredContext::new(&footfall::context!(PASS = "opaque")),
        }
    }

    fn record(&self) {
        let _scope = scope(&RECORD_SCOPE, self.context.handle());
        counter_add(&DRAW_OPS, self.context.handle(), 42);
        counter_add(&DRAW_OPS, RENDERER.handle(), 42);
    }
}
```

The low-level `scope_begin` and `scope_end` calls remain available when a scope
crosses lexical boundaries.

A scope lasts as long as its guard. Bind the guard to a named variable such as
`_scope`; `let _ = scope(..)` drops the guard immediately and records an empty
scope, even though the compiler's `must_use` hint suggests that form.

Descriptors are identified by address, so declare them as `static` items,
never `const`; the compiler rejects a `const` descriptor where one is
required. Label keys are identified by name, so give keys distinct names; a
library should prefix its keys with its own name.

## Context Discipline

The architectural rule is simple:

- build a `Context` where an owner is created, extending its parent's labels
- register it once and pass the `ContextHandle` explicitly; never register on
  every emission
- release it when the owner goes away, usually by dropping a
  `RegisteredContext`
- register after installing the runtime, or use `LazyContext` for contexts in
  `static` items; a backend never learns about contexts registered before it
  was installed, and attributes their observations to the root context
- keep values compact and aggregatable

Equal contexts registered separately get distinct handles and are not merged,
so share the handle rather than re-registering the same labels.

Typical label keys name the owners an app reasons about, such as `frame`,
`transaction`, `pass`, `node`, `resource`, `cache`, or `reason`.

A handle is valid while its `RegisteredContext` is alive. Keep the
`RegisteredContext` with its owner rather than copying the handle out of a
temporary: once the context is released, later emissions with its handle are
attributed to the root context.

Avoid building instrumentation around ad hoc strings when an id or integer will do.

## Getting Data Out

A runtime snapshot holds everything captured so far: contexts, counters,
histograms, and timestamped events. Read it directly, or export it:

```rust
use footfall_runtime::Runtime;

fn write_trace(runtime: &Runtime) -> std::io::Result<()> {
    let snapshot = runtime.snapshot();
    footfall_export::write_chrome_trace_file(&snapshot, "trace.json")
}
```

Open `trace.json` in [Perfetto](https://ui.perfetto.dev) or
`chrome://tracing`. Scopes appear as slices on one track per thread, counters
as counter tracks, and contexts and arguments as slice arguments.

## What Lives Where

Call sites should stay in subsystem code:

- descriptors
- semantic context construction
- emission calls

Collection and attach should stay near application startup:

- runtime install
- fanout setup
- session server setup

That keeps the instrumentation seam boring and lets the host app change how it
inspects data without rewriting every call site.

## Always-On vs Diagnostic Use

Use counters and a small amount of scoped tracing for always-on instrumentation.

Set runtime capture policy explicitly when installing the process runtime:

- `InstrumentationLevel::Off` for compiled-in but inactive instrumentation
- `InstrumentationLevel::Counters` for broad low-detail observation
- `InstrumentationLevel::Scoped` for counters plus scope/marker/flow timing
- `InstrumentationLevel::Trace` for the full current observation set, including resources

For a runtime installed with `Runtime::install_global`, the level also sets
`footfall_core`'s process-wide maximum level, so excluded emissions return
after one atomic load instead of reaching the runtime.

Turn on richer downstream inspection when needed:

- local snapshots
- local attach
- Tracy
- export

The emission seam should not need to change between those modes.

To remove instrumentation from a build entirely, build the final binary with
`RUSTFLAGS="--cfg footfall_compile_out"`. Every emission then compiles to an
empty inlined function and no backend sees anything. It is a compiler `cfg`
rather than a cargo feature so that no dependency, and no `--all-features`
run, can switch it on for you; libraries should never set it.

## Current Local Attach Pattern

If you want a separate consumer app to connect locally:

1. install `footfall_runtime`
2. expose a local session server through `footfall_session_wtransport`
3. let the consumer discover and fetch sessions through that server

The current examples are the reference shape:

- [session_server.rs](../examples/basic/src/session_server.rs)
- [session_consumer.rs](../examples/basic/src/session_consumer.rs)

## What Is Still Missing

- there is no polished consumer UI yet
- the local attach path is intentionally localhost-first
