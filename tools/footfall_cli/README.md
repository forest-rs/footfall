# footfall_cli

The `footfall` command: watch a running Footfall session live, record it to a
capture file, and ask the capture questions later.

```sh
cargo install footfall_cli

footfall top                                 # watch the running session
footfall record demo.ffcap                   # save it until Ctrl-C
footfall check demo.ffcap                    # what it gives the analyses, and lacks
footfall analyze summary demo.ffcap          # frames, scopes, counters, losses
footfall analyze slow demo.ffcap             # what the slow frames did differently
footfall analyze pacing demo.ffcap           # which frames were shown late, and why
footfall analyze pacing demo.ffcap --json    # the same, every frame, for other programs
footfall analyze compare before.ffcap after.ffcap
footfall analyze span demo.ffcap 13909       # one span and the work around it
footfall export demo.ffcap                   # one Perfetto trace; --format json for Chrome
footfall top --file demo.ffcap               # replay a capture in the live view
```

Programs serve their sessions with
[`footfall_session_wtransport`](https://docs.rs/footfall_session_wtransport),
and `footfall` finds them through a per-user state directory without
configuration. Set `FOOTFALL_STATE_DIR` to the same directory for both to
keep experiments apart from anything else running. Add `--help` to any
command to see its options.

## Checking an integration

`footfall check` says what a capture gives the analyses. That makes it the
first thing to run after instrumenting a program or wiring up a frame
scheduler. Each line is one thing an analysis uses:

| Check | What it looks for |
|---|---|
| `frames` | frame markers |
| `numbered` | frames marked with their number |
| `scheduled` | scheduled points: when each frame was to begin |
| `opportunities` | frame opportunities around the frames that wait for one |
| `deadlines` | deadline points on numbered frames |
| `submissions` | submission points |
| `buffers` | buffer requested and acquired points: how long frames waited for a buffer |
| `presentations` | presentation or drop points |
| `bases` | submission and presentation times that state their basis |
| `clocks` | times and spans on clocks that were never synced |
| `gpu` | GPU work for the frames that were shown |
| `counter_samples` | counter samples at frame ends |
| `losses` | anything lost or cut short |
| `visibility` | whether the output looked visible at all |

Each check reports a status:

- **present**: the capture has it everywhere it applies.
- **partial**: some of the frames or times have it, and some don't.
- **missing**: none do.
- **suspect**: it's there but looks like an artifact of how the run was
  recorded. For example, a long run of frames dropped for no stated reason
  with none shown is what a sleeping display or a hidden window looks like.
- **not recorded**: the producer's capture level or settings left it out,
  so the capture can't say.
- **n/a**: it needs something else that is missing.

Anything short of present also says what depends on it, the call that
records it, and the section of the
[instrumenting guide](https://github.com/forest-rs/footfall/blob/main/docs/instrumenting-an-app.md)
that explains it.

Some frames are left out of the counts and named, rather than counted
against the capture:

- **Still in flight:** a frame whose point the capture ended too soon to
  receive. At most 16 such frames at the end of each stream are left out;
  more than that means the producer stopped reporting the point.
- **Dropped before submitting:** a frame that was superseded, discarded,
  missed its deadline, or had no surface can't have a submission time.

`--require CHECK`, repeated, fails unless each named check is present. It
lets an integration test or CI job keep a producer honest:

```sh
footfall check run.ffcap --require presentations --require bases --require gpu
```

`--json` prints the same as one document: `footfall_version`, `capture`,
and `items`, the checks in the order above. Each item has:
- `id` and `status` (`present`, `partial`, `missing`, `suspect`,
  `not_recorded`, or `not_applicable`), and `reason` for the last two;
- `found`, and `have` and `of` where it's counted;
- `needed_for`, `fix`, and `guide`.

## Slow frames

`analyze summary` and `analyze slow` count a frame as slow when its work was
submitted after its own deadline, when the trace records deadlines (as frame
schedulers such as `frameclock` do). The deadline follows whatever refresh
rate the scheduler planned for, including a variable one, so there is no
budget to choose. Frames with no deadline are left out of that count.

When the trace has no deadlines, a frame is slow when it takes longer than a
fixed budget, 16.7 ms (one refresh at 60 Hz), and the report says the
default was used. `--budget-ms` sets the budget and uses it even when the
trace has deadlines. `footfall top` always uses a budget.

## Pacing as JSON

`footfall analyze pacing --json` prints the judgment as one JSON document,
for scripts and other tools to check rather than read:

```sh
# How many frames were submitted after their commit deadline?
footfall analyze pacing run.ffcap --json |
  jq '[.frames[] | select(.met_deadline == false)] | length'
```

The document holds:

- `footfall_version`, and `capture`: the file, process, process id,
  runtime instance (hex), and the producer's Footfall version.
- `tolerance_ns`, `counts` (with `late_causes`, `late_support`, and
  `severity`: how many misses and hitches were `partial` and `full`),
  `opportunities` (how many were `recorded`, `skipped` by the platform,
  and `unplaced`, and their `intervals`), `begin_lateness`, `buffer_waits`
  (how long frames waited for a buffer, when longer than the tolerance),
  `deadlines` (how many frames were submitted by their deadlines, `met`,
  and after, `missed`), `presentation_basis`, `intervals` (with `breaks`
  and `stutters`, and the same, with `cadence`, `opportunities`, and
  `skipped_opportunities`, for each frame stream in `streams`, since
  windows can run at different rates), `cadence` (how many frames were
  `judged`, `steady`, `late_but_steady`, `stalled`, and `hurried`, and the
  `hitches`), `lateness`, `latency`, `latency_steps` (each lasting change
  in latency: the `stream`, `generation`, and `number` of its first frame,
  `from_ns`, `to_ns`, and whether it `rose`),
  `prediction_error` for the `predicted`, `target`, and `expected` times,
  `wasted`, and `unplaced_points`: the figures the text report prints.
- `frames`, every numbered frame in order: its `label` as the text report
  writes it, `stream`, `generation`, and `number`; `frame_id`, the
  sequence of its begin marker, when it has one; `outcome` (`on_time`,
  `early`, `late`, `dropped`, `submitted_late`, or `unknown`) with `by_ns`
  and whether it was `missed` (a dropped frame whose reason is not a miss,
  such as `superseded`, is not); `why`, for a late frame, with its
  `cause`, the `party` it points at, its `support`, whether it was shown
  `whole_refreshes` late, and its `buffer_wait_ns`; its `cadence` (`kind`,
  `steady`, `stalled`, or `hurried`, and `by_ns`) against the frame shown
  before it, when known; its `severity` (`partial` or `full`, or `null`
  for a frame with no miss or hitch, such as one on time or late only as
  steadily as the frame before, and for one whose refresh interval is not
  known), and its `buffer_wait_ns`; whether its work `met_deadline`, by
  its `submitted` point or else the end of its `work`, and
  `submitted_late_ns`, how long after; what it `begins_at`
  (`time` or `opportunity`), whether its begin was judged
  (`begin_judged`), and `began_late_ns` and `opportunity_late_ns`; its CPU `work`
  and `gpu` ranges, `gpu_busy_ns`, and `latency_ns`; `unplaced_points`,
  how many of its points had a time that could not be placed; and
  `points`, each
  frame point recorded for it by kind (`scheduled`, `predicted`,
  `buffer_requested`, `buffer_acquired`, `deadline`, `target`,
  `sampled`, `submitted`, `expected`, `presented`, and `dropped` with its
  `reason`), with its `at_ns`, `basis` (`observed`, `estimated`, or
  `unknown`), and `args`.

Times (`at_ns`, `start_ns`, `end_ns`) are nanoseconds since the producer's
runtime started, and every other `_ns` field is a duration in nanoseconds.
Names are snake case. A distribution is an object with its `count` and,
when it has any, `mean_ns`, `median_ns`, `p90_ns`, `p99_ns`, `min_ns`, and
`max_ns`. Fields missing for a frame are `null`; a point it never had is
left out of `points`.

## Where it comes from

Everything the command shows comes from Footfall's public API: the session
client, capture files, and [`footfall_analysis`](https://docs.rs/footfall_analysis).

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
