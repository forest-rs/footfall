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
`Trace::span`. Frames have `FrameId`s the same way. Ids mean nothing in
another instance: `Trace::push` refuses a snapshot from one, and a tool
following a restarting producer starts a new trace.

A trace following a live session grows until the tool calls
`forget_before`, which drops what ended earlier; answers then count the
forgotten time as unobserved.

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
