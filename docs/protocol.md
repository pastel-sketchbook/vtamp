# Local protocol, version 2

The CLI is the recommended automation interface. These details are for contributors building another local client.

## Transport

Connect to the per-user Unix socket printed by `vtamp doctor --json`. Send a four-byte unsigned **big-endian** byte count, followed by that many bytes of UTF-8 JSON. The limit is 16 MiB in either direction. A normal connection handles one request and one reply, then closes. Request reads and reply writes have deadlines; an idle or slow client cannot block playback.

```json
{"version":2,"request":{"command":"pause"}}
```

The `Command`, `Request`, `Reply`, `State`, and `Event` types in `src/model.rs` are the source of truth for field names. Commands are internally tagged with `command` in snake_case. Paths supplied by clients must be absolute; the CLI resolves relative paths before sending them. The server's working directory is not the invoking shell's directory.

```json
{"version":2,"ok":false,"error":{"code":"version_mismatch","message":"Client and server protocol versions differ; restart the server with this binary"}}
```

A version mismatch is rejected before dispatch. There is no TCP listener and no network discovery. Socket permissions restrict clients to the same OS user.

## Ownership and ordering

The server has one state/SQLite owner and serializes received commands. Independent connections are ordered by arrival, not by wall-clock client invocation. Requests are not automatically retried. Only keyed `queue_edit` requests are deduplicated, as described below. For other non-idempotent operations, a disconnect after sending a command has an unknown result; check the state before retrying.

`pause` and `resume` are idempotent. Queue entries have independent UUIDs even if their track IDs are equal. State revisions increase after mutations. Direct path imports complete asynchronously; the success reply means the imported entries have been appended. A library scan reply instead acknowledges the background job; wait for `library_changed` or `scanning: false` for its result.

`play` with a library `track` ID reuses the current queue entry if its track matches, otherwise the first matching entry in queue order. It appends only when no match exists. The server resolves this against its current state, so repeated requests do not create duplicates. `queue_add` always appends and permits duplicates; existing duplicates are never removed automatically.

At most four direct imports and one catalog scan run at a time. There are bounded connection/command/event limits. Busy errors are recoverable; retry after backoff and inspection as appropriate.

## Watch

Send `{"version":2,"request":{"command":"watch"}}`. The first reply contains the current `State`. Keep the connection open. Subsequent frames contain success envelopes whose `data` is an `Event`:

```json
{"version":2,"ok":true,"data":{"event":"state","data":{"queue":[],"current_id":null,"status":"stopped","position_ms":0,"volume":70,"shuffle":false,"repeat":"off","revision":0,"queue_revision":0,"play_next":[],"scheduled_stop":null,"scanning":false,"last_error":null}}}
```

```json
{"version":2,"ok":true,"data":{"event":"progress","data":{"position_ms":1000,"revision":7}}}
```

`library_changed` and `shutdown` have no data payload. Watch subscriptions are established before the initial snapshot is taken. A client should ignore queued state events with revisions lower than its most recent snapshot and progress events whose revision does not match its current state. On event-buffer lag, the server obtains and emits a new snapshot. Reconnect after a dropped stream and replace local state from the new snapshot; never infer the server's lifetime from one UI connection.

The CLI's NDJSON watch output normalizes the first snapshot into a `state` event, so every printed line follows the same event-envelope shape.

## Agent commands

Protocol 2 keeps the existing command names and adds:

| Command | Request fields | Successful data |
| --- | --- | --- |
| `now` | none | Compact playback snapshot; see README |
| `queue_page` | `offset`, `limit` | `items`, `total`, `offset`, `queue_revision` |
| `library_search` | `filter`, `offset`, `limit` | Existing `tracks`, `total`, `offset` page |
| `library_track` | `id` | One `Track` |
| `queue_edit` | `edit`, `dry_run`, `if_queue_revision`, `request_id` | `applied`, previous/new queue revision, `changes` |
| `scan_status` | `id` | Persisted scan job |
| `stop_after_current` | none | Updated state |
| `sleep_set` | `milliseconds` (1–86,400,000) | Updated state |
| `sleep_status` | none | `scheduled_stop` |
| `sleep_cancel` | none | Updated state |

The filter has optional `query`, `title`, `artist`, `album`, `exclude`, and `exact`
fields; defaults are empty query/exclusions, no field constraints, and substring
matching. Normalize with NFKC and lowercase. Combine all positive filters with
AND and reject matches containing any exclusion. Exact matching affects only
explicit field filters. Results retain the existing search/path ordering.

Queue edit documents and CLI examples are described in README > For scripts and
agents. The command's nullable guard and request ID fields may be omitted;
`dry_run` is required. Operations address library IDs for additions and entry IDs
for removals/moves. Apply to a candidate, validate every step, commit session and
optional receipt together, then publish in memory and emit a state event. Invalid
edits and dry runs do not mutate state or revisions. Dry-run generated IDs are
provisional. At most 1000 operations and 10,000 queue entries are accepted.

`queue_revision` advances on membership/order, pending `play_next`, or current
entry changes, including natural advancement and legacy commands. It does not
advance for volume, elapsed time, pause, or a no-op edit. Explicit pending entries
are consumed before the ordinary shuffle pool. Repeat-one still precedes that
pool on natural endings, except when an after-current stop is due.

A request ID covers the parsed edit and revision precondition. Successful receipts
are replayed before checking the current revision and survive restarts for 24
hours. Conflicting content is rejected. Keep at most 10,000 unexpired receipts;
reject new keyed edits when full. Expired IDs may be reused. This guarantee applies
to `queue_edit` only (the CLI's multi-track add maps to it), not playback side
effects. Receipts return historical results and never overwrite later state.

## Scan jobs and reservations

`library_add`, `library_remove`, and `library_scan` acknowledge `{scanning:true,
job_id}`. Concurrent scans return `scan_in_progress` with `error.details.job_id`.
Scan work runs off the serialized owner. Catalog replacement and the completed
job report commit together. `library_changed` is emitted on successful replacement;
`scan_completed` carries the terminal `ScanJob`, including failed jobs. This event
can precede the following state event that sets `scanning:false`.

Jobs contain `job_id`, `status`, `started_at_ms`, `finished_at_ms`, `summary`, and
`error`. Summary counts added/updated/removed/unchanged records, all warnings,
and up to 100 `{path,message}` warning details. Retain the last 100 terminal jobs.
Startup marks previously running jobs interrupted. CLI `--wait` polls the specific
job with a bounded timeout; timing out does not cancel server work.

State includes nullable `scheduled_stop`: either `{kind:"after_current",
queue_item_id}` or `{kind:"deadline",deadline_ms}`. A reservation replaces the
previous one. Natural completion of the bound entry stops even in repeat-one;
changing the entry cancels that reservation. Deadlines use Unix milliseconds and
are checked even while paused or recovering output. Both stop/reset position and
keep the queue. Explicit stops, cancellation, and server restart clear reservations;
client disconnects do not.

## Compatibility and storage

All envelopes advertise protocol 2. Clients must report `version_mismatch` when
connected to protocol 1; restart with matching binaries and reattach clients.
Database version 2 adds normalized field columns, receipts, and scan jobs using
an atomic migration. Preserve old track IDs and session data; older binaries
reject the new database version. The response error object may include optional
`details` in addition to stable `code` and human-readable `message`.
