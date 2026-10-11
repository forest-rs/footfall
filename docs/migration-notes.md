# Migration Notes

This document records public behavior changes while Footfall is still pre-1.0.

## Current Workspace Baseline

The workspace is now a small crate family rather than the original two-crate
seed:

- `footfall_core` and `footfall` define the call-site surface.
- `footfall_runtime` collects bounded in-process snapshots.
- `footfall_session` defines the transport-neutral consumer contract.
- `footfall_session_wtransport` provides a localhost WebTransport attach path.
- `footfall_export`, `footfall_tracy`, `footfall_fanout`, and `footfall_memory` stay
  downstream of the core seam.

## Runtime Capture Levels

`Runtime` capture behavior follows `InstrumentationLevel`:

- `Off`: no runtime collection.
- `Counters`: counters and gauges only.
- `Scoped`: counters, gauges, scopes, frame/pass/transaction markers, and flows.
- `Trace`: all currently supported observations, including resources.

Existing constructors keep working. New code that wants explicit policy should
use `Runtime::with_level`, `Runtime::named_with_level`, or `Runtime::set_level`.
For compatibility, `Runtime::new` and `Runtime::named` continue to default to
`Trace`.

Runtime collection now uses one context interner plus sharded counter and event
stores. Snapshot payloads keep the same public shape; event rows are merged and
ordered by timestamp during snapshot extraction, with ties broken by a
runtime-assigned recording sequence so events recorded by one thread within the
same microsecond keep their program order. `TimedObservation` carries that
sequence in its `sequence` field.

## Session Wire Format

The transport-neutral envelope model remains in `footfall_session`. The
WebTransport codec is an implementation detail of `footfall_session_wtransport`,
but it is now documented in
[WebTransport Session Wire Format](session-wire-format.md) and bounded so
consumer bugs fail predictably instead of allocating from untrusted lengths.

## Ergonomic Helpers

The low-level emission API remains available, but common call sites can now use:

- `scope` and `ScopeGuard` for drop-based scope pairing.
- marker helpers such as `frame_begin`, `frame_end`, `pass_begin`, and `pass_end`.
- `context!`, `scope_desc!`, `counter_desc!`, and `resource_kind_desc!` macros
  for compact static descriptors and contexts.
- `Runtime::install_global_*` helpers and `Runtime::with_session_name` to avoid
  spelling `Box::leak` in application setup.

## Chrome Trace Export Tracks

`footfall_export` now lays out Chrome Trace Event JSON by recording thread
instead of by semantic context:

- Timed observations use the runtime-assigned thread id as `tid`, with one
  `thread N` track per thread. The canonical context id moves into `args` as
  `context_id`, alongside the expanded `context` labels.
- Events keep the snapshot order instead of being re-sorted by timestamp and
  kind, so same-microsecond scopes on one thread no longer nest.
- A scope end closes the slice on the track where the scope began. An end
  whose begin is not in the snapshot is skipped and counted in the
  `footfall_unmatched_scope_ends` internal counter.
- Counters and gauges become one Chrome counter track per descriptor and
  context, named `name [context labels]`, with a single numeric series named
  after the descriptor unit. Aggregates and capture metadata live on track
  `0`.

## Reserved Scope Token

`ScopeToken::INVALID` (raw value `0`) is returned when no scope was recorded:
before a backend is installed, while the capture level excludes scopes, or
when the runtime cannot intern the scope's context. `scope_end` ignores it, and
the runtime, Tracy, and fan-out backends ignore it too. Backends issue tokens
starting at `1`. Previously the no-op paths returned the descriptor id, so a
guard started before backend installation could end an unrelated scope that
happened to receive the same token.
