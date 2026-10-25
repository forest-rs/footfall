# Following one edit through a capture

An edit fails. The application still has readable geometry, and removing the
graph does not release every buffer. We want to find the failed execution,
understand which work ran, and explain the retained storage from recorded facts.

Footfall supplies the observations and their connections. The application supplies
what a publication, cutoff, graph root, or undo root means. This walkthrough follows
one investigation using the standard CLI, then shows the same path on a loaded
Rust trace. No application-specific report generator is needed.

## A capture we can all reproduce

From this checkout, build the CLI and write the small contract fixture:

```sh
cargo build -p footfall_cli --release --locked
mkdir -p .local
cargo run -p footfall_examples --bin edit_inspection -- .local/edits.ffcap 2
```

The destination must be new. In the commands below, `footfall` means the built
`./target/release/footfall` binary; use that path if it is not on your shell's PATH.
`jq` only shortens JSON for display; work selection and traversal happen in Footfall.

The fixture has seven edits: initial construction, two amplitude changes, a
quality change that rebuilds topology, a rejected change, graph retirement, and
undo release. Times and event sequences are deterministic. It is a synthetic
contract fixture reduced from execution's procedural example, not a native graph
replay or a VM performance measurement. A real capture has its own identities,
sizes, and application messages. The investigation steps are the same.

Start by asking what edits exist:

```sh
footfall analyze summary .local/edits.ffcap --scope procedural.edit --json \
  | jq '.selection.units[] | {id, scope_arguments}'
```

Each unit has a Footfall scope identity and the application's `edit` and `kind`
arguments. Those are different identities: the rejected application's edit is **5**,
while its Footfall scope begins at event sequence **96**. Scope IDs, message
sequences, flow IDs, checkpoint sequences, and resource lifetime handles belong
to one runtime instance. Keep the capture's `instance` with saved query results.

## Find the rejected edit, then narrow the question

We know the example calls this kind of edit `rejected`. Select it directly:

```sh
footfall analyze work .local/edits.ffcap \
  --scope procedural.edit --arg kind=rejected --json \
  | jq '{id, membership, open, counts, scope_ids, message_sequences, flow_ids, checkpoint_sequences}'
```

The survey returns:

```json
{
  "id": 96,
  "membership": "captured_ancestry",
  "open": false,
  "counts": {"scopes": 3, "open_scopes": 0, "messages": 5, "flows": 1, "checkpoints": 2, "resources": 0},
  "scope_ids": [96, 101, 105],
  "message_sequences": [102, 104, 107, 109, 110],
  "flow_ids": [5],
  "checkpoint_sequences": [99, 113]
}
```

This is a map for the next questions. `--detail` returns the connected scopes,
messages, full retained flows, checkpoints, and resource observations together.
For a large investigation, use identities from the survey to request just the
observations you need. If the selector matches several edits, Footfall reports
the count and fails; narrow with another `--arg`, context, or label.

`captured_ancestry` matters. These observations belong to the edit through the
recorded scope stack. Another thread running at the same time does not become
its child. Selecting a transaction marker instead uses temporal membership:

```sh
footfall analyze work .local/edits.ffcap --kind transaction --json
```

This intentionally fails because seven transactions match. The marker survey
supplies an identity for each transaction; rejected transaction **95** can be
inspected directly, without adding a distinct producer context:

```sh
footfall analyze summary .local/edits.ffcap --kind transaction --json
footfall analyze work .local/edits.ffcap --kind transaction --id 95 --detail --json
```

The ID is constrained by the selected kind and any context/label predicates.
Errors distinguish an absent completed identity from an existing identity that
does not match those selectors. Marker inspection reports `interval_activity`:
scopes overlapping the marker interval, including scopes already running at its
start, and point observations inside its half-open range. This establishes no
ancestry or causality. A scope merely touching an endpoint is excluded; an
instantaneous scope belongs at the start, and is excluded at the end.

## Which execution failed?

Look for the node's body outcome inside the selected scope:

```sh
footfall analyze messages .local/edits.ffcap --within 96 \
  --name procedural.body_outcome --arg node=u:2 --json \
  | jq '.messages[] | {sequence, text, arguments, span}'
```

Message **107** says `failed`, with unsigned arguments `node=2` and `attempt=5`.
Its enclosing scope is **105**. `u:2` forces an unsigned integer; `s:2` is the
string `"2"` and selects a different value. JSON argument and label objects include
`value_type`, so an unsigned `2` remains distinguishable from a signed `2`. Message arguments are not scope
arguments: `work --arg` filters the work scope, while `messages --arg` filters
messages. The fixture's body messages carry node/attempt, and their enclosing
edit supplies the edit identity.

Follow the scope and message independently:

```sh
footfall analyze span .local/edits.ffcap 105 --json
footfall analyze messages .local/edits.ffcap --sequence 107 --json
```

The scope is `procedural.deform`, with parent **96** and the same node/attempt
arguments. In the real execution consumer, those application identities lead to
source and VM recordings through its manifest. Footfall does not invent that
mapping or infer a VM failure from a long scope duration.

## What happened around the failure?

Ask for publication and query-failure notifications in this edit:

```sh
footfall analyze messages .local/edits.ffcap --within 96 \
  --name procedural.published --json
footfall analyze messages .local/edits.ffcap --within 96 \
  --name procedural.failed --json
```

The publication notification is message **109**, naming node 0 (`parameters`).
The query-failure notification is **110**. The body failure was **107**. We have
an observed notification order: body failure, publication notification, query
failure notification.

That answers which notification preceded the failure report. It cannot establish
native publication times relative to the actual query return: the procedural
consumer emits these report notifications after the query returns. To answer a
stronger timing question, the producer must instrument the native operation.
Footfall preserves recorded order without converting notification order into
an application execution guarantee.

Was topology construction attempted? Ask for its recorded cutoff:

```sh
footfall analyze messages .local/edits.ffcap --within 96 \
  --name procedural.cutoff --arg node=u:1 --json
footfall analyze work .local/edits.ffcap \
  --scope procedural.edit --arg kind=rejected --detail --json \
  | jq '.scopes[] | {id, name}'
```

Message **104** identifies `grid` as cut off. The connected scope list contains
`procedural.edit`, `procedural.tape`, and `procedural.deform`; there is no grid
construction scope. Together, with complete recording evidence, these support
the application's explanation that grid construction was cut off. Absence alone
would be weaker, particularly if recording was disabled or events were lost.

The flow gives another route through the work:

```sh
footfall analyze flows .local/edits.ffcap --within 105 --json
footfall analyze flows .local/edits.ffcap --id 5 --json
```

Selecting inside the failed body returns flow **5** with its begin, body step,
and end. Only the body step is marked `matched` in the first query. Endpoints
outside the selected scope stay visible, so the selection does not erase the
connection we followed. `endpoints_present` says both endpoints were retained;
recording evidence separately says whether intermediate events might be missing.

## Why is storage still alive after graph removal?

Find the initial and retirement edits:

```sh
footfall analyze work .local/edits.ffcap --scope procedural.edit --arg kind=initial --json
footfall analyze work .local/edits.ffcap --scope procedural.edit --arg kind=retirement --json
```

The initial scope is **1**. Retirement ends at **1300 ns**, relative to the
fixture's runtime. Follow resources created inside that initial scope and still
observed alive after retirement:

```sh
footfall analyze resources .local/edits.ffcap --created-in 1 --alive-at-ns 1300 --json \
  | jq '.resources[] | {lifetime, kind, resource_id, size_at_query_bytes}'
```

Two remain: topology lifetime **9**, 32 bytes, and points lifetime **13**, 64 bytes.
They total **96 application-reported bytes**. Drill into one lifetime:

```sh
footfall analyze resources .local/edits.ffcap --lifetime 13 --json
```

Its creation scope leads back to the initial edit; its eventual drop leads to
undo release. A lifetime handle uses the first retained observation sequence.
It differs from the logical resource ID, which the application can reuse after
a drop. Save lifetime handles, not positions in a JSON array.

We have established retained creation provenance and observed survival.
Calling the owner an undo stack requires the application's ownership observations
or knowledge of this fixture. Resource events alone cannot distinguish an undo
root from a cache, leak, or another reference. In the real procedural capture,
`procedural.root` messages provide the application's ownership observations.

## Compare amplitude and topology edits in bytes

The first amplitude edit covers `[240,430)` ns; the quality edit covers
`[690,930)` ns. Ask the accounting engine directly:

```sh
footfall analyze resource-interval .local/edits.ffcap \
  --start-ns 240 --end-ns 430 --group kind --json
footfall analyze resource-interval .local/edits.ffcap \
  --start-ns 690 --end-ns 930 --group kind --json
```

| Observation | First amplitude edit | Quality/topology edit |
| --- | ---: | ---: |
| Created bytes | 64 | 240 |
| Released bytes | 0 | 128 |
| Created topology bytes | 0 | 48 |
| Ending live bytes | 224 | 336 |

The amplitude edit creates a deformation buffer and reuses topology. The first
old deformation remains retained for undo, explaining its zero release count.
The quality edit creates points and topology and releases obsolete graph storage.
These are different workloads, not a speedup comparison.

Each number carries `exact`; retain that flag when copying results. Activity is
half-open: an event at `end` belongs to the next interval. Baseline and ending
states are taken **before** events at their respective timestamps, with equal-time
events ordered by sequence inside the interval. Drop bytes use the last supported
size before the drop. Resizes contribute separate `grown` and `shrunk` measures.

Accounting describes global activity during the interval. With overlapping edits,
it cannot assign every byte exclusively to one edit. `--group creation-root` and
`--group creation-scope` describe where lifetimes originated; an explicit unknown
origin group preserves missing or unscoped creation evidence. Groups do not infer
current ownership, and group peaks cannot be added to obtain a global peak.

## Check sampled boundaries without inventing a baseline

The fixture also has named before/after-edit checkpoints. Inspect their values
or compare two identities from the work survey:

```sh
footfall analyze checkpoints .local/edits.ffcap --from 99 --to 113 --json
```

That rejected edit changes neither the created-byte counter nor the live-byte
gauge. Compare the initial boundaries, **2** and **19**, and the distinction is
visible: the first boundary has no initial live-gauge sample. The missing gauge
baseline remains unknown. Resource accounting can independently sum the retained
creation events; it cannot manufacture an earlier counter checkpoint.

Old captures without named checkpoints still support messages, flows, and
resource inspection. Their application `storage` messages remain messages, not
Footfall checkpoint batches. Add named checkpoints and counter/gauge updates at
the producer when sampled boundary comparisons are needed.

## Keep the evidence with the answer

Each query has two distinct evidence layers:

- `evidence` describes whether the signal was enabled, coverage of the selected
  range, gaps, open work, retained event history, and producer-attested recording
  enablement. `recorded` describes the latest snapshot; `recording_history`
  describes the historical minimum. An Off→Trace transition cannot make earlier
  unrecorded work a complete empty answer. Protocol 1.0 captures retain their
  facts, with `history_unknown` because they did not attest historical settings.
- `capture_read` describes truncation and skipped foreign snapshots, alongside
  capture/runtime diagnostics. A usable prefix does not establish a complete file.

An empty complete result can support absence. An empty incomplete result cannot.
A gap before an interval can invalidate its baseline; forgetting that gap later
does not restore certainty. Unknown release sizes are excluded from observed
sums, rather than substituted with zero-sized resources. Incomplete survivors are
candidates, not proof of uninterrupted retention. An open scope can be inspected
by ID, but its end and full outcome remain unknown.

For longer result sets, request `--limit 20`, then pass the returned `next` as
`--after` with the same predicates. Message pages use sequence order; flow pages
use flow-ID order. Counts can change when a live trace receives new snapshots.
Pagination limits the answer size; current queries still scan retained histories.

## Run the same investigation without reloading the file

A CLI process loads its capture for each command. A persistent consumer can load
once and use the same operations repeatedly:

```rust
use footfall::analysis::{EventFilter, MessageQuery, Page, ResourceGrouping, Trace, WorkId};
use footfall::session::capture::CaptureReader;

fn investigate(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let captured = Trace::from_capture(CaptureReader::new(std::fs::File::open(path)?)?)?;
    if !captured.complete() { return Err("investigation needs a complete file read".into()); }
    let trace = captured.trace();
    let edits = trace.spans().named("procedural.edit").with_arg("kind", "rejected").iter();
    if edits.len() != 1 { return Err("expected exactly one rejected edit".into()); }
    let work = trace.inspect_work(WorkId::Scope(edits[0].span().id()))?;
    let outcomes = trace.select_messages(&MessageQuery {
        filter: EventFilter { within: Some(edits[0].span().id()), ..Default::default() },
        name: Some("procedural.body_outcome".into()),
        args: vec![("node".into(), 2_u64.into())],
        ..Default::default()
    }, Page::default())?;
    let storage = trace.resource_interval(work.range, Some(ResourceGrouping::Kind));
    println!("{:?} {:?}", outcomes.items, storage.activity);
    Ok(())
}
```

A live consumer can push later snapshots into its mutable `Trace` and query it
again. Results borrow that trace; finish using them before pushing or forgetting
history. Parsing, serialization, and transport belong to the host. This change
provides no persistent server, MCP adapter, or query language.

## What the real procedural capture showed

An isolated copy of execution commit
`41d577c691b64be4424e449060eba2de0a948ccc`, with eight amplitude edits, local
Footfall, and added named checkpoints, answered the same questions:

- Rejected edit **11**, scope **387**: node **2**, attempt **11**, reported body
  failure at message **402**. Node 0's publication notification **405** preceded
  query-failure notification **407**. These remain notification-order facts.
- Grid/node **1** had cutoff message **406** and no construction scope in the
  rejected edit's captured descendants.
- Initial topology lifetime **14** retained **24,576 bytes**, and initial points
  lifetime **19** retained **26,136 bytes**, after graph retirement: **50,712 bytes**
  in total. Application root messages supplied the ownership interpretation.
- First amplitude edit: **26,136 created bytes**, no topology creation. Quality
  edit: **86,544 created**, **52,272 released**, **163,392 peak live bytes**.
- Fourteen edits supplied **28 complete named checkpoint batches**. An absent
  initial live-gauge sample remained unknown. The file retained all **489 events**
  despite **233 runtime evictions** after streaming; those evictions were not loss.

Those real identities illustrate that supported queries answer the consumer's
questions. They are not reusable handles for the synthetic capture above.
[Measured analysis costs](../benchmarks/substrate/INSPECTION.md) separate loaded
queries, repeated CLI loading, and process RSS at several capture lengths.
The [analysis guide](analysing-a-trace.md#connected-work-inspection) defines the
API and evidence contracts in more detail.
