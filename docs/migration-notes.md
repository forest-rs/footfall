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

Runtime collection keeps registered contexts in a handle-keyed table and
writes counters and events into per-thread shards (see
[Registered Context Handles](#registered-context-handles)). Snapshot payloads
keep the same public shape; event rows are merged and
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
before a backend is installed or while the capture level excludes scopes.
`scope_end` ignores it, and
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

Emissions have since moved from `&Context` to registered context handles; see
[Registered Context Handles](#registered-context-handles). `Args` are still
passed by reference.

## Process-Wide Maximum Level

`footfall_core` now keeps a process-wide maximum level
(`footfall_core::set_max_level` / `max_level`, re-exported by `footfall`).
Every emission checks it with one relaxed load before reaching the backend,
so disabled and level-excluded emissions return without dynamic dispatch.

- The level starts at `InstrumentationLevel::Off`, so emissions before any
  backend is installed stop at that check.
- `install_backend` raises it to `InstrumentationLevel::Trace` on success.
- `Runtime::install_global` then sets it to the runtime's level, and
  `Runtime::set_level` keeps it in sync for that runtime. A runtime used
  inside a fan-out leaves it alone; call `set_max_level` yourself if you
  want early filtering there.
- Scope ends and `End` markers are never filtered, so lowering the level
  while a scope or frame is open still closes it in every backend.

A backend installed directly with `install_backend` that relied on seeing
emissions after something lowered the maximum level will now miss them.

## Registered Context Handles

Emissions no longer carry a `Context`. A context is registered once per
semantic owner and every emission carries its `ContextHandle`, a `u64`, so
attribution costs the same however many labels the context has, and the
runtime no longer hashes and interns a context under one process-wide lock on
every call.

```rust
use footfall::{RegisteredContext, context, counter_add, scope};

// Before: the context travels with, and is interned on, every call.
let pass = context!(PASS = 1_u32, NODE = 42_u32);
let _scope = scope(&RECORD, &pass);
counter_add(&DRAW_OPS, &pass, 1);

// After: register once where the owner lives, emit with the handle.
let pass = RegisteredContext::new(&context!(PASS = 1_u32, NODE = 42_u32));
let _scope = scope(&RECORD, pass.handle());
counter_add(&DRAW_OPS, pass.handle(), 1);
```

- Every emission function, `ScopeGuard::new`/`with_args`, and every `Backend`
  method take a `ContextHandle` where they took `&Context`.
  `ContextHandle::ROOT` replaces `&Context::root()` and needs no registration.
- `register_context` returns a handle and `release_context` releases it.
  `RegisteredContext` releases on drop; `RegisteredContext::into_handle`
  keeps the context registered for the rest of the process.
- `LazyContext` holds a context in a `static` and registers it on the first
  `handle()` call made after a backend is installed. Contexts registered
  eagerly before installation are never seen by the backend, and their
  observations are attributed to the root context.
- Equal contexts registered separately get distinct handles and are not
  merged. Register once per owner and reuse the handle.
- `Backend` gains `context_registered(handle, &Context)` and
  `context_released(handle)`, both defaulting to no-ops. Backends that
  attribute observations keep the context from registration; Tracy renders it
  once, and fan-out forwards both calls to its children.
- Handles are allocated by `footfall_core` from one process-wide counter and
  never reused, so all backends share them. Snapshot `ContextId`s carry the
  handle values: sparse, with `0` for the root context, and stable while the
  context stays registered. `Snapshot::context` looks ids up instead of
  indexing by position.
- The runtime keeps live contexts across session resets and purges released
  ones when the next session starts, so observations recorded before release
  still resolve in that session's snapshots.
- `Snapshot::dropped_contexts` now counts registrations refused because the
  runtime's context table was full, plus counter rows and events whose handle
  the runtime does not know; those are kept and attributed to the root context
  instead of being dropped.
- Counters and events are written into per-thread shards and merged when a
  snapshot is taken, so threads recording the same counter no longer contend.
  A counter key updated from several threads takes one slot of the counter
  capacity per shard it lands in. Gauges written from several threads resolve
  to the most recent write by timestamp.
- `footfall_tracing` registers a context per span from its numeric and
  boolean fields, on top of its parent span's labels, and releases it when the
  span closes. Span scopes and the events recorded inside a span are
  attributed to that context instead of the root.
- `footfall_memory`'s `TrackedScratchVec::new` and `CacheStats::new` take a
  `ContextHandle`.
- The session wire layout is unchanged; context ids on the wire are now
  sparse handle values, as described in
  [WebTransport Session Wire Format](session-wire-format.md).

## Non-Exhaustive Enums

Enums that are expected to grow are now `#[non_exhaustive]`, so adding a
variant is not a breaking change. Matches outside the defining crate need a
wildcard arm:

- `footfall_core`: `Unit`, `MarkerKind`, `LabelValue`, `InstrumentationLevel`.
- `footfall_runtime`: `CounterKind`, `Observation`.
- `footfall_session`: `Payload`, `OwnedSessionPayload`, `OwnedObservation`,
  `OwnedLabelValue`, `SessionErrorCode`, `SessionStreamStatus`,
  `SessionMessage`.
- `footfall_session_wtransport`: `SessionRequestKind`, `ServedRequestKind`.

`MarkerPhase`, `FlowPhase`, and `SessionResult` stay exhaustive: they are
complete by definition. The variants of `Observation` and `OwnedObservation`
are not individually non-exhaustive, so consumers can still build them in
fixtures and tests; a new field on an existing variant would be a breaking
change and would come with a protocol version bump.

Servers should answer an unknown `SessionRequestKind` with
`respond_error`. The session encoder rejects values it has no encoding for
with an `InvalidInput` error instead of guessing a tag.

## Private Fields

Public data types no longer expose their fields, so fields can be added or
re-represented without a breaking change. Read values through accessors named
after the old fields, for example `snapshot.events()`,
`counter.value()`, `envelope.payload()`, and `announcement.id()`.
`String` fields are returned as `&str`, `Option<String>` as `Option<&str>`,
and `Vec` fields as slices.

- `footfall_runtime`: `Snapshot`, `SnapshotMetadata`, `ContextSnapshot`,
  `CounterSnapshot`, `HistogramSnapshot`, and `TimedObservation`. Build
  snapshots by hand, for example as test fixtures, with
  `Snapshot::builder(SnapshotMetadata::new("app"))`, which keeps contexts
  sorted by id and events in timestamp and sequence order.
  `SnapshotMetadata` gains `with_session_name`, `with_session_sequence`, and
  `with_wall_clock_anchor_micros`. `HistogramSnapshot::new` creates an empty
  row and `HistogramSnapshot::record` adds samples. `ContextSnapshot`,
  `CounterSnapshot`, and `TimedObservation` have `new` constructors.
- `footfall_session`: `Chunking`, `EnvelopeHeader`, `SnapshotSummary`,
  `DroppedSummary`, `Envelope`, every `Owned*` row and envelope type, and the
  live protocol types (`SessionMethod`, `SessionRequest`, `SessionResponse`,
  `SessionError`, `SessionStreamStart`, `SessionStreamItem`,
  `SessionStreamEnd`). Each has a `new` constructor taking its fields in
  declaration order, or keeps its existing constructors. Request, stream
  start, and stream item values gain `into_payload`, and responses gain
  `into_result`, for taking ownership.
- `footfall_session_wtransport`: `SessionAnnouncement`, `ServedRequest`, and
  `SessionServerConfig`. Configure a server with
  `SessionServerConfig::default().with_bind_address(..).with_path(..)`; the
  default still binds to localhost.
- `footfall_memory`: `ScratchMetrics` and `CacheMetrics` have `new`
  constructors.
