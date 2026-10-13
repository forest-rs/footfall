# footfall_cli

The `footfall` command: watch a running Footfall session live, record it to a
capture file, and ask the capture questions later.

```sh
cargo install footfall_cli

footfall top                                 # watch the running session
footfall record demo.ffcap                   # save it until Ctrl-C
footfall analyze summary demo.ffcap          # frames, scopes, counters, losses
footfall analyze slow demo.ffcap             # what the slow frames did differently
footfall analyze pacing demo.ffcap           # which frames were shown late, and why
footfall analyze pacing demo.ffcap --json    # the same, every frame, for other programs
footfall analyze compare before.ffcap after.ffcap
footfall analyze span demo.ffcap 13909       # one span and the work around it
footfall export demo.ffcap                   # one Chrome trace, for Perfetto
footfall top --file demo.ffcap               # replay a capture in the live view
```

Programs serve their sessions with
[`footfall_session_wtransport`](https://docs.rs/footfall_session_wtransport),
and `footfall` finds them through a per-user state directory without
configuration. Set `FOOTFALL_STATE_DIR` to the same directory for both to
keep experiments apart from anything else running. Add `--help` to any
command to see its options.

## Pacing as JSON

`footfall analyze pacing --json` prints the judgment as one JSON document,
for scripts and other tools to check rather than read:

```sh
# How many frames were submitted after their commit deadline?
footfall analyze pacing run.ffcap --json |
  jq '[.frames[] | .points | select(.submitted and .deadline)
       | select(.submitted.at_ns > .deadline.at_ns)] | length'
```

The document holds:

- `footfall_version`, and `capture`: the file, process, process id,
  runtime instance (hex), and the producer's Footfall version.
- `tolerance_ns`, `counts` (with `late_causes` and `late_support`),
  `presentation_basis`, `intervals` (with `breaks` and `stutters`),
  `lateness`, `latency`, `prediction_error` for the `predicted`, `target`,
  and `expected` times, `wasted`, and `unplaced_points`: the figures the
  text report prints.
- `frames`, every numbered frame in order: its `label` as the text report
  writes it, `stream`, `generation`, and `number`; `frame_id`, the
  sequence of its begin marker, when it has one; `outcome` (`on_time`,
  `early`, `late`, `dropped`, `submitted_late`, or `unknown`) with `by_ns`
  and whether it was `missed` (a dropped frame whose reason is not a miss,
  such as `superseded`, is not); `why`, for a late frame; its CPU `work`
  and `gpu` ranges, `gpu_busy_ns`, and `latency_ns`; and `points`, each
  frame point recorded for it by kind (`predicted`, `deadline`, `target`,
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
