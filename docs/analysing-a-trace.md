# Analysing a Trace

`footfall::analysis` answers questions about what an instrumented program
did, from its snapshots: in process, from a live session, or from a capture
file. This guide covers the model, the questions it answers, and what each
answer says about its own completeness.

## The Trace

A `Trace` holds what one runtime instance recorded, as far as its snapshots
show. Build one from a snapshot, or push each window of a subscription or
capture file:

```rust
use footfall::analysis::{Continuity, Trace};
use footfall::runtime::Runtime;
use footfall::session::OwnedSnapshot;

let runtime = Runtime::default();
let mut trace = Trace::new();
let mut cursor = runtime.cursor();
for _ in 0..3 {
    // ... the program records ...
    let window = runtime.snapshot_since(cursor);
    cursor = window.next_cursor();
    let pushed = trace.push(&OwnedSnapshot::from(window)).expect("one runtime instance");
    if let Continuity::Gap { missed } = pushed.continuity() {
        eprintln!("{missed} events were evicted before this read");
    }
}
```

A trace pairs scope begins and ends into `Span`s, each on its thread's
`Track` at its depth, and markers into `Frame`s. It keeps counter samples
as `CounterSeries`, messages, resource lifetimes, and flows, each flow step
placed in the span it happened in.

Every span has a `SpanId`, the sequence number of the event that began it.
It names the same span in every snapshot and capture file of that runtime
instance, so a finding can cite it and anyone can look it up again with
`Trace::span`. Frames have `FrameId`s the same way, and a frame the program
numbered itself is also found by that number with `Trace::frame_numbered`. Ids mean nothing in
another instance: `Trace::push` refuses a snapshot from one, and a tool
following a restarting producer starts a new trace.

A trace following a live session grows until the tool calls
`forget_before`, which drops what ended earlier; answers then count the
forgotten time as unobserved. That covers frame points too: a frame's
points go together once all were recorded earlier and its markers and GPU
work are gone, so feedback that arrives after a frame's work keeps the
frame's target with it. Clocks keep their last reading before the cut, so
the times after it convert as they did.

## GPU Work

Work recorded on a timeline that is not a CPU thread, such as a GPU queue,
gets a `Track` of its own: `Trace::timelines` lists them, and its spans
nest and have self time like a thread's. Their times come from another
clock, converted through the trace's `Clocks`; `Span::placement` says how,
and an answer's `Basis` counts converted spans, how far the farthest lay
from a clock reading, and spans that could not be placed because their
clock was never synced.

Spans nest whatever order they are read back in, and a span placed past its
clock's last reading, at the clock's nominal rate, moves when the next
reading arrives; `Pushed::moved_spans` lists the spans a snapshot moved.
So a trace built from a live session's windows ends up the same as one
built from a single snapshot of the same events, provided each clock's
readings arrive in tick order. `Converted::settled` says whether a
conversion can still change.

Queries cover every track unless narrowed: `on_threads` for the CPU,
`on_timelines` or `on_timeline` for GPU work. A frame's GPU work usually
runs while the CPU works on the next frame, so spans reported with a frame
number count toward that frame when a query or comparison selects frames,
whole, wherever they ran; `Trace::frame_spans` lists them.

## Frame Pacing

`Trace::pacing(tolerance)` joins each numbered frame's CPU work, its GPU
work, and the frame points a frame scheduler or display reported into a
`FramePacing`: its deadline, its predicted, target, and expected display
times, when it was shown, how long its GPU was busy, and an `Outcome`.
Frames are numbered within frame streams, such as one per window; each
frame is judged against the frame before it in its own stream, and
`Pacing::in_stream` narrows the answers to one stream's frames.

| Outcome | Meaning |
|---|---|
| `OnTime` | Shown within `tolerance` of its target, either way |
| `Early { by }` | Shown more than `tolerance` before its target: on screen sooner than the time it was made for |
| `Late { by, cause }` | Shown after its target, and why |
| `Dropped { reason }` | Never shown, and why |
| `SubmittedLate { by }` | Submitted past its deadline, with no presentation time to confirm it |
| `Unknown` | Too little reported to tell |

A late frame's `LateCause` is the first of these the trace shows, in order:

| Cause | The frame |
|---|---|
| `MissedDeadline` | had its CPU work submitted after its deadline |
| `Backlog` | was in time, but the frame before it was still being shown at this one's target |
| `Gpu` | was in time, but its GPU work finished after its target |
| `Display` | was submitted and rendered in time with nothing ahead of it: the compositor or display held it |
| `AfterSubmit` | was in time with nothing ahead of it, but has no GPU work to tell the GPU from the display |
| `Unknown` | has no deadline or CPU work to judge |

So the first frame of a run of late frames carries the cause, and the ones
behind it say `Backlog`.

The cause is the leading explanation, not proof. `FramePacing::why` says
what the trace shows: every condition that held (submitted after the
deadline, GPU work finishing after the target, the frame before still
waiting), whether the submission time was reported or read from the end
marker, whether GPU work was recorded, how the presentation time was known,
and how strongly the trace supports the cause:

| `Support` | Meaning |
|---|---|
| `Observed` | The trace recorded the condition: a late submission, or recorded GPU work finishing after the target |
| `Inferred` | It rests on something not observed: a queue read from the frame before's presentation, or an estimated presentation time |
| `Unresolved` | The program's part looked in time, and what came after was not observed |

`OutcomeCounts::late_support` counts late frames by support, so an answer
can say how much of it rests on inference. Only some drops are misses (`DropReason::is_miss`):
a frame that missed its deadline or found no surface leaves the previous
frame on screen, but one the program discarded or superseded, or that a
later frame replaced, leaves nothing missing. `OutcomeCounts` counts those
as `skipped` rather than `dropped`, and `Pacing::wasted` adds up the CPU and
GPU work spent on every frame never shown, and says how many of them had
each recorded. A frame's GPU busy time counts the time covered by its spans
on each GPU timeline once, however they nest or overlap, and is `None` when
no GPU work was recorded for it.

`Pacing::prediction_error` says how good each prediction of a frame's
display time was: the platform's `Predicted` time, the scheduler's
`Target`, and the `Expected` time at submit, each against when the frame
was shown, as how far off and how many frames were shown early or late by
more than the tolerance. A scheduler that plans against bad predictions
misses frames it had time for.

A frame's deadline is judged by its `Submitted` time when the recorder
reported one, and by its end marker otherwise. `FramePacing::basis` says how
each time was known, and `Pacing::presentation_basis` how many presentation
times were observed and how many estimated: a frame judged late on an
estimated presentation time is only as late as the estimate.
`FramePacing::args` returns the numbers a recorder attached to a point,
such as a scheduler's state when it planned the frame.

`Unknown` is not on time: without a target and a presentation time, a
frame's pacing cannot be judged, and a trace with no presentation feedback
says so rather than counting every frame as fine. An early frame is not a
miss, but it is not fine either: an animation drawn for its target shows
the wrong moment.

`Pacing::intervals` is how long each presented frame stayed on screen,
from one presentation to the next. A frame known never to have been shown
leaves the one before it on screen, so the interval runs across it and
shows the stall. Where the trace cannot say what was on screen, because a
frame reported neither a presentation nor a drop, a frame number is
missing, or events were lost, the interval is left out and counted in
`interval_breaks` rather than guessed. `Pacing` also gives stutters
(intervals at least half again the median), how late the late frames were,
and latency from each frame's begin marker to its presentation. A frame's
`work` is the time between its markers, waits included, and its latency
starts at its begin marker, not at the input it answers.
`frame_ids` turns an outcome into frames to compare:

```rust
use footfall::analysis::{Comparison, FrameId, GroupBy, Outcome, Population, Selection, Trace};
use std::time::Duration;

fn why_late(trace: &Trace) {
    let pacing = trace.pacing(Duration::from_millis(1));
    let frames = |ids: Vec<FrameId>| {
        Population::frames(trace, ids.into_iter().filter_map(|id| trace.frame(id)))
    };
    let on_time = frames(pacing.frame_ids(|outcome| outcome == Outcome::OnTime));
    let missed = frames(pacing.frame_ids(Outcome::missed));
    let comparison = Comparison::between(&on_time, &missed)
        .selecting(Selection::Inside)
        .group_by(GroupBy::Name)
        .run();
    for row in comparison.rows().iter().take(5) {
        println!("{}: {:+.0} µs per frame", row.describe(), row.change_micros());
    }
}
```

## Asking Where the Time Went

`Trace::spans` starts a `SpanQuery`. Narrow it, choose how spans count, group
it, and ask for a table:

```rust
use footfall::analysis::{GroupBy, Selection, Trace};

fn slow_frame_breakdown(trace: &Trace) {
    let slow: Vec<_> = trace
        .frames()
        .iter()
        .filter(|frame| frame.duration().as_micros() > 16_667)
        .map(|frame| frame.id())
        .collect();
    let table = trace
        .spans()
        .in_frames(&slow)
        .selecting(Selection::Inside)
        .group_by(GroupBy::Label("pass".into()))
        .group_by(GroupBy::Name)
        .table();
    for row in table.rows().iter().take(10) {
        let stats = row.stats();
        println!(
            "{}: {} spans, {:?} total, {:?} self, p99 {:?}",
            row.describe(),
            stats.count(),
            stats.total(),
            stats.self_time(),
            stats.time().p99()
        );
    }
    println!("{}", table.basis());
}
```

Rows come most total time first. Grouping by a label never drops spans:
those whose context lacks the label form a `(no pass)` group of their own.

### Grouping by an Argument

A label belongs to a span's context, shared by everything recorded under
it. An argument belongs to one span, such as the number of nodes one layout
pass handled. `GroupBy::Arg` groups by an argument's value, `with_arg` and
`without_arg` narrow to spans with or without one, and `Span::arg` reads
one. Spans without the argument form a `(no nodes)` group of their own.

Each distinct value is a group, so a number that rarely repeats makes
nearly as many groups as spans. To see cost against such a number, read it
from each span and group the spans in ranges that suit it:

```rust
use footfall::analysis::{DurationStats, OwnedLabelValue, Trace};
use std::collections::BTreeMap;

fn layout_by_nodes(trace: &Trace) {
    let mut by_thousands = BTreeMap::<u64, Vec<_>>::new();
    for selected in trace.spans().named("layout").iter() {
        if let Some(OwnedLabelValue::Unsigned(nodes)) = selected.span().arg("nodes") {
            by_thousands
                .entry(nodes / 1000)
                .or_default()
                .push(selected.time());
        }
    }
    for (thousands, times) in by_thousands {
        let stats = DurationStats::of(times);
        println!(
            "{}..{} nodes: {} spans, median {:?}, max {:?}",
            thousands * 1000,
            thousands * 1000 + 999,
            stats.count(),
            stats.median(),
            stats.max()
        );
    }
}
```

### How Spans Count Toward a Range

A span that crosses a range's edge, such as a worker task that began in one
frame and finished in the next, can count in more than one way. The query
says which with a `Selection`:

| Selection | Counts | Use it for |
|---|---|---|
| `StartsWithin` (default) | spans that began in a range, whole | totals over consecutive frames that add up without double counting |
| `Overlaps` | spans that ran at all during a range, whole | finding everything that was running |
| `Inside` | only the time inside the range, with self time clipped too | how a frame's time was spent |

## What an Answer Covers

Every table carries a `Basis`. It separates answers that look alike:

- **Nothing happened**: no rows, and `basis.is_complete()`.
- **Not recorded**: `basis.recorded()` says the producer's level excluded
  spans, or instrumentation was compiled out.
- **Not finished**: `basis.open()` counts matching spans still running,
  whose durations are unknown.
- **Cut off**: `basis.unmatched_ends()` counts ends whose begins came before
  the trace.
- **Missing**: `basis.gaps()` lists where events were evicted before a read,
  and `basis.interrupted()` counts spans that were open across a gap, whose
  nested spans, and so self time, may be incomplete.
- **Unobserved**: `basis.unobserved()` is selected time after the latest
  snapshot or before what the trace forgot.

Its `Display` writes this as a short statement. Durations are elapsed
wall-clock time on the recording thread. Footfall does not measure CPU
time, so a span that waited for a lock counts its waiting.

## Looking at Examples

`SpanQuery::instances` returns the spans closest to the median and the
worst, each with its id. From any span, `Trace::parent`, `Trace::children`,
`Trace::frame_at`, `Trace::flows_through`, and the messages recorded inside
it (`Message::span`) lead to the work around it, and `Span::location` leads
to its code: the function, file, and line where its scope descriptor is
declared, when the descriptor records them.

## Comparing

A `Comparison` measures two `Population`s of units per unit: slow frames
against the rest, or one run against another. It measures self time by
default, so a change shows up in the scope that did the work rather than in
every scope around it; `measuring(Metric::Total)` measures whole spans
instead. Each `ComparisonRow` splits
the change in time per unit into *volume*, from running more or fewer
spans, and *cost*, from each span taking longer. Three times as many layout
calls, each as fast, calls for a different fix than the same calls each
three times slower.

```rust
use footfall::analysis::{Comparison, GroupBy, Population, Selection, Trace};

fn why_slow(trace: &Trace) {
    let (slow, normal): (Vec<_>, Vec<_>) = trace
        .frames()
        .iter()
        .partition(|frame| frame.duration().as_micros() > 16_667);
    let comparison = Comparison::between(&Population::frames(trace, normal), &Population::frames(trace, slow))
        .selecting(Selection::Inside)
        .group_by(GroupBy::Name)
        .run();
    for row in comparison.rows().iter().take(5) {
        println!(
            "{}: {:+.0} µs per frame ({:+.0} from more calls, {:+.0} from slower calls, ± {:.0})",
            row.describe(),
            row.change_micros(),
            row.volume_micros(),
            row.cost_micros(),
            row.standard_error_micros().unwrap_or(f64::NAN),
        );
    }
    for caveat in comparison.caveats() {
        println!("caution: {caveat:?}");
    }
}
```

Grouping a comparison by an argument as well as by name can change the
answer. When slow frames' layouts each took longer because each handled
far more nodes, `layout` alone shows the change as cost, but
`GroupBy::Arg("nodes".into())` shows it as volume: spans like
`layout / nodes=12000`, which normal frames never ran.

`standard_error_micros` estimates how much of a change is noise from how
much the per-unit time varies; it needs at least two units a side. A
comparison also lists `Caveat`s: too few units, runs from different
runtime instances (matched by names and labels rather than ids), different
producer versions or capture settings, or an incomplete side.

## Drawing a Timeline

Each `Track` holds one thread's spans in rows by depth. `Track::spans_in`
returns a row's spans for a visible range by binary search, and
`Track::zones` merges runs of spans too short to see at the current zoom.
`CounterSeries::samples_in` gives a plot's points, `Trace::messages_in` the
messages, `Trace::resources_alive_at` the live resources, and
`Trace::frames_in` the frame markers.
