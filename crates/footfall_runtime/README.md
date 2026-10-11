# footfall_runtime

Bounded in-process capture for Footfall. `Runtime` is a `Backend` that keeps
counters, histograms, and timestamped observations in fixed-capacity per-thread
buffers, resolves registered contexts, and produces `Snapshot`s for export,
analysis, or serving to external consumers.

Capacity is fixed when the runtime is created. Whatever does not fit is
dropped and counted, and every snapshot reports the dropped totals.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
