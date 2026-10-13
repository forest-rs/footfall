# Changelog

All notable changes to the published Footfall crates are recorded here. The
crates share one version.

## 0.1.0 (unreleased)

The first release.

### Instrumenting

- `footfall` is the entry point. Its root is the instrumentation API, `no_std`
  with no dependencies, and features add the runtime, export, sessions,
  analysis, WebTransport serving, the `tracing` layer, and Tracy as modules.
- Scopes, counters, gauges, histograms, frame, pass, and transaction
  markers, causal flows, messages, and resource lifecycles, each named by a
  `static` descriptor and attributed to a registered semantic context.
- Markers can carry the program's own frame number, so overlapping frames
  pair correctly and later observations can find a frame by number.
- Clock syncs and pairs relate other clocks, such as a GPU's or a frame
  scheduler's host clock, to the runtime's, and `Clocks` converts their
  times, following drift between syncs.
- Timelines that are not CPU threads, such as GPU queues: `timeline_span`
  records finished work in the timeline's clock with its frame and flow.
  Analysis places it on a track of its own and counts it toward its frame;
  the Chrome export draws each timeline as a track.
- Frame points record a numbered frame's deadline, predicted, target, and
  expected display times, presentation, or drop and its reason whenever the
  frame scheduler or display reports them. `Trace::pacing` says which
  frames were late and why (missed deadline, backlog, GPU, display),
  dropped, skipped, or not known, with their CPU and GPU work, how far off
  each prediction was, and the work spent on frames never shown.
- `scope!` for a scope that lasts to the end of the block; `context!` and
  `args!` for labels and per-instance arguments.
- Instrumentation levels, and `--cfg footfall_compile_out` to compile every
  emission away.

### Capturing

- `footfall_runtime`: bounded per-thread capture that keeps the newest events,
  with cursor reads that never drain, counter samples at frame ends, and
  accounting for everything it could not keep.
- Nanosecond timestamps on a monotonic clock that starts with the runtime,
  so even the shortest scopes have a duration.
- Snapshots record their provenance: runtime instance, process, wall-clock
  anchor, producer version, level, and capacities.

### Consuming

- `footfall_session`: `OwnedSnapshot`, the one form snapshots take outside
  the process, live or from a file; a compact versioned wire codec; and
  capture files.
- `footfall_session_wtransport`: serve runtimes to local tools, which find
  every producer without configuration, then fetch snapshots or subscribe
  to windows of changes. Graceful shutdown delivers everything recorded.
- `footfall_analysis`: a `Trace` of one runtime instance, built from a
  snapshot or a subscription's windows: spans on per-thread tracks with ids
  that hold across snapshots and captures, frames with counter changes,
  counter series, messages, resource lifetimes, and flows. Span queries
  filter by frame, time, name, thread, label, or argument and group into
  tables by name, thread, context, label, or argument;
  comparisons split a change into more work and slower work; and every
  answer says what it covers.
- Scopes declared with `scope_desc!`'s two-argument form record their
  function, file, and line. Snapshots, the wire format, and capture files
  carry the location once per payload rather than per event, analysis
  spans return it from `Span::location`, and the Chrome trace export shows
  it as each scope's `location` argument.
- `footfall_export`: Chrome Trace JSON for Perfetto.

### Integrations

- `footfall_tracing`: existing `tracing` spans and events, captured unchanged.
- `footfall_tracy`: a live Tracy backend, for the Tracy 0.14 profiler.
  Timelines become Tracy GPU contexts, synced from Footfall's clock syncs
  and pairs, and frame points a plot of how late each frame was shown.
- `footfall_fanout`: one backend that feeds two.
- `footfall_memory`: scratch-buffer and cache metrics built on counters.
