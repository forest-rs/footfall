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

The transport-neutral contract lives in `footfall_session`.

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

The current local attach path is implemented by `footfall_session_wtransport`.
Its concrete WebTransport encoding is documented in
[WebTransport Session Wire Format](session-wire-format.md).

A Rust consumer uses `SessionClient`:

```rust,no_run
# async fn read() -> Result<(), footfall_session_wtransport::ClientError> {
use footfall_session_wtransport::SessionClient;

let client = SessionClient::local().await?;
let discovery = client.discover().await?;
let session = &discovery.sessions()[0];
let snapshot = client.fetch(session.id(), None).await?;
let next = client.fetch(session.id(), Some(snapshot.next_cursor())).await?;
# let _ = next;
# Ok(())
# }
```

`fetch` returns an `OwnedSnapshot`, reassembled from its envelopes and
checked against the publication contract. Consumers that handle envelopes
themselves, for example from another transport, use
`OwnedSnapshot::from_envelopes`, which rejects a publication that breaks
the contract with an `AssembleError` naming the chunk and the rule.

Discovery returns the server's protocol version and its
`SessionAnnouncement` rows, each carrying:

- `id`
- `process_name`
- `session_name`

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
another producer holds the default, and registers its URL in a local
registry. Every local producer shares one self-signed identity, managed in
Footfall's state directory, so consumers need no hash or URL prompt:

1. launch one or more producers
2. `SessionClient::local_servers()` returns a client for each, default port
   first; `SessionClient::local()` connects to the default port alone
3. discover each server's sessions
4. fetch or subscribe

A registration can outlive a producer that crashed; its client times out,
and consumers skip it.

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
contexts and counters cross the wire once. When the producer stops,
`Subscription::next` fails with `ClientError::Closed`. Polling with `fetch`
and the previous snapshot's cursor reads the same windows without holding a
stream open.

## Saving and Reloading

`footfall_session::capture` writes snapshots to a capture file and reads
them back as the same `OwnedSnapshot` values a live consumer receives, so
live and offline tools share one model, one set of identities, and one
export path. `CaptureWriter::write_owned` saves snapshots received from a
session, for example each one a subscription delivers, and `CaptureReader`
yields them in order.

`OwnedSnapshot::append` joins consecutive windows from one runtime instance
into one snapshot, counting events neither window held as missed, and
`footfall_export::to_chrome_trace_json` turns any `OwnedSnapshot` into a
trace for Perfetto. `footfall_session::codec` encodes and decodes single
envelopes for consumers that carry them some other way.

## What a UI Actually Needs

A useful first UI does not need every Footfall detail.

It needs:

- session list
- process/session naming
- contexts by id
- counters by descriptor and context
- resource lifecycle events
- missed-event and drop totals

That is enough to build:

- cache summaries
- resource summaries
- basic timeline inspection
- semantic grouping by context owner

## Recommended Separation

Snapshot publication and live tool sessions are related but distinct.

`footfall_session` owns a small transport-neutral live-session vocabulary:

- request ids
- stream ids
- domain-qualified method names
- generic request/response payload containers
- generic ordered stream item containers
- generic error/status containers

Domain payloads stay outside Footfall. A UI toolkit, profiler, renderer, or asset
pipeline can put typed payloads in those containers for same-process use, or an
encoded byte payload for a network transport. This keeps Footfall responsible for
correlation and session shape without turning it into an RPC framework or
making it learn product-specific concepts such as element ids, layer ids, or
resource inspector sections.

Keep the UI outside this repo if it helps focus:

- Footfall repo owns producers, collection, transport, and examples
- UI repo owns views, interaction, layout, and consumer-side caching

The interface between them should stay:

- versioned session envelopes
- discovery plus snapshot requests over the local session server

## Compatibility Rule

If the session contract changes, document it in terms of:

- protocol version impact
- discovery shape changes
- new payload kinds or ordering changes
- consumer behavior that must change
- wire-format limits that affect allocation or chunking

That is the bar for treating an external UI as a first-class consumer rather
than an internal experiment.
