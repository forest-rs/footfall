# Spoor

Spoor is a family of Rust crates for low-overhead instrumentation, semantic
tracing, counters, memory observation, and downstream projection into existing
profiling and visualization tools.

The workspace currently contains:

- `spoor_core`: foundational ids, descriptors, context, levels, and emission
  primitives
- `spoor`: a thin facade over `spoor_core`
- `examples/basic`: a top-level example crate to avoid spreading dev
  dependencies through core packages
