# footfall

Low-overhead instrumentation with semantic attribution.

Footfall records what work happened and which semantic owner it happened
under: a frame, a pass, a cache, a worker. Instrumented code emits scopes,
counters, markers, flows, messages, and resource lifecycles through one
process-wide backend. With no backend installed, or with instrumentation
compiled out, an emission costs about a nanosecond or nothing at all.

This crate is the one entry point. Its root is the instrumentation API, which
is `no_std` and has no dependencies; features add the parts an application or
a tool needs as modules.

## Choosing features

| You are writing | Depend on |
|---|---|
| A library | `footfall`, with no features |
| An application that captures and exports | `footfall` with `runtime` and `export` |
| An application others can attach to live | `footfall` with `runtime` and `wtransport` |
| A tool that attaches to applications | `footfall` with `wtransport` and `analysis` |

| Feature | Module | What it adds |
|---|---|---|
| `std` | | Standard-library conveniences in the instrumentation API |
| `runtime` | `footfall::runtime` | Bounded in-process capture with snapshots and cursors |
| `export` | `footfall::export` | Perfetto traces, and Chrome Trace JSON for `chrome://tracing` |
| `session` | `footfall::session` | Snapshots for consumers, the envelope codec, capture files |
| `analysis` | `footfall::analysis` | Traces of snapshots: span queries, frames, comparisons, timelines |
| `wtransport` | `footfall::wtransport` | Serve sessions and attach to them over local WebTransport |
| `tracing` | `footfall::tracing_layer` | A `tracing` layer that forwards spans and events |
| `tracy` | `footfall::tracy` | A live Tracy backend |

`wtransport` and `tracy` are not available on wasm targets; there, the
features add nothing. `wtransport` runs on Tokio, so serve and attach from
within a Tokio runtime.

## Quick start

```toml
[dependencies]
footfall = { version = "0.2", features = ["runtime", "export"] }
```

```rust
use footfall::{
    ContextHandle, CounterDesc, LabelKey, RegisteredContext, ScopeDesc, context, counter_add,
    frame_begin, frame_end,
};
use footfall::runtime::Runtime;

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
        footfall::scope!(&RENDER, opaque.handle());
        counter_add(&UPLOAD_BYTES, opaque.handle(), 4096);
    }
    frame_end(ContextHandle::ROOT);
}

// Read back what was captured, or export it for a trace viewer.
let snapshot = runtime.snapshot();
for counter in snapshot.counters() {
    println!("{} = {} {}", counter.name(), counter.value(), counter.unit().as_str());
}
let trace = footfall::export::to_perfetto_trace(&snapshot.into());
println!("{} bytes of Perfetto trace", trace.len());
```

Save the trace to a file, or write one directly with
`footfall::export::write_perfetto_trace_file`, and open it in
[Perfetto](https://ui.perfetto.dev). `to_chrome_trace_json` writes Chrome
Trace JSON instead, for `chrome://tracing` and other tools that read it.

## Guides

- [Migrating from 0.1 to 0.2](https://github.com/forest-rs/footfall/blob/main/docs/migrating-to-0.2.md)
- [Instrumenting an app](https://github.com/forest-rs/footfall/blob/main/docs/instrumenting-an-app.md)
- [Session consumer contract](https://github.com/forest-rs/footfall/blob/main/docs/session-consumer-contract.md)
- [Performance](https://github.com/forest-rs/footfall/blob/main/docs/performance.md)
- [Examples](https://github.com/forest-rs/footfall/tree/main/examples)

## Crates

Each module is a crate of its own, which other crates can depend on directly:
`footfall_core` (the instrumentation API), `footfall_runtime`,
`footfall_export`, `footfall_session`, `footfall_analysis`,
`footfall_session_wtransport`, `footfall_tracing`, and `footfall_tracy`.
`footfall_fanout` (two backends at once) and `footfall_memory` (scratch-buffer
and cache metrics) are separate crates.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
