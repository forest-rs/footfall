# Migrating from 0.1 to 0.2

Footfall 0.2.0 is a coordinated release of the public crates and CLI. It
adds frame plans, cadence, buffer waits, severity, and Perfetto frame
timelines. It also changes some Rust API construction rules and the
default export format. The minimum Rust version remains 1.88.

## Update crate requirements together

Change application and library requirements from `0.1` to `0.2`, retaining
the features each needs:

```toml
[dependencies]
footfall = { version = "0.2", features = ["runtime", "export"] }
```

All published Footfall crates share version 0.2.0, including direct users
of `footfall_core`, `footfall_runtime`, and the adapters. Update instrumented
libraries in the same process too: separate 0.1 and 0.2 copies of the core
have separate installed backends. Check `cargo tree -d` for an old Footfall
dependency left behind by another library.

## Read analysis results without constructing them

`Why` and the counts returned by `Pacing` are now `#[non_exhaustive]`:
`OutcomeCounts`, `LateCauseCounts`, `SupportCounts`, `DeadlineCounts`,
`BasisCounts`, `PredictionError`, and `WastedWork`. Their fields remain
public to read, but downstream crates cannot construct them with struct
literals. Destructuring must include `..`, allowing later releases to add
fields:

```rust
use footfall::analysis::{FramePacing, Why};

fn explain(frame: &FramePacing) {
    if let Some(Why { cause, support, .. }) = frame.why() {
        println!("{cause}, {support}");
    }
}
```

Obtain results from the analysis API. For counts that implement `Default`,
use that constructor when an empty value is needed. Match non-exhaustive
enums with a fallback arm. New outcomes include `Held` and `Synchronous`;
new causes include `WaitedForBuffer` and `Mispredicted`. The updated
[analysis guide](analysing-a-trace.md#frame-pacing) explains their meaning.

Analysis conclusions can improve while a capture's recorded facts remain
the same. In particular, steady lateness is now distinguished from a
cadence hitch, and a scheduler's deliberate holds are distinguished from
misses. Revisit assertions or scripts that equate every late frame with a
new hitch.

Buffer waits remain optional evidence. If a producer cannot separate them
from its application work, do not invent buffer-point times: the measured
work includes both and `buffer_wait` remains unknown. A missed submission
deadline alone does not establish how much time was computation or waiting.

## Upgrade readers before producers

The Rust crate version is 0.2.0; the session and capture protocol is **1.1**.
These are separate version numbers.

- New consumers read protocol 1.0 captures and producers. Existing release
  captures are retained unchanged and tested for compatibility.
- Protocol 1.0 consumers reject protocol 1.1 envelopes and captures. They
  cannot skip the new frame opportunity, frame plan, or buffer-point data.
- Upgrade the CLI and other consumers first, then upgrade producers. Once
  0.2.0 is published, `cargo install footfall_cli --version 0.2.0 --locked`
  installs the matching command-line reader.

Custom consumers should use the current session decoder and its version
checks rather than interpreting unknown tags as older layouts. See the
[wire contract](session-wire-format.md#compatibility-rule).

## Choose the export format explicitly in scripts

`footfall export run.ffcap` now writes `run.pftrace`, Perfetto's protobuf
format. The output retains nanosecond timing precision and carries frame
timeline verdicts. To keep Chrome Trace JSON output:

```sh
footfall export run.ffcap --format json -o run.json
```

The explicit Rust APIs remain available:
`footfall::export::to_perfetto_trace` and `to_chrome_trace_json`, with their
corresponding file writers. Existing calls to the JSON API still write JSON.

## Check structured pacing results for partial reads

`analyze summary`, `analyze slow`, and `analyze compare` also support `--json`.
Use `--kind transaction` or `--kind pass` for marked work, or `--scope NAME`
with `--arg KEY=VALUE` for argument-bearing scope instances. The same selectors
apply to `check`, with `work`, `spans`, and `losses` checks for non-frame work.
Non-frame slow analysis needs an explicit `--budget-ms` rather than a frame
deadline. See the [CLI guide](../tools/footfall_cli/README.md#transactions-and-scoped-work).

`check --require losses` now distinguishes cumulative runtime eviction from
missing capture events. A complete streamed capture passes after runtime
eviction; scripts should not use eviction counts as evidence of capture loss.

`footfall analyze pacing --json` now includes:

```json
"capture_read": {"complete": true, "truncated": false, "skipped_snapshots": 0}
```

For a truncated file or snapshots skipped from another runtime instance,
the usable prefix is still analyzed and the command succeeds, with stderr
warnings and `complete: false`. This flag describes consumption of the
file, not the completeness of instrumentation or whether the runtime lost
events. Use `footfall check --require losses` and the relevant signal
checks for those questions. Scripts should retain new outcome and cause
names as unknown when they cannot interpret them, rather than treating
them as on time.
