# Changelog

All notable changes to the published Footfall crates are recorded here. The
crates share one version.

## Unreleased

### Fixed

- Complete streamed captures no longer report runtime eviction as capture
  loss or fail `check --require losses` for it. Scope reports describe elapsed
  wall time, including waits, rather than CPU consumption.
- Closing scopes admitted before the runtime level is lowered to `Off` or
  `Counters` still emits their end events. New scopes remain filtered and
  invalid scope tokens remain ignored.

### Instrumenting

- `counter_checkpoint` samples counters and gauges at a named boundary outside
  rendering frames. Sampling frequency is explicit, can be disabled, and scans
  all counter shards. Automatic transaction/pass-end sampling is opt-in through
  `RuntimeBuilder::counter_samples_on`. Protocol 1.1 records checkpoint boundaries
  and their expected batch sizes; older readers must be upgraded first.
- `FramePointKind::BufferRequested` and `FramePointKind::BufferAcquired`
  record when a frame asked for a buffer to render into, such as by
  calling `nextDrawable`, `dequeueBuffer`, or `vkAcquireNextImageKHR`, and
  when it got one. They are frame point kinds 9 and 10 of protocol `1.1`.
- `frame_opportunity` records each chance the platform gives the program
  to begin a frame, such as a display-link callback, against the frame
  stream it was for, with the interval the platform means to leave before
  the next one. `FramePointKind::Scheduled` records the earliest time a
  frame's work was meant to begin, and whether it begins at that time or
  at the first opportunity from then on. `Backend::frame_opportunity`
  receives opportunities, and `footfall_fanout` passes them to both of
  its backends.
- `frame_plan` records what the frame scheduler decided for a frame beyond
  its times, as a `FramePlan`: the interval it meant between the frame and
  the one before it, the pipeline depth it planned for, how much later
  than the platform's prediction it aimed the frame for latency it
  learned, whether it held the frame back on purpose and why
  (`HoldReason::DrainQueue`), and whether the frame was paced or
  `FrameKind::Synchronous`, presented as soon as it was built, outside
  pacing. A scheduler had only args for these, four to a point and opaque
  to analysis. `Backend::frame_plan` receives plans, and `footfall_fanout`
  passes them to both of its backends.

- Protocol 1.1 snapshots attest the minimum runtime and process-gate recording
  level. Changing levels between reads cannot certify earlier unrecorded work as
  absent or resource totals as exact. Older captures retain explicit unknown
  historical coverage. Current recording state remains separately inspectable.

### Consuming

- Connected work inspection follows captured scope ancestry or explicitly labeled
  marker-interval activity. Typed message and flow queries provide bounded pages,
  stable event identities, full retained flows with matching steps, and recording
  evidence. CLI JSON supports surveys and targeted detail, including open scopes.
- Resource interval accounting exposes application-reported create/drop/resize
  bytes, boundary states, peaks, and survivor lifetime handles that distinguish
  logical ID reuse. Unknown sizes and missing history retain explicit uncertainty;
  grouping by kind or creating work does not infer ownership.

- Resource lifetimes retain create/update/drop provenance: event identities,
  contexts, threads, and enclosing scopes. `analyze resources` can select
  resources created by an edit scope and observed alive at a later time.
  Missing origins stay unknown; creating work does not imply current ownership.
- Named counter checkpoints expose their retained values, batch completeness,
  and originating scope. `Trace::counter_changes` and `analyze checkpoints`
  compare explicit boundaries, retaining unknown deltas and gauge endpoints.
  Differences describe global interval activity, not exclusive transaction
  attribution.
- Summary, slow-work analysis, comparison, and capture checks can select
  transaction or pass markers, or named scope instances with context labels
  and argument filters. Summary and comparison commands expose structured
  JSON with selected units, coverage, and read status. Non-frame slow-work
  analysis uses an explicit elapsed-time budget.
- `FramePacing::plan` returns what the scheduler decided for a frame, as a
  `Plan`, and analysis acts on it. A frame presented outside pacing is
  `Outcome::Synchronous`: it is not judged against a deadline or target,
  and is left out of cadence, severity, stutters, and latency steps. A
  frame shown late by no more than the scheduler held it back is
  `Outcome::Held`, not a miss, and a frame off its spacing by about its
  hold is `Cadence::Held`, not a hitch; neither has a severity, and a late
  frame's `Why::held_back` says when it was held. Without targets, a
  frame's cadence is judged against the sum of planned intervals since the
  last shown frame, including dropped frames. If any intervening interval
  is unknown, cadence is left unjudged. A
  `LatencyStep` says when the plans' pipeline depth or latency allowance
  changed across it. `OutcomeCounts` and `CadenceCounts` count held and
  synchronous frames, and `Pacing::planned` the frames with plans.
- `FramePacing::buffer_wait` says how long a frame waited for a buffer,
  and `Pacing::buffer_waits` how long the frames that waited did. A frame
  submitted after its deadline by less than it waited has the new cause
  `WaitedForBuffer`, which points at the queue: without the wait it would
  have made it. `FramePacing::severity` says how bad a frame's miss or
  hitch was, `Severity::Partial` for less than a refresh and
  `Severity::Full` for a refresh or more, within the tolerance, as
  Android's frame timeline judges it. Only frames shown late or early, or
  dropped for a miss, have one: frames late as steadily as the one before
  have none, and neither do frames shown on time, though they caught up.
  `Pacing::severity` counts them. The refresh interval fallback is computed
  once per stream when needed, keeping severity analysis from rescanning
  every presentation interval for each frame.
- The Perfetto export carries each frame's plan on its life: the interval
  the scheduler meant, its pipeline depth and latency allowance, any hold
  and why, and whether it was synchronous. On the frame timeline, a frame
  shown late by its hold is late with no jank, and a synchronous frame has
  no expected slice, no present type, and no jank.
- The Perfetto export draws a frame's wait for a buffer inside its build,
  and its frame timeline records take their severity from
  `footfall_analysis` and call a frame that waited for a buffer `Buffer
  Stuffing`.
- The Perfetto export also writes each judged frame as Android's frame
  timeline writes it, so Perfetto draws the process's `Expected Timeline`
  and `Actual Timeline`, colors each frame by its verdict, and its frame
  timeline tables and jank queries work on Footfall traces. A frame late
  as steadily as the one before is `Buffer Stuffing`, latency and not
  jank; one late through its own work is `App Deadline Missed`; one the
  platform made late is `SurfaceFlinger Scheduling`, `Prediction Error`,
  or `Display HAL`. `footfall_export` now depends on `footfall_analysis`
  for the verdicts.
- `Pacing::latency_steps` finds each lasting change in latency: where the
  time from frames' begin markers to the screen moved by at least half a
  refresh and stayed for `LATENCY_STEP_FRAMES` frames or more. A frame
  queued in the presentation pipeline shows here, though a scheduler that
  learns the extra latency keeps its frames on time and their cadence
  steady. `LatencyStep` says where, from what, and to what. Missing frame
  numbers, missing latency measurements, and event loss restart the run
  and its baseline, so isolated samples cannot establish a lasting change.
- `footfall_export::to_perfetto_trace` writes Perfetto's own protobuf
  trace format, which keeps every time to the nanosecond, and
  `write_perfetto_trace_file` writes it to a file. Threads, timelines, and
  counters get tracks of their own, flows join the events they connect,
  and each frame stream is a group of tracks: its frame opportunities,
  each frame's life from when it was to begin until it was shown, split
  into waiting, building, and in flight with its deadline and target, on
  stacked lanes for frames in flight together, and how long each shown
  frame stayed on screen. Every event of a frame, including its markers and
  GPU work, carries the frame as its correlation id, so Perfetto colors a
  frame alike wherever it appears. Chrome Trace JSON stays for other tools; Perfetto
  reads its times as doubles, which lose sub-microsecond precision at
  wall-clock times, so nested GPU work in it can be misplaced.
- `FramePacing::cadence` judges smoothness apart from latency: whether a
  shown frame came as long after the frame shown before it, in its stream,
  as their targets were apart. It is `Steady`, `Stalled` (the frame before
  stayed on screen too long) or `Hurried` (the content jumped ahead), with
  how far off. `Pacing::cadence` counts them, and
  `CadenceCounts::late_but_steady` says how many late frames were late by
  a steady amount: latency, such as frames queued ahead, not hitches.
  Unlike `Pacing::stutters`, each frame is held to its own target, so a
  cadence the program chose to change is not a hitch.
- Late frames have a new cause, `Mispredicted`: shown a fraction of a
  refresh after their targets rather than whole refreshes, so no refresh
  came at the target. `Why::whole_refreshes` says which, where the
  platform states its refresh interval with each frame opportunity.
  `LateCause::party` says who a cause points at: the program, the
  platform, frames queued ahead, or not known.
- The Chrome trace export draws each frame stream's opportunities as
  instants on a track of their own, so Perfetto shows the callbacks the
  platform made, and the ones it skipped, beside the frames. It also draws
  each stream's frames on three tracks, in the order a frame meets them:
  `frames built`, a slice for each frame's CPU work between its markers
  with an instant at its deadline; `frame targets`, an instant at each
  frame's target; and `frames on screen`, a slice from each shown frame's
  presentation to the next, with its time after its target, and dropped
  frames as instants. A frame built late ends after its deadline, a late
  frame's slice on screen starts after its target, and a stutter is a long
  slice.
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
- The session protocol is `1.1`: it adds the frame opportunity and frame
  plan observations (13 and 14) and the scheduled, buffer requested, and
  buffer acquired frame point kinds (8, 9, and 10). Captures written at
  `1.0` still read the same.
- The Chrome trace export gives timeline tracks small thread ids. They
  were `(1 << 32) + id`, and Perfetto keeps only 32 bits of a thread id,
  so a GPU timeline landed on a thread's track and took its name.

### Integrations

- `footfall_tracy` draws frame pacing live. Each frame stream's frame
  opportunities are a Tracy frame set, whose frame graph shows each skipped
  callback as a long frame, with a plot of the wait between them. Each
  frame's CPU work, between its numbered markers, is a zone on a `frames
  built` row as soon as it ends, and each shown frame's time on screen a
  zone on a `frames on screen` row, in the frame scheduler's clock. Plots
  show each frame's latency from its work to the screen and its slack
  before its deadline.

### Tools

- `footfall analyze pacing --json` includes `capture_read`, with whether
  every snapshot was consumed, whether the file was truncated, and how many
  snapshots from other runtime instances were skipped. Warnings remain on
  stderr, and a usable prefix still produces a successful result.
- `footfall analyze pacing` says how many frames were the scheduler's
  choices rather than misses or hitches: shown late by their hold, off
  their spacing by it, or presented outside pacing. Latency steps say when
  the plans' depth or latency allowance changed with them. `--json`
  carries each frame's `plan`, the `held` and `synchronous` outcomes, the
  `held` cadence, a late frame's `held_back_ns`, the `held`,
  `synchronous`, and `planned` counts, and each step's `depth` and
  `latency_allowance`. `footfall check` has a `plans` check.
- `footfall analyze pacing` reports how many misses and hitches were full
  and partial, how long frames waited for a buffer, and frames late for
  want of one; `--json` carries each frame's `severity` and
  `buffer_wait_ns`. `footfall check` has a `buffers` check.
- `footfall analyze pacing` reports how often latency stepped up and down,
  with the first steps; `--json` lists every step in `latency_steps`.
- `footfall export` writes a Perfetto trace, `.pftrace`, by default;
  `--format json` writes Chrome Trace JSON as before.
- `footfall analyze pacing` reports how many frames kept their cadence,
  stalled, or hurried, how many late frames were late by a steady amount,
  and late frames whose targets were mispredicted; `--json` carries the
  counts overall and for each stream, each frame's `cadence`, and each
  late frame's `party` and `whole_refreshes`.
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
