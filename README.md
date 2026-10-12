# Footfall

Footfall is a family of Rust crates for low-overhead instrumentation, semantic
tracing, counters, memory observation, and downstream projection into existing
profiling and visualization tools.

The crates:

- `footfall`: the instrumentation API; start here
- `footfall_core`: the same API with the reference documentation
- `footfall_runtime`: bounded in-process capture and snapshots
- `footfall_export`: Chrome Trace JSON export for Perfetto and `chrome://tracing`
- `footfall_tracing`: a `tracing` layer that forwards spans and events into
  Footfall
- `footfall_tracy`: a live Tracy backend
- `footfall_fanout`: feeds two backends at once
- `footfall_memory`: scratch-buffer and cache metrics built on counters
- `footfall_session`: transport-neutral session envelopes for external
  consumers
- `footfall_session_wtransport`: serves and fetches sessions over local
  WebTransport

The workspace also holds `footfall_benchmarks`, the Criterion benchmarks for
emission overhead, and `examples/basic`, which keeps example dev-dependencies
out of the published crates.

## External Use

Footfall is intended to be usable from outside this repo in two directions:

- instrument your own application with `footfall`, `footfall_runtime`, and
  related helper crates
- build a separate consumer or UI that attaches to a running Footfall session

Start here:

- [Instrumenting an App](docs/instrumenting-an-app.md)
- [Session Consumer Contract](docs/session-consumer-contract.md)
- [WebTransport Session Wire Format](docs/session-wire-format.md)
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
