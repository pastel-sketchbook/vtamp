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
- Media keys/Now Playing integration has been discussed but is not implemented.
  Do not treat a feasibility discussion as a shipped feature.

## Code map

| Location | Responsibility |
| --- | --- |
| `src/model.rs` | Shared state, commands, replies, events, protocol version, queue identity |
| `src/engine.rs` | Playback state machine, queue edits, shuffle/history, repeat, output recovery |
| `src/audio.rs` | `PlaybackBackend` boundary, rodio adapter, default-device and stream monitoring |
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

## Protect the active listening session

Use `target/release/vtamp` explicitly when validating a new release build. A
`vtamp` on PATH or an already running daemon may still be an older executable.
Inspect `status --json` and `doctor --json` before drawing conclusions.

Default macOS paths are:

- Data, `state.db`, `ui.json`, `server.log`: `~/Library/Application Support/vtamp/`
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
- Library searches are paginated; `gg`/`G` span the current search results, not
  just the loaded page. Reject stale page responses and avoid playing stale rows
  while a destination page loads. Ctrl-F/B move ten entries in the focused list.
- Keep terminal input and incoming server/artwork messages immediately actionable
  through the event loop. Do not reintroduce a 100 ms polling gate for keys or
  image completion; the periodic tick is for time-based updates.
- Decode and encode artwork off the input loop. Keep the old cover visible while
  preparing a replacement, swap when ready, and reject obsolete generations.
  Show `No album art` for missing/failed artwork, not briefly during decoding.
  Layout resizing may need to hide an image that no longer fits. Hide pixel art
  under overlays and restore it when they close.

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
