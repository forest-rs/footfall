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
- Each case gets 1 s of warmup and 3 s of measurement. Criterion reports a
  point estimate of the time per iteration with a 95% confidence interval.
- Cases that buffer events reset the runtime, outside the timed region,
  every 65536 iterations, so no sample spills into the drop path however
  many iterations Criterion chooses.
- The `counter_add threads/N` cases spawn their threads first and release
  them together through a barrier. The reported time is wall time per
  operation across all threads, so a value above the single-thread cost
  means aggregate throughput went down.

## Cases

| Case | Measures |
| --- | --- |
| `none/counter_add`, `none/scope begin+end`, `none/scope guard`, `none/message` | The emission seam with no backend installed, which stops at the process-wide maximum level check. |
| `none/context build/N labels` | Building a `Context` with 1, 4, or 8 labels. |
| `runtime/off/...` | The runtime installed at `InstrumentationLevel::Off`. Its level drives the core maximum level, so these stop at the same check. |
| `runtime/counters/counter_add`, `gauge_set`, `histogram_record` | One aggregate update under a reused context. |
| `runtime/counters/counter_add 1024 rotating contexts` | Counter updates cycling through 1024 distinct contexts, so context interning and aggregate lookups hit a larger table. |
| `runtime/counters/counter_add context built per call` | The same context rebuilt at the call site before each update. |
| `runtime/counters/counter_add threads/N` | 1, 2, 4, or 8 threads updating one counter, each under its own context. |
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
- Branch: `hot-path-overhead`

| Case | Estimate | 95% interval |
| --- | ---: | ---: |
| none/counter_add | 0.43 ns | 0.43 ns – 0.43 ns |
| none/scope begin+end | 0.57 ns | 0.57 ns – 0.57 ns |
| none/scope guard | 0.63 ns | 0.62 ns – 0.63 ns |
| none/message | 0.42 ns | 0.42 ns – 0.42 ns |
| none/context build/1 labels | 11.21 ns | 11.20 ns – 11.23 ns |
| none/context build/4 labels | 18.33 ns | 18.31 ns – 18.34 ns |
| none/context build/8 labels | 50.00 ns | 49.92 ns – 50.08 ns |
| runtime/off/counter_add | 0.45 ns | 0.45 ns – 0.45 ns |
| runtime/off/scope begin+end | 0.57 ns | 0.57 ns – 0.57 ns |
| runtime/counters/counter_add | 183.67 ns | 183.51 ns – 183.85 ns |
| runtime/counters/gauge_set | 182.57 ns | 182.34 ns – 182.83 ns |
| runtime/counters/histogram_record | 180.14 ns | 179.87 ns – 180.42 ns |
| runtime/counters/counter_add 1024 rotating contexts | 210.56 ns | 210.28 ns – 210.87 ns |
| runtime/counters/counter_add context built per call | 204.00 ns | 203.85 ns – 204.16 ns |
| runtime/counters/counter_add threads/1 | 175.47 ns | 175.22 ns – 175.72 ns |
| runtime/counters/counter_add threads/2 | 269.25 ns | 267.41 ns – 271.16 ns |
| runtime/counters/counter_add threads/4 | 547.13 ns | 537.36 ns – 557.24 ns |
| runtime/counters/counter_add threads/8 | 404.14 ns | 400.73 ns – 407.10 ns |
| runtime/scoped/scope begin+end | 208.91 ns | 208.52 ns – 209.41 ns |
| runtime/scoped/scope guard | 208.21 ns | 207.99 ns – 208.44 ns |
| runtime/scoped/message | 175.04 ns | 174.89 ns – 175.21 ns |
| runtime/scoped/message_with 2 args | 172.67 ns | 172.46 ns – 172.89 ns |
| runtime/scoped/tracing span enter+exit | 263.40 ns | 263.13 ns – 263.70 ns |
| runtime/scoped/tracing event | 210.16 ns | 209.97 ns – 210.35 ns |
| runtime/trace/resource_update | 170.83 ns | 170.02 ns – 171.57 ns |
| runtime/capture/snapshot 128 frames | 60.8 µs | 60.5 µs – 61.0 µs |
| runtime/capture/chrome trace export 128 frames | 271.0 µs | 270.8 µs – 271.2 µs |
| fanout/counters/counter_add | 186.41 ns | 186.01 ns – 186.96 ns |
| fanout/scoped/scope begin+end | 231.62 ns | 231.35 ns – 231.94 ns |

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

## Reading the Results

- With no backend, or with the runtime at `Off`, a call costs under 1 ns:
  one relaxed atomic load and a branch inlined at the call site. Building
  with `RUSTFLAGS="--cfg footfall_compile_out"` removes even that.
- Every captured observation costs roughly 170 to 210 ns, whether it is a
  counter, histogram, scope pair, message, or resource update. Passing
  `Context` and `Args` by reference took a few nanoseconds off most of the
  captured paths.
- More threads still lower aggregate counter throughput: per-op wall time is
  above the single-thread cost at 2, 4, and 8 threads. Every captured
  observation interns its context under one runtime-wide lock; replacing
  per-call interning with interned context handles is planned separately.
