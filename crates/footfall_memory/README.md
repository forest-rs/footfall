# footfall_memory

Memory-oriented helpers built on Footfall counters: metrics for scratch
buffers (current bytes, peak bytes, growth events) and caches (current bytes,
hits, misses, evictions), plus a `TrackedScratchVec` that reports its own
growth.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
