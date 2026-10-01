# Agent guide

## Project and scope

vtamp is a local music player with a persistent playback server and detachable
terminal clients. The name combines virtual terminal and Winamp. Music must keep
playing when a TUI exits or a tmux client detaches.

- Single Rust crate, edition 2024, Rust 1.90 or newer. Use `Cargo.lock`.
- macOS is the supported platform. Keep platform-specific code isolated for
  future Linux support; do not claim Linux support without testing it.
- TUI: ratatui + crossterm; audio: rodio; transport: local Unix sockets;
  persistence: bundled SQLite. The website is static HTML/CSS/JS without a build step.
- Product UI and repository documentation are English. Match the user's language
  in conversation.
- Read `README.md` for user workflows, `docs/protocol.md` for transport semantics,
  and `PRODUCT.md`, `DESIGN.md`, and `docs/themes.md` for UI work. Check the code
  when older prose disagrees with behavior, and update affected documentation.
- macOS media keys and Now Playing are implemented in the playback server.
  The system chooses the active media app; vtamp does not intercept global keys.

## Code map

| Location | Responsibility |
| --- | --- |
| `src/model.rs` | Shared state, commands, replies, events, protocol version, queue identity |
| `src/engine.rs`, `src/queue_edit.rs` | Playback state machine, pure batch queue validation, explicit play-next priority, shuffle/history, stop reservations, output recovery |
| `src/audio.rs`, `src/audio/macos.rs` | Shared decoder entry point, macOS AudioToolbox AAC source, rodio adapter, default-device and stream monitoring |
| `src/media_controls.rs`, `src/media_controls/macos.rs`, `src/media_controls/app_bundle.rs`, `build.rs` | Media command mapping, Now Playing publication, main-thread AppKit loop, private signed app bundle and embedded identity |
| `src/daemon.rs` | Server lifecycle, serialized command handling, authoritative state and database writes, background scans |
| `src/client.rs`, `src/wire.rs` | Client connections, server startup, bounded framed JSON transport |
| `src/library.rs`, `src/store.rs` | Metadata/art extraction, catalog scans/search, SQLite persistence |
| `src/platform.rs` | Paths, instance isolation, private runtime directories |
| `src/cli.rs`, `src/main.rs` | CLI parsing, command dispatch, output and process entry point |
| `src/tui.rs` | Input/event loop, client state, responsive layout, navigation and overlays |
| `src/artwork.rs` | Terminal capability detection, Sixel/Kitty selection, tmux passthrough guard |
| `src/cover.rs` | Background image preparation, buffered cover swap, stale-result rejection |
| `src/theme.rs`, `src/settings.rs` | Semantic palettes and client-local `ui.json` preferences |
| `src/tmux.rs`, `vtamp.tmux`, `scripts/tmux-status.sh` | Bounded now-playing CLI output and optional tmux plugin |
| `site/` | Landing page, local font, real terminal screenshots |
| `assets/icon.png`, `assets/vtamp.icns`, `scripts/build-icons.py` | Approved V-meter icon and reproducible macOS/web size exports |
| `scripts/capture-site.py`, `scripts/capture-window.swift` | Isolated Ghostty + tmux screenshot capture and atomic publication |

## Development and checks

Run these for Rust changes, with targeted tests first when investigating a bug:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

`--offline` can be added when dependencies are already cached. The macOS workflow
in `.github/workflows/ci.yml` runs the checks above. Do not silently update the
lockfile to work around a build failure. Documentation-only changes do not need
an audio session or a full Rust rebuild.

For screenshot tooling or the tmux plugin, also run:

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
```

These Python tests need no personal music or GUI. The plugin tests use private
tmux servers and a PTY; they skip when tmux is unavailable. Report skips rather
than describing them as successful live validation.

- Engine unit tests use a fake `PlaybackBackend`; use them for deterministic
  transitions and regression cases. Inject a seeded RNG for shuffle assertions.
- `tests/server.rs` covers real isolated server processes, startup races,
  persistence, subscriptions, malformed requests, and stale sockets.
- `tests/media.rs` includes a small synthesized AAC fixture with an extended-size
  MP4 `mdat` atom. Preserve this regression; ordinary MP4 files do not cover it.
  All media tests use `audio::decode_file`, the same entry point as playback.
  Native AAC tests also cover stereo, codec selection, EOF, and seeking; they
  need macOS codec-service access but do not open an output device. If a sandbox
  blocks AudioToolbox (including PCM format setup), rerun outside the sandbox.
- `tests/themes.rs` and `tests/tmux.rs` cover CLI behavior and instance isolation.
- Tests requiring personal media or an audio device are ignored by default.
  Passing the default suite does not prove actual playback or terminal graphics.

Optional local checks:

```sh
VTAMP_TEST_MUSIC_DIR="$HOME/work/tapmusic/out" cargo test --locked --test media -- --ignored
VTAMP_TEST_AUDIO_FILE="/absolute/path/to/a-track.m4a" cargo test --locked --lib audio::tests::real_output -- --ignored
```

The real-output test is muted and needs a track at least 15 seconds long. It
injects output changes; it does not physically disconnect headphones. A sandbox
can prevent CoreAudio access even when decoding works. If device access fails,
distinguish environment restrictions from a player regression and use an
authorized normal-terminal or escalated run for actual output verification.

For real macOS media-key routing, build release and run
`python3 scripts/check-media-keys.py` in an interactive desktop session. It uses
generated silent audio, a muted private server, and a Swift key-posting helper.
Only the test helper requires Accessibility permission; vtamp's receiver does not.
This temporarily publishes a Now Playing item and posts global keys, so avoid
competing playback in other apps. Check Control Center artwork visually as well.
Ordinary tests and screenshot capture explicitly set `VTAMP_MEDIA_KEYS=0`, even
if the invoking environment enables it.

## Protect the active listening session

Use `target/release/vtamp` explicitly when validating a new release build. A
`vtamp` on PATH or an already running daemon may still be an older executable.
Inspect `status --json` and `doctor --json` before drawing conclusions.

Default macOS paths are:

- Data, `state.db`, `ui.json`, `server.log`: `~/Library/Application Support/vtamp/`
- Generated media-server app bundles: `macos/<generation>/vtamp.app` inside that data directory
- Cover cache: `~/Library/Caches/vtamp/covers/`
- Socket: `/tmp/vtamp-<uid>/control.sock`

Use a unique, short, absolute `VTAMP_HOME` for mutating tests. All test clients
must use the same value. Keep the complete socket path below macOS's 104-byte
limit. For example, after building:

```sh
vtamp_test_home=$(mktemp -d /tmp/vtamp-agent.XXXXXX)
VTAMP_HOME="$vtamp_test_home" target/release/vtamp server start
VTAMP_HOME="$vtamp_test_home" target/release/vtamp volume 0
VTAMP_HOME="$vtamp_test_home" target/release/vtamp status --json
# Run test commands with the same VTAMP_HOME.
VTAMP_HOME="$vtamp_test_home" target/release/vtamp server stop
```

Automated harnesses must stop their own servers and clean up temporary files in
`finally`/cleanup handlers, including failures. Scope tmux cleanup to the private
socket; never issue an unqualified `tmux kill-server` against the user's session.
Do not seek, clear, replace, or stop the user's playback just to run a test unless
that interaction is part of the authorized task. Prefer a paused, muted copy.
Use SQLite's backup API with a read-only source connection for a live database
snapshot; copying `state.db` alone can omit WAL contents.

The maintainer's `~/work/tapmusic/out/*.m4a` files may be used locally. Do not
modify them or commit music, databases, caches, logs, or standalone album art.
Some tracks genuinely have no cover: inspect metadata and sidecar files before
diagnosing a graphics failure. Bills and Liebestraum have been useful local
missing-art cases, but their presence is not a portable test prerequisite.

## Agent CLI contracts

- Protocol and database versions are 2. New server startup migrates v1 databases
  atomically, preserving IDs; old binaries reject v2. Do not restart the listener
  just to upgrade during an active user listening session.
- `now`, paginated `queue list`, field search, track lookup, scan-status, and sleep
  status are read-only and never auto-start a server. Dry-run edits also must not
  start a server or write state. Unpaginated queue list retains its array response.
- Batch queue edits validate a candidate before any mutation. Protect the current
  entry from removal and preserve output/position. Commit the session and optional
  request receipt in the same DB transaction before publishing the candidate.
- Request IDs cover parsed edits plus their revision guard. Replay before checking
  the guard; preserve successful receipts for 24 hours across restarts. Do not
  evict unexpired receipts to make room. This applies to new batch edits/adds,
  not legacy playback or path-import commands. Report the limits accurately.
- Queue revision tracks entry membership/order, explicit play-next entries, and
  current entry identity across every mutation path and natural advancement.
  Position/volume/pause do not invalidate queue guards. Avoid cloning the full
  queue in the periodic playback tick when nothing changed.
- Explicit play-next entries precede shuffle in their supplied order; repeat-one
  still wins on natural endings unless after-current stop is scheduled. Preserve
  the remaining shuffle pool and persist pending entries across restart.
- Scan jobs run off the owner thread. Commit successful catalog replacement and
  the terminal report together; retain 100 terminal jobs. On startup mark running
  jobs interrupted. Wait timeouts do not cancel work. Count all warnings but cap
  detailed path/message records at 100.
- Stop reservations run in the server, including during output recovery. A new
  reservation replaces the old one. After-current binds to an entry and is cleared
  when that identity changes; deadlines use wall time, including pause/system sleep.
  Stop/cancel/restart clears reservations. No timer may resume playback.
- Use fake clocks/backend tests for ordering and timers, transaction failure
  injection for edits, and isolated process tests for receipts and scan reports.

## Behavior to preserve

- `q`, Escape, and Ctrl+C detach the TUI. `vtamp stop` stops playback and resets
  position; **`vtamp server stop` stops the daemon**. Restart restores paused.
- TUI-only changes need a new attachment, not a daemon restart. Engine/audio/server
  changes need the running daemon restarted to take effect. Explain this clearly
  when delivering a rebuilt binary; rebuilding does not replace a running process.
- Read-only status/query commands do not start a server. Theme commands use local
  preferences without connecting to it. Preserve machine-readable JSON and exit codes.
- A disconnected or timed-out mutation has an unknown outcome. Inspect state
  before retrying `next`, queue additions, or another non-idempotent operation.
- Queue entry IDs differ from library track IDs. Library Enter / `play --track`
  reuses the current matching entry, then the first match, and appends only if
  absent. Explicit enqueue permits duplicates. Never silently deduplicate a queue.
- Shuffle changes playback order, not visible queue order. New entries must be
  mixed into the remaining unplayed pool without reintroducing visited entries.
  Test both manual next and natural endings. Repeat-one applies only to natural
  endings; manual next must still advance.
- Output-device recovery must retain track, position, volume, pause state, queue,
  and shuffle/history. An unavailable output is not an unreadable track to skip.
- On macOS, only AAC uses AudioToolbox; inspect the actual codec rather than the
  extension so ALAC/m4a remains on Symphonia. Do not add an AAC software fallback.
  Keep `symphonia-codec-aac` out of the macOS target dependency graph (it may remain
  in Cargo.lock for non-macOS). Native handles have exclusive ownership and are
  disposed on every exit; preserve bounded PCM buffering and frame-based seeking.
- Spectrum visualization is read-only and observes decoded samples before volume.
  Keep FFT, allocation, locks, and socket I/O out of the analysis tap's sample path.
  Use bounded lossy storage and latest-value streams; no subscribers means no FFT.
  Preserve stereo energy without phase cancellation. Flush stale analysis on seek,
  pause/resume, track changes, and output recovery. SpectrumWatch is an optional
  protocol-2 stream separate from State/Event, without database or revision writes.
  `v` toggles a client preference. Hidden-list keys must not affect playback/queue;
  Tab returns to the previous list and slash opens Library search. Theme saves and
  spectrum toggles must preserve each other's stored preference.
- Media controls default on for the ordinary macOS server and off with
  `VTAMP_HOME`; `VTAMP_MEDIA_KEYS=0|1` overrides this at server startup. Only the
  server-lock owner registers handlers. Keep AppKit on the main thread, audio
  and transport off it, and command callbacks nonblocking. Publish Now Playing
  only after playback begins; retain it on pause, clear it on stop/shutdown,
  freeze progress during output recovery, and discard obsolete artwork results.
  Media-enabled server startup prepares and re-execs a private app bundle before
  AppKit starts. Its ad-hoc signing identifier must match `CFBundleIdentifier`,
  and Launch Services must register the final bundle path for Now Playing icons.
  Do not use a personal signing certificate or require an Apple developer account.
  Preserve immutable bundle generations, concurrent startup and CLI-only fallback.
  Keep the API and dependencies macOS-specific; do not add a global keyboard hook.
- Library searches are paginated; `gg`/`G` span the current search results, not
  just the loaded page. Reject stale page responses and avoid playing stale rows
  while a destination page loads. Ctrl-F/B move ten entries in the focused list.
  `/` opens a blank search draft; Esc preserves the applied filter, while Enter
  applies the draft (an empty draft clears the filter).
- Keep terminal input and incoming server/artwork messages immediately actionable
  through the event loop. Do not reintroduce a 100 ms polling gate for keys or
  image completion; redraw deadlines are for time-based updates. Suppress empty
  terminal diffs, and stop the 20 Hz spectrum timer once bars and peaks settle.
  Preserve progress/notice expiry and invalidate presentation after terminal clears.
- Decode and encode artwork off the input loop. Keep the old cover visible while
  preparing a replacement, swap when ready, and reject obsolete generations.
  Show `No album art` for missing/failed artwork, not briefly during decoding.
  Layout resizing may need to hide an image that no longer fits. Hide pixel art
  under help/theme overlays and restore it when they close. Bottom search/folder
  prompts do not overlap the cover; keep it visible without clearing the terminal.

## Terminal and UI verification

The maintainer uses Ghostty inside tmux. Verify graphics fixes in that combination
with real pixels; `tmux capture-pane` and ratatui buffer tests can establish text
and layout behavior but cannot prove that an album cover rendered correctly.
Discover current panes, client capabilities, and dimensions instead of hardcoding
pane IDs, PIDs, window IDs, or a previously observed tmux version.

Automatic art selection inside tmux checks native Sixel support in both tmux and
its attached client terminals, then probes Kitty, then falls back to halfblocks.
Ghostty + tmux has been verified using Kitty. A Sixel-capable tmux alone is not
enough: rows of `+` or `SIXEL IMAGE` can be tmux placeholders. Sixel is sent natively;
Kitty uploads use passthrough and Unicode placeholders. Keep passthrough changes
pane-local and restore them on detach or detection failure. Probe before starting
the crossterm event reader; automatic Kitty replies require the active pane.

Exercise track changes, rapid navigation, missing art, theme/help overlays,
resizing, and detach after graphics/input changes. Important layout boundaries:
40×12 minimum; 72+ columns and 12–27 rows use player/browser columns; 28+ rows use
the stacked layout; 100+ columns in the tall layout show both Library and Queue.
Use semantic theme roles. The TUI defaults to Catppuccin Mocha; the website keeps
its charcoal/green identity. Client theme preferences must not mutate playback.

For tmux status work, preserve the user's format and settings. The plugin only
substitutes `#{vtamp}`; it must not impose colors, clocks, keys, refresh intervals,
or width. Status requests have a 500 ms deadline and must not start the daemon.
Unavailable/stopped state emits an empty line to clear old text. Preserve Unicode
graphemes and escape metadata for tmux; do not interpolate it as shell code.

## Website, browser, and screenshots

Preview with `python3 -m http.server 8765 --directory site`. Use the system-installed
**agent-browser** CLI for all browser work, always headed and with the repository
session name. Do not substitute another browser tool.

```sh
agent-browser --headed --session vtamp open http://localhost:8765
agent-browser --headed --session vtamp snapshot
agent-browser --headed --session vtamp click @e1
agent-browser --headed --session vtamp screenshot
```

Read pages with `snapshot`; refs come from the latest snapshot. See
`agent-browser --help` for other commands. Check mobile overflow, keyboard access,
and reduced motion for relevant website changes. No frontend dependency install
is needed.

Refresh the real terminal gallery with the repository capture tooling:

```sh
python3 scripts/capture-site.py --output /tmp/vtamp-screenshots
python3 scripts/capture-site.py
```

The first command stages images for inspection; the second replaces the website
set. Requirements: macOS, `/Applications/Ghostty.app`, tmux, Python 3, Rust,
`swiftc`, and screen-recording permission. Select the desired covered track in the
source instance first. The curated gallery uses Training Montage by Vince DiCola;
preserve that choice unless the user requests another. The script copies live
state into an isolated paused instance, captures Wide / Compact Library / Compact
Queue, verifies artwork pixels, publishes atomically, and restores the front app.
Do not replace this with text-only terminal screenshots or a fabricated mockup.
Inspect all resulting PNGs. Temporary capture diagnostics can contain local paths
and song titles. Album art depicted in approved screenshots retains its owners'
rights and is not covered by the project's MIT license.

## Git and delivery

The maintainer authorizes commits in logical units without repeated permission.
Review the diff, stage only task changes, and preserve unrelated work. Do not push
unless explicitly requested, and never ask “Should I push?”

Use English Conventional Commits with a concise title, exactly one blank line,
then mandatory bullets explaining the changes. No blank lines between bullets,
extra leading/trailing blank lines, or AI attribution metadata.

```text
fix(playback): shuffle newly queued tracks

- Mix additions into the remaining unplayed pool
- Cover natural endings and manual advancement with regression tests
```

Invoke `git commit` directly using `-m "title" -m "body"` with literal newlines,
or `git commit -F <message-file>`. Never use ANSI-C `$'...'` quoting or wrap the
commit in `sh -c` / `zsh -lc`.

Report what changed, relevant checks and their limits, the commit, and any needed
reattach/server restart. Do not claim live playback or visual verification from
unit tests alone. Keep this guide aligned with the implementation as it evolves.
