# Client plugins (API 1)

Plugins add document/list panels and commands to an attached vtamp client. They
are separate local programs speaking newline-delimited JSON over stdin/stdout;
Python, JavaScript, Rust, or any other language can implement the protocol.
vtamp owns terminal input, layout, themes, wrapping, and timed highlighting.

Register a program you trust. Plugins run with your OS user's privileges, can
access files and the network, and can invoke the existing vtamp CLI. Process
isolation protects the client's event loop; it is **not a security sandbox**.
The host provides no audio, metadata replacement, or background-service hooks.

## Start with Hello Panel

[Hello Panel](../examples/hello-panel/main.py) is a small Python example with a
greeting and a counter action. It uses only Python 3's standard library and needs
no music, network, credentials, or yt-dlp. Its `target: "none"` command runs from
the CLI even when no playback server exists.

From the repository root, build the host if you have not already built this
version. Python 3 must be available as `python3` on PATH; vtamp does not install it.
The commands below assume Cargo's default target directory.

```sh
cargo build --locked --release --bin vtamp
target/release/vtamp plugin add examples/hello-panel/plugin.json
target/release/vtamp plugin list --json
target/release/vtamp plugin run hello-panel:show --json
```

The list should contain `hello-panel` with no warnings. The run command emits a
snapshot whose `data.view.title` is `Hello Panel` and whose items include
`Hello from a Python plugin.` and `Count: 0`. It stays open to handle messages;
press Ctrl+C to exit. `plugin run` displays output only; use the TUI to invoke
actions. These plugin commands never start a playback server.

To try the panel in the TUI, open a new attachment with this build:

```sh
target/release/vtamp
```

Press `:`, type `Hello`, and choose **Hello Panel · Show greeting** with Enter.
Press `a`, select **Increment counter**, and press Enter. `Count: 0` becomes
`Count: 1`; repeat to increment it again. Esc closes the panel. Opening the
command again starts a new process and resets the counter. No track needs to be
playing. Registration takes effect on new attachments; an existing playback
server does not need restarting. Opening the TUI follows the usual server
startup behavior, unlike `plugin run`.

To unregister the example:

```sh
target/release/vtamp plugin remove hello-panel
```

Close any open panel and reattach to see the updated command menu. Keep the
example directory in place while registered: registration stores a path, not a
copy of its files.

## Write your own plugin

Copy the [Hello Panel directory](../examples/hello-panel/) to your own project.
Change `id` and `name` in `plugin.json`, the command title, and the content of
`show()` in `main.py`, then register your manifest. The directory can live anywhere;
you do not need a vtamp source checkout, Rust dependency, or SDK to run it with a
host that supports API 1. The working directory is the manifest's directory, so
`["python3", "main.py"]` also works outside the vtamp repository.

The example demonstrates the complete interactive loop:

1. Read `init`, check `api_version`, and reply with `ready`.
2. Read `invoke` and use `context.generation` on every result. A `view` replaces
   the full document; its items are plain text and its actions have stable IDs.
3. Read `action`, check its generation and ID, and publish the updated view.
   In this example, the action changes only an in-memory counter.
4. Keep reading until `shutdown` or EOF. For a one-shot command, send `done`
   after the view and exit; the finished panel stays readable in the TUI.

`send()` writes one JSON line and flushes stdout immediately. Buffering output
can make the host's handshake time out. Write diagnostics with
`print("message", file=sys.stderr)`, keeping stdout exclusively for the protocol.
The example stores the latest host generation and ignores stale actions; it also
handles `context` messages so it can be extended to a track-aware command.

Choose `playing` for a panel that follows playback or `selected` for one pinned
to the track selected when invoked. Handle `context.track: null` for missing
tracks. For slow file/network work, keep reading messages while a worker runs,
cancel obsolete work on generation changes, and tag results with the generation
that started the work. The [Pastel Transcript example](#pastel-transcript-example)
demonstrates those longer-running requests and optional timed items.

| Example | What it demonstrates | Requirements |
| --- | --- | --- |
| [Hello Panel](../examples/hello-panel/) | Plain text, actions, and a persistent session | Python 3; works offline without a track |
| [Pastel Transcript](../examples/pastel-transcript/) | Playback context, external content, caching, cancellation, and timed highlighting | Separate Rust build; a supported YouTube track; network for uncached content |

Both use the same JSON protocol. Providers, caption parsing, and timing estimates
belong to the example; the host supplies rendering, context, and process lifetime.
Untimed notes, lists, and action panels can omit timing fields entirely.

## Register and use

```sh
vtamp plugin add /absolute/path/to/plugin.json
vtamp plugin list --json
vtamp plugin run my-plugin:show --json
vtamp plugin remove my-plugin
```

`add` validates and records the manifest's absolute path in the data directory's
`plugins.json`. It does not execute the program, copy files, install dependencies,
or contact the network. A conflicting ID requires removal before replacement;
registering the same manifest again is harmless. `remove` leaves program files,
data, and caches intact. None of these commands starts a playback server.

Reattach after registration or configuration changes. Press `:` to search
extension commands, arrows to select, Enter to run, or Esc to close. In a panel,
arrows/j/k scroll, PageUp/PageDown (or Ctrl-B/F) page, Home/End jump, Enter invokes
the selected list item's action, and `a` opens its action menu. `f` resumes timed
follow after manual scrolling; Esc/q closes the panel, and `:` opens the command
menu. The menu uses the real cursor for IME input.

The client starts a process only on invocation. Closing the panel, opening another
command, or detaching requests shutdown, then kills the process group after
500 ms if necessary. Each attachment has an independent session. Plugins may
share their on-disk cache, so use atomic writes and per-request temporary files.
A finished panel remains readable until closed; invoke its command again to
start a new process. Crashes and malformed output become panel errors.

`plugin run` is a headless host for development and scripts. `--json` emits the
usual response envelope with `data` containing snapshots of `generation`,
`view`, `notice`, `finished`, and `error`. Without it, results are printed as text.
Output is coalesced to the latest view, not an audit log. The process runs until
`done`, failure, or Ctrl+C. Failure exits 1. Commands targeting `selected` require
`--track ID` (a library track ID); other targets reject that argument. It watches
an existing server and reconnects when available, but never starts one. A missing
server is represented by `connected: false`. A selected-track lookup must succeed
before execution. A `none` command does not connect to the server at all.

## Manifest and bindings

```json
{
  "api_version": 1,
  "id": "my-plugin",
  "name": "My Plugin",
  "exec": ["python3", "main.py"],
  "commands": [
    {"id": "show", "title": "Show track notes", "target": "playing"}
  ]
}
```

The working directory is the manifest's directory. An executable containing `/`
is resolved relative to it, or used as an absolute path; a bare executable name
is found on PATH. Arguments are passed literally without shell interpolation.
Use an executable script or explicitly name its interpreter. vtamp does not
install optional runtimes. `VTAMP_HOME` is inherited by the plugin and its children.

IDs contain lowercase ASCII letters, digits, and single separating hyphens (up
to 80 bytes). A manifest is at most 64 KiB and declares 1–64 unique commands.
Unknown manifest fields are errors. An incompatible API, missing file, or changed
ID is diagnosed without hiding other plugins. `plugin list --json` includes the
loaded manifests, absolute paths, valid bindings, and warnings. Invalid registry
JSON is left intact and must be corrected before registration writes can succeed.

`target` is `playing` (follows the current playback entry), `selected` (pins the
track selected at invocation), or `none`. Selection does not change playback.
An absent current track is passed as null so the plugin can explain its state.

Optional bindings live in `plugins.json`, separate from theme preferences:

```json
{
  "plugins": {"my-plugin": "/absolute/path/to/plugin.json"},
  "bindings": {"L": "my-plugin:show"}
}
```

Bindings are single printable ASCII characters. Existing keys, optional import
keys, and multi-key prefixes are reserved. Invalid/conflicting bindings are
reported and ignored; commands remain available from `:`. Bindings apply only
outside text fields and modals, never to global desktop input.

## Messages

Every message is one UTF-8 JSON line, at most 1 MiB including its newline. stdout
is reserved for protocol messages; write diagnostics to stderr. The host drains
stderr and retains its last 8 KiB for error reports. API version 1 is independent
of the playback server's protocol and database versions.

1. Host sends `{"type":"init","api_version":1,"data_dir":"…","cache_dir":"…","vtamp":"…"}`.
2. Plugin replies `{"type":"ready","api_version":1}` within five seconds.
3. Host sends `{"type":"invoke","command":"show","context":{…}}`.
4. Plugin publishes `view` and `notice` messages as work completes. It may remain
   running for new contexts and actions, or return `done` for a one-shot command.

`data_dir` and `cache_dir` are per-plugin absolute paths created on invocation.
On macOS these are the vtamp data directory's `plugin-data/<id>/` and cache
directory's `plugins/<id>/`. With `VTAMP_HOME`, they are
`$VTAMP_HOME/plugin-data/<id>/` and `$VTAMP_HOME/plugins/<id>/`. `vtamp` is the current host
executable's absolute path; use it for existing CLI commands, preserving the
environment. No credential store or automatic package updates are provided.

A context contains `generation`, `connected`, nullable `track`, nullable
`playback_id`, `status`, and `position_ms`. `track` uses the documented Track
shape, including optional `source` metadata. `playback_id` is a queue/direct
entry ID, not a library ID. A selected track only has playback position when it
is also the current track. The host sends `{"type":"context","context":{…}}`
when this state changes; progress uses the existing server cadence. Treat
contexts as replaceable snapshots. The host coalesces progress while busy.

Track metadata or playback-entry changes advance `generation`; progress,
pause/resume, and connectivity changes alone do not. Copy the generation onto
every result. Results for older generations are ignored. Cancel obsolete network
requests and children on a new generation. A host shutdown is
`{"type":"shutdown"}`; EOF also means the session is over.

```json
{"type":"view","generation":1,"view":{"title":"Track notes","subtitle":"Caption timings","items":[{"text":"First sentence.","start_ms":1000,"end_ms":2500},{"text":"More information","action":"details"}],"actions":[{"id":"retry","title":"Retry"}]}}
```

Each view replaces the previous document. `subtitle` and `actions` default to
empty. Items require plain `text`; `action`, `start_ms`, and `end_ms` are optional.
Times are nonnegative milliseconds; an end must exceed its start. A timed item
without an end remains active until a later item starts. Untimed items never
highlight automatically. Supply ascending times. At most 10,000 items and 64
panel actions are accepted. Control characters other than text newlines/tabs
are rejected; raw ANSI and terminal graphics are not supported.

The host uses its local playback clock for highlighting, so there is no need to
send frames or highlight updates. It wraps content once per width/text change,
keeps the active item near the middle, and stops following on manual navigation.
Unknown duration is fine when the plugin has real timestamps; leave times absent
when timing is unavailable. A plugin may replace estimates with better timings.

Selecting an item/action sends
`{"type":"action","id":"retry","generation":1}`. Actions refer to plugin
functions, not automatic host shell commands. The plugin may use the documented
vtamp CLI for explicit playback operations; it must obey the existing unknown
outcome/retry rules. The host does not automatically retry actions.

`{"type":"notice","generation":1,"message":"No published transcript."}`
updates status without replacing an existing document in that generation.
`{"type":"done","generation":1}` completes the session. Output without a
handshake, unknown message types, oversized messages, blocked input queues,
unexpected EOF, and invalid JSON terminate that plugin session. Host input,
process I/O, and parsing run independently of playback and terminal input.

## Pastel Transcript example

This larger Rust example fetches published transcripts and follows the current
track. It only supports videos with a document in
[Pastel Sketchbook's transcript repository](https://github.com/pastel-sketchbook/pastelsketchbook/tree/main/homepage/public/transcripts).
It is not a general YouTube transcription service. Start with
[Hello Panel](#start-with-hello-panel) to test the plugin host without a provider.

From the repository root, build both the host and the optional example:

```sh
cargo build --locked --release --bin vtamp --example pastel-transcript
target/release/vtamp plugin add examples/pastel-transcript/plugin.json
```

The example manifest expects the default Cargo target directory. If using
`CARGO_TARGET_DIR`, change its exec path to your built executable before
registration. It is not installed or enabled by default.

### Try a supported video

[The Road to SolidJS 2.0](https://youtu.be/00kCzR10M1w) has a
[published transcript](https://github.com/pastel-sketchbook/pastelsketchbook/blob/main/homepage/public/transcripts/00kCzR10M1w.md).
If it is not already in your library, import it with existing `yt-dlp`, `ffmpeg`,
and `ffprobe` installations (see [imports](imports.md#add-music)):

```sh
target/release/vtamp library add 'https://youtu.be/00kCzR10M1w' --audio-only --wait
target/release/vtamp
```

In the new TUI attachment, find the imported track in Library and play it. Press
`:`, choose **Pastel Transcript · Show transcript**, and press Enter. The panel
should show prose and highlight text as playback advances. The subtitle reports
**Estimated timings** initially, or **Caption timings** when matching captions
are available. Arrows scroll manually; `f` resumes following; Esc closes. You do
not need to restart the server to use a newly registered plugin.

For output and error details without a TUI, run this while the track is current:

```sh
target/release/vtamp plugin run pastel-transcript:show --json
```

It watches playback without starting it; press Ctrl+C to exit. Transcript lookup
failures appear in `data.notice` and leave the plugin running for another track
or retry. Process or protocol failures end the command.

| Message or symptom | What to check |
| --- | --- |
| Command missing from `:` | Run `plugin list --json` with the same binary and `VTAMP_HOME`, resolve warnings, then reattach. |
| `Cannot start plugin` | Build the example and check that the manifest's exec path points to the executable. |
| `Nothing is playing.` | Choose and play the imported track. |
| `This track has no YouTube transcript.` | The track has no YouTube source metadata; import the URL through vtamp. A separately downloaded local file does not supply that context. |
| `No transcript published for this video` | Choose a video with a matching Markdown document in the transcript repository. |
| `Cannot fetch transcript` | Check network access to `raw.githubusercontent.com`; then reopen or retry the panel. |
| `Estimated timings` remains | Captions were unavailable or did not align; the transcript still works with approximate timing when the track has a duration. |

### Implementation notes

This example adapts [Pastel Sketchbook's contribution, d4d0be3](https://github.com/pastel-sketchbook/vtamp/commit/d4d0be38ed347da7f2671122a93eb91469f51a58)
under the repository's MIT license. It fetches the published Markdown documents
from `pastel-sketchbook/pastelsketchbook`, parses prose/capture notes, and uses
optional yt-dlp caption alignment. No real provider is contacted by its fixture
tests. Its reuse of vtamp Rust types and process helpers is a convenience for this
in-tree example; external plugins only need to implement the JSON protocol.

The plugin first returns prose with character-weighted estimated timing, then
replaces that timing when original-language captions align. Missing yt-dlp,
captions, or matching words leaves the estimate in place. Requests and child
processes are cancelled on track changes or shutdown. Network/cache documents
are limited to 256 KiB, cache writes are atomic, and concurrent caption fetches
use separate temporary directories. The panel action retries using the cache;
remove the plugin's cache files manually to force a fresh download.
