# footfall_analysis

Answers about what a Footfall-instrumented program did, from its
snapshots, live or from capture files, with what each answer covers.

- A `Trace` holds one runtime instance's spans on per-thread tracks,
  frames, counter series, messages, resource lifetimes, and flows, built
  from a snapshot or from each window of a subscription.
- A `SpanQuery` narrows spans to frames, time ranges, names, threads, or
  labels, says how spans crossing a range's edge count, and groups them into
  a table, or returns typical and worst examples with ids that name them in
  every snapshot and capture of the instance.
- A `Comparison` of slow frames against the rest, or of one run against
  another, splits each change into more work and slower work.
- Tracks answer what a timeline draws for a visible range, merging spans
  too short to see.

Every answer carries a `Basis`: whether the data was recorded at all, what
was counted, what was left out, and where events are missing. The
[analysis guide](https://github.com/forest-rs/footfall/blob/main/docs/analysing-a-trace.md)
walks through it.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
