# Measuring capture inspection

`inspection` measures repeated queries on one loaded trace separately from
capture decoding and trace construction. Supply a representative capture:

```sh
FOOTFALL_CAPTURE="$PWD/edits.ffcap" cargo bench -p footfall_benchmarks --bench inspection
cargo build -p footfall_benchmarks --example inspect_capture --release --locked
/usr/bin/time -l target/release/examples/inspect_capture edits.ffcap 30
```

The example reports bytes, windows, events, spans, messages, resource lifetimes,
named checkpoints, retained/recording-history status, and median/min/max nanoseconds for
30 repetitions. `decode` retains snapshots without building a trace; `construct`
uses already decoded snapshots; `decode_and_construct` does both from in-memory
file bytes. The loaded message/flow queries select a rejected edit by captured
ancestry. Work inspection gathers its connected observations; resource accounting
covers the entire trace and groups by kind. Criterion's message/flow cases instead
survey the whole trace. Neither benchmark installs a global runtime.

For fresh-process cost, prebuild the CLI, run a warm-up, then repeat:

```sh
/usr/bin/time -l target/release/footfall analyze work edits.ffcap \
  --scope procedural.edit --arg kind=rejected --json > /dev/null
```

Measure elapsed process time outside the command as well as peak RSS. Report
spawn, loader, parsing, query, JSON serialization, and output costs together for
this case. It reloads the capture on every call. Repeated loaded-query timings
exclude these process and file-read costs.

## Procedural consumer baseline

Measured on 2026-10-26, Apple silicon, macOS 27.0.1, Rust 1.99.0, optimized release
builds. Three fresh harness processes per capture, 30 repetitions of each
operation per process; numbers below are medians of the three medians. Fresh CLI
measurements use 15 processes per capture after one warm-up. The file cache was
warm, stdout was discarded, and no builds ran during measurements. This is a
baseline of the new operations, with no performance optimization claimed.

The real consumer was execution commit
`41d577c691b64be4424e449060eba2de0a948ccc`, procedural example, default density 32,
with 8/64/1,024 amplitude edits followed by topology, rejected, recovery,
retirement, and undo-release work. An isolated source copy used local Footfall
and added named before/after-edit checkpoints, cumulative created-byte counters,
and live-byte gauges updated at buffer create/drop. The first gauge baseline
was absent. Runtime event capacity was 256; streaming preserved every event
before eviction. All three regenerated protocol 1.1 captures had complete event history,
producer-attested recording history, and file reads.
No original consumer files were changed.

| Amplitude edits | Bytes | Windows | Events | Spans | Messages | Lifetimes | Checkpoints |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 8 | 23,039 | 15 | 489 | 63 | 158 | 15 | 28 |
| 64 | 121,070 | 71 | 2,561 | 343 | 830 | 71 | 140 |
| 1,024 | 1,838,035 | 1,031 | 38,081 | 5,143 | 12,350 | 1,031 | 2,060 |

All operation times below are microseconds. Resource queries cover the whole
history; message/flow/work queries select one rejected edit.

| Amplitude edits | Decode | Construct | Decode + construct | Messages loaded | Flows loaded | Work loaded | Resources loaded |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 8 | 145.54 | 54.12 | 183.54 | 5.21 | 3.17 | 14.75 | 3.08 |
| 64 | 693.71 | 269.08 | 921.58 | 40.42 | 23.88 | 113.71 | 13.92 |
| 1,024 | 9,918.25 | 4,243.96 | 13,948.92 | 922.58 | 551.75 | 2,579.46 | 239.71 |

| Amplitude edits | Fresh CLI median (ms) | CLI min–max (ms) | CLI peak RSS median (MiB) | Harness peak RSS range (MiB) |
| ---: | ---: | ---: | ---: | ---: |
| 8 | 8.28 | 7.41–9.90 | 9.06 | 3.88–4.02 |
| 64 | 9.37 | 8.63–10.31 | 9.91 | 6.48–6.89 |
| 1,024 | 27.60 | 26.93–28.44 | 21.25 | 50.98–55.67 |

RSS is process residency, including executable, allocator retention, input,
and temporary allocations. The harness retains both decoded snapshots and a
trace and repeatedly constructs/drops more traces; its peak is deliberately
**not** presented as exclusive trace memory or live heap. The fresh CLI's peak
has a different workload and is not directly subtractable from it. The host was
not isolated from other applications; fresh-process timings include scheduling
and loader variation. Small-capture startup variation should not be mistaken for
a query-cost improvement or regression.

Loading once removes repeated decoding and process startup, but loaded inspection
still scales with retained history: queries scan all candidate messages, flows,
and scopes, and ancestry traversal adds work. Paging bounds returned items rather
than scanning or all temporary matching storage. Resource accounting sorts retained
observations and revisits live state. These measurements do not demonstrate
constant-cost queries, bounded analysis memory, or CPU/GPU execution performance.
A persistent host and targeted indexes are future work, justified by workload
measurements rather than implied by the existence of a typed API.

For a reproducible synthetic contract fixture independent of the execution checkout,
use `cargo run -p footfall_examples --bin edit_inspection -- NEW.ffcap 2`.
Its storage sizes and workload differ from the real consumer; report that difference
if using it for measurements. See the [investigation walkthrough](../../docs/investigating-an-edit.md)
for the consumer questions and the [analysis contract](../../docs/analysing-a-trace.md#connected-work-inspection)
for evidence and boundary semantics.
