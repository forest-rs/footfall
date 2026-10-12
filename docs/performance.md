# Performance

Footfall's overhead is measured, not asserted. The Criterion benchmarks in
`benchmarks/substrate` time the emission seam and the bundled backends.

## Running

```sh
cargo bench -p footfall_benchmarks                    # every case
cargo bench -p footfall_benchmarks -- scope           # only cases whose name contains "scope"
cargo bench -p footfall_benchmarks --bench runtime    # one backend group
```

To compare two branches, save a baseline on one and compare against it on
the other:

```sh
git switch main
cargo bench -p footfall_benchmarks -- --save-baseline main
git switch my-branch
cargo bench -p footfall_benchmarks -- --baseline main
```

Criterion then reports the change for each case and whether it is
statistically significant or within noise. Results and saved baselines live
in `target/criterion`.

## Method

- `footfall_core` installs one backend per process and never uninstalls it,
  so each backend group is its own bench target, and Cargo runs every bench
  target in its own process:
  - `none`: no backend installed
  - `runtime`: `footfall_runtime` installed directly, plus the
    `footfall_tracing` layer as the global `tracing` subscriber
  - `fanout`: `footfall_fanout` over the runtime and the core no-op backend
  - `capture`: `footfall_runtime` holding only a seeded capture
- Each case gets 1 s of warmup and 3 s of measurement. Criterion reports a
  point estimate of the time per iteration with a 95% confidence interval.
- The runtime is never reset. Cases that buffer events fill the event buffer
  during warm-up, so every timed emission also evicts the oldest event: the
  steady state of any long-running capture.
- The `counter_add threads/N` cases spawn their threads first and release
  them together through a barrier. The reported time is wall time per
  operation across all threads, so a value above the single-thread cost
  means aggregate throughput went down.

## Cases

| Case | Measures |
| --- | --- |
| `none/counter_add`, `none/scope begin+end`, `none/scope guard`, `none/message` | The emission seam with no backend installed, which stops at the process-wide maximum level check. |
| `none/context build/N labels` | Building a `Context` with 1, 4, or 8 labels. |
| `none/context register+release`, `runtime/counters/context register+release` | Registering and releasing one context, with no backend and with the runtime, which records it in its context table. |
| `runtime/off/...` | The runtime installed at `InstrumentationLevel::Off`. Its level drives the core maximum level, so these stop at the same check. |
| `runtime/counters/counter_add`, `gauge_set`, `histogram_record` | One aggregate update under a registered context's handle. |
| `runtime/counters/counter_add 1024 rotating contexts` | Counter updates cycling through 1024 registered contexts, so aggregate lookups hit a larger table. |
| `runtime/counters/counter_add with register+release per call` | The anti-pattern: building, registering, and releasing a context around every update. |
| `runtime/counters/counter_add threads/N` | 1, 2, 4, or 8 threads updating one counter, each under its own registered context. |
| `runtime/scoped/scope begin+end`, `scope guard` | One buffered scope pair, through manual tokens or the drop guard. |
| `runtime/scoped/message`, `message_with 2 args` | One buffered message event, including the bounded text copy. |
| `runtime/scoped/tracing span enter+exit`, `tracing event` | The same work arriving through `tracing` and the `footfall_tracing` layer. |
| `runtime/trace/resource_update` | One buffered resource observation. |
| `runtime/capture/snapshot 128 frames`, `chrome trace export 128 frames` | Snapshotting a seeded 128-frame capture, and exporting it as Chrome trace JSON. |
| `fanout/...` | Counter and scope cost when the runtime sits behind the fanout adapter. |

## Results

These numbers are indicative and machine-specific. Compare runs on one
machine, ideally through saved baselines; don't compare them across
machines.

- Machine: Apple M3 Max, 16 cores, 64 GB
- OS: macOS 27.0.1
- Toolchain: rustc 1.98.1
- Branch: `analysis`, the tip of the stack that moved the runtime to cursor
  reads

| Case | Estimate | 95% interval |
| --- | ---: | ---: |
| none/counter_add | 0.42 ns | 0.42 ns – 0.42 ns |
| none/scope begin+end | 0.54 ns | 0.53 ns – 0.55 ns |
| none/scope guard | 0.62 ns | 0.62 ns – 0.62 ns |
| none/message | 0.42 ns | 0.41 ns – 0.42 ns |
| none/context register+release | 2.66 ns | 2.66 ns – 2.67 ns |
| none/context build/1 labels | 11.20 ns | 11.18 ns – 11.21 ns |
| none/context build/4 labels | 18.05 ns | 18.04 ns – 18.06 ns |
| none/context build/8 labels | 49.72 ns | 49.65 ns – 49.78 ns |
| runtime/off/counter_add | 0.44 ns | 0.44 ns – 0.44 ns |
| runtime/off/scope begin+end | 0.57 ns | 0.57 ns – 0.57 ns |
| runtime/counters/counter_add | 44.76 ns | 44.60 ns – 44.87 ns |
| runtime/counters/gauge_set | 44.83 ns | 44.81 ns – 44.85 ns |
| runtime/counters/histogram_record | 41.53 ns | 41.51 ns – 41.57 ns |
| runtime/counters/counter_add 1024 rotating contexts | 47.02 ns | 46.99 ns – 47.04 ns |
| runtime/counters/context register+release | 103.07 ns | 102.70 ns – 103.45 ns |
| runtime/counters/counter_add with register+release per call | 299.35 ns | 295.18 ns – 303.60 ns |
| runtime/counters/counter_add threads/1 | 42.93 ns | 42.34 ns – 43.44 ns |
| runtime/counters/counter_add threads/2 | 22.58 ns | 22.41 ns – 22.72 ns |
| runtime/counters/counter_add threads/4 | 11.62 ns | 11.60 ns – 11.64 ns |
| runtime/counters/counter_add threads/8 | 10.70 ns | 10.25 ns – 11.11 ns |
| runtime/scoped/scope begin+end | 77.79 ns | 77.70 ns – 77.90 ns |
| runtime/scoped/scope guard | 77.68 ns | 77.59 ns – 77.81 ns |
| runtime/scoped/message | 43.20 ns | 43.10 ns – 43.31 ns |
| runtime/scoped/message_with 2 args | 41.65 ns | 41.62 ns – 41.70 ns |
| runtime/scoped/tracing span enter+exit | 210.01 ns | 209.71 ns – 210.36 ns |
| runtime/scoped/tracing event | 113.89 ns | 113.77 ns – 114.04 ns |
| runtime/trace/resource_update | 37.27 ns | 37.22 ns – 37.34 ns |
| runtime/capture/snapshot 128 frames | 47.2 µs | 47.1 µs – 47.2 µs |
| runtime/capture/chrome trace export 128 frames | 413.1 µs | 412.2 µs – 414.0 µs |
| fanout/counters/counter_add | 45.12 ns | 45.09 ns – 45.16 ns |
| fanout/scoped/scope begin+end | 94.33 ns | 94.25 ns – 94.44 ns |

### Effect of the Cursor Runtime

Criterion point estimates before and after the stack that replaced drained
capture sessions with cursor reads over a buffer that keeps the newest
events, compared against a baseline saved on `main` on the machine above.
The two runs measure different states for buffered events: `main` reset the
runtime between chunks, so its emissions never reached a full buffer, while
the stack never resets, so every timed emission also evicts the oldest
event. The capture cases now run on a runtime of their own and the seeded
capture includes a counter sample per frame.

| Case | Before | After |
| --- | ---: | ---: |
| none/counter_add | 0.42 ns | 0.42 ns |
| none/scope begin+end | 0.50 ns | 0.54 ns |
| none/scope guard | 0.65 ns | 0.62 ns |
| none/message | 0.41 ns | 0.42 ns |
| none/context register+release | 2.66 ns | 2.66 ns |
| none/context build/1 labels | 11.23 ns | 11.20 ns |
| none/context build/4 labels | 18.43 ns | 18.05 ns |
| none/context build/8 labels | 49.93 ns | 49.72 ns |
| runtime/off/counter_add | 0.42 ns | 0.44 ns |
| runtime/off/scope begin+end | 0.50 ns | 0.57 ns |
| runtime/counters/counter_add | 44.32 ns | 44.76 ns |
| runtime/counters/gauge_set | 44.22 ns | 44.83 ns |
| runtime/counters/histogram_record | 41.38 ns | 41.53 ns |
| runtime/counters/counter_add 1024 rotating contexts | 47.18 ns | 47.02 ns |
| runtime/counters/context register+release | 99.83 ns | 103.07 ns |
| runtime/counters/counter_add with register+release per call | 163.85 ns | 299.35 ns |
| runtime/counters/counter_add threads/1 | 44.63 ns | 42.93 ns |
| runtime/counters/counter_add threads/2 | 22.83 ns | 22.58 ns |
| runtime/counters/counter_add threads/4 | 12.38 ns | 11.62 ns |
| runtime/counters/counter_add threads/8 | 11.74 ns | 10.70 ns |
| runtime/scoped/scope begin+end | 69.65 ns | 77.79 ns |
| runtime/scoped/scope guard | 69.65 ns | 77.68 ns |
| runtime/scoped/message | 36.97 ns | 43.20 ns |
| runtime/scoped/message_with 2 args | 36.59 ns | 41.65 ns |
| runtime/scoped/tracing span enter+exit | 196.62 ns | 210.01 ns |
| runtime/scoped/tracing event | 104.17 ns | 113.89 ns |
| runtime/trace/resource_update | 32.29 ns | 37.27 ns |
| runtime/capture/snapshot 128 frames | 29.0 µs | 47.2 µs |
| runtime/capture/chrome trace export 128 frames | 275.8 µs | 413.1 µs |
| fanout/counters/counter_add | 44.82 ns | 45.12 ns |
| fanout/scoped/scope begin+end | 90.38 ns | 94.33 ns |

### Effect of the Hot-Path Changes

Criterion point estimates before and after `hot-path-overhead` (contexts
and args passed by reference, and the process-wide maximum level checked
before dispatch), compared against a baseline saved on
`core-backend-redesign` on the machine above:

| Case | Before | After |
| --- | ---: | ---: |
| none/counter_add | 4.81 ns | 0.43 ns |
| none/scope begin+end | 8.49 ns | 0.57 ns |
| none/scope guard | 9.06 ns | 0.63 ns |
| none/message | 8.63 ns | 0.42 ns |
| runtime/off/counter_add | 5.07 ns | 0.45 ns |
| runtime/off/scope begin+end | 8.49 ns | 0.57 ns |
| runtime/counters/counter_add | 186.94 ns | 183.67 ns |
| runtime/counters/gauge_set | 186.96 ns | 182.57 ns |
| runtime/counters/histogram_record | 180.32 ns | 180.14 ns |
| runtime/counters/counter_add 1024 rotating contexts | 213.37 ns | 210.56 ns |
| runtime/counters/counter_add context built per call | 202.40 ns | 204.00 ns |
| runtime/counters/counter_add threads/1 | 182.67 ns | 175.47 ns |
| runtime/counters/counter_add threads/2 | 287.09 ns | 269.25 ns |
| runtime/counters/counter_add threads/4 | 586.73 ns | 547.13 ns |
| runtime/counters/counter_add threads/8 | 412.47 ns | 404.14 ns |
| runtime/scoped/scope begin+end | 209.97 ns | 208.91 ns |
| runtime/scoped/scope guard | 208.25 ns | 208.21 ns |
| runtime/scoped/message | 183.20 ns | 175.04 ns |
| runtime/scoped/message_with 2 args | 176.82 ns | 172.67 ns |
| runtime/scoped/tracing span enter+exit | 263.98 ns | 263.40 ns |
| runtime/scoped/tracing event | 212.46 ns | 210.16 ns |
| runtime/trace/resource_update | 166.77 ns | 170.83 ns |
| runtime/capture/snapshot 128 frames | 59.2 µs | 60.8 µs |
| runtime/capture/chrome trace export 128 frames | 276.2 µs | 271.0 µs |
| fanout/counters/counter_add | 190.94 ns | 186.41 ns |
| fanout/scoped/scope begin+end | 242.76 ns | 231.62 ns |

### Effect of Context Handles

Criterion point estimates before and after `context-handles` (emissions
carry a registered `ContextHandle` instead of a `Context`, and the runtime
writes per-thread shards), compared against a baseline saved on
`hot-path-overhead` on the machine above. The case that rebuilt a context
per call was replaced by the register+release cases, which appear only
in the results above.

| Case | Before | After |
| --- | ---: | ---: |
| none/counter_add | 0.43 ns | 0.44 ns |
| none/scope begin+end | 0.57 ns | 0.57 ns |
| none/scope guard | 0.63 ns | 0.62 ns |
| none/message | 0.42 ns | 0.42 ns |
| runtime/off/counter_add | 0.45 ns | 0.45 ns |
| runtime/off/scope begin+end | 0.57 ns | 0.57 ns |
| runtime/counters/counter_add | 183.67 ns | 44.81 ns |
| runtime/counters/gauge_set | 182.57 ns | 44.44 ns |
| runtime/counters/histogram_record | 180.14 ns | 41.76 ns |
| runtime/counters/counter_add 1024 rotating contexts | 210.56 ns | 47.55 ns |
| runtime/counters/counter_add threads/1 | 175.47 ns | 44.53 ns |
| runtime/counters/counter_add threads/2 | 269.25 ns | 26.16 ns |
| runtime/counters/counter_add threads/4 | 547.13 ns | 15.11 ns |
| runtime/counters/counter_add threads/8 | 404.14 ns | 12.64 ns |
| runtime/scoped/scope begin+end | 208.91 ns | 69.47 ns |
| runtime/scoped/scope guard | 208.21 ns | 70.34 ns |
| runtime/scoped/message | 175.04 ns | 37.10 ns |
| runtime/scoped/message_with 2 args | 172.67 ns | 36.98 ns |
| runtime/scoped/tracing span enter+exit | 263.40 ns | 197.91 ns |
| runtime/scoped/tracing event | 210.16 ns | 104.35 ns |
| runtime/trace/resource_update | 170.83 ns | 32.41 ns |
| runtime/capture/snapshot 128 frames | 60.8 µs | 32.2 µs |
| runtime/capture/chrome trace export 128 frames | 271.0 µs | 279.7 µs |
| fanout/counters/counter_add | 186.41 ns | 45.41 ns |
| fanout/scoped/scope begin+end | 231.62 ns | 91.02 ns |

## Reading the Results

- With no backend, or with the runtime at `Off`, a call costs under 1 ns:
  one relaxed atomic load and a branch inlined at the call site. Building
  with `RUSTFLAGS="--cfg footfall_compile_out"` removes even that.
- Every captured observation costs roughly 35 to 80 ns: a timestamp, an
  uncontended per-thread shard lock, and an aggregate lookup or event append.
  With a full event buffer, which a long capture always reaches, an event
  also evicts the recording thread's oldest event, 4 to 6 ns more than an
  append into free space. Before context handles an observation cost 170 to
  210 ns, most of it hashing and interning the `Context` under one
  runtime-wide lock.
- Counter throughput now grows with threads: per-op wall time at 2, 4, and 8
  threads is below the single-thread cost, where before it was one and a
  half to three times above it. Threads land in separate shards and share no
  lock on the counter path.
- Registering a context costs about 100 ns with the runtime installed.
  Registering per call, as in the anti-pattern case, costs about 300 ns per
  update, because each update then also admits a new counter key that the
  runtime later retires. Register once per owner.
- The event path still shares two atomics across threads, the event-capacity
  reservation and the recording sequence. A snapshot reads only its window,
  found by binary search in each thread's buffer, so reading recent events
  from a large buffer stays cheap; Chrome export remains the largest cost.
