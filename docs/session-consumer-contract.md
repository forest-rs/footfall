# Session Consumer Contract

This document defines the minimum contract an external consumer or UI needs in
order to attach to Footfall.

## Scope

A consumer does not participate in hot-path emission.

Its job is to:

- discover one or more active sessions
- fetch snapshot data
- interpret the versioned session envelope model
- render or query that data

## Canonical Data Model

The transport-neutral contract lives in `footfall_session`, which `footfall`
re-exports as `footfall::session` with the `session` feature.

The important types are:

- `Envelope<'a>`
- `Payload<'a>`
- `OwnedSessionEnvelope`
- `OwnedSessionPayload`

Snapshot publication has a fixed shape:

1. `SnapshotStart`
2. at most one `Threads` envelope
3. zero or more `Contexts` chunks
4. zero or more `RetiredContexts` chunks, only in a changes snapshot
5. zero or more `Counters` chunks
6. zero or more `Histograms` chunks
7. zero or more `Events` chunks
8. `SnapshotEnd`

That ordering is the contract a consumer should rely on.

## What a Snapshot Is

A producer's runtime keeps a recent window of history, not a session that
someone drains, so any number of consumers can read it without affecting one
another:

- Counters, gauges, and histograms are cumulative since the runtime was
  created. Every snapshot carries their current values; to see change over
  time, compare two snapshots, or use the counter-sample events producers
  record at each frame end, which give each counter's value per frame.
- Events are numbered by a sequence that counts every event the runtime
  recorded. A snapshot covers a window of that sequence: `since` up to
  `next`. Request the next snapshot with the previous one's
  `next_cursor()`, and you receive only newer events.
- The runtime keeps a bounded number of events and evicts the oldest to make
  room. `missed` in the window counts events in the range that were evicted
  before you read them; read more often, or ask for a larger event capacity,
  if it is not zero.
- Every snapshot names the runtime's random `instance_id`. Context, descriptor,
  and thread ids and event sequence numbers are only meaningful within one
  instance. A cursor from an earlier instance, for example from before the
  producer restarted, reads from the start, and the snapshot's instance id
  tells you to discard what you cached.

`SnapshotStart` also says which `footfall_runtime` version produced the
snapshot and the level and capacities it was capturing with. The level tells
you what an empty snapshot means: no events at the `counters` level means
scopes were not being captured, not that none ran.

## Required Consumer Behavior

A conforming consumer should:

- check the protocol version
- accept chunked publication
- build a context table from `Contexts`
- resolve counter and event `context_id`s through that table by id, not by
  position; ids are sparse producer handles that stay fixed for the life of
  the producer's runtime instance
- keep the cursor from each snapshot and pass it to the next request
- reset its caches when the instance id changes
- treat the window's `missed` count and the `SnapshotEnd` totals as real loss
  accounting, not noise

It should not assume:

- a single chunk per payload kind
- a fixed number of sessions on a server
- that all sessions expose the same descriptors or context shapes

## Discovery Contract

The current local attach path is implemented by `footfall_session_wtransport`,
available as `footfall::wtransport` with the `wtransport` feature.
Its concrete WebTransport encoding is documented in
[WebTransport Session Wire Format](session-wire-format.md).

A Rust consumer finds every session on the machine with
`SessionClient::discover_local`, which asks each registered producer at once
and reports the ones that did not answer:

```rust,no_run
# async fn read() -> Result<(), Box<dyn std::error::Error>> {
use footfall::wtransport::SessionClient;
use std::time::Duration;

let discovery = SessionClient::discover_local(Duration::from_secs(1)).await?;
let session = discovery.session("main")?;
let client = session.client();
let snapshot = client.fetch(session.id(), None).await?;
let next = client.fetch(session.id(), Some(snapshot.next_cursor())).await?;
# let _ = next;
# Ok(())
# }
```

Each producer registers the hash of its own certificate with its URL, so a
consumer can reach every local producer even when they created different
identities or one renewed the shared identity while others run.

Session ids are unique per producer, not per machine, so
`LocalDiscovery::session` fails, listing the producers, when several offer
the same id; each announcement carries the producer's process name, process
id, and runtime instance id to choose by. `SessionClient::new(url, hash)`
connects to one known server, and `SessionClient::local()` to whichever
producer holds the default port.

`fetch` returns an `OwnedSnapshot`, reassembled from its envelopes and
checked against the publication contract. Consumers that handle envelopes
themselves, for example from another transport, use
`OwnedSnapshot::from_envelopes`, which rejects a publication that breaks
the contract with an `AssembleError` naming the chunk and the rule.

Discovery returns each server's protocol version and its
`SessionAnnouncement` rows, each carrying:

- `id`
- `process_name`
- `session_name`
- `instance_id` and `process_id` of the producer

## Errors

Every client failure is a `ClientError` variant a consumer can act on:

- `TimedOut`: no complete response within the client's timeout, usually
  because no server is listening; the default is five seconds
- `CertificateRejected`: the client rejected the server's certificate, with a
  `CertificateProblem` saying why: the hash does not match the pin, the
  certificate expired or is not valid yet, or it cannot be pinned
- `SessionRejected`: the server refused the session, usually a wrong path
- `Server`: the server answered with an error code (`not_found` for an
  unknown session id, `unsupported` for a request it does not handle) and a
  message
- `Incompatible`: the server speaks a protocol version the client cannot
  decode, named with both versions
- `Malformed`: the response broke the wire format or publication contract
- `Closed`: the server closed the connection, usually because the producer
  stopped
- `LocalIdentity`: the local certificate could not be read, usually because
  no local producer has run yet
- `InvalidUrl`, `Unreachable`, and `Io` for the remaining transport failures

The local certificate lives at
`SessionIdentity::localhost_certificate_path()`; consumers that cannot call
`load_localhost_certificate_hash`, such as a browser, can hash the DER
certificate in that file with SHA-256 to get the pin.

The server URL itself is the attach point. Discovery is no longer file-backed.

## Local Attach Contract

A producer makes itself attachable with `LocalSessionServer::start`, which
serves its runtimes on the default local port, or on an ephemeral port when
another producer holds the default, and registers its URL and the hash of its
certificate in a per-user registry. Producers share a self-signed localhost
identity kept in Footfall's state directory, but each registers its own hash,
so producers that started together, or one that renewed the identity, stay
reachable. Consumers need no hash or URL prompt:

1. launch one or more producers
2. `SessionClient::discover_local()` asks every registered producer for its
   sessions at once, and reports those that did not answer
3. fetch or subscribe through each session's client

`SessionClient::local_servers()` returns a client per registered producer,
and `SessionClient::local()` one for the default port, for consumers that
discover servers one at a time. A registration can outlive a producer that
crashed; its client times out, and discovery reports it as unreachable.

Set `FOOTFALL_STATE_DIR` to the same directory for producers and consumers
to keep a set of them apart from everything else on the machine.

## Following a Session

`SessionClient::subscribe(session_id, since, interval)` opens a stream on
which the server sends a snapshot now and then one each time
`Subscription::next` asks, at most once per `interval`, each holding the
events recorded since the previous one. The server takes each snapshot only
when it is asked for, so nothing queues for a slow consumer: however long it
takes between calls, the next snapshot is fresh, covers everything since the
last, and counts events evicted in the meantime as `missed`. After the first
snapshot the server sends only what changed, and `Subscription::next`
applies it, so each snapshot it returns is full and current while unchanged
contexts and counters cross the wire once. `Subscription::last_window_bytes` reports what each window cost on the wire.
A producer that stops with `LocalSessionServer::shutdown` sends every
subscriber a final window covering everything up to the stop, after which
`Subscription::next` fails with `ClientError::Closed`; one dropped without
shutting down, or one that crashes or is killed, loses the events since the
last window, and a killed producer shows up only as `ClientError::TimedOut`
once the interval and the client's timeout have passed. Polling with `fetch`
and the previous snapshot's cursor reads the same windows without holding a
stream open.

## Saving and Reloading

`footfall::session::capture` writes snapshots to a capture file and reads
them back as the same `OwnedSnapshot` values a live consumer receives, so
live and offline tools share one model, one set of identities, and one
export path. `CaptureWriter::write_owned` saves snapshots received from a
session, for example each one a subscription delivers, and `CaptureReader`
yields them in order. A program records itself with `CaptureRecorder`,
which writes what its runtime recorded since the last write each time it
is asked, so the file holds every event once.

`CaptureReader::join` reads a whole capture as one snapshot, and
`footfall::analysis::Trace::from_capture` as one trace, pushing each
snapshot as a live consumer would, so events lost between them stay gaps
where they were lost. Both keep what precedes a cut-short end and skip
snapshots from another runtime instance, and say whether they did.

Captures from a release stay readable by later releases at the same
protocol major version. One Footfall cannot decode fails with an
`IncompatibleVersion` inside the `io::Error`, naming the version that wrote
it, so a tool can say which Footfall would read it rather than call the
file damaged; see [capture files](session-wire-format.md#capture-files).

To analyse a session as it runs, push each window into a
`footfall::analysis::Trace`. It pairs scopes and frames that span windows,
skips events it has already seen, records events evicted between reads as
gaps, and refuses a window from another runtime instance, so a restarted
producer starts a new trace. `Trace::forget_before` bounds its memory.

`OwnedSnapshot::append` joins consecutive windows from one runtime instance
into one snapshot, counting events neither window held as missed, and
`footfall::export::to_chrome_trace_json` turns any `OwnedSnapshot` into a
trace for Perfetto. `footfall::session::codec` encodes and decodes single
envelopes for consumers that carry them some other way.

## Building a Tool

`footfall::analysis` does the interpretation most tools need: per-thread
tracks of nested spans for a timeline, frames with their counter changes,
grouped and filtered span tables, typical and worst examples, comparisons
that split a change into more work and slower work, and with every answer a
statement of what it covers. A tool built on it needs little more than
layout; the [analysis guide](analysing-a-trace.md) shows how.

Two consumers show the range:

- the [`attach`](../examples/src/bin/attach.rs) example follows a session and
  prints each window's frames in about a hundred lines;
- the [`footfall` command](../tools/footfall_cli/README.md) records sessions,
  analyzes captures, and has a live terminal view, `footfall top`: frame
  times against a budget, where the time goes, counters per frame, messages,
  and losses, live or from a capture file.

A tool in another language reads the [wire format](session-wire-format.md)
directly. Keep tools outside the instrumented process and talk to it through
the session protocol, so they can change without touching producers.

## Compatibility Rule

If the session contract changes, document it in terms of:

- protocol version impact
- discovery shape changes
- new payload kinds or ordering changes
- consumer behavior that must change
- wire-format limits that affect allocation or chunking

That is the bar for treating an external UI as a first-class consumer rather
than an internal experiment.
