# Spoor

Spoor is a family of Rust crates for low-overhead instrumentation, semantic
tracing, counters, memory observation, and downstream projection into existing
profiling and visualization tools.

The workspace currently contains:

- `spoor_core`: foundational ids, descriptors, context, levels, and emission
  primitives
- `spoor`: a thin facade over `spoor_core`
- `spoor_runtime`: bounded in-process collection and snapshot extraction
- `spoor_memory`: scratch and cache helpers layered over core counters
- `spoor_session`: transport-neutral snapshot envelopes for downstream
  consumers
- `spoor_session_wtransport`: local attach path for discovery and snapshot
  fetch over WebTransport
- `spoor_fanout`: backend composition for feeding multiple sinks at once
- `spoor_benchmarks`: top-level benchmark wind tunnel for substrate overhead
- `examples/basic`: a top-level example crate to avoid spreading dev
  dependencies through core packages

## External Use

Spoor is intended to be usable from outside this repo in two directions:

- instrument your own application with `spoor`, `spoor_runtime`, and related
  helper crates
- build a separate consumer or UI that attaches to a running Spoor session

Start here:

- [Instrumenting an App](docs/instrumenting-an-app.md)
- [Session Consumer Contract](docs/session-consumer-contract.md)
- [Migration Notes](docs/migration-notes.md)

## Local Attach

The current local attach workflow is:

1. your process installs `spoor_runtime`
2. your process serves sessions through `spoor_session_wtransport`
3. a consumer connects to the fixed local server and discovers active sessions

The examples show that flow directly:

- `cargo run -p basic --bin session_server`
- `cargo run -p basic --bin session_consumer`
