# Footfall

Footfall is a family of Rust crates for low-overhead instrumentation, semantic
tracing, counters, memory observation, and downstream projection into existing
profiling and visualization tools.

The workspace currently contains:

- `footfall_core`: foundational ids, descriptors, context, levels, and emission
  primitives
- `footfall`: a thin facade over `footfall_core`
- `footfall_runtime`: bounded in-process collection and snapshot extraction
- `footfall_memory`: scratch and cache helpers layered over core counters
- `footfall_session`: transport-neutral snapshot envelopes for downstream
  consumers
- `footfall_session_wtransport`: local attach path for discovery and snapshot
  fetch over WebTransport
- `footfall_fanout`: backend composition for feeding multiple sinks at once
- `footfall_tracing`: a `tracing` subscriber layer that forwards spans and
  events into Footfall
- `footfall_benchmarks`: Criterion benchmarks for substrate overhead
- `examples/basic`: a top-level example crate to avoid spreading dev
  dependencies through core packages

## External Use

Footfall is intended to be usable from outside this repo in two directions:

- instrument your own application with `footfall`, `footfall_runtime`, and
  related helper crates
- build a separate consumer or UI that attaches to a running Footfall session

Start here:

- [Instrumenting an App](docs/instrumenting-an-app.md)
- [Session Consumer Contract](docs/session-consumer-contract.md)
- [WebTransport Session Wire Format](docs/session-wire-format.md)
- [Migration Notes](docs/migration-notes.md)
- [Performance](docs/performance.md)

## Local Attach

The current local attach workflow is:

1. your process installs `footfall_runtime`
2. your process serves sessions through `footfall_session_wtransport`
3. a consumer connects to the fixed local server and discovers active sessions

The examples show that flow directly:

- `cargo run -p basic --bin session_wtransport` for a one-process
  serve/discover/fetch round trip
- `cargo run -p basic --bin session_server`
- `cargo run -p basic --bin session_consumer`

The session examples include direct Footfall instrumentation plus `tracing`
spans/events bridged through `footfall_tracing`. For a compact Chrome Trace JSON
view of just the tracing bridge:

- `cargo run -p basic --bin tracing_demo > /tmp/footfall-tracing.json`
