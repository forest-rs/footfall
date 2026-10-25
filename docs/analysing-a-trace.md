# Analysing a Trace

`footfall::analysis` answers questions about what an instrumented program
did, from its snapshots: in process, from a live session, or from a capture
file. This guide covers the model, the questions it answers, and what each
answer says about its own completeness.

## Edits, resources, and counter intervals

Rendering frames are one work unit. `Trace::marks(MarkerKind::Transaction)` and
`Trace::marks(MarkerKind::Pass)` expose other marked ranges; `Population::frames`
accepts these too. Alternatively, select argument-bearing scope instances with
`trace.spans().named(...).with_arg(...)` and pass their ranges to
`Population::ranges`. The CLI exposes both through `--kind` and `--scope`;
see [transactions and scoped work](../tools/footfall_cli/README.md#transactions-and-scoped-work).

`Trace::counter_checkpoints` exposes named global sampling boundaries.
`Trace::counter_changes(before_sequence, after_sequence)` compares retained
endpoint values. Overlapping intervals can include the same activity and must
not be summed as exclusive edit allocations. Incomplete batches and gaps leave
deltas unknown. Sampling frequency and cost belong to the producer.

`Resource::creation` identifies observed creating work, while
`Resource::observations` retains each update and drop with its own scope,
context, thread, time, and event sequence. Follow the creation's span and its
parents to the edit's arguments. A resource can survive later edits without
changing that origin. Unknown creations and missing scope begins remain
unknown. Undo roots and graph cache ownership require application observations;
they cannot be deduced from the creating scope or lifetime alone.

## Connected work inspection

For a runnable investigation with sample queries and results, start with
[Following one edit through a capture](investigating-an-edit.md).

Load once, then use `Trace::select_messages`, `Trace::select_flows`,
`Trace::inspect_work`, and `Trace::resource_interval`. These typed operations
accept no paths, serialization settings, or transport state. A session host can
push later snapshots into the same trace and run the same queries again.

```rust
use footfall::analysis::{EventFilter, MessageQuery, Page, Trace, WorkId};

fn investigate(trace: &Trace) -> Result<(), Box<dyn std::error::Error>> {
    let edits = trace.spans().named("edit").with_arg("edit", 11_u64).iter();
    if edits.len() != 1 { return Err("select exactly one edit".into()); }
    let id = edits[0].span().id();
    let work = trace.inspect_work(WorkId::Scope(id))?;
    let query = MessageQuery {
        filter: EventFilter { within: Some(id), ..Default::default() },
        name: Some("body_outcome".into()),
        args: vec![("node".into(), 2_u64.into())],
        ..Default::default()
    };
    let first = trace.select_messages(&query, Page::default())?;
    for message in first.items {
        println!("{}: {}", message.sequence(), message.text());
    }
    println!("{} scopes, evidence {:?}", work.scopes.len(), work.evidence);
    Ok(())
}
```

Scope membership follows captured parent identities, including while scopes are
open. Simultaneous work on another thread is excluded. `OpenSpan` exposes its
parent and begin-event arguments. Open inspections describe retained observations
so far and set `open`; they cannot establish a completed result. A missing work
identity is an error. The CLI's selector form requires exactly one completed unit;
use `analyze span ID --json` for an open scope whose begin identity is known.

Marker inspection selects scopes overlapping `[begin,end)` and point observations
inside it across threads and labels it
`IntervalActivity`. It establishes no descendant, causal, or ownership relation.
Flow queries return all retained steps of each matching flow and label matching
step sequences, preserving endpoints outside the selected scope or time range.
Endpoint presence (`Flow::is_complete`, JSON `endpoints_present`) and recording
continuity are separate facts: intermediate steps can be lost even with both
endpoints present.

Message pages order by event sequence; flow pages order by flow ID. `Page::after`
is exclusive, and `QueryPage::next` supplies the cursor when another page exists.
`total` counts retained matches before pagination. Keep the query and runtime
instance fixed when continuing. These operations currently scan retained data;
pagination bounds the returned page, not scan cost or temporary matching storage.
Counts can change after live updates or pruning. `Trace::message`, `Trace::flow`,
`Trace::span`, and `Trace::resource_lifetime` support identity-based follow-up.

`QueryEvidence` separates latest signal availability (`recorded`) from
producer-attested historical enablement (`recording_history`), alongside selected
coverage, retained gaps, open work, refused/unattributed contexts, lost counter
updates, and whether history is complete. Completeness remains conservative
after *any* event loss or forgetting, including losses preceding the selection.
Protocol 1.1 snapshots attest the minimum runtime/process-gate level. Re-enabling
recording cannot certify earlier disabled history. Older snapshots and hand-built
snapshots without attestation have unknown coverage; retained events remain
inspectable, while completeness and exact storage totals are withheld.
Interrupted nesting can hide descendants; an empty incomplete answer cannot
establish that none ran. Ancestry references whose scopes were forgotten remain
identities, without substituting a later scope. Inspect file-read status separately:
`CapturedTrace::complete` and CLI `capture_read` describe truncated ends and foreign
snapshots; successful queries on a retained prefix cannot make that file complete.

## Resource interval accounting

`Trace::resource_interval(range, grouping)` reports create/drop/resize activity,
live baseline, ending bytes, peak live bytes, and ending survivor identities.
Sizes are application-reported tracked bytes, excluding allocator overhead, RSS,
and uninstrumented allocations. Creation provenance does not establish present
ownership by a graph, cache, or undo stack.

Activity uses additive half-open intervals `[start,end)`. Boundary states are
immediately **before** all events at the boundary timestamp. Equal timestamps
are ordered by event sequence within the interval, so an intermediate create or
resize peak survives a same-timestamp drop. An event at `end` belongs to the next
interval. Adjacent intervals' create/drop/grow/shrink sums add; their peaks do not.
A zero-length interval has no activity and identical boundary states. This differs
from `Resource::alive_at(t)`, which applies events at `t` to the lifetime model.

Release sizes come from the last supported size before drop. Increases and
decreases on updates are reported separately from creation and release; they do
not claim a particular allocator operation occurred. A lifetime first seen as an
update or drop is never extrapolated backward as live before that observation.
`ResourceLifetimeId` uses the first retained observation sequence and distinguishes
logical ID reuse after a drop. It stays stable across live pushes and pruning
while the lifetime is retained.

Each byte measure has `observed` and `exact`. Missing creation or size evidence,
gaps, forgotten history, coverage, disabled instrumentation, or sum overflow make
exact totals unavailable. Unsupported stale sizes are excluded from release and
boundary sums. Incomplete survivors are candidates from supported retained state,
not proof of uninterrupted retention. A gap before the interval can invalidate
its baseline even when no events are missing inside the interval. Pruning the gap
does not restore completeness. Overflow saturates the observed sum and clears
exactness.

Group by kind, nearest creation scope, or highest fully resolved creation ancestor.
Unknown and unscoped origins have an explicit group. Groups describe origins of
lifetimes; global interval activity can overlap other edits and cannot be assigned
exclusively to the selected work. Named counter checkpoints independently report
sampled boundaries; resource events cannot supply samples an older capture never
recorded. An absent initial gauge sample stays unknown.

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

A capture file reads back as the same trace, with what could not be added:

```rust
use footfall::analysis::Trace;
use footfall::session::capture::CaptureReader;

fn read(path: &str) -> std::io::Result<Trace> {
    let captured = Trace::from_capture(CaptureReader::new(std::fs::File::open(path)?)?)?;
    if !captured.complete() {
        eprintln!(
            "{path}: {} snapshots from another run skipped, cut short: {}",
            captured.skipped(),
            captured.truncated().is_some()
        );
    }
    Ok(captured.into_trace())
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
work, the frame points a frame scheduler or display reported, and the
frame opportunities the platform offered its stream into a `FramePacing`:
when it was scheduled to begin and whether it began late, its deadline,
its predicted, target, and expected display times, when it was shown, how
long its GPU was busy, and an `Outcome`.
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
| `Held { by, reason }` | Shown late by no more than the scheduler deliberately held it back; not a miss |
| `Synchronous` | Presented outside the scheduler's pacing, such as a synchronous resize; not judged against a target or deadline |
| `Unknown` | Too little reported to tell |

A late frame's `LateCause` is the first of these the trace shows, in order:

| Cause | The frame |
|---|---|
| `WaitedForBuffer` | was submitted late by no more than it waited for a rendering buffer; removing the wait would have met its deadline |
| `MissedDeadline` | had its CPU work submitted after its deadline |
| `SkippedOpportunity` | was to begin at a frame opportunity, and the platform skipped the one it was meant for, so it began at a later one |
| `BeganLate` | began more than `tolerance` after it could have: after its scheduled time, or after the opportunity it was to begin at |
| `Backlog` | was in time, but the frame before it was still being shown at this one's target |
| `Gpu` | was in time, but its GPU work finished after its target |
| `Mispredicted` | was shown a fraction of a refresh after its target, rather than whole refreshes, where the platform stated its interval; the target is inferred to have missed the refresh phase |
| `Display` | was submitted and rendered in time with nothing ahead of it: the compositor or display held it |
| `AfterSubmit` | was in time with nothing ahead of it, but has no GPU work to tell the GPU from the display |
| `Unknown` | has no deadline or CPU work to judge |

`Backlog` can explain later frames submitted in time while a preceding
frame was still waiting to be shown. A frame's own missed deadline or
late begin takes precedence. `LateCause::party` groups these explanations
as the program, the platform, the queue, or unknown; it does not turn an
inferred explanation into an observed cause.

`FramePacing::buffer_wait` is the time between its `BufferRequested` and
`BufferAcquired` points. A wait longer than the tolerance contributes to
`Pacing::buffer_waits`. When estimated buffer or submission times establish
`WaitedForBuffer`, its support is inferred.

If the producer cannot separate buffer acquisition from application work,
leave those points absent. `buffer_wait` is then `None` (`null` in JSON),
not zero, and the frame's measured work retains the combined elapsed time.
A late submission can still be observed as `MissedDeadline`, but it does
not prove that computation rather than an unobserved wait caused the miss.
Its `Party::Program` is the leading explanation under the available
evidence, not a measurement of CPU execution time. Use `footfall check`'s
`buffers` check to see whether a capture can make that distinction.

`FramePacing::plan` returns the scheduler's recorded interval, pipeline
depth, learned latency allowance, deliberate hold, and paced or synchronous
kind. A hold that explains the lateness produces `Outcome::Held`; lateness
beyond the hold retains its normal cause and records the hold in
`Why::held_back`. Synchronous frames are excluded from cadence, severity,
presentation intervals, stutters, and latency steps, and restart the rhythm.

When a frame began is judged from its `Scheduled` point and its begin
marker. A frame scheduled to begin at a time (`BeginsAt::Time`) could have
begun then; one scheduled to begin at an opportunity
(`BeginsAt::Opportunity`) could have begun at the first of its stream's
frame opportunities from then on. `FramePacing::began_late` says how much
later than that it began, when more than the tolerance: a timer that woke
late, or a program busy when its opportunity came.
`FramePacing::opportunity_late` says, for a frame that waited for an
opportunity, how much later than due that opportunity came, when it came
at least half an interval after the opportunity before it said the next
would: the platform skipped the one the frame was meant for. A frame that
began at the opportunity it got did not begin late, even when that
opportunity came late. `FramePacing::begin_judged` says whether the trace
had what judging the begin needs; without opportunities for its stream, a
frame that waits for them is not judged. Whether its opportunity came
needs no begin marker, so a frame dropped before it began, such as one
that expired waiting, still says when the platform skipped the
opportunity it waited for.

`Pacing::opportunities` counts the opportunities recorded and how many the
platform skipped between them, from the interval each gave or its stream's
median spacing, leaving out waits across lost events.
`Pacing::opportunity_intervals` is the time from each to the next, steady
at the refresh interval while the platform offers every refresh, and
`Pacing::begin_lateness` how late the frames that began late began,
whether or not they were shown late. A stutter with skipped opportunities
under it is the platform's; one under frames that began late is the
program's.

The cause is the leading explanation, not proof. `FramePacing::why` says
what the trace shows: every condition that held (submitted after the
deadline, the opportunity it was meant for skipped, a late begin, GPU work
finishing after the target, the frame before still waiting), whether the
submission time was reported or read from the end marker, whether when it
began could be judged, whether GPU work was recorded, how the submission and presentation
times were known, and how strongly the trace supports the cause:

| `Support` | Meaning |
|---|---|
| `Observed` | The trace recorded the condition, from times that were not estimates: a late submission, a skipped opportunity, a late begin, or recorded GPU work finishing after the target |
| `Inferred` | It rests on a queue read from the previous frame, a mispredicted target, or estimated buffer, submission, or presentation times |
| `Unresolved` | The program's part looked in time, and what came after was not observed |

Deadlines and targets are the scheduler's plans, taken as given. And an
observed condition is not proof that it made the frame late, only the
leading explanation. A time recorded without a basis
(`TimingBasis::Unknown`) is taken at its word, so a cause it shows can be
`Observed`; `Why::unstated_basis` says when one does.

`Outcome`, `LateCause`, `Support`, and `Why` display as a few words, a
`Why` with every condition that held, as in `the platform skipped its
opportunity, observed; its opportunity came 8.33 ms late`.

`OutcomeCounts::late_support` counts late frames by support, and how many
of the observed rest on unstated times (`observed_unstated`), so an answer
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
`interval_breaks` rather than guessed. Frames still waiting for their
reports when the trace ends are not a break, since no presentation after
them is left out. `Pacing` also gives stutters (intervals at least half
again the median of their own stream's, a heuristic for a frame held an
extra refresh), how late the late frames were,
and latency from each frame's begin marker to its presentation. A frame's
`work` is the time between its markers, waits included, and its latency
starts at its begin marker, not at the input it answers.

`FramePacing::cadence` judges smoothness separately from lateness. `Steady`
means the spacing matched the targets; `Stalled` or `Hurried` says by how
much it differed. A frame can be late but steady, so its lateness describes
latency rather than another hitch. Without both targets, cadence uses the
sum of planned intervals since the last shown frame, including known
drops. Missing intervals leave cadence unjudged. A spacing difference
explained by a deliberate hold is `Cadence::Held`, not a hitch.
`Pacing::cadence` counts these, including `late_but_steady` and `hitches`.

`FramePacing::severity` grades late or early presentations and drops that
are misses. It is `Partial` for less than one refresh and `Full` for a
refresh or more, within the tolerance, or a drop for a miss. A late frame
with steady cadence, an on-time frame, and a held or synchronous frame
have no severity. When cadence broke, its spacing error is judged;
otherwise its presentation error is. An unknown refresh interval leaves
the severity unknown. `Pacing::severity` counts the partial and full misses.

`Pacing::latency_steps` finds changes of at least half a refresh that
persist for `LATENCY_STEP_FRAMES` measured frames (five). Missing frame
numbers, latency measurements, lost events, or synchronous frames restart
the run and its baseline. A step also says whether the scheduler's pipeline
depth or latency allowance changed across it. This exposes a lasting
latency increase even when the scheduler moved its targets to compensate.

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
