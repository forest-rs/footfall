# Migration Notes

This document records public behavior changes while Spoor is still pre-1.0.

## Current Workspace Baseline

The workspace is now a small crate family rather than the original two-crate
seed:

- `spoor_core` and `spoor` define the call-site surface.
- `spoor_runtime` collects bounded in-process snapshots.
- `spoor_session` defines the transport-neutral consumer contract.
- `spoor_session_wtransport` provides a localhost WebTransport attach path.
- `spoor_export`, `spoor_tracy`, `spoor_fanout`, and `spoor_memory` stay
  downstream of the core seam.

## Runtime Capture Levels

`Runtime` capture behavior follows `InstrumentationLevel`:

- `Off`: no runtime collection.
- `Counters`: counters and gauges only.
- `Scoped`: counters, gauges, scopes, frame/pass/transaction markers, and flows.
- `Trace`: all currently supported observations, including resources.

Existing constructors keep working. New code that wants explicit policy should
use the level-aware constructors or setters added by `spoor_runtime`.

## Session Wire Format

The transport-neutral envelope model remains in `spoor_session`. The
WebTransport codec is an implementation detail of `spoor_session_wtransport`,
but it is now documented and bounded so consumer bugs fail predictably instead
of allocating from untrusted lengths.
