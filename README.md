<div align="center">
  <img src="site/mark.svg" width="64" alt="vtamp">
  <h1>vtamp</h1>
  <p><strong>Music stays. Your terminal moves on.</strong></p>
  <p>A detachable terminal music player. Rust. Local files. macOS first.</p>
</div>

There used to be a little player on the corner of your desktop. A playlist, an album cover, a green display. It did one thing, and it felt like yours.

These days, a lot of us live in a terminal. We still want that player. We just don't want it to own a tmux pane for the rest of the day.

**vtamp** brings a little **v**irtual **t**erminal and a little Win**amp** spirit together. Its playback server keeps the music going; its interface comes and goes. Open it, pick a track, press `q`, get back to work. Come back from another pane whenever you like.

```text
$ vtamp                   # Start the server if needed. Attach the player.
                          # Pick a track. Press Enter. Press q to detach.
$ vtamp next              # Change tracks without opening the interface.
$ vtamp pause             # Make some room for a conversation.
$ vtamp                   # Same server. Same queue. Pick up where it is now.
```

No account. No streaming subscription. No permanent pane. Your music stays on your machine.

## What ships in v0.1

- Persistent playback server, with multiple TUI and CLI clients.
- Folder-based library, title/artist/album search, and an editable shared queue.
- AAC and ALAC in m4a/MP4, MP3, FLAC, WAV, and Ogg Vorbis playback.
- Embedded album covers, sidecar covers, and a built-in fallback image.
- Play, pause, seek, volume, shuffle, repeat, and automatic track advancement.
- JSON commands and an event stream for scripts and AI agents.
- Saved queue, playback position, volume, shuffle, and repeat settings.
- A small, dependency-free [landing page](site/).

macOS is the supported platform for this release. The platform and playback boundaries are isolated for future Linux support; Linux is not yet part of the tested support matrix. Streaming, named playlists, media keys, EQ, crossfade, gapless playback, and login-time startup are not implemented.

## Install from source

You need **Rust 1.90 or newer** and the macOS Command Line Tools (`xcode-select --install` if you do not have them). From this source checkout:

```sh
cargo install --locked --path .
```

Make sure `~/.cargo/bin` is on your `PATH`. Or run directly from the checkout:

```sh
cargo run --release
```

There is no published crates.io package or Homebrew formula yet. The application does not require FFmpeg, Chafa, a Node runtime, or a separate database installation. SQLite is bundled.

## First listen

```sh
vtamp library add ~/Music
vtamp
```

Folder registration starts a background scan. The library appears as the scan finishes. Select a track with the arrow keys or `j` / `k`, then press `Enter`. Press `q` to close the interface while the music keeps playing.

You can also start directly with files or a folder:

```sh
vtamp play '/path/to/album'
vtamp play '/path/to/first.m4a' '/path/to/second.flac'
```

`play PATH...` appends the supported files and starts the first new queue entry. `queue add PATH...` appends without interrupting playback. Adding paths directly to the queue does not register them as library roots. Directory imports use artist, album, track number, and path order. Nested symbolic-link directories are not followed.

The first-run volume is 70% of system output. `vtamp volume 35` sets the player to 35%; it does not change the system volume.

## The interface is disposable

```text
 TUI in pane A ──┐
 TUI in pane B ──┼── local Unix socket ── playback server ── audio device
 CLI / agents ──┘                            │
                                      library + session
```

`vtamp` and `vtamp attach` start a per-user background server if none is running. It is a detached process using the same binary, with no controlling terminal. Exiting the interface, closing its pane, or detaching tmux leaves playback running.

`q`, `Esc`, and `Ctrl+C` close the TUI. `Esc` first closes an open search or folder prompt. **Stopping playback and stopping the server are separate actions:**

```sh
vtamp stop          # Keep the server and queue; reset playback position.
vtamp server stop   # Save the session and stop the server.
vtamp server start  # Restore the session, paused.
```

The server saves queue/configuration changes immediately and checkpoints position every five seconds. A normal server stop saves the latest position. After an unexpected crash, up to five seconds of position may be lost. A server restart always restores the current track **paused**, so merely inspecting or attaching does not unexpectedly start sound. The server is not a login service and does not restart itself after logout or a crash.

Read-only commands (`status`, `watch`, volume without a value, queue listing, library queries, and server status/stop) do not start a server. Playback and library mutation commands do. A disconnected TUI waits for the server to return; it does not replay commands whose outcome might be unknown.

## Keys

| Key | Action |
| --- | --- |
| `q`, `Esc`, `Ctrl+C` | Detach the interface |
| `Space` | Play / pause |
| `n` / `b` | Next / previous track |
| `←` / `→` | Seek backward / forward 10 seconds |
| `+` / `-` | Volume up / down 5 percentage points |
| `s` | Toggle shuffle |
| `R` | Cycle repeat: off → all → one |
| `Tab` | Switch library / queue focus |
| `j` / `k`, `↓` / `↑` | Move selection |
| `PageDown` / `PageUp` | Move selection by ten entries |
| `/` | Search title, artist, and album; Enter applies |
| `a` | Add a music folder |
| `r` | Rescan registered folders |
| `[` / `]` | Previous / next library page (200 tracks) |
| `Enter` | Append and play a library track, or play a selected queue entry |
| `e` | Append a library track without interrupting playback |
| `d` | Remove the selected queue entry |
| `J` / `K` | Move the selected queue entry down / up |
| `?` | Show key reference |

Wide panes show the library and queue side by side. Narrow panes show the focused list; `Tab` switches between them. Cover art is shown when there is room. Below 40 columns or 12 rows the UI shows a compact size notice and still allows detaching.

## Covers, terminals, and tmux

vtamp detects graphics support when you attach. Inside tmux, **both tmux and the terminals attached to the pane's session** must support Sixel and report usable pixel dimensions for a pixel-rendered cover. Other panes fall back to color halfblocks. tmux's own Sixel response alone is insufficient: a Sixel-enabled tmux can still be attached from a terminal that cannot display it.

| Option | Rendering |
| --- | --- |
| `--art auto` | Native Sixel in supporting tmux panes; detected graphics protocol elsewhere; halfblocks as fallback |
| `--art halfblocks` | Unicode upper/lower blocks with foreground and background colors |
| `--art sixel` | Force native Sixel graphics when support is known but automatic detection fails |
| `--art kitty` | Explicit Kitty graphics protocol; requires terminal/multiplexer support |
| `--art none` | Hide album art |

```sh
vtamp                     # Automatically use native Sixel when supported in tmux.
vtamp --art sixel
vtamp --art halfblocks
vtamp --art kitty
```

Sixel is sent directly to the current terminal or tmux pane, without passthrough wrapping, so a Sixel-enabled tmux can manage the image across pane redraws and window switches. This requires native Sixel support in both tmux and its client terminal. The probe waits at most 250 ms; automatic mode falls back to halfblocks if Sixel or usable pixel dimensions cannot be detected. Explicit `--art sixel` still queries cell dimensions, with a 10×20-pixel estimate if the terminal supplies none.

If forced Sixel shows `SIXEL IMAGE` or rows of `+`, tmux is substituting its text placeholder because its client cannot render Sixel. Use `--art auto` to fall back safely. Building tmux with Sixel support does not add that protocol to the outer terminal. Ghostty supports the [Kitty graphics protocol](https://ghostty.org/docs/features); `--art kitty` is the separate graphics option for compatible passthrough setups.

Halfblocks work without terminal graphics passthrough. For true color, configure your terminal and tmux for RGB color if necessary. The separate explicit Kitty mode may require passthrough:

```tmux
set -g allow-passthrough on
```

vtamp does **not** change your tmux configuration. Kitty graphics through multiplexers can vary with terminal and tmux versions, especially when switching windows. Use halfblocks if images linger, disappear, or render incorrectly. The explicit Kitty mode is opt-in, not a compatibility guarantee for every Ghostty/kitty/tmux combination.

Artwork comes from the embedded front cover first, then the first embedded picture, then `cover.jpg`, `cover.png`, `cover.jpeg`, `folder.jpg`, `folder.png`, `Folder.jpg`, or `Cover.jpg` beside the audio. Embedded art is cached at up to 512×512 pixels; graphics modes fit those pixels to the cover area while preserving aspect ratio. Missing or undecodable art uses a built-in image. Image decoding, resizing, and Sixel encoding run outside the UI input loop.

## Commands

Run `vtamp --help` or `vtamp COMMAND --help` for argument details. All non-TUI commands accept `--json` before or after the command.

| Command | Behavior |
| --- | --- |
| `play [PATH...]` | Resume, or append paths and play the first new entry |
| `play --track ID` | Append and play a library track |
| `play --queue-item ID` | Play an existing queue entry |
| `pause`, `resume`, `toggle` | Playback state controls; pause/resume are idempotent |
| `stop` | Stop and reset position, keeping the queue |
| `next`, `prev` | Move to the next or previous track |
| `seek 90`, `seek +10`, `seek -10` | Absolute or relative seconds; decimals supported |
| `volume [0..100]` | Read or set volume |
| `shuffle on\|off` | Shuffle without repeating entries within a traversal |
| `repeat off\|one\|all` | Repeat mode; repeat-one affects natural endings, not manual skipping |
| `queue list` | List queue entries with independent entry IDs |
| `queue add PATH...`, `queue add --track ID` | Append without starting playback |
| `queue remove ID` | Remove a queue entry |
| `queue move ID INDEX` | Move an entry to a **zero-based** destination index |
| `queue clear` | Stop playback and empty the queue |
| `library add PATH`, `library remove PATH` | Register/unregister a directory; never delete music files |
| `library scan` | Rescan registered roots in the background |
| `library list [--offset N] [--limit N]` | List indexed tracks, default 200, maximum 1000 per page |
| `library search QUERY [--offset N] [--limit N]` | Search normalized title/artist/album text |
| `library roots` | Show registered roots |
| `status` | Current playback state and queue |
| `watch` | Initial state, state changes, progress heartbeats, and library events |
| `server start\|status\|stop` | Explicit server lifecycle |
| `doctor` | Paths, connectivity, terminal environment, and default output device |

The queue is capped at 10,000 entries. Queue entry IDs are distinct from library track IDs: adding a song twice produces two independently editable entries. Library IDs survive rescans of the same canonical path. A file moved to a different path is a new library entry.

Library scans are explicit, not filesystem watchers. A damaged file produces a warning while other files continue scanning. Unavailable roots retain their previous catalog entries. Unregistering a root updates the catalog after the scan, but existing queue entries retain their file paths. The player skips unreadable files, attempts each fallback candidate only once, and stops if none can play. Audio device errors leave the server available; a subsequent `play` or `resume` attempts to reopen the default output device.

## For scripts and agents

Use the CLI; you do not need to drive the TUI or implement the socket protocol.

```sh
vtamp status --json
vtamp pause --json
vtamp library search 'night' --limit 20 --json
vtamp play --track 'TRACK_ID' --json
vtamp queue remove 'QUEUE_ENTRY_ID' --json
vtamp watch --json
```

Every JSON response has a protocol version and `ok`. Successful responses have `data`; failures have an error code and message. Times are integer milliseconds, volume is an integer from 0 to 100, and playback status is `playing`, `paused`, or `stopped`.

```json
{"version":1,"ok":true,"data":{"scanning":true}}
```

```json
{"version":1,"ok":false,"error":{"code":"server_unavailable","message":"Cannot connect to vtamp…"}}
```

`status` returns `queue`, `current_id`, `status`, `position_ms`, `volume`, `shuffle`, `repeat`, `revision`, `scanning`, and `last_error`. Each queue entry contains `id` and `track`; each track includes its library ID, path, title, artist, album, track number, duration, and optional local cover path. `current_id` identifies a **queue entry**, not a library track. It is null before a current entry is selected. A stopped player may still have a selected entry.

`library list` and `library search` return `{ "tracks": [...], "total": N, "offset": N }`. The default query page is bounded; use `--offset` to retrieve subsequent pages.

`watch --json` emits one response envelope per line (NDJSON), starting with a `state` event. Later events are `state`, `progress`, `library_changed`, and `shutdown`:

```json
{"version":1,"ok":true,"data":{"event":"progress","data":{"position_ms":102000,"revision":7}}}
```

State events contain the full state; progress events update position for their matching state revision. Heartbeats occur about once a second, including while paused. A slow subscriber gets a fresh state after event-buffer lag. `Ctrl+C` stops watching without stopping playback.

Exit status is **0** for success, **1** for a connection or operation failure, and **2** for invalid CLI arguments. JSON errors go to stdout with the same envelope. Normal logs go to stderr; server logs go to a file. Current error codes include `invalid_arguments`, `server_unavailable`, `server_busy`, `version_mismatch`, `invalid_request`, `timeout`, `operation_failed`, and `client_error`. Parse the code; the human message can change. A timeout or disconnect during a mutation has an unknown outcome: inspect the queue or state before retrying an operation such as `next` or `queue add`.

## Storage and troubleshooting

On macOS, persistent data lives under `~/Library/Application Support/vtamp/`; cover thumbnails are under `~/Library/Caches/vtamp/covers/`. The control socket is `/tmp/vtamp-<uid>/control.sock`. Directories are private to the current user. A held advisory lock ensures one server, and a later launch recovers stale sockets left by crashes.

`state.db` holds the library and session. `server.log` holds diagnostics; a log larger than 5 MiB is rotated at the next server start. Run `vtamp doctor --json` for the exact paths and device information on your machine.

For isolated development or independent test instances, set an absolute, short `VTAMP_HOME`:

```sh
VTAMP_HOME=/tmp/vtamp-dev cargo run -- server start
VTAMP_HOME=/tmp/vtamp-dev cargo run -- status --json
VTAMP_HOME=/tmp/vtamp-dev cargo run -- server stop
```

This places data, covers, and the runtime socket under that directory. Keep the complete socket path under macOS's 104-byte limit. All clients for an instance must use the same `VTAMP_HOME`.

- **No sound:** inspect `vtamp doctor`, system output selection, player volume, and `last_error`. Sandboxed development shells may not see CoreAudio devices even when decoding succeeds. Run the built binary in a normal terminal for output-device access.
- **A changed output device:** this release does not proactively follow default-device changes. Pause and restart the server, then resume to open the current default device.
- **Missing songs:** rescan with `vtamp library scan`, wait for `scanning` to become false, and inspect `last_error` or the server log. Only supported local formats are scanned.
- **Client/server version mismatch after rebuilding:** stop the old server with its matching binary before replacing it. `doctor` reports paths; `server run` is also available as a foreground diagnostic command.
- **Terminal looks wrong:** try `--art halfblocks` or `--art none`. Normal errors and panics restore terminal modes; after an uncatchable kill, your shell's `reset` command can restore the terminal.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Core tests use a fake audio backend. Process integration tests run isolated real servers and cover concurrent startup, subscriptions, persistence, malformed requests, and stale-socket recovery without requiring an audio device. A tiny original synthesized AAC fixture tests extended-size MP4 `mdat` metadata and decoding in CI.

Optional local-media verification, without redistributing your music:

```sh
VTAMP_TEST_MUSIC_DIR=/path/to/m4a/files cargo test --test media -- --ignored
```

The implementation separates the queue state machine (`engine`), the rodio adapter (`audio`), metadata/catalog work (`library`, `store`), local transport (`wire`, `client`, `daemon`), platform paths (`platform`), and human/CLI interfaces (`tui`, `cli`). The server alone owns authoritative state and SQLite writes. Scans and artwork work run separately from playback control. See [the protocol notes](docs/protocol.md) for low-level integration.

To preview the landing page:

```sh
python3 -m http.server 8765 --directory site
```

Open `http://localhost:8765`. No build step or external network requests are needed. The demo is a simulation with original sample titles and artwork, not a browser audio player.

## Contributing and license

Small, focused changes are welcome. Include the behavior you changed, a reproducible example for bugs, and relevant validation. Avoid bundling personal music, private paths, cache files, or copyrighted album covers. Keep platform-specific work behind the existing boundaries.

vtamp is [MIT licensed](LICENSE). Its Rust dependencies retain their own licenses. The bundled Space Grotesk font is distributed under the [SIL Open Font License](site/fonts/OFL.txt). vtamp is an independent project inspired by the experience of classic desktop players, not an affiliation with Winamp.
