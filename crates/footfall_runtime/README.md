# footfall_runtime

Bounded in-process capture for Footfall. `Runtime` is a `Backend` that keeps
cumulative counters and histograms and a recent window of timestamped events
in fixed-capacity per-thread buffers, resolves registered contexts, and
produces `Snapshot`s for export, analysis, or serving to external consumers.

Any number of readers can read a runtime without draining it: each snapshot
returns a cursor, and reading from that cursor returns only newer events.
Capacity is fixed when the runtime is created. The oldest events are evicted
to make room, and whatever else does not fit is dropped and counted in every
snapshot.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
