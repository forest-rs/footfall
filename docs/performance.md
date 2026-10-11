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
- Branch: `context-handles`

| Case | Estimate | 95% interval |
| --- | ---: | ---: |
| none/counter_add | 0.44 ns | 0.44 ns – 0.45 ns |
| none/scope begin+end | 0.57 ns | 0.57 ns – 0.57 ns |
| none/scope guard | 0.62 ns | 0.62 ns – 0.63 ns |
| none/message | 0.42 ns | 0.42 ns – 0.42 ns |
| none/context register+release | 2.68 ns | 2.68 ns – 2.69 ns |
| none/context build/1 labels | 11.24 ns | 11.22 ns – 11.26 ns |
| none/context build/4 labels | 18.27 ns | 18.25 ns – 18.30 ns |
| none/context build/8 labels | 50.11 ns | 50.01 ns – 50.22 ns |
| runtime/off/counter_add | 0.45 ns | 0.45 ns – 0.45 ns |
| runtime/off/scope begin+end | 0.57 ns | 0.56 ns – 0.57 ns |
| runtime/counters/counter_add | 44.81 ns | 44.76 ns – 44.88 ns |
| runtime/counters/gauge_set | 44.44 ns | 44.40 ns – 44.48 ns |
| runtime/counters/histogram_record | 41.76 ns | 41.74 ns – 41.78 ns |
| runtime/counters/counter_add 1024 rotating contexts | 47.55 ns | 47.50 ns – 47.61 ns |
| runtime/counters/context register+release | 102.05 ns | 101.61 ns – 102.48 ns |
| runtime/counters/counter_add with register+release per call | 163.44 ns | 162.92 ns – 164.01 ns |
| runtime/counters/counter_add threads/1 | 44.53 ns | 44.49 ns – 44.58 ns |
| runtime/counters/counter_add threads/2 | 26.16 ns | 24.07 ns – 28.68 ns |
| runtime/counters/counter_add threads/4 | 15.11 ns | 13.72 ns – 16.61 ns |
| runtime/counters/counter_add threads/8 | 12.64 ns | 11.94 ns – 13.37 ns |
| runtime/scoped/scope begin+end | 69.47 ns | 69.41 ns – 69.52 ns |
| runtime/scoped/scope guard | 70.34 ns | 70.28 ns – 70.40 ns |
| runtime/scoped/message | 37.10 ns | 36.98 ns – 37.20 ns |
| runtime/scoped/message_with 2 args | 36.98 ns | 36.82 ns – 37.15 ns |
| runtime/scoped/tracing span enter+exit | 197.91 ns | 197.73 ns – 198.12 ns |
| runtime/scoped/tracing event | 104.35 ns | 104.28 ns – 104.43 ns |
| runtime/trace/resource_update | 32.41 ns | 32.38 ns – 32.45 ns |
| runtime/capture/snapshot 128 frames | 32.2 µs | 31.5 µs – 32.8 µs |
| runtime/capture/chrome trace export 128 frames | 279.7 µs | 279.3 µs – 280.3 µs |
| fanout/counters/counter_add | 45.41 ns | 45.29 ns – 45.52 ns |
| fanout/scoped/scope begin+end | 91.02 ns | 90.87 ns – 91.18 ns |

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
- Every captured observation costs roughly 30 to 70 ns: a timestamp, an
  uncontended per-thread shard lock, and an aggregate lookup or event append.
  Before context handles it was 170 to 210 ns, most of it hashing and
  interning the `Context` under one runtime-wide lock.
- Counter throughput now grows with threads: per-op wall time at 2, 4, and 8
  threads is below the single-thread cost, where before it was one and a
  half to three times above it. Threads land in separate shards and share no
  lock on the counter path.
- Registering a context costs about 100 ns with the runtime installed, so
  registering per call, as in the anti-pattern case, gives back most of the
  gain. Register once per owner.
- The event path still shares two atomics across threads, the event-capacity
  reservation and the recording sequence, and snapshot merge and Chrome
  export remain the largest costs.
