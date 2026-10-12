# WebTransport Session Wire Format

This document is the byte-level specification of the format used by
`footfall_session_wtransport`, complete enough to write a client in another
language. The envelope encoding is implemented by `footfall_session::codec`
and also used by capture files (`footfall_session::capture`), which store a
`FFALLCAP` header, the `u16` protocol major and minor, and then
length-prefixed envelopes exactly as a snapshot response carries them. The semantic model is the transport-neutral envelope model in
`footfall_session`; see the [consumer contract](session-consumer-contract.md)
for what the values mean and how to use them.

## Transport

The default local endpoint is:

- URL: `https://127.0.0.1:43091/footfall/session`
- Identity: self-signed localhost certificate managed by the crate; the
  certificate is valid for 14 days and is rotated automatically when it is
  expired or close to expiry, so clients should re-read the pinned hash on
  each connection attempt
- Streams: one bidirectional stream per discovery, snapshot, or
  subscription request
- A session request on any path other than the server's session path is
  refused with HTTP status 404

## Primitive Encodings

- `u8` is one byte. `u16` is two bytes, little-endian; only the protocol
  version uses it, so the first four bytes of every envelope identify its
  version whatever else changes.
- `u32` and `u64` are unsigned LEB128: seven bits per byte, least
  significant group first, with the high bit set on every byte but the last.
  A value takes one byte below 128 and at most ten bytes. Decoders reject
  encodings longer than ten bytes and values that do not fit the type.
- `i64` is zigzag-encoded (`(n << 1) ^ (n >> 63)`), then written as a `u64`,
  so small values of either sign take few bytes.
- `delta` is an `i64` difference from the same field of the previous row in
  the payload, or from `0` for the first row.
- `string` is a `u64` byte length followed by that many bytes of UTF-8.
- `name` is a descriptor's name `string`, written only the first time its
  descriptor id appears in the payload. Later rows of the same payload with
  that id omit it, and a decoder reuses the name it read first. Each payload
  starts afresh, so no state crosses envelopes.
- `option<T>` is a `u8` presence tag, `0` for absent or `1` for present,
  followed by a `T` when present.
- `count` is a `u64` element count followed by that many elements.

## Requests

Discovery request:

| Field | Type | Notes |
|---|---|---|
| magic | 8 bytes | `FFALLD01` |

Snapshot request:

| Field | Type | Notes |
|---|---|---|
| magic | 8 bytes | `FFALLS01` |
| session id | `string` | from a discovery announcement |
| since | `option<cursor>` | absent for everything retained |

Subscription request:

| Field | Type | Notes |
|---|---|---|
| magic | 8 bytes | `FFALLU01` |
| session id | `string` | from a discovery announcement |
| since | `option<cursor>` | where the first snapshot's events start |
| interval | `u32` | milliseconds between snapshots; servers wait at least 10 |

The client keeps its side of the stream open. The server answers with a
snapshot response right away, then waits for the client to write one byte
(any value) before each later one, and also until `interval` has passed
since the previous one; it takes each snapshot only then. Each starts at the
previous one's next cursor, and every one after the first is a changes
snapshot. The sequence ends when the client closes the stream or the
connection, or with an error response, for example for an unknown session
id.

A `cursor` is `u64 instance_id` then `u64 sequence`: the instance id and next
sequence from a previous snapshot's `SnapshotStart`. The client finishes its
send side after the request.

A server that receives any other magic answers with an error response whose
code is `unsupported`, so a client speaking a newer request framing learns
that rather than seeing the stream close.

## Responses

The first response byte is a response tag:

- `0`: discovery response
- `1`: snapshot response
- `2`: error response

Discovery response payload:

1. `u16` protocol major and `u16` protocol minor: the version the server
   speaks, which a client checks before reading further
2. `u64` session count
3. that many session announcements

Each session announcement is:

1. session id `string`
2. process name `string`
3. session name `option<string>`
4. `u64` instance id of the serving runtime, or `0`
5. `u32` process id of the producer, or `0`

Snapshot response payload:

1. `u64` envelope count
2. that many envelope frames, each a `u32` frame length followed by exactly
   that many bytes of encoded envelope

Error response payload:

1. `u8` error code
2. error message `string`

| Code | Name | Meaning |
|---|---|---|
| `0` | `invalid_request` | the request was malformed |
| `1` | `not_found` | no session has the requested id |
| `2` | `unsupported` | the server does not handle this request |
| `3` | `failed` | the request failed after it was accepted |
| `4` | `incompatible` | the peers' protocol versions cannot interoperate |

Clients treat an unknown code as `failed`.

## Envelopes

Each encoded envelope is a header, a `u8` payload tag, and the payload:

| Field | Type |
|---|---|
| protocol major | `u16` |
| protocol minor | `u16` |
| chunk index | `u64` |
| payload tag | `u8` |
| payload | per tag |

The current protocol version is `1.0`. Payload tags, in publication order:

| Tag | Payload | Occurs |
|---|---|---|
| `0` | `SnapshotStart` | once, first |
| `1` | `Threads` | at most once |
| `2` | `Contexts` | zero or more chunks |
| `3` | `RetiredContexts` | zero or more chunks, only in a changes snapshot |
| `4` | `Counters` | zero or more chunks |
| `5` | `Histograms` | zero or more chunks |
| `6` | `Events` | zero or more chunks |
| `7` | `SnapshotEnd` | once, last |

Chunk indexes count from `0` at `SnapshotStart` and increase by one per
envelope.

A decoder rejects unsupported versions, unknown tags, truncated payloads, and
trailing bytes after a valid payload. Minor versions are additive: a decoder
accepts envelopes with its own major version and an equal or older minor
version, and rejects a different major version or a newer minor version.

### SnapshotStart

| Field | Type | Notes |
|---|---|---|
| process name | `string` | |
| session name | `option<string>` | |
| instance id | `u64` | random per runtime; see below |
| process id | `u32` | the producer's operating-system process id, or `0` |
| wall-clock anchor | `u64` | nanoseconds since the Unix epoch at runtime creation |
| producer version | `string` | version of `footfall_runtime` |
| level | `u8` | `0` off, `1` counters, `2` scoped, `3` trace |
| counter capacity | `u64` | |
| event capacity | `u64` | |
| context capacity | `u64` | |
| capture flags | `u8` | bit 0: counter samples recorded; bit 1: instrumentation compiled out; other bits zero |
| kind | `u8` | `0` full, `1` changes; see below |
| captured at | `u64` | nanoseconds since runtime creation |
| window since | `u64` | first event sequence covered |
| window next | `u64` | sequence the next read starts from |
| window missed | `u64` | events in the window evicted before the read |
| thread rows | `u32` | rows in `Threads` |
| context rows | `u32` | rows across `Contexts` chunks |
| retired context ids | `u32` | ids across `RetiredContexts` chunks |
| counter rows | `u32` | rows across `Counters` chunks |
| histogram rows | `u32` | rows across `Histograms` chunks |
| event rows | `u32` | rows across `Events` chunks |

### Threads

A `count` of thread rows, each `u64` thread id followed by the thread's
operating-system name as `option<string>`.

### Contexts

A `count` of context rows. Each row is a `u64` context id, a `u8` released
flag (`1` when the producer has released the context and keeps it only while
events recorded under it remain), and a `count` of labels, at most `footfall_core::CONTEXT_INLINE_CAPACITY`; each label is a key
name `string` followed by a label value.

A label value is a `u8` tag and its payload:

| Tag | Kind | Payload |
|---|---|---|
| `0` | unsigned | `u64` |
| `1` | signed | `i64` |
| `2` | bool | `u8`, `0` false or `1` true |
| `3` | string | `string` |

### RetiredContexts

A `count` of `u64` context ids.

### Full and Changes Snapshots

A full snapshot carries every context, thread, counter, and histogram the
producer holds. A subscription sends a full snapshot first and then, for
each later window, a changes snapshot: only the contexts, threads, counters,
and histograms that are new or changed since the previous snapshot on that
subscription, plus the ids of contexts retired since, whose counters and
histograms are gone too. Events are always the window's events. A consumer
applies changes snapshots in order to the state built from the full one;
`OwnedSnapshot::apply` does this, and the Rust `Subscription` hands out the
resulting full snapshots. Snapshot responses to fetch requests, and capture
files, are always full.

### Counters

A `count` of counter rows:

| Field | Type | Notes |
|---|---|---|
| descriptor id | `u64` | |
| name | `name` | |
| unit | `u8` | see below |
| context id | `u64` | |
| kind | `u8` | `0` counter (cumulative sum), `1` gauge (last value) |
| value | `u64` | |
| updated at | `u64` | nanoseconds since runtime creation |

Units:

| Tag | Unit |
|---|---|
| `0` | bytes |
| `1` | count |
| `2` | nanoseconds |
| `3` | microseconds |
| `4` | ratio, in parts per million (`1_000_000` is `1.0`) |
| `5` | items |

### Histograms

A `count` of histogram rows: descriptor id `u64`, name `name`, unit `u8`,
context id `u64`, then `count`, `sum`, `min`, and `max` as `u64`, then
`footfall_runtime::HISTOGRAM_BUCKET_COUNT` (64) bucket counts as `u64`, then
the last update time as `u64`. Bucket `i` counts samples whose bit length is
`i`; `min` is meaningless while `count` is zero.

### Events

A `count` of event rows. Each row is the timestamp (nanoseconds since
runtime creation) as a `delta`, the sequence number as a `delta`, the `u64`
thread id, then a `u8` observation tag and its fields:

| Tag | Observation | Fields |
|---|---|---|
| `0` | scope begin | descriptor id `u64`, name `name`, context id `u64`, token `u64`, args |
| `1` | scope end | token `u64` |
| `2` | resource create | kind id `u64`, kind name `name`, resource id `u64`, context id `u64`, size `u64` |
| `3` | resource update | kind id `u64`, kind name `name`, resource id `u64`, context id `u64`, size `u64` |
| `4` | resource drop | kind id `u64`, kind name `name`, resource id `u64`, context id `u64` |
| `5` | marker | kind `u8` (`0` frame, `1` transaction, `2` pass), phase `u8` (`0` begin, `1` end), context id `u64` |
| `6` | flow | flow id `u64`, phase `u8` (`0` begin, `1` step, `2` end), context id `u64` |
| `7` | message | descriptor id `u64`, name `name`, context id `u64`, text `string`, args |
| `8` | counter sample | descriptor id `u64`, name `name`, unit `u8`, context id `u64`, kind `u8`, value `u64` |

A counter sample is a counter's or gauge's value at a frame end, recorded
for each one the frame updated; a counter's change over a frame is the
difference from its previous sample.

`args` is a `u8` entry count, at most `footfall_core::ARGS_INLINE_CAPACITY`,
followed by that many entries of key `string` and label value. Message text is
truncated by producers to `footfall_runtime::MESSAGE_TEXT_MAX_BYTES`.

### SnapshotEnd

| Field | Type | Notes |
|---|---|---|
| refused contexts | `u64` | registrations refused, since runtime creation |
| unattributed | `u64` | rows and events in this snapshot attributed to the root context |
| updated after release | `u64` | counter and histogram rows in this snapshot updated after their context was released |
| counter updates | `u64` | updates dropped for lack of a counter key, since runtime creation |
| evicted events | `u64` | events evicted to make room, since runtime creation |
| retired rows | `u64` | counter and histogram rows removed with retired contexts, since runtime creation |

## Identities and Ordering

The instance id identifies one runtime from creation to exit. Context ids,
descriptor ids, thread ids, and event sequence numbers are only meaningful
within one instance; a consumer that sees a new instance id must drop what it
cached from the old one.

Context id `0` is the empty root context. Other context ids are the
producer's registered context handles: opaque, sparse, and stable while the
producer keeps the context. Resolve them through the `Contexts` rows of the
same snapshot, never by position. Counter, histogram, and event rows only
reference ids present in those rows; rows whose handle the producer could not
resolve arrive attributed to `0` and are counted as unattributed.

Descriptor ids are dense integers from `1`, shared across descriptor kinds and
assigned in first-seen order, fixed for the life of the instance. They are
never memory addresses. Each payload that refers to a descriptor carries its
name once, next to the first row with its id.

Thread ids are small integers assigned in first-emission order within one
process lifetime; they are not operating-system thread ids.

Event sequence numbers count every event the instance recorded, from `0`. A
snapshot's events are the retained ones numbered from window since up to
window next; window missed counts the rest of that range, which were evicted
before the read. Event rows are published by timestamp, with ties broken by
sequence, so events recorded by one thread keep their program order even when
they share a timestamp. Keep row order across `Events` chunks rather than
re-sorting by timestamp and observation kind.

All timestamps are nanoseconds on a monotonic clock that starts at runtime
creation; adding the wall-clock anchor yields wall-clock time.

## Payload Limits

The codec enforces these bounds before allocating dynamic payload data:

- string payload: 64 KiB
- encoded envelope frame: 16 MiB
- envelopes per snapshot response: 1,000,000
- session announcements per discovery response: 4,096
- rows per thread, context, counter, histogram, or event envelope: 65,536
- labels per context row: `footfall_core::CONTEXT_INLINE_CAPACITY`

These are protocol limits, not tuning recommendations. Producers should still
chunk snapshots to keep latency and consumer memory bounded.

## Compatibility Rule

The protocol stays at `1.0` until Footfall is first published, so changes
before then do not bump it. After that, changes that add payload tags, reorder
snapshot publication, change field encoding, or change the meaning of existing
tags require a protocol version bump. Consumers must already handle chunked
publication, so changing chunk sizes does not require a version bump.
