# Local protocol, version 1

The CLI is the recommended automation interface. These details are for contributors building another local client.

## Transport

Connect to the per-user Unix socket printed by `vtamp doctor --json`. Send a four-byte unsigned **big-endian** byte count, followed by that many bytes of UTF-8 JSON. The limit is 16 MiB in either direction. A normal connection handles one request and one reply, then closes. Request reads and reply writes have deadlines; an idle or slow client cannot block playback.

```json
{"version":1,"request":{"command":"pause"}}
```

The `Command`, `Request`, `Reply`, `State`, and `Event` types in `src/model.rs` are the source of truth for field names. Commands are internally tagged with `command` in snake_case. Paths supplied by clients must be absolute; the CLI resolves relative paths before sending them. The server's working directory is not the invoking shell's directory.

```json
{"version":1,"ok":false,"error":{"code":"version_mismatch","message":"Client and server protocol versions differ; restart the server with this binary"}}
```

A version mismatch is rejected before dispatch. There is no TCP listener and no network discovery. Socket permissions restrict clients to the same OS user.

## Ownership and ordering

The server has one state/SQLite owner and serializes received commands. Independent connections are ordered by arrival, not by wall-clock client invocation. Requests are not automatically retried or deduplicated. A disconnect after sending a command has an unknown result; check the state before retrying a non-idempotent operation.

`pause` and `resume` are idempotent. Queue entries have independent UUIDs even if their track IDs are equal. State revisions increase after mutations. Direct path imports complete asynchronously; the success reply means the imported entries have been appended. A library scan reply instead acknowledges the background job; wait for `library_changed` or `scanning: false` for its result.

`play` with a library `track` ID reuses the current queue entry if its track matches, otherwise the first matching entry in queue order. It appends only when no match exists. The server resolves this against its current state, so repeated requests do not create duplicates. `queue_add` always appends and permits duplicates; existing duplicates are never removed automatically.

At most four direct imports and one catalog scan run at a time. There are bounded connection/command/event limits. Busy errors are recoverable; retry after backoff and inspection as appropriate.

## Watch

Send `{"version":1,"request":{"command":"watch"}}`. The first reply contains the current `State`. Keep the connection open. Subsequent frames contain success envelopes whose `data` is an `Event`:

```json
{"version":1,"ok":true,"data":{"event":"state","data":{"queue":[],"current_id":null,"status":"stopped","position_ms":0,"volume":70,"shuffle":false,"repeat":"off","revision":0,"scanning":false,"last_error":null}}}
```

```json
{"version":1,"ok":true,"data":{"event":"progress","data":{"position_ms":1000,"revision":7}}}
```

`library_changed` and `shutdown` have no data payload. Watch subscriptions are established before the initial snapshot is taken. A client should ignore queued state events with revisions lower than its most recent snapshot and progress events whose revision does not match its current state. On event-buffer lag, the server obtains and emits a new snapshot. Reconnect after a dropped stream and replace local state from the new snapshot; never infer the server's lifetime from one UI connection.

The CLI's NDJSON watch output normalizes the first snapshot into a `state` event, so every printed line follows the same event-envelope shape.
