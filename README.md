# Footfall

Low-overhead instrumentation for Rust that records what work happened and
which owner it happened under.

A trace that says "this frame took 40 ms in `layout`" is a start. Footfall
also records that it was the transparent pass of the main window, after a
font change, with the glyph cache missing 60% of lookups. Instrumented code
attributes every scope, counter, marker, flow, message, and resource
lifecycle to a semantic context: a frame, a pass, a cache, a worker. Tools
can then group and compare by the things your program is made of, not only
by thread and call stack.

- **Cheap when idle, free when compiled out.** An emission costs under a
  nanosecond with no backend installed, and nothing at all with
  `--cfg footfall_compile_out`.
- **Bounded.** The in-process runtime keeps counters and a window of recent
  events in fixed memory, and counts everything it could not keep.
- **Attachable.** A running program serves its capture to local tools over
  WebTransport; any number of them can follow it live without draining it.
- **Analyzable anywhere.** The same snapshot form comes from the process
  itself, a live session, or a capture file, and turns into spans, frames,
  per-frame counter changes, or a Chrome trace for Perfetto.
- **Bridged.** Existing `tracing` spans and events flow in unchanged, and a
  Tracy backend shows everything live in Tracy.

## Quick start

```toml
[dependencies]
footfall = { version = "0.1", features = ["runtime", "export"] }
```

```rust
use footfall::runtime::Runtime;
use footfall::{ContextHandle, CounterDesc, LabelKey, RegisteredContext, ScopeDesc, context};

static PASS: LabelKey = LabelKey::new("pass");
static RENDER: ScopeDesc = ScopeDesc::new("render");
static UPLOAD_BYTES: CounterDesc = CounterDesc::bytes("upload_bytes");

let runtime = Runtime::builder()
    .process_name("my_app")
    .install_global()
    .expect("no other backend is installed");
let opaque = RegisteredContext::new(&context!(PASS = "opaque"));

footfall::frame_begin(ContextHandle::ROOT);
{
    footfall::scope!(&RENDER, opaque.handle());
    footfall::counter_add(&UPLOAD_BYTES, opaque.handle(), 4096);
}
footfall::frame_end(ContextHandle::ROOT);

let trace = footfall::export::to_chrome_trace_json(&runtime.snapshot().into());
println!("{} bytes of Chrome trace JSON", trace.len());
```

Libraries depend on `footfall` with no features: the instrumentation API is
`no_std`, has no dependencies, and does nothing until the application installs
a backend. The [`footfall` crate documentation](crates/footfall/README.md)
lists the features for applications and tools.

## See it

The [examples](examples/README.md) include a fake renderer that serves its
capture, and the [`footfall` command](tools/footfall_cli/README.md) watches
it live, records it, and asks the recording questions, all through the
public API:

```sh
cargo run -p footfall_examples --bin demo_app        # in one terminal
cargo run -p footfall_cli -- top                     # in another
```

```text
footfall top  demo_app  pid 63470 · session main · level trace  ● live
last 240 frames  mean 12.8 ms · p95 14.7 ms · max 46.3 ms · 2 over budget  (budget 16.7 ms)
https://127.0.0.1:43091/footfall/session · main
╭ Frame times · newest on the right · red over budget ─────────────────────────────────────────────╮
│                                                                                                 █│
│                                                                                                 █│
│                                                                                                 █│
│                                                                                                 █│
│                                                                                                 █│
│                                                                                                 █│
│                 ▁      ▁            ▂▁    ▂   ▂               ▁               ▁   ▁             █│
│▃▅▆▅▅▅▅▅▇▆▄▇█▇▆▄▃█▇▄▇▅▅▄█▄▁▅▄▅▆▆▅▃▃▃▅██▄▄▃▂█▂▅▇██▆▇▁▃█▄▃▅▄▇█▅▃▇█▄▄▇▂▅▇▅▆▂▄▇▅▅█ █▅▄▅█▅▂▆▄▇▄▄▂▆▅▆▆▄█│
│██████████████████████████████████████████████████████████████████████████████████████████████████│
│██████████████████████████████████████████████████████████████████████████████████████████████████│
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
╭ Scopes · recent windows · by total time ─────────╮╭ Counters · sampled at each frame end ────────╮
│scope            count  total    self     max     ││counter          last      per frame now      │
│raster_tile      3,840  3.80 s   3.80 s   2.44 ms ││draw_calls       62        72        16,517   │
│frame            240    3.06 s   1.91 ms  46.3 ms ││glyph_cache_byte 1.00 MiB  gauge     1.00 MiB │
│paint            240    2.08 s   2.06 s   24.9 ms ││glyph_hits       30        28        6,815    │
│layout           240    500.0 ms 500.0 ms 35.8 ms ││glyph_misses     1         1         398      │
│shape_text       240    201.1 ms 134.2 ms 3.99 ms ││layers           4         gauge     4        │
│composite        240    199.6 ms 199.6 ms 1.18 ms ││layout_nodes     12,000    1,177     246,900  │
│update           240    81.3 ms  81.3 ms  1.57 ms ││tiles_rastered   16        16        3,840    │
│upload_glyphs    163    66.9 ms  66.9 ms  3.52 ms ││upload_bytes     2.0 KiB   3.4 KiB   796.0 KiB│
│compile_shader   1      15.8 ms  15.8 ms  15.8 ms ││                                              │
╰──────────────────────────────────────────────────╯╰──────────────────────────────────────────────╯
╭ Messages · newest first ─────────────────────────╮╭ Capture ─────────────────────────────────────╮
│renderer  font changed; relayout everything       ││nothing lost                                  │
│renderer  compiling shader variant                ││events   3,376 last window · 3,233/s          │
│                                                  ││wire     31.6 KiB per window                  │
│                                                  ││instance 3cbf60168cd45f74                     │
│                                                  ││missed   0 events in 0 gaps                   │
│                                                  ││evicted  0 of 65,536 buffered                 │
╰──────────────────────────────────────────────────╯╰──────────────────────────────────────────────╯
 q quit   p pause   s sort by self time   · 240 frames seen
```

## Guides

- [Instrumenting an app](docs/instrumenting-an-app.md): descriptors,
  contexts, levels, capture, export, and serving.
- [Session consumer contract](docs/session-consumer-contract.md): what a tool
  attaching to a session can rely on.
- [Analysing a trace](docs/analysing-a-trace.md): asking where time went,
  what changed, and what an answer covers.
- [Session wire format](docs/session-wire-format.md): the bytes, for tools in
  other languages.
- [Architecture](docs/architecture.md): how the crates fit and why.
- [Performance](docs/performance.md): how overhead is measured, and results.

## Crates

| Crate | `footfall` feature | Role |
|---|---|---|
| `footfall` | | The entry point: the instrumentation API, and every part below as a module |
| `footfall_core` | | The instrumentation API and its reference documentation |
| `footfall_runtime` | `runtime` | Bounded in-process capture with snapshots and cursors |
| `footfall_export` | `export` | Chrome Trace JSON for Perfetto and `chrome://tracing` |
| `footfall_session` | `session` | Snapshots for consumers, the envelope codec, capture files |
| `footfall_analysis` | `analysis` | Traces of snapshots: span queries, frames, comparisons, timelines |
| `footfall_session_wtransport` | `wtransport` | Serve and attach to sessions over local WebTransport |
| `footfall_tracing` | `tracing` | A `tracing` layer that forwards into Footfall |
| `footfall_tracy` | `tracy` | A live Tracy backend |
| `footfall_fanout` | | One backend that feeds two, such as a runtime and Tracy |
| `footfall_memory` | | Scratch-buffer and cache metrics built on counters |

The workspace also holds the [benchmarks](benchmarks/substrate) and the
[examples](examples).

## Status

The session protocol is `1.1`. Captures stay readable by later releases at
the same protocol major version, or fail with an error naming the version
that wrote them; see [capture files](docs/session-wire-format.md#capture-files).
The minimum supported Rust version is 1.88.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
