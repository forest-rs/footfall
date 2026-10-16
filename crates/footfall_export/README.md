# footfall_export

Exports Footfall runtime snapshots as Chrome Trace Event JSON, which Perfetto
and `chrome://tracing` open directly. Scopes become slices on per-thread
tracks, timeline work slices on a track per timeline, frame opportunities
instants on a track per frame stream, counters counter tracks, and contexts
and arguments travel as event arguments.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
