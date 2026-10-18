# Instrumenting an App

This is the shortest path for using Footfall in an application. For how the
pieces fit, see [the architecture](architecture.md); for runnable programs,
see [the examples](../examples/README.md).

## Goal

A host application should be able to:

- emit counters, scopes, markers, and resource lifecycle events
- attach semantic context to those observations
- collect them locally with bounded runtime cost
- expose them to downstream tooling without coupling hot paths to transport code

## Recommended Stack

Depend on `footfall` and turn on what the application needs:

```toml
[dependencies]
footfall = { version = "0.1", features = ["runtime", "export"] }
```

- no features: descriptors, contexts, and emission calls. This is the only
  Footfall dependency an instrumented library needs.
- `runtime`: bounded local collection, as `footfall::runtime`
- `export`: write a capture as a Perfetto trace, or as Chrome Trace JSON, as
  `footfall::export`
- `wtransport`: a local attach path for tools, as `footfall::wtransport`
- `tracing`: forward `tracing` spans and events, as `footfall::tracing_layer`
- `tracy`: live viewing in Tracy, as `footfall::tracy`; combine it with the
  runtime through the `footfall_fanout` crate
- the `footfall_memory` crate when you want scratch or cache helpers

## Minimal Setup

1. Define static descriptors close to the subsystem being instrumented.
2. Install one `footfall::runtime::Runtime` once at process startup.
3. Register a `Context` once per semantic owner and emit with its handle.
4. Keep transport and UI concerns out of the hot path.

Example shape:

```rust
use footfall::{
    CounterDesc, InstrumentationLevel, LabelKey, LazyContext, RegisteredContext, ScopeDesc,
    counter_add, scope,
};
use footfall::runtime::Runtime;

static RECORD_SCOPE: ScopeDesc = footfall::scope_desc!("record_frame");
static DRAW_OPS: CounterDesc = footfall::counter_desc!(count, "draw_ops");

// Footfall ships no label vocabulary; declare the keys your app attributes by.
static SUBSYSTEM: LabelKey = LabelKey::new("subsystem");
static PASS: LabelKey = LabelKey::new("pass");

// A context for the whole subsystem, registered on first use after install.
static RENDERER: LazyContext = LazyContext::new(|| footfall::context!(SUBSYSTEM = "renderer"));

fn install_footfall() -> &'static Runtime {
    Runtime::builder()
        .process_name("my_app")
        .level(InstrumentationLevel::Scoped)
        .install_global()
        .expect("runtime should install once")
}

/// One semantic owner: the opaque pass, with its context registered once.
struct OpaquePass {
    context: RegisteredContext,
}

impl OpaquePass {
    fn new() -> Self {
        Self {
            context: RegisteredContext::new(&footfall::context!(PASS = "opaque")),
        }
    }

    fn record(&self) {
        let _scope = scope(&RECORD_SCOPE, self.context.handle());
        counter_add(&DRAW_OPS, self.context.handle(), 42);
        counter_add(&DRAW_OPS, RENDERER.handle(), 42);
    }
}
```

The low-level `scope_begin` and `scope_end` calls remain available when a scope
crosses lexical boundaries.

A scope lasts as long as its guard. Bind the guard to a named variable such as
`_scope`, or use `footfall::scope!(&RECORD_SCOPE, handle)`, which covers the
rest of the block; `let _ = scope(..)` drops the guard immediately and records
an empty scope, even though the compiler's `must_use` hint suggests that form.

Descriptors are identified by address, so declare them as `static` items,
never `const`; the compiler rejects a `const` descriptor where one is
required. Label keys are identified by name, so give keys distinct names; a
library should prefix its keys with its own name.

Give `scope_desc!` a second argument, the function or module the scope
belongs to, and the descriptor also records where it is declared: that name
with the file and line of the macro call. Snapshots carry the location with
each scope, so tools can lead from a span to its code, and the Chrome trace
export shows it as the scope's `location` argument:

```rust
use footfall::ScopeDesc;

static LAYOUT: ScopeDesc = footfall::scope_desc!("layout", "renderer::layout");
assert_eq!(
    LAYOUT.location().map(|location| location.function()),
    Some("renderer::layout")
);
```

## Context Discipline

The architectural rule is simple:

- build a `Context` where an owner is created, extending its parent's labels
- register it once and pass the `ContextHandle` explicitly; never register on
  every emission
- release it when the owner goes away, usually by dropping a
  `RegisteredContext`
- register after installing the runtime, or use `LazyContext` for contexts in
  `static` items; a backend never learns about contexts registered before it
  was installed, and attributes their observations to the root context
- keep values compact and aggregatable

Equal contexts registered separately get distinct handles and are not merged,
so share the handle rather than re-registering the same labels.

Typical label keys name the owners an app reasons about, such as `frame`,
`transaction`, `pass`, `node`, `resource`, `cache`, or `reason`.

A handle is valid while its `RegisteredContext` is alive. Keep the
`RegisteredContext` with its owner rather than copying the handle out of a
temporary. If a released handle is used anyway, the runtime attributes the
events recorded with it to the root context and counts them in
`Drops::unattributed`, and counts counter rows updated with it in
`Drops::updated_after_release`, because an aggregated row cannot be split.

Avoid building instrumentation around ad hoc strings when an id or integer will do.

## Getting Data Out

A runtime keeps a recent window of history that anyone can read without
draining it. A snapshot holds the current contexts, counters, and histograms,
and the retained events; read it directly, or export it:

```rust
use footfall::runtime::Runtime;

fn write_trace(runtime: &Runtime) -> std::io::Result<()> {
    let snapshot = runtime.snapshot();
    footfall::export::write_perfetto_trace_file(&snapshot.into(), "trace.pftrace")
}
```

To capture one region of work, take a cursor first and read only what was
recorded after it:

```rust
use footfall::runtime::Runtime;

fn capture_region(runtime: &Runtime, work: impl FnOnce()) -> footfall::runtime::Snapshot {
    let cursor = runtime.cursor();
    work();
    runtime.snapshot_since(cursor)
}
```

Open `trace.pftrace` in [Perfetto](https://ui.perfetto.dev). Scopes appear
as slices on one track per thread, counters as counter tracks, contexts and
arguments as slice arguments, GPU work on its own tracks, and each frame
stream as a group of tracks showing every frame's life. For
`chrome://tracing`, write Chrome Trace JSON with `write_chrome_trace_file`
instead.

To record a run to a capture file that the [`footfall`
command](../tools/footfall_cli/README.md) reads, without serving a session,
write what was recorded since the last write every so often, such as every
few hundred frames, and once more at exit:

```rust
use footfall::runtime::Runtime;
use footfall::session::capture::CaptureRecorder;
use std::{fs::File, io::BufWriter};

fn record(runtime: &Runtime, frames: impl Iterator<Item = u64>) -> std::io::Result<()> {
    let file = BufWriter::new(File::create("run.ffcap")?);
    let mut recorder = CaptureRecorder::new(runtime, file)?;
    for frame in frames {
        // ... the frame ...
        if frame % 300 == 0 {
            let window = recorder.write()?;
            if window.missed() > 0 {
                eprintln!("{} events evicted before they were written", window.missed());
            }
        }
    }
    let finished = recorder.finish()?;
    eprintln!("wrote {} events to run.ffcap", finished.events());
    Ok(())
}
```

Each write is flushed, so a run that ends abruptly leaves every complete
write readable. Write often enough that the runtime's event buffer does not
evict between writes; see [Sizing Capacity](#sizing-capacity).

## Sizing Capacity

A runtime never grows past the capacities it was built with; what does not
fit is dropped and counted. Check `Snapshot::drops` before trusting totals,
and size each capacity for what the app emits:

- `counter_capacity`: counter, gauge, and histogram descriptors times the
  contexts each is recorded under. Recording a counter under a context per
  frame or per item multiplies the keys it needs; for per-frame values,
  record under a stable context and use counter samples instead (below).
- `event_capacity`: two events per scope plus one per marker, flow step,
  message, or resource change, over the span of history you want to keep.
  When it is full, the oldest events are evicted.
- `context_capacity`: registered contexts, live and released. A released
  context is retired, with its counter rows, only once every event recorded
  before its release has been evicted, so with a large event buffer the
  released contexts add up: size this for the live contexts plus those
  released over the span of history the event buffer keeps. Snapshots mark
  released contexts (`ContextSnapshot::is_released`), and `Drops::retired_rows`
  counts the counter rows retired with them, which totals across contexts no
  longer include.
- The `tracing` layer registers a context for every span with numeric or
  boolean fields, and releases it when the span closes, so busy spans count
  against `context_capacity` too.

Each `Drops` count names the capacity to raise.

## Per-Frame Values

With frame markers (`frame_begin` and `frame_end`), each frame end records
a counter sample for every counter and gauge the frame updated: its value
at that moment, merged across threads. A counter's change over a frame is
the difference between consecutive samples, so per-frame upload bytes or
draw counts need no context per frame. Samples also plot as a time series
for each counter in the Chrome export. Turn them off with
`RuntimeBuilder::counter_samples(false)`.

When the program has its own frame numbers, such as a frame scheduler's
frame index, mark frames with `frame_begin_numbered` and
`frame_end_numbered`. A numbered end closes the begin with the same number,
so frames that overlap, with one being encoded while the previous one is
still in flight, pair correctly, and analysis finds a frame by its number
with `Trace::frame_numbered`.

```rust
use footfall::ContextHandle;

for frame_index in 0..3_u64 {
    footfall::frame_begin_numbered(frame_index, ContextHandle::ROOT);
    // ... build and submit the frame ...
    footfall::frame_end_numbered(frame_index, ContextHandle::ROOT);
}
```

A program with more than one stream of frames, such as several windows,
gives each a `FrameStream` and numbers its frames within it, so two windows'
frame 42 stay apart in markers, timeline work, and frame points. When a
window's frame driver or surface is recreated and its numbering starts
again, give the stream a new generation. Plain numbers belong to
`FrameStream::DEFAULT`.

```rust
use footfall::{ContextHandle, FrameStream};

let main_window = FrameStream::new(1);
let inspector = FrameStream::new(2);
footfall::frame_begin_numbered(main_window.frame(42), ContextHandle::ROOT);
footfall::frame_end_numbered(main_window.frame(42), ContextHandle::ROOT);
footfall::frame_begin_numbered(inspector.frame(42), ContextHandle::ROOT);
footfall::frame_end_numbered(inspector.frame(42), ContextHandle::ROOT);

// The inspector's surface was recreated, and its numbering began again.
let inspector = inspector.with_generation(1);
footfall::frame_begin_numbered(inspector.frame(0), ContextHandle::ROOT);
footfall::frame_end_numbered(inspector.frame(0), ContextHandle::ROOT);
```

## Other Clocks

Some times come from clocks other than the runtime's: a GPU's timestamp
counter, or the host clock a frame scheduler or display reports in, such
as Mach absolute time or a performance counter. Name each with a static
`ClockDesc` and tell the runtime how it relates:

- `clock_sync` records that a clock read some ticks just now. Call it
  right after reading the clock, for example once per frame.
- `clock_pair` records two clocks read at the same moment, such as the GPU
  and host timestamps that Metal's `sampleTimestamps`, Vulkan's calibrated
  timestamps, or Direct3D's clock calibration return together.

Analysis converts a clock's times to the runtime's from these readings,
interpolating between syncs so drift between the clocks is followed. A
GPU clock paired with a synced host clock converts through it.

```rust
use footfall::{ClockDesc, ClockReading, TickPeriod};

static HOST: ClockDesc = ClockDesc::new("host");
static GPU: ClockDesc = ClockDesc::new("gpu");

// Mach absolute time on Apple silicon: 125/3 ns per tick.
let host_period = TickPeriod::new(125, 3);
let host_now: u64 = 1_000_000; // read the host clock here
footfall::clock_sync(ClockReading::new(&HOST, host_now, host_period));

// A GPU and host timestamp sampled together.
let (gpu_ticks, host_ticks): (u64, u64) = (7_000, 1_000_240);
footfall::clock_pair(
    ClockReading::new(&GPU, gpu_ticks, TickPeriod::NANOSECOND),
    ClockReading::new(&HOST, host_ticks, host_period),
);
```

## GPU and Other Timelines

Work that does not run on a CPU thread, such as passes on a GPU queue,
goes on a timeline: name it with a static `TimelineDesc` (`TimelineDesc::gpu`
for a GPU queue) and report each piece of work once its times are known,
with `timeline_span`. The times are ticks of the timeline's clock, such as
the GPU's timestamp counter; relate that clock to the runtime's with
`clock_sync` or `clock_pair`, as above, and analysis places the work.

Give each span the frame it belongs to with `in_frame`, and the flow its
CPU submit began with `after_flow`, so analysis can count a frame's GPU
work toward that frame, even though it runs while the CPU works on the next
one, and lead from the submit to the work. Report a frame's whole span
before the passes inside it, so they nest.

```rust
use footfall::{
    ClockDesc, ClockReading, ContextHandle, FlowId, ScopeDesc, TickPeriod, TimelineDesc,
    TimelineSpan,
};

static GRAPHICS: TimelineDesc = TimelineDesc::gpu("graphics");
static GPU_CLOCK: ClockDesc = ClockDesc::new("gpu");
static SHADOWS: ScopeDesc = ScopeDesc::new("shadows");

let frame = 1234_u64;
// When submitting: start a flow the GPU work will end.
footfall::flow_begin(FlowId::new(frame), ContextHandle::ROOT);

// Frames later, with the pass's timestamps read back and the clock synced:
footfall::clock_sync(ClockReading::new(&GPU_CLOCK, 9_000_000, TickPeriod::NANOSECOND));
footfall::timeline_span(
    &TimelineSpan::new(
        &GRAPHICS, &SHADOWS, ContextHandle::ROOT,
        &GPU_CLOCK, TickPeriod::NANOSECOND, 8_000_000, 8_400_000,
    )
    .in_frame(frame)
    .after_flow(FlowId::new(frame)),
);
```

In Tracy, through `footfall_tracy`, each timeline is a GPU context: a row
of its own above the CPU threads, with its spans as GPU zones. The context
is made once the timeline's clock is synced, and each later sync keeps it in
step. The backend sends a frame's spans when the CPU frame they were
reported in ends, nested by containment whatever order they came in. Tracy
has no flows, and its CPU-to-GPU delay shows the time to read the work back
rather than the time since submitting it.

## Frame Pacing

Whether a frame was shown when it was meant to be is known only to the
frame scheduler and the display, and often only frames later. Record each
time as it becomes known with `frame_point`, against the frame's number:
its `Deadline` (the latest it could be submitted) and `Target` (when it
should be shown) when it is planned, and `Presented` when the display
reports it, or `FramePoint::dropped` with a `DropReason` if it never will
be. Report what the platform predicted too, when it says: `Predicted`, as
a display link's target timestamp when the frame begins, and `Expected`,
the presentation time known at submit. The times are ticks of the
scheduler's clock, synced with `clock_sync` like any other clock. With
frames marked by number and GPU work reported with its frame, analysis then
says which frames were late, by how much, why, how good the predictions
were, and what the late frames' work did differently.

Say why a frame was dropped. A frame the program superseded with newer work
(`DropReason::Superseded`) is not a miss, while one still waiting when its
deadline passed (`DropReason::MissedDeadline`) is, and so is one the
platform reports it never presented without saying why
(`DropReason::NotPresented`). A frame dropped because the display was
asleep or the window hidden or minimized is `DropReason::OutputUnavailable`.
Without it, a run that nobody could see looks like one that dropped every
frame.

Say how each time of something that happened was known, with
`FramePoint::with_basis`: a presentation time the compositor reported is
`TimingBasis::Observed`, one assumed to be the next refresh is
`TimingBasis::Estimated`, and analysis counts how many of its judgments rest
on estimates. Record `Submitted` when the frame was handed to the
compositor or swapchain, which can differ from when its CPU work ended, and
`Sampled`, the time its content was made for, such as the animation time.

Say when each frame was meant to begin, and what it waited for. Record
each frame opportunity the platform offers as it arrives, with
`frame_opportunity`: every display-link callback, vblank notification, or
compositor frame callback, including those at which no frame began, with
the interval the platform means to leave before the next, such as the
refresh interval. And record the `Scheduled` point when the frame is
planned: the earliest time its work was meant to begin, and whether it
`BeginsAt::Time`, when the program wakes for it, or
`BeginsAt::Opportunity`, at the first opportunity from then on. Analysis
then tells a frame the program began late from one whose opportunity the
platform skipped, and counts the opportunities skipped.

```rust
use footfall::{
    BeginsAt, ClockDesc, FrameOpportunity, FramePoint, FramePointKind, FrameStream, TickPeriod,
};

static HOST: ClockDesc = ClockDesc::new("host");
let period = TickPeriod::new(125, 3); // Mach absolute time
let (now, refresh) = (1_000_000_u64, 200_000_u64); // 120 Hz

// In every display-link callback:
footfall::frame_opportunity(
    &FrameOpportunity::at(FrameStream::DEFAULT, &HOST, period, now).with_interval(refresh),
);
// When planning frame 1234 to begin at the next callback:
footfall::frame_point(&FramePoint::at(
    1234,
    FramePointKind::Scheduled(BeginsAt::Opportunity),
    &HOST,
    period,
    now + refresh / 2,
));
```

Say what the scheduler decided, too. A `FramePlan`, recorded with
`frame_plan` when the frame is planned, carries the interval the scheduler
meant between this frame and the one before it, the pipeline depth it
planned for, how much later than the platform's prediction it aimed the
frame for latency it learned, and any hold: a frame held back on purpose,
such as a refresh to let a frame queued in the presentation pipeline
drain. Analysis then holds the frame to the cadence the scheduler meant,
so a deliberate slower one is not a hitch, explains a held frame as the
scheduler's choice rather than jank, and says when a change of latency
came with a change of depth or allowance. A frame presented as soon as it
was built, outside pacing, such as one a window resize waits for, is
`synchronous()`: it has no deadline or target to miss and is no part of
its stream's cadence.

```rust
use footfall::{FramePlan, HoldReason, TickPeriod};

let period = TickPeriod::new(125, 3); // Mach absolute time
let refresh = 200_000_u64; // 120 Hz

// When planning frame 1234, two deep, held back a refresh to drain a
// frame queued ahead:
footfall::frame_plan(
    &FramePlan::new(1234, period)
        .with_interval(refresh)
        .with_depth(2)
        .with_hold(refresh, HoldReason::DrainQueue),
);
// A frame drawn for a live resize, presented at once:
footfall::frame_plan(&FramePlan::new(1235, period).synchronous());
```

Numbers the scheduler worked from, such as its pipeline depth or safety
margin, belong with the frame they describe: attach them to that frame's
point with `FramePoint::with_args` rather than setting a gauge when the
report arrives, which may be frames later. `FramePacing::args` returns them.

```rust
use footfall::{
    Args, ClockDesc, ClockReading, FramePoint, FramePointKind, TickPeriod, TimingBasis,
};

static HOST: ClockDesc = ClockDesc::new("host");
let period = TickPeriod::new(125, 3); // Mach absolute time
let (now, target, deadline) = (1_000_000_u64, 1_400_000_u64, 1_300_000_u64);

footfall::clock_sync(ClockReading::new(&HOST, now, period));
footfall::frame_point(&FramePoint::at(1234, FramePointKind::Target, &HOST, period, target));
footfall::frame_point(
    &FramePoint::at(1234, FramePointKind::Deadline, &HOST, period, deadline)
        .with_args(Args::none().with("pipeline_depth", 2_u32)),
);
// Frames later, when the display reports the frame:
footfall::frame_point(
    &FramePoint::at(1234, FramePointKind::Presented, &HOST, period, target)
        .with_basis(TimingBasis::Observed),
);
// Or, if newer work replaced the next frame before it was submitted:
footfall::frame_point(&FramePoint::dropped(1235, footfall::DropReason::Superseded));
```

Live in Tracy, through `footfall_tracy`, a frame stream's opportunities are
a frame set, whose frame graph shows each skipped callback as a long frame,
and its frames are two GPU-context rows, each frame's CPU work and how long
each shown frame stayed on screen, with plots of each frame's latency from
its work to the screen and its slack before its deadline.

## Checking What You Recorded

Run `footfall check` on a capture to see what it gives the analyses. It
covers frame markers and numbers, deadlines, submissions, presentations,
stated timing bases, clock syncs, GPU work, counter samples, losses, and
whether the output looked visible. For each one it says whether the
capture has it, which conclusions depend on it, and the call that records
it, with a link to the section of this guide that explains it. Add
`--require` to make it a test:

```sh
footfall check run.ffcap --require presentations --require gpu
```

## What Lives Where

Call sites should stay in subsystem code:

- descriptors
- semantic context construction
- emission calls

Collection and attach should stay near application startup:

- runtime install
- fanout setup
- session server setup

That keeps the instrumentation seam boring and lets the host app change how it
inspects data without rewriting every call site.

## Always-On vs Diagnostic Use

Use counters and a small amount of scoped tracing for always-on instrumentation.

Set runtime capture policy explicitly when installing the process runtime:

- `InstrumentationLevel::Off` for compiled-in but inactive instrumentation
- `InstrumentationLevel::Counters` for broad low-detail observation
- `InstrumentationLevel::Scoped` for counters plus scope/marker/flow timing
- `InstrumentationLevel::Trace` for the full current observation set, including resources

For a runtime installed with `Runtime::install_global`, the level also sets
`footfall_core`'s process-wide maximum level, so excluded emissions return
after one atomic load instead of reaching the runtime.

Turn on richer downstream inspection when needed:

- local snapshots
- local attach
- Tracy
- export

The emission seam should not need to change between those modes.

To remove instrumentation from a build entirely, build the final binary with
`RUSTFLAGS="--cfg footfall_compile_out"`. Every emission then compiles to an
empty inlined function and no backend sees anything. It is a compiler `cfg`
rather than a cargo feature so that no dependency, and no `--all-features`
run, can switch it on for you; libraries should never set it.

## Serving a Live Session

To let a separate consumer app read the process while it runs, serve the
runtime with the `wtransport` feature:

```rust
use footfall::runtime::Runtime;
use footfall::wtransport::{LocalSessionServer, ServedSession};

async fn serve(runtime: &'static Runtime) -> std::io::Result<LocalSessionServer> {
    LocalSessionServer::start(vec![ServedSession::new("main", runtime)]).await
}
```

Keep the returned server alive for as long as the process should be
attachable, and call `shutdown` on it before exiting so attached tools
receive everything recorded up to the end. Tools find it with
`SessionClient::discover_local()`, and any number of them can read or
subscribe at once without affecting each other or the runtime.

If the application also forwards `tracing` into Footfall, filter that layer
to the application's own targets: the QUIC stack behind the session server
traces every packet, and an unfiltered layer captures the work of serving the
capture.

The reference shapes:

- the [demo_app](../examples/src/bin/demo_app.rs) example instruments a
  renderer and serves it
- the [attach](../examples/src/bin/attach.rs) example is a minimal tool that
  follows a session
- the [`footfall` command](../tools/footfall_cli/README.md) records, watches,
  and analyzes sessions

## Limits

- the local attach path is intentionally localhost-first
