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
- `Runtime::install_global` and `Runtime::with_session_name` to avoid spelling
  `Box::leak` in application setup.

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

## Backend Trait

`footfall_core::Backend` is now a trait instead of a struct of function
pointers. Every method has a no-op default, so a backend overrides only the
primitives it handles, and adding a primitive is no longer a breaking change.
Backends carry their own state, so the per-crate `OnceLock` registrations are
gone:

- `install_backend` takes `&'static dyn Backend`. `Backend::NOOP` is replaced
  by the `NoopBackend` type.
- `Runtime` implements `Backend`. `Runtime::install_global(self)` is the single
  install entry point and returns `Result<&'static Runtime, InstallBackendError>`.
  The free functions `footfall_runtime::install_global`, `register_global`, and
  `backend`, the `Runtime::install_global_with_capacity`,
  `install_global_named`, and `install_global_named_with_level` helpers, and
  `footfall_runtime::InstallError` are removed. Build the runtime with its
  constructors and call `install_global`.
- `footfall_tracy::Tracy` is renamed to `TracyBackend` and implements
  `Backend`, with `TracyBackend::install_global`. The free `install_global`,
  `register_global`, and `backend` functions and `footfall_tracy::InstallError`
  are removed.
- `Fanout::new` takes two `&'static dyn Backend` values and `Fanout` implements
  `Backend`, so fan-outs nest. `Fanout::install_global(self)` replaces the free
  `footfall_fanout::install_global`, and `footfall_fanout::InstallError` is
  removed.

To compose backends, leak the children and install the fan-out:

```rust
let runtime: &'static Runtime = Box::leak(Box::new(Runtime::new(512, 4096)));
let tracy: &'static TracyBackend = Box::leak(Box::new(TracyBackend::new()));
Fanout::new(runtime, tracy).install_global()?;
```

## Descriptor Identity

Descriptor identity is now the descriptor's address instead of a hand-picked
id, so two crates can no longer collide by choosing the same number.

- `ScopeDesc::new`, `ScopeDesc::with_location`, `CounterDesc::new`, `count`,
  `bytes`, `EventDesc::new`, and `ResourceKindDesc::new` drop their id
  argument. The `scope_desc!`, `counter_desc!`, `event_desc!`, and
  `resource_kind_desc!` macros drop it too, for example
  `scope_desc!("record_frame")` and `counter_desc!(count, "draw_ops")`.
- Emission functions, `ScopeGuard`, and `Backend` methods take
  `&'static` descriptors. Declare descriptors as `static` items. Neither
  `&CONST_DESC` nor a `const DESC: &ScopeDesc = &ScopeDesc::new(..)` compiles,
  because every use of a `const` would be a different descriptor.
- `id()` derives an opaque id from the address. Ids are unique among live
  descriptors within one process run and are not stable across runs. The
  address never leaves the process: snapshots, exports, and the session wire
  carry dense, session-scoped ids from `1` instead, shared across descriptor
  kinds and assigned in first-seen order. Compare descriptor ids only within
  one capture session, and never against `desc.id()` in the producer.
- Snapshot counters and histograms are now sorted by name and context id
  instead of arriving in hash-map order.
- Descriptors are no longer `Copy` or `Clone`, and `==` compares identity.
- `footfall_tracing` leaks one descriptor per callsite instead of allocating
  ids from a reserved range; `TRACING_DESC_ID_BASE` is removed.

## User-Defined Label Keys

`LabelKey` is no longer a closed enum of UI terms (`Frame`, `Widget`, `Node`,
...). Applications and libraries declare their own keys as `static` items:

```rust
static FRAME: LabelKey = LabelKey::new("frame");

let context = Context::root().with(&FRAME, 7_u32);
let context = footfall::context!(FRAME = 7_u32);
```

- `Context::with`, `try_with`, `get`, and `contains` take `&'static LabelKey`.
  `context!` takes paths to static keys instead of variant names.
- `ContextEntry::key` returns `&'static LabelKey`; use `LabelKey::name` where
  `LabelKey::as_str` was used. Keys are identified by name, in process and on
  the wire, so `static` and `const` keys with the same name are the same key.
- Consumers outside the process see keys by name. `OwnedContextEntry::key` is
  now a `String`.
- The session protocol version is now `2.0`: context labels carry the key
  name string instead of a `u8` vocabulary tag. `1.x` consumers cannot decode
  `2.0` envelopes. See [WebTransport Session Wire Format](session-wire-format.md).

## Contexts and Args by Reference

Emission functions, `ScopeGuard::new`/`with_args`, and every `Backend`
method now take `&Context` and `&Args` instead of copying them. A `Context`
is about 264 bytes and an `Args` about 168, so passing them by value cost a
large copy per call, and fan-out backends copied them again for each inner
backend.

```rust
// Before
counter_add(&DRAW_OPS, context, 1);
let _scope = scope_with(&RECORD, context, args!("items" => 3_u32));

// After
counter_add(&DRAW_OPS, &context, 1);
let _scope = scope_with(&RECORD, &context, &args!("items" => 3_u32));
```

Backends that retain a context or arguments copy them (`*context`,
`*args`); both types are still `Copy`.
