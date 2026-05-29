# Instrumenting an App

This is the shortest path for using Spoor from an application outside this repo.

## Goal

A host application should be able to:

- emit counters, scopes, markers, and resource lifecycle events
- attach semantic context to those observations
- collect them locally with bounded runtime cost
- expose them to downstream tooling without coupling hot paths to transport code

## Recommended Stack

For the current happy path, use:

- `spoor` for core ids, descriptors, contexts, and emission calls
- `spoor_runtime` for bounded local collection
- `spoor_memory` when you want scratch or cache helpers
- `spoor_session_wtransport` if you want a local attach path for consumers

## Minimal Setup

1. Define static descriptors close to the subsystem being instrumented.
2. Install one `spoor_runtime::Runtime` once at process startup.
3. Emit observations from hot code using explicit `Context` values.
4. Keep transport and UI concerns out of the hot path.

Example shape:

```rust
use spoor::{
    CounterDesc, InstrumentationLevel, ScopeDesc, counter_add, scope,
};
use spoor_runtime::Runtime;

static RECORD_SCOPE: ScopeDesc = spoor::scope_desc!(1, "record_frame");
static DRAW_OPS: CounterDesc = spoor::counter_desc!(count, 2, "draw_ops");

fn install_spoor() -> &'static Runtime {
    Runtime::install_global_named_with_level(
        "my_app",
        512,
        4096,
        InstrumentationLevel::Scoped,
    )
    .expect("runtime should install once")
}

fn record_frame(frame: u32) {
    let context = spoor::context!(Frame = frame, Pass = 1_u32);
    let _scope = scope(&RECORD_SCOPE, context);
    counter_add(&DRAW_OPS, context, 42);
}
```

The low-level `scope_begin` and `scope_end` calls remain available when a scope
crosses lexical boundaries.

## Context Discipline

The architectural rule is simple:

- pass `Context` explicitly
- extend it where ownership changes
- keep values compact and aggregatable

Good labels:

- `Frame`
- `Transaction`
- `Pass`
- `Node`
- `Resource`
- `Cache`
- `Reason`

Avoid building instrumentation around ad hoc strings when an id or integer will do.

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

Turn on richer downstream inspection when needed:

- local snapshots
- local attach
- Tracy
- export

The emission seam should not need to change between those modes.

## Current Local Attach Pattern

If you want a separate consumer app to connect locally:

1. install `spoor_runtime`
2. expose a local session server through `spoor_session_wtransport`
3. let the consumer discover and fetch sessions through that server

The current examples are the reference shape:

- [session_server.rs](/Users/bruce/Development/forest-rs/spoor/examples/basic/src/session_server.rs)
- [session_consumer.rs](/Users/bruce/Development/forest-rs/spoor/examples/basic/src/session_consumer.rs)

## What Is Still Missing

The current stack is good enough for external use, but still early:

- there is no descriptor registry or id allocation story yet
- there is no polished consumer UI yet
- the current local attach path is intentionally localhost-first

Those are next-step ergonomics, not missing architecture.
