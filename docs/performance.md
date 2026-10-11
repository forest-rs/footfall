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
| `none/counter_add`, `none/scope begin+end`, `none/scope guard`, `none/message` | The emission seam with no backend installed: the call, the backend lookup, and argument copies. |
| `none/context build/N labels` | Building a `Context` with 1, 4, or 8 labels. |
| `runtime/off/...` | The runtime installed at `InstrumentationLevel::Off`, which filters everything. |
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
- Branch: `bench-methodology`

| Case | Estimate | 95% interval |
| --- | ---: | ---: |
| none/counter_add | 4.78 ns | 4.78 ns – 4.79 ns |
| none/scope begin+end | 8.34 ns | 8.30 ns – 8.38 ns |
| none/scope guard | 8.73 ns | 8.71 ns – 8.74 ns |
| none/message | 8.51 ns | 8.50 ns – 8.52 ns |
| none/context build/1 labels | 8.40 ns | 8.36 ns – 8.44 ns |
| none/context build/4 labels | 10.47 ns | 10.46 ns – 10.49 ns |
| none/context build/8 labels | 23.07 ns | 22.85 ns – 23.26 ns |
| runtime/off/counter_add | 5.26 ns | 5.25 ns – 5.26 ns |
| runtime/off/scope begin+end | 9.19 ns | 9.16 ns – 9.21 ns |
| runtime/counters/counter_add | 162.65 ns | 158.68 ns – 167.48 ns |
| runtime/counters/gauge_set | 152.15 ns | 152.03 ns – 152.27 ns |
| runtime/counters/histogram_record | 147.04 ns | 146.58 ns – 147.55 ns |
| runtime/counters/counter_add 1024 rotating contexts | 188.66 ns | 188.21 ns – 189.19 ns |
| runtime/counters/counter_add context built per call | 172.13 ns | 171.72 ns – 172.62 ns |
| runtime/counters/counter_add threads/1 | 148.52 ns | 148.14 ns – 148.90 ns |
| runtime/counters/counter_add threads/2 | 239.23 ns | 237.96 ns – 240.45 ns |
| runtime/counters/counter_add threads/4 | 481.02 ns | 475.95 ns – 485.97 ns |
| runtime/counters/counter_add threads/8 | 352.31 ns | 351.08 ns – 353.50 ns |
| runtime/scoped/scope begin+end | 180.03 ns | 179.38 ns – 180.66 ns |
| runtime/scoped/scope guard | 177.76 ns | 177.60 ns – 177.92 ns |
| runtime/scoped/message | 154.57 ns | 153.32 ns – 156.08 ns |
| runtime/scoped/message_with 2 args | 147.96 ns | 147.12 ns – 148.96 ns |
| runtime/scoped/tracing span enter+exit | 271.53 ns | 270.87 ns – 272.30 ns |
| runtime/scoped/tracing event | 219.82 ns | 219.22 ns – 220.50 ns |
| runtime/trace/resource_update | 138.46 ns | 138.21 ns – 138.72 ns |
| runtime/capture/snapshot 128 frames | 49.6 µs | 49.5 µs – 49.6 µs |
| runtime/capture/chrome trace export 128 frames | 276.6 µs | 275.7 µs – 277.5 µs |
| fanout/counters/counter_add | 158.64 ns | 158.44 ns – 158.86 ns |
| fanout/scoped/scope begin+end | 213.94 ns | 213.79 ns – 214.10 ns |

## Reading the Results

- With no backend, or with the runtime at `Off`, a call costs about 5 to
  9 ns. Most of that is copying the `Context` and `Args` values through the
  backend call.
- Every captured observation costs roughly 140 to 180 ns, whether it is a
  counter, histogram, scope pair, message, or resource update.
- More threads lower aggregate counter throughput: per-op wall time is above
  the single-thread cost at 2, 4, and 8 threads.
