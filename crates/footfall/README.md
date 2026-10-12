# footfall

Low-overhead instrumentation with semantic attribution.

Footfall records what work happened and which semantic owner it happened
under: a frame, a pass, a cache, a worker. Instrumented code emits scopes,
counters, markers, flows, messages, and resource lifecycles through one
process-wide backend. With no backend installed, or with instrumentation
compiled out, an emission costs about a nanosecond or nothing at all.

This crate is the instrumentation API. It re-exports `footfall_core`, which
has the full reference for descriptors, contexts, levels, and compiling
instrumentation out. Libraries depend on `footfall` alone; the application
chooses where the data goes.

## Quick start

```rust
use footfall::{
    ContextHandle, CounterDesc, LabelKey, RegisteredContext, ScopeDesc, context, counter_add,
    frame_begin, frame_end, scope,
};
use footfall_runtime::Runtime;

// Descriptors and label keys are `static` items, declared once.
static PASS: LabelKey = LabelKey::new("pass");
static RENDER: ScopeDesc = ScopeDesc::new("render");
static UPLOAD_BYTES: CounterDesc = CounterDesc::bytes("upload_bytes");

// The application installs a backend once, before emitting.
let runtime = Runtime::builder()
    .process_name("my_app")
    .install_global()
    .expect("no other backend is installed");

// Register a context once per long-lived owner, then emit with its handle.
let opaque = RegisteredContext::new(&context!(PASS = "opaque"));

// Each frame: mark it, and do the work under the owner's context. Frame ends
// record each counter's value, which gives per-frame changes.
for _ in 0..3 {
    frame_begin(ContextHandle::ROOT);
    {
        let _render = scope(&RENDER, opaque.handle());
        counter_add(&UPLOAD_BYTES, opaque.handle(), 4096);
    }
    frame_end(ContextHandle::ROOT);
}

// Read back what was captured, or export it for a trace viewer.
let snapshot = runtime.snapshot();
for counter in snapshot.counters() {
    println!("{} = {} {}", counter.name(), counter.value(), counter.unit().as_str());
}
let chrome_trace_json = footfall_export::to_chrome_trace_json(&snapshot.into());
# let _ = chrome_trace_json;
```

## Crates

| Crate | Role |
|---|---|
| `footfall` | Instrumentation API for libraries and applications. |
| `footfall_core` | The same API, with the reference documentation. |
| `footfall_runtime` | Bounded in-process capture with snapshots. |
| `footfall_export` | Chrome Trace JSON export for Perfetto and `chrome://tracing`. |
| `footfall_tracing` | `tracing` layer that forwards spans and events into Footfall. |
| `footfall_tracy` | Live Tracy backend. |
| `footfall_fanout` | Feeds two backends at once, such as a runtime and Tracy. |
| `footfall_memory` | Scratch-buffer and cache metrics built on counters. |
| `footfall_session` | Transport-neutral session envelopes for external consumers. |
| `footfall_session_wtransport` | Serves and fetches sessions over local WebTransport. |
| `footfall_analysis` | Scope, frame, and counter analysis over snapshots, live or from files. |

## Guides

- [Instrumenting an app](https://github.com/forest-rs/footfall/blob/main/docs/instrumenting-an-app.md)
- [Session consumer contract](https://github.com/forest-rs/footfall/blob/main/docs/session-consumer-contract.md)
- [Performance](https://github.com/forest-rs/footfall/blob/main/docs/performance.md)

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
