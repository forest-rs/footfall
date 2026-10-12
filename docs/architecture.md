# Footfall Architecture

Footfall is a family of small crates for instrumenting Rust programs. Its job
is to record what work happened and which semantic owner it happened under,
cheaply enough to leave in production code, and to get that record to the
tools that need it: in process, over a local session, or from a file.

It is not a system profiler, a logging framework, or a viewer. It provides a
narrow, durable seam that instrumented code calls, and replaceable pieces
behind it.

## The Shape

```text
 instrumented code                      footfall (footfall_core)
   scope!, counter_add, frame_begin  →  static descriptors + ContextHandle
                                          │ one indirection, after a level check
                                          ▼
                                     a Backend, installed once per process
                     ┌────────────────────┼─────────────────────┐
              footfall_runtime     footfall_tracy        footfall_fanout
              bounded capture      live Tracy            two backends at once
                     │
           snapshot / snapshot_since(cursor)
                     │
     ┌───────────────┼──────────────────────┬──────────────────────────┐
 footfall_export  footfall_session      footfall_session_wtransport  (your code)
 Chrome trace     OwnedSnapshot, codec,  serve and attach over
                  capture files          local WebTransport
                     │
              footfall_analysis
              traces, span queries, comparisons
```

`footfall_tracing` sits beside the instrumented code: a `tracing` layer that
turns spans and events into Footfall emissions. `footfall_memory` builds
cache and scratch-buffer metrics from counters. The `footfall` crate
re-exports the instrumentation API at its root and each part above as a
module behind a feature, so most users depend on it alone.

## Crate Boundaries

Each crate owns one thing and says what it does not own.

- **`footfall_core`** owns ids, static descriptors, contexts, label keys,
  instrumentation levels, and the emission seam: the `Backend` trait, the
  process-wide backend, and the emission functions. It is `no_std` with no
  dependencies. It does not own buffering, clocks, export, or transport.
- **`footfall`** owns the recommended surface: the core API at its root, and
  the other crates as feature-gated modules. It adds no architecture of its
  own.
- **`footfall_runtime`** owns bounded in-process collection: per-thread
  shards, the context table, timestamps, sequence numbers, cursors, and
  snapshots. It does not own wire formats, export, or UI policy.
- **`footfall_session`** owns the form snapshots take outside the process
  (`OwnedSnapshot`), the versioned envelopes they travel in, the codec, and
  capture files. It is transport-neutral.
- **`footfall_session_wtransport`** owns one transport: local discovery,
  identity and certificate pinning, and request, response, and
  subscription framing over WebTransport.
- **`footfall_analysis`** owns interpretation that every tool would
  otherwise repeat: a `Trace` of one runtime instance with spans on
  per-thread tracks, frames, counter series, and flows; queries and
  comparisons over it; and a statement of what each answer covers.
- **`footfall_export`** owns projection into Chrome Trace JSON.
- **`footfall_tracing`**, **`footfall_tracy`**, **`footfall_fanout`**, and
  **`footfall_memory`** are adapters and helpers at the edges.

Dependencies point one way: core at the bottom, the runtime on core, the
session on the runtime, and transports, analysis, and export on the session.
Nothing downstream redefines the core vocabulary.

## The Emission Seam

Instrumented code calls ordinary functions with static descriptors and a
context handle:

- A descriptor (`ScopeDesc`, `CounterDesc`, `EventDesc`, `ResourceKindDesc`)
  names one kind of observation. Its identity is its address, so descriptors
  are `static` items; the types make reaching one through a `const` a compile
  error.
- A `ContextHandle` names the owner. Contexts are registered once per owner,
  and every emission carries only the integer handle, so attribution costs
  the same however many labels the context has.

Each emission checks the process-wide maximum level, then makes one dynamic
call to the installed backend. With no backend installed, the check fails
and the call costs well under a nanosecond. With `--cfg footfall_compile_out`,
the functions are empty and inlined away. The hot path never allocates,
never formats strings, and never takes a lock in the runtime's steady state.

Context is explicit rather than thread-local. That keeps the cost of a call
visible at the call site and works the same in async, batch, and
ownership-heavy code. Helpers such as `scope!` and `RegisteredContext` make
the explicit form convenient without hiding it.

## Instrumentation Levels

Levels are cumulative: `Off`, `Counters`, `Scoped` (adds scopes, markers,
flows, and messages), and `Trace` (adds resource lifecycles). The installed
backend sets the process-wide maximum, and every emission above it returns
at the level check. Counter-only collection is useful on its own and needs
no event buffer.

## The Runtime

The runtime is a backend that keeps:

- cumulative counters, gauges, and histograms per descriptor and context,
  admitted against a fixed key capacity;
- a fixed-capacity buffer of timestamped events per thread, which keeps the
  newest events and evicts the oldest when full;
- a table of registered contexts, which keeps a released context until
  every event recorded before its release has been evicted, then retires it
  when the room is needed.

Each recording thread writes to its own shard. Every event gets a global
sequence number. Reading never drains anything: `snapshot` returns what is
retained, and `snapshot_since(cursor)` returns the events since a cursor,
the next cursor, and how many events in that range were evicted before the
read. Any number of readers can follow one runtime independently.

Everything the runtime cannot keep is counted rather than lost silently:
evicted events, refused context registrations, dropped counter updates,
unattributed rows, updates after release, and retired rows. Each snapshot
also records how the runtime was capturing (level, capacities, whether
counter samples are recorded, whether instrumentation is compiled out) and
which runtime it came from (a random instance id, process id, wall-clock
anchor, producer version), so a reader can tell "nothing happened" from "not
captured" and a restart from a gap.

At each frame end the runtime records the value of every counter the frame
updated. Those samples are what make per-frame counter changes possible.

## Sessions

A runtime snapshot belongs to its process. `footfall_session` defines the
form it takes everywhere else, `OwnedSnapshot`, which a consumer gets the
same way whether it was taken in process, received live, or read from a
file. Snapshots travel as versioned, chunked envelopes. A subscription
sends one full snapshot and then, for each later window, only what changed
plus the window's events; `OwnedSnapshot::apply` folds the changes into the
state.

The wire encoding is compact because events dominate traffic: LEB128
integers, deltas for timestamps and sequence numbers, and descriptor names
written once per payload. The protocol version is the only fixed-width
field, so any reader can check it first. The format is specified in
[the wire-format document](session-wire-format.md), and the guarantees a
consumer can rely on in [the consumer contract](session-consumer-contract.md).

The WebTransport transport is localhost-first. Each producer has a
self-signed identity and registers its address and certificate hash in a
per-user state directory, so tools find every producer on the machine
without configuration. Subscriptions are demand-driven: the server takes
each snapshot only when the client asks for the next one, so a slow client
never receives stale data and its losses are reported exactly.

## Analysis

A `Trace` is built from one runtime instance's snapshots, pushed one after
another. Sequence numbers make the bookkeeping exact: events a trace has seen
are skipped when snapshots overlap, and the numbers missing from a snapshot
are the events evicted before it was read, which become gaps placed in time
by the events around them. Spans and frames are identified by the sequence
number of the event that began them, so an id cited from one snapshot finds
the same span in any other snapshot or capture of the instance.

Every answer carries a `Basis` that says what it covers: whether the
producer recorded the data at all, how spans crossing a range's edge were
counted, which spans were left out because they were still open or began
before the trace, and where events are missing. A consumer can tell
"nothing happened" from "nothing was recorded" from "the data is
incomplete" without consulting anything else.

Analysis works on `Duration`s rather than the runtime's microsecond
timestamps, so a finer clock would not change its interface.

## Invariants

- Core emission never allocates and never formats strings.
- Descriptors are static and stable for the life of the process.
- Memory is bounded; data dropped under pressure is counted.
- Counter-only collection stays useful without a trace buffer.
- Context is explicit and composable.
- Domain vocabulary lives in descriptors and labels, not in core enums.
- Every snapshot says where it came from and how it was captured.

## Alternatives Considered

- **One crate for everything.** It would give low-level code a large
  dependency surface, make `no_std` harder, and couple emission to
  collection, export, and transport. Rejected for separate crates with a
  facade.
- **Thread-local scoped context as the primary model.** It hides the cost of
  attribution and fits async and explicit-ownership code poorly. Rejected
  for explicit handles, with scoped helpers on top.
- **Draining capture sessions.** A runtime that hands its buffer to one
  reader at a time makes every additional tool a competitor. Rejected for
  cursor reads over a buffer that keeps the newest events.
- **Push-only streaming.** A server that pushes on a timer queues stale data
  behind a slow client. Rejected for subscriptions where the client asks for
  each window.

## Non-Goals

- Replacing ETW, DTrace, LTTng, perf, or other system tracing.
- A general-purpose logging framework or arbitrary event bags.
- Allocator interception; memory observation starts from memory the program
  already owns.
- A bundled viewer. Footfall exports to existing tools and gives other
  tools a stable session protocol; `footfall_top` is an example of one.

## History

The design began from the brief in
[`history/original-design-brief.md`](history/original-design-brief.md). The
first working design document, which planned the initial slice, is kept in
[`history/first-slice-architecture.md`](history/first-slice-architecture.md).
