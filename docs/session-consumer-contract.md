# Session Consumer Contract

This document defines the minimum contract an external consumer or UI needs in
order to attach to Spoor.

## Scope

A consumer does not participate in hot-path emission.

Its job is to:

- discover one or more active sessions
- fetch snapshot data
- interpret the versioned session envelope model
- render or query that data

## Canonical Data Model

The transport-neutral contract lives in `spoor_session`.

The important types are:

- `Envelope<'a>`
- `Payload<'a>`
- `OwnedSessionEnvelope`
- `OwnedSessionPayload`

Snapshot publication has a fixed shape:

1. `SnapshotStart`
2. zero or more `Contexts` chunks
3. zero or more `Counters` chunks
4. zero or more `Events` chunks
5. `SnapshotEnd`

That ordering is the contract a consumer should rely on.

## Required Consumer Behavior

A conforming consumer should:

- check the protocol version
- accept chunked publication
- build a context table from `Contexts`
- resolve counter and event `context_id`s through that table
- treat `SnapshotEnd` dropped counts as real loss accounting, not noise

It should not assume:

- a single chunk per payload kind
- a fixed number of sessions on a server
- that all sessions expose the same descriptors or context shapes

## Discovery Contract

The current local attach path is implemented by `spoor_session_wtransport`.

The consumer-facing contract is:

- connect to the local session server
- request discovery
- receive zero or more `SessionAnnouncement` rows
- choose one `session_id`
- request a snapshot for that `session_id`

`SessionAnnouncement` currently carries:

- `id`
- `process_name`
- `session_name`

The server URL itself is the attach point. Discovery is no longer file-backed.

## Local Attach Contract

The current default local attach model is:

- fixed local server URL
- stable local identity managed internally
- no hash or URL prompt in the normal consumer UX

So an external local consumer should aim to feel like:

1. launch producer
2. launch consumer
3. discover sessions
4. attach

That is the right default user experience.

## What a UI Actually Needs

A useful first UI does not need every Spoor detail.

It needs:

- session list
- process/session naming
- contexts by id
- counters by descriptor and context
- resource lifecycle events
- dropped-data totals

That is enough to build:

- cache summaries
- resource summaries
- basic timeline inspection
- semantic grouping by context owner

## Recommended Separation

Keep the UI outside this repo if it helps focus:

- Spoor repo owns producers, collection, transport, and examples
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

That is the bar for treating an external UI as a first-class consumer rather
than an internal experiment.
