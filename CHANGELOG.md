# Changelog

All notable changes to the published Footfall crates are recorded here. The
crates share one version.

## Unreleased

### Instrumenting

- `frame_opportunity` records each chance the platform gives the program
  to begin a frame, such as a display-link callback, against the frame
  stream it was for, with the interval the platform means to leave before
  the next one. `FramePointKind::Scheduled` records the earliest time a
  frame's work was meant to begin, and whether it begins at that time or
  at the first opportunity from then on. `Backend::frame_opportunity`
  receives opportunities, and `footfall_fanout` passes them to both of
  its backends.

### Consuming

- The Chrome trace export draws each frame stream's opportunities as
  instants on a track of their own, so Perfetto shows the callbacks the
  platform made, and the ones it skipped, beside the frames.
- `Trace::pacing` judges when each frame began against when it could have:
  at its scheduled time, or at the first of its stream's opportunities
  from then on. `FramePacing::began_late` says how much later it began,
  and `FramePacing::opportunity_late` how much later than due the
  opportunity it waited for came, when the platform skipped the one it was
  meant for. Late frames have two new causes, `SkippedOpportunity` and
  `BeganLate`, judged after `MissedDeadline`, and `Why` carries both
  conditions and whether the begin could be judged. A frame that waited
  for a skipped display-link callback, begun a refresh late and shown a
  refresh late though submitted by its deadline, was `AfterSubmit` and
  unresolved; it is now `SkippedOpportunity`, observed.
  `Pacing::opportunities` counts the opportunities recorded and skipped,
  `Pacing::opportunity_intervals` their spacing, and
  `Pacing::begin_lateness` how late the frames that began late began.
- `Outcome`, `LateCause`, `Support`, `Why`, and `DropReason` display as a
  few words, such as `late by 8.33 ms: the platform skipped its
  opportunity`, and a `Why` lists every condition that held, so a program
  can print a frame's pacing without restating the analysis.
- Breaking: `Why` and the counts `Pacing` returns (`OutcomeCounts`,
  `LateCauseCounts`, `SupportCounts`, `DeadlineCounts`, `BasisCounts`,
  `PredictionError`, `WastedWork`, and the new `OpportunityCounts`) are
  `#[non_exhaustive]`, so later releases can add to them. Their fields
  stay public to read.
- The session protocol is `1.1`: it adds the frame opportunity observation
  and the scheduled frame point kind. Captures written at `1.0` still read
  the same.
- The Chrome trace export gives timeline tracks small thread ids. They
  were `(1 << 32) + id`, and Perfetto keeps only 32 bits of a thread id,
  so a GPU timeline landed on a thread's track and took its name.

### Tools

- `footfall analyze pacing` reports each stream's frame opportunities and
  how many the platform skipped, how many frames began later than they
  could have, and the two new late causes; `--json` carries them, with
  each frame's `begins_at`, `began_late_ns`, and `opportunity_late_ns`.
  `footfall check` has two more checks: `scheduled`, whether frames say
  when they were to begin, and `opportunities`, whether the frames that
  wait for one have their stream's recorded. The release captures predate
  both.
- `footfall check` says what a capture gives the analyses: frame markers
  and numbers, deadlines, submissions, presentations, stated timing bases,
  clock syncs, GPU work, counter samples, losses, and whether the output
  looked visible. For each one it says whether the capture has it, what
  depends on it, and the call that records it. `--require` fails unless
  the named checks are fully there, and `--json` prints the result for
  other programs.

## 0.1.0 (2026-10-14)

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
  Numbers belong to a `FrameStream`, such as one per window, with a
  generation for when its numbering starts again; plain numbers belong to
  the default stream.
- Clock syncs and pairs relate other clocks, such as a GPU's or a frame
  scheduler's host clock, to the runtime's, and `Clocks` converts their
  times, following drift between syncs. `Clocks::forget_before` bounds
  their history for live tools, and `Converted::settled` says whether
  later readings could still change a conversion.
- Timelines that are not CPU threads, such as GPU queues: `timeline_span`
  records finished work in the timeline's clock with its frame and flow.
  Analysis places it on a track of its own and counts it toward its frame,
  nesting it whatever order it is read back in and moving it when later
  clock readings place it better; the Chrome export draws each timeline as
  a track.
- Frame points record a numbered frame's deadline, predicted, target, and
  expected display times, presentation, or drop and its reason whenever the
  frame scheduler or display reports them. `Trace::pacing` says which
  frames were early, late and why (missed deadline, backlog, GPU,
  display), dropped, skipped, or not known, with their CPU and GPU work,
  how long each stayed on screen, how far off each prediction was, and the
  work spent on frames never shown. A late frame's `why` lists every
  condition that held and says whether its cause was observed, inferred, or
  left unresolved by what the trace recorded. Points say whether their
  time was observed or estimated, can carry the recorder's numbers about
  the frame, such as a scheduler's state, and include when the frame was
  submitted and the time its content was made for. `Pacing::deadlines`
  counts the frames submitted by and after their own deadlines.
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
- Snapshots are plain values that need only `alloc`: without its `std`
  feature `footfall_runtime` has the snapshot types alone, for programs
  without `std` to build snapshots by hand.

### Consuming

- `footfall_session`: `OwnedSnapshot`, the one form snapshots take outside
  the process, live or from a file; a compact versioned wire codec; and
  capture files. `CaptureRecorder` records a program's own runtime to a
  capture as it runs and says what it wrote when it finishes, and
  `CaptureReader::join` reads a whole capture as one snapshot, saying what
  it could not join. Captures stay readable by later releases at the same
  protocol major version, or fail with an `IncompatibleVersion` naming the
  version that wrote them; captures recorded by real programs are tested
  with every change.
- Without `std`, on `alloc` alone, `footfall_session` has `OwnedSnapshot`,
  publishing and reassembling envelopes, and the codec, which fails with a
  `CodecError` saying whether an envelope could not be encoded, its bytes
  are malformed, or a newer protocol produced it. Capture files need `std`.
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

### Tools

- `footfall_cli`, the `footfall` command: `top` watches a running session
  live or replays a capture, `record` saves a session to a capture file,
  `analyze` summarizes a capture (scopes, timelines, counters, and
  histograms), explains its slow frames and its frame pacing, compares two
  runs, or shows one span, and `export` writes a Chrome trace. `summary`
  and `slow` judge each frame against its own deadline when the trace
  records them, so they hold at any refresh rate, and against a budget
  (16.7 ms by default, or `--budget-ms`) when it does not.
  `analyze pacing --json` prints the pacing judgment, every frame with its
  points, basis, args, and why, as one JSON document for other programs.
