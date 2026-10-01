# Local protocol, version 6

The CLI is the recommended automation interface. These details are for contributors building another local client.

## Transport

Connect to the per-user Unix socket printed by `vtamp doctor --json`. Send a four-byte unsigned **big-endian** byte count, followed by that many bytes of UTF-8 JSON. The limit is 16 MiB in either direction. A normal connection handles one request and one reply, then closes. Request reads and reply writes have deadlines; an idle or slow client cannot block playback.

```json
{"version":6,"request":{"command":"pause"}}
```

The `Command`, `Request`, `Reply`, `State`, and `Event` types in `src/model.rs` are the source of truth for field names. Commands are internally tagged with `command` in snake_case. Paths supplied by clients must be absolute; the CLI resolves relative paths before sending them. The server's working directory is not the invoking shell's directory.

```json
{"version":6,"ok":false,"error":{"code":"version_mismatch","message":"Client and server protocol versions differ; restart the server with this binary"}}
```

A version mismatch is rejected before dispatch. There is no TCP listener and no network discovery. Socket permissions restrict clients to the same OS user.

## Ownership and ordering

The server has one state/SQLite owner and serializes received commands. Independent connections are ordered by arrival, not by wall-clock client invocation. Requests are not automatically retried. Only keyed `queue_edit` requests are deduplicated, as described below. For other non-idempotent operations, a disconnect after sending a command has an unknown result; check the state before retrying.

`pause` and `resume` are idempotent. Queue entries have independent UUIDs even if their track IDs are equal. State revisions increase after mutations. Direct path imports complete asynchronously; the success reply means the imported entries have been appended. A library scan reply instead acknowledges the background job; wait for `library_changed` or `scanning: false` for its result.

`play` with a library `track` ID reuses the current queue entry if its track matches, otherwise the first matching entry in queue order. It appends only when no match exists. The server resolves this against its current state, so repeated requests do not create duplicates. `queue_add` always appends and permits duplicates; existing duplicates are never removed automatically.

`play_direct` accepts exactly one of `track` (library ID) or `path` (one absolute
audio file, not a directory). It starts playback without changing queue contents
or its cursor. State has nullable `direct`, with the same `{id, track}` shape as a
queue item, and nullable `queue_cursor`, remembering the last queued entry while
direct playback is active. `current_id` identifies either `direct` or a queued
item. `now.current` resolves either source, and `now.current_in_queue` distinguishes
them. Direct playback IDs are not valid queue-edit targets.

Natural completion and manual next continue after `queue_cursor`, with normal
play-next/shuffle priority; no cursor means start at the front. Repeat-one repeats
the direct track. Repeat-all wraps the queue, or repeats the direct track if the
queue is empty. Previous restarts a direct track. Queue clearing leaves direct
playback running. Removing the remembered cursor moves it to the preceding entry
(or null), preserving the next position. Direct-to-direct changes do not advance
`queue_revision`; the saved queue cursor remains part of queue revision checks.
After-current reservations bind to the playback ID, including direct playback.

At most four direct imports and one catalog scan run at a time. There are bounded connection/command/event limits. Busy errors are recoverable; retry after backoff and inspection as appropriate.

## Watch

Send `{"version":6,"request":{"command":"watch"}}`. The first reply contains the current `State`. Keep the connection open. Subsequent frames contain success envelopes whose `data` is an `Event`:

```json
{"version":6,"ok":true,"data":{"event":"state","data":{"queue":[],"current_id":null,"direct":null,"queue_cursor":null,"status":"stopped","position_ms":0,"volume":70,"shuffle":false,"repeat":"off","revision":0,"queue_revision":0,"play_next":[],"scheduled_stop":null,"scanning":false,"last_error":null,"stream_status":null}}}
```

```json
{"version":6,"ok":true,"data":{"event":"progress","data":{"position_ms":1000,"revision":7}}}
```

`library_changed` and `shutdown` have no data payload. Watch subscriptions are established before the initial snapshot is taken. A client should ignore queued state events with revisions lower than its most recent snapshot and progress events whose revision does not match its current state. On event-buffer lag, the server obtains and emits a new snapshot. Reconnect after a dropped stream and replace local state from the new snapshot; never infer the server's lifetime from one UI connection.

The CLI's NDJSON watch output normalizes the first snapshot into a `state` event, so every printed line follows the same event-envelope shape.

## Spectrum subscription

Send `{"version":6,"request":{"command":"spectrum_watch"}}` on a separate
connection. The first and subsequent replies contain a `SpectrumFrame` directly
in `data`, not a `State` or `Event`. Fields are `generation`, nullable `current_id`,
`active`, `low_hz`, `high_hz`, and `levels` (32 finite values in 0–1). An initial
inactive frame can precede the first analyzed window. Frequency bands are logarithmic
from 40 Hz to the lower of 16 kHz and Nyquist. Values are display magnitudes over
a −70 to −10 dB display range, not calibrated loudness measurements.

The stream publishes at up to 20 Hz. Frames are disposable: each subscriber keeps
the latest value, socket writes have a two-second deadline, and EOF releases its
subscription. There is no replay, persistence, or playback revision change.
Analysis is shared across subscribers and sleeps without demand or while paused;
unchanged inactive frames are not retransmitted. Active frames also serve as
liveness heartbeats. Audio capture uses bounded preallocated
storage and never waits for analysis or sockets.
A seek, reload, or output reset changes `generation`; clients discard old results
and clear their peaks. Pause/resume also flushes internal capture epochs.

Prepared PCM in the output format is observed before app volume, without changing
the samples sent to the output. Stereo channels contribute power independently,
avoiding phase cancellation; multichannel outputs use their front pair. Stale
samples produce an
inactive zero frame. TUI clients additionally reject a frame for a different
current queue entry and let the display decay if frames stop arriving.

Ordinary `watch`, `status`, and `now` do not include spectrum frames. A spectrum
error does not disconnect the ordinary state stream. All connections must use
the same protocol version as the server.

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

`library_list` accepts an optional `anchor` library track ID in addition to
`query`, `offset`, and `limit`. With an anchor, the server ignores the supplied
offset and locates the page containing that track in the ordinary `search,path`
ordering. It preserves the query if the track matches, otherwise clears it.
The response includes the effective `query`, `offset`, `tracks`, and `total`.
A missing anchor returns `track_not_found`. Without an anchor, the existing list
response is unchanged. This is an additive version-5 extension; older servers
ignore the field, so clients must verify the response query and target identity.

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

## Live radio (version 6)

Tracks contain either a local `path` or an HTTP(S) `url`, represented internally
by `PlaybackSource`. File JSON retains its existing shape and numeric
`duration_ms`. Live tracks have `url`, `duration_ms: null`, and empty artist/album
fields; their registration name is the title. YouTube import provenance remains
in the independent `source` field. Library lookup, field search, pagination,
anchors, direct playback, and queue edits accept stream track IDs.

| Command | Request fields | Successful data |
| --- | --- | --- |
| `stream_add` | `entries`: array of `{url,name}` | `added`, `existing`, newly added `tracks`, `first_registered_id` |
| `stream_remove` | `id` | `removed` ID |
| `stream_preview` | absolute local `path` | array of validated `{url,name}` entries |

Registration validates all entries before one database transaction. Up to 1000
entries are accepted. URLs must be HTTP(S) without embedded credentials; their
fragments are removed and URL parsing normalizes them. `first_registered_id`
identifies the first input channel, including when it was already registered;
clients can use it as a Library anchor when no new tracks were added. An already
registered URL keeps its identity and name. This is registration deduplication, not keyed request
receipt replay. Queue additions still allow duplicates. Successful registration
and removal emit `library_changed`, without changing queue revisions or playback.
Unregistering leaves saved queue/direct copies intact.

`stream_preview` reads a UTF-8 M3U/PLS regular file off the owner thread (1 MiB,
1000 entries maximum); it writes nothing. The CLI `--preview` does the same work
locally and never starts the server. Invalid entries fail the complete operation;
HLS manifests with `#EXT-X-` tags must instead be registered by their stream URL.

State adds nullable `stream_status`: `connecting`, `buffering`, `live`, or
`reconnecting`. It is null for files, paused/stopped radio, and restored sessions.
`status: playing` indicates playback intent; `stream_status: live` confirms native
playback has advanced. `now` also includes `is_live`. Radio position remains zero;
`duration_ms` and `remaining_ms` in `now` are null. Connection-state changes update
the ordinary state revision, never queue revision. Unchanged radio sessions do
not write periodic position checkpoints.

Pause/stop release native playback and cancel retries. Resume and reconnect use
the registered URL, resolving redirects again. Native stalls get a 20-second
watchdog and capped exponential reconnect backoff. Unsupported/missing resources
pause with `last_error`. Network endings never trigger natural queue advancement.
Seek is unsupported, as is `stop_after_current`; deadline sleep timers still work.
SpectrumWatch remains available but radio provides inactive frames. System Now
Playing marks live media and disables timeline seeking.

Database version 6 adds a separate stream registry and a combined catalog view.
File scans only replace file records. Existing IDs, file JSON, sessions, and
request receipts are preserved; old binaries reject this schema. Radio restores
paused with zero position and without network activity. Native stream playback
requires macOS, including when `VTAMP_MEDIA_KEYS=0`.

## Compatibility and storage

All envelopes advertise protocol 6. Clients must report `version_mismatch` when
connected to older versions; restart with matching binaries and reattach clients.
Database version 5 protects saved direct-playback items and queue cursors from
older binaries; previous sessions load with both fields null. Direct playback also
restores paused at its saved position. Database version 4 adds a nullable album override and replaces legacy missing-album
placeholders with empty strings in the catalog, search index, and saved queue.
Structured source albums are applied when available. Version 3 first adds import
jobs/items and metadata/source records; version 2 adds normalized field columns,
receipts, and scan jobs. Each migration step is atomic. Preserve old track IDs and
session data; older binaries
reject the new database version. The response error object may include optional
`details` in addition to stable `code` and human-readable `message`.

## Optional installed-tool imports

The server detects an executable `yt-dlp` on its PATH or at the configured path.
Without it, clients omit import controls/help/status messages. Nothing is installed
automatically. See [configuration and workflows](imports.md).

| Command | Request fields | Successful data |
| --- | --- | --- |
| `import_available` | none | `available` boolean; no subprocesses |
| `import_capabilities` | none | Import tool paths/versions, YouTube configuration, `authentication_tested: false` |
| `import_preview` | `request` | `preview` with normalized URL, title, playlist flag, entries |
| `import_lookup` | `video_ids` (up to 10,000) | IDs already present with existing files |
| `import_start` | `request` | `job_id`, `status: queued` |
| `imports` | none | Recent jobs, newest first |
| `import_status` | `id`, `offset`, `limit` (1–1000) | `job`, paginated `items`, `offset` |
| `import_cancel` | `id` | Updated job; running jobs enter `cancelling` |
| `import_retry` | `id` | New job for unfinished retryable entries |
| `library_edit` | `id`, nullable `title`, `artist`, `album` | Updated Track |
| `library_retag` | `id` | Updated Track; manual overrides remain authoritative |

For `library_edit`, an omitted/null field is unchanged. An empty `album` string
explicitly clears it and remains authoritative across scans and retagging.
Title and artist must remain nonempty. Track `album` is an empty string when
absent; TUI clients omit missing albums and associated separators.

An import request has `url`, `playlist` (default false), optional single-video
`title`/`artist`, and optional `video_ids` to freeze a preview or retry subset.
An empty ID in a playlist represents an unavailable entry and is reported as a
failure. Other IDs must be valid 11-character video IDs. A single video's frozen
ID must match its URL. Playlists are capped at 10,000 entries. There are at most
32 queued/running jobs, one download worker, and four auxiliary preview/metadata
workers. Jobs run in submission order; configuration is captured at submission.

Jobs carry `job_id`, normalized source URL, title, status/stage, nullable total
and current item, added/skipped/failed counts, bytes/total/speed/ETA, timestamps,
error, and a monotonically increasing revision. Terminal statuses are completed,
partial, failed, cancelled, and interrupted. Items carry an index, video ID,
title, status, optional track ID, metadata result/warning, and error. Job history
retains 100 terminal jobs. On startup all unfinished jobs become interrupted;
only an explicit retry starts them again. Jobs optionally carry
`first_added_track_id`, committed with the first successful publication and
preserved for the rest of the job. It identifies the first added track in playlist
order, excluding skipped/failed items. Older reports can omit it.

When the integration is available, `watch` emits an `imports` snapshot after its
initial State and on lag resynchronization, plus `import_progress` events. Ignore
older revisions of the same job. Import progress does not advance playback or
queue revisions. Existing playback event contracts are unchanged.

Audio, artwork, and the source manifest are prepared in a private staging
folder. Publication waits for catalog scans/direct path imports to finish, then
renames the completed directory and commits the track, source/overrides, and
successful item report together. A failed DB commit leaves a recoverable directory;
retry or a later scan can adopt it. Scans never enumerate staging directories.
Deduplication is by source video ID and existing file, independently of queue
entry identity. Metadata edits update queued copies without changing queue order,
current entry, playback position, or queue revision. Audio files are not rewritten.

The Track's optional `source` contains `provider: youtube`, `video_id`, canonical
`video_url`, original title, channel identity/name/URL, a bounded description,
and any structured music metadata. Local tracks omit it. The CLI preview performs
extraction locally and an optional read-only lookup; it never starts a server or
creates a database/job.
