# Footfall examples

Runnable programs that show Footfall end to end. Each is a binary in this
crate; run one with `cargo run -p footfall_examples --bin <name>`, and add
`-- --help` to see its options.

| Example | What it shows |
|---|---|
| [`quickstart`](src/bin/quickstart.rs) | Instrument a loop, read the capture back, summarize it, and write a trace for Perfetto. Start here. |
| [`demo_app`](src/bin/demo_app.rs) | A fake renderer with worker threads, varying costs, and the occasional slow frame, serving its capture to local tools until Ctrl-C. |
| [`record_demo`](src/bin/record_demo.rs) | The fake renderer, recorded to a capture file in-process with `CaptureRecorder`, for a fixed number of frames. |
| [`attach`](src/bin/attach.rs) | The smallest useful tool: find a session, subscribe, and print each window's frames. Read this one for the client API. |
| [`tracing_bridge`](src/bin/tracing_bridge.rs) | Plain `tracing` spans and events, captured by Footfall without changing them. |
| [`tracy_live`](src/bin/tracy_live.rs) | The demo renderer shown live in Tracy 0.14, with Footfall capturing the same emissions. |

## Try it

The quick start runs on its own:

```text
$ cargo run -p footfall_examples --bin quickstart
footfall quickstart · 30 frames, two passes

Frames
  30 frames · mean 2.25 ms · p95 2.47 ms · max 2.48 ms · 0 over 16.7 ms

Where the time went
  scope     count       total        self        mean         max
  render       60     55.9 ms     55.9 ms      932 µs     1.14 ms
  update       30     11.4 ms     11.4 ms      381 µs      396 µs

By pass
  pass="opaque"          34.0 ms  render 34.0 ms
  pass="transparent"     22.0 ms  render 22.0 ms
...
Wrote footfall-quickstart.pftrace; open it at https://ui.perfetto.dev.
```

To see a live session, start the demo in one terminal and watch it from
another with the [`footfall` command](../tools/footfall_cli/README.md):

```sh
cargo run -p footfall_examples --bin demo_app
cargo run -p footfall_cli -- top
```

`footfall top` shows the frame-time chart against a 16.7 ms budget, the
scopes that took the most time, how each counter moved per frame, the
renderer's messages, and whether anything was lost. The demo has a font
change every few seconds that blows the frame budget; watch for the red bar
and the message that explains it.

Record a session and look at it later:

```sh
cargo run -p footfall_cli -- record demo.ffcap
cargo run -p footfall_cli -- analyze summary demo.ffcap
cargo run -p footfall_cli -- analyze slow demo.ffcap --by-arg nodes
cargo run -p footfall_cli -- analyze pacing demo.ffcap
cargo run -p footfall_cli -- export demo.ffcap
cargo run -p footfall_cli -- top --file demo.ffcap
```

Producers and tools find each other through a per-user state directory. Set
`FOOTFALL_STATE_DIR` to the same directory for both to keep experiments apart
from anything else running.

## How the examples use Footfall

All of them depend on the `footfall` crate alone, with the features they
need, plus `footfall_fanout` and `footfall_memory`. The shared code in
[`src/`](src) is the demo renderer, which follows the
[instrumenting guide](../docs/instrumenting-an-app.md), and the formatting
used by the printed reports.
