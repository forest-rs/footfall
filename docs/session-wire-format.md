# WebTransport Session Wire Format

This document records the concrete wire format used by
`footfall_session_wtransport`. The semantic contract is still the
transport-neutral envelope model in `footfall_session`; this format is the current
localhost WebTransport encoding of that model.

## Transport

The default local endpoint is:

- URL: `https://127.0.0.1:43091/footfall/session`
- Identity: self-signed localhost certificate managed by the crate; the
  certificate is valid for 14 days and is rotated automatically when it is
  expired or close to expiry, so clients should re-read the pinned hash on
  each connection attempt
- Streams: one bidirectional stream per discovery or snapshot request

All integer fields are little-endian. Strings are encoded as `u32 byte_len`
followed by UTF-8 bytes.

## Requests

Discovery request:

1. magic bytes `FFALLD01`
2. end stream

Snapshot request:

1. magic bytes `FFALLS01`
2. session id string
3. end stream

## Responses

The first response byte is a response tag:

- `0`: discovery response
- `1`: snapshot response
- `2`: error response

Discovery response payload:

1. `u64 session_count`
2. repeated session announcements

Each session announcement is:

1. session id string
2. process name string
3. session name tag, where `0` means absent and `1` means present
4. session name string when present

Snapshot response payload:

1. `u64 envelope_count`
2. repeated envelope frames

Each envelope frame is:

1. `u32 frame_len`
2. exactly `frame_len` encoded envelope bytes

Error response payload:

1. error message string

## Envelope Frame

Each encoded envelope starts with:

1. `u16 protocol_major`
2. `u16 protocol_minor`
3. `u64 session_sequence`
4. `u64 chunk_index`
5. `u8 payload_tag`

Current codec version is `1.0`. The payload tags are:

- `0`: `SnapshotStart`
- `1`: `Contexts`
- `2`: `Counters`
- `3`: `Events`
- `4`: `SnapshotEnd`
- `5`: `Histograms`

Publication order is `SnapshotStart`, `Contexts`, `Counters`, `Histograms`,
`Events`, `SnapshotEnd`; the `Histograms` tag is numerically last only because
it was added after the original five.

A decoder rejects unsupported versions, unknown tags, truncated payloads, and
trailing bytes after a valid payload. Minor versions are additive: a decoder
accepts envelopes with its own major version and an equal or older minor
version, and rejects a different major version or a newer minor version.

## Row Fields Worth Calling Out

Snapshot metadata carries a `u64` wall-clock anchor after the session
sequence: the wall-clock time of runtime creation in microseconds since the
Unix epoch. All snapshot timestamps are relative to runtime creation, so
`anchor + at_micros` yields wall-clock time for any row.

Counter rows carry a `u8` unit tag after the descriptor name:

- `0`: bytes
- `1`: count
- `2`: nanos
- `3`: micros
- `4`: ratio
- `5`: items

Event rows carry a `u64` runtime-assigned thread id after the timestamp. Ids
are small integers assigned in first-emission order within one process
lifetime; they are not OS thread ids.

Scope-begin rows end with a per-instance argument list: a `u8` count bounded
by `footfall_core::ARGS_INLINE_CAPACITY`, followed by that many entries of key
string plus tagged label value (the same value encoding used by context
labels).

Histogram rows carry descriptor id, name string, unit tag, and context id,
followed by `count`, `sum`, `min`, `max`, all
`footfall_runtime::HISTOGRAM_BUCKET_COUNT` bucket values, and the last update
time, each as `u64`.

Event rows use a `u8` observation tag: `0` scope begin, `1` scope end, `2`
resource create, `3` resource update, `4` resource drop, `5` frame marker,
`6` flow, `7` message. Message rows carry descriptor id, name string, context
id, text string (truncated by producers to
`footfall_runtime::MESSAGE_TEXT_MAX_BYTES`), and a per-instance argument list.

## Payload Limits

The codec enforces these bounds before allocating dynamic payload data:

- string payload: 64 KiB
- encoded envelope frame: 16 MiB
- envelopes per snapshot response: 1,000,000
- session announcements per discovery response: 4,096
- rows per context, counter, histogram, or event envelope: 65,536
- labels per context row: `footfall_core::CONTEXT_INLINE_CAPACITY`

These are protocol limits, not tuning recommendations. Producers should still
chunk snapshots to keep latency and consumer memory bounded.

## Compatibility Rule

Changes that add payload tags, reorder snapshot publication, change field
encoding, or change the meaning of existing tags require a protocol version
bump and a migration note. Consumers must already handle chunked publication and
dropped-data summaries, so changing chunk sizes does not require a version bump.
