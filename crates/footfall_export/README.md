# footfall_export

Exports Footfall snapshots as traces for [Perfetto](https://ui.perfetto.dev)
and `chrome://tracing`.

`to_perfetto_trace` writes Perfetto's own protobuf format, which keeps every
time to the nanosecond. Scopes become slices on their thread's track,
timeline work slices on a track per timeline, and counters counter tracks,
with contexts and arguments as typed event arguments; flows join the events
they connect. Each frame stream is a group of tracks: the frame
opportunities the platform offered, each frame's life from when it was to
begin until it was shown, split into waiting, building, and in flight, with
its deadline and target, stacked on as many lanes as frames in flight at
once, and how long each shown frame stayed on screen. Every event of a frame,
including its numbered markers and GPU work, carries the frame as its
correlation id, so Perfetto colors one frame alike wherever it appears.
Each judged frame is also written as Android's frame timeline writes it, so
Perfetto draws an expected and an actual timeline for the process, colors
each frame by `footfall_analysis`'s verdict, and its frame and jank queries
work on the trace.

`to_chrome_trace_json` writes Chrome Trace Event JSON, which more tools read.
Perfetto reads its times as doubles, which at wall-clock times keep about a
quarter of a microsecond, so prefer the Perfetto format for Perfetto. Its
frame streams are drawn on four tracks: opportunities, each frame's CPU work
with its deadline, its target, and how long it stayed on screen.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
