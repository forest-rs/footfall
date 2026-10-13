# Release captures

Capture files recorded by real programs, kept so later versions of Footfall
are tested against them. [`tests/captures.rs`](../tests/captures.rs) reads
each one with today's Footfall and checks both that it decodes and what it
says: frames, pacing, scopes, and counters.

They hold Footfall to its capture-format promise (see the
[wire format](../../docs/session-wire-format.md#capture-files)): a capture a
released Footfall wrote stays readable by later releases at the same
protocol major version, means the same thing when read, or fails with an
error naming the version that wrote it.

## Rules

- Never re-record or edit a capture here. If a test fails, either the
  change misreads captures people already have, or the analysis changed
  what it concludes on purpose: then update the figures in the test and
  say so in the changelog.
- Add captures, don't replace them: one from each release whose format or
  recording changed, named for what recorded it.
- Before the first release the protocol may still change. If it does,
  re-record these with the release itself and update their provenance.

## The captures

### `demo.ffcap`

The examples' fake renderer: worker threads, a glyph cache, a pretend GPU
on a 24 MHz clock synced each frame, a pretend 60 Hz display reporting
presentations, a shader compile, and frames superseded before submit.
240 frames, 17,498 events in 5 snapshots, nothing lost.

```sh
cargo run --release -p footfall_examples --bin record_demo -- demo.ffcap --frames 240
```

Recorded 2026-10-14 with Footfall `3956846` (producer version 0.1.0,
protocol 1.0) on macOS 27.0.1, Apple M3 Max.

### `lightweald-metal.ffcap`

Lightweald's `lighting_showcase` on Metal, paced by `frameclock` and
recorded through `frameclock_footfall`: frame points with stated timing
bases from Core Animation's presentation feedback, GPU work from Metal
commit feedback on its own timeline, and frameclock's scheduler state. A
real run, so it has deadline misses, backlog, and frames the display held.
331 frames over 3 seconds at 120 Hz, 5,267 events in 2 snapshots, nothing
lost.

```sh
LIGHTWEALD_BACKEND=metal LIGHTWEALD_FOOTFALL=lightweald-metal.ffcap \
  LIGHTWEALD_FOOTFALL_SECONDS=3 cargo run --release -p lighting_showcase
```

Recorded 2026-10-13 with lightweald `ed5cc8d` (Footfall `ae9da5b`,
subduction `3ea1776`, forest-rs/subduction#51) on macOS 27.0.1, Apple M3
Max, built-in Liquid Retina XDR display (ProMotion).
