# footfall_analysis

Scope, frame, and counter analysis over Footfall snapshots. It works on the
`OwnedSnapshot` a consumer holds, whether read in process, fetched from a
live session, or loaded from a capture file:

- `Spans` pairs scope begins and ends into spans with durations, self time,
  and nesting, and summarizes them by name or by a context label, while
  reporting scopes cut off by the snapshot's window instead of hiding them.
- `frames` pairs frame markers into frames with their durations and each
  counter's change over the frame, from the counter samples the runtime
  records at frame ends.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
