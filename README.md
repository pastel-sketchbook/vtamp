<div align="center">
  <img src="site/mark.png" width="64" alt="vtamp">
  <h1>vtamp</h1>
  <p><strong>A music player for the terminal that keeps playing after you detach.</strong></p>
  <p>Local files and live radio. One persistent playback server, any number of terminal clients, and a JSON CLI for scripts and agents. Rust. macOS first.</p>
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

On an Apple Silicon Mac, `brew install rath/tap/vtamp` is all it takes; see [Install](#install) for the source build.

## What ships in v0.1

- Persistent playback server, with multiple TUI and CLI clients.
- Folder-based library, title/artist/album search, and an editable shared queue.
- Live radio on macOS: register HTTP(S) URLs or import M3U/PLS channel lists; HLS, MP3, and AAC use native playback.
- AAC and ALAC in m4a/MP4, MP3, FLAC, WAV, and Ogg Vorbis playback.
- Embedded album covers, sidecar covers, and a built-in fallback image.
- Read-only audio spectrum with multicolor bars and falling peaks; press `v` to toggle.
- Nine color themes, including Catppuccin Mocha and Latte, with live previews and saved preferences.
- High-resolution album art via Sixel or Kitty graphics, automatically detected with a color halfblock fallback.
- Play, pause, seek, volume, shuffle, repeat, and automatic track advancement.
- macOS media keys and Now Playing metadata, including album art, after detaching.
- Agent-friendly CLI: compact now-playing JSON, field filters, atomic queue edits with retry receipts, scan reports, and server-owned stop timers.
- JSON commands and an event stream for scripts and AI agents.
- An optional tmux status-bar plugin for the current track and playback time.
- Saved queue, playback position, volume, shuffle, and repeat settings.
- A small, dependency-free [landing page](site/).

macOS is the supported platform for this release. The platform and playback boundaries are isolated for future Linux support; Linux is not yet part of the tested support matrix. Named playlists, EQ, crossfade, gapless playback, and login-time startup are not implemented.

## Install

### Homebrew

On an Apple Silicon Mac:

```sh
brew install rath/tap/vtamp
```

The formula installs a prebuilt, self-contained binary from the
[GitHub release](https://github.com/rath/vtamp/releases); Rust is not required.
Upgrade with `brew update && brew upgrade vtamp`, then run `vtamp server stop`
so the next `vtamp` starts the new build: a running playback server is never
replaced by an install. Intel Macs build from source.

### Install from source

You need **Rust 1.90 or newer** and the macOS Command Line Tools (`xcode-select --install` if you do not have them). Clone the repository and install:

```sh
git clone https://github.com/rath/vtamp.git
cd vtamp
cargo install --locked --path .
```

Make sure `~/.cargo/bin` is on your `PATH`. Or run directly from the checkout:

```sh
cargo run --release
```

There is no published crates.io package. Local playback does not require FFmpeg, Chafa, a Node runtime, or a separate database installation. SQLite is bundled. Optional installed-tool imports are described in [the integration guide](docs/imports.md).

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

## Live radio

Register a channel using its stable HTTP(S) URL and a name:

```sh
vtamp library stream add 'https://radio.bsod.kr/stream?stn=kbs&ch=1fm' --name 'KBS 1FM'
vtamp play --track STREAM_ID
vtamp library stream import ~/Downloads/seoul.m3u --preview
vtamp library stream import ~/Downloads/seoul.m3u
vtamp library stream remove STREAM_ID
```

`STREAM_ID` is the registration response’s `first_registered_id`.

For [radio.bsod.kr](https://radio.bsod.kr/), copy the channel's **fixed URL**
(고정 URL), or export the selected region as M3U or PLS. A final broadcast URL
may contain an expiring token; vtamp reconnects using the URL you registered.
No yt-dlp, FFmpeg, account, or download step is needed for radio playback.

In the TUI, press **a** and enter a stream URL, then its channel name. The same
prompt accepts a local M3U/PLS file; review its channels and press Enter to add
all. Registration focuses Library and selects the added channel (the first new
channel for a playlist, or the existing channel for a duplicate). A filter is
cleared only when it hides that channel. Press Enter to play it immediately.
Escape cancels. Lists must be UTF-8, at most 1 MiB and 1000 channels, and
contain HTTP(S) entries. Invalid entries reject the whole import. HLS manifests
are playback inputs, not channel lists: register their HTTP(S) URL instead.
`--preview` only reads the file, without starting the server or writing state.

Channels appear in Library and Queue with **LIVE**. Enter, Ctrl+Enter, and **e**
retain their usual play/direct-play/enqueue behavior. Duplicate registrations
keep the existing name and ID; explicit queue additions still allow duplicates.
Folder rescans preserve channels. Press **d** on a Library channel and confirm
to unregister it; current playback and queued copies remain available. To change
a registered name or URL, remove it and register it again.

The server shows Connecting, Buffering, LIVE, or Reconnecting. Pause closes the
connection; resume connects to the current broadcast. A server restart restores
radio paused without contacting the station. Network interruptions retry the same
channel with backoff (1, 2, 4, 8, 16, then 30 seconds); pause, stop, and switching
channels cancel retries. Unsupported sources pause with a diagnostic. A dropped
connection does not advance the queue. Next/previous remain manual navigation.

Live radio has no seekable timeline, natural ending, or spectrum in this release.
Use `vtamp sleep set 30m` to stop on a timer; `stop --after-current` rejects live
channels. Native playback supports HLS and HTTP(S) MP3/AAC; login/DRM services,
YouTube live, recording, and timeshift are outside this feature. Availability
still depends on the broadcaster and your network/location.

## The interface is disposable

```text
 TUI in pane A ──┐
 TUI in pane B ──┼── local Unix socket ── playback server ── audio device
 CLI / agents ──┘                            │
                                      library + session
```

`vtamp` and `vtamp attach` start a per-user background server if none is running. It is a detached process using the same binary, with no controlling terminal. Exiting the interface, closing its pane, or detaching tmux leaves playback running.

`q`, `Esc`, and `Ctrl+C` close the TUI. `Esc` first closes an open search or folder prompt, then clears an applied search; once no search is applied, it closes the TUI. **Stopping playback and stopping the server are separate actions:**

```sh
vtamp stop          # Keep the server and queue; reset playback position.
vtamp server stop   # Save the session and stop the server.
vtamp server start  # Restore the session, paused.
```

The server saves queue/configuration changes immediately and checkpoints a changing position every five seconds. Unchanged paused/stopped sessions do not keep writing checkpoints. A normal server stop saves the latest position. After an unexpected crash, up to five seconds of position may be lost. A server restart always restores the current track **paused**, so merely inspecting or attaching does not unexpectedly start sound. The server is not a login service and does not restart itself after logout or a crash.

Read-only commands (`status`, `watch`, volume without a value, queue listing, library queries, and server status/stop) do not start a server. Playback and library mutation commands do. A disconnected TUI waits for the server to return; it does not replay commands whose outcome might be unknown.

## macOS media keys and Now Playing

Start a track in vtamp, then use the keyboard's **play/pause**, **previous**, and
**next** media keys (usually on F8, F7, and F9). They keep working after you close
the TUI or detach tmux. Depending on your keyboard settings, hold `Fn` to send the
media action instead of an ordinary function key.

The server registers with macOS using `MPRemoteCommandCenter` and supplies the
title, artist, album, cover, duration, and playback position to Now Playing.
System play/pause, previous/next, stop, and playback-position commands use the
same queue and playback behavior as the CLI. The controls and information shown
depend on the macOS surface; Control Center does not always show a timeline.
No separate app installation, Dock icon, keyboard monitoring, or Accessibility
permission is needed by vtamp.

The V-meter application icon matches the website. On first media-enabled server
startup, vtamp prepares a private app bundle under its data directory's
`macos/` folder, registers it with Launch Services, and runs the server from it.
The bundle is signed locally with `codesign --sign -` (ad-hoc signing): no Apple
developer account, certificate, keychain identity, or network service is used.
This supplies macOS with the icon and matching application identity; it is not
Developer ID signing or notarization for distributing downloaded binaries.
Unchanged builds reuse their bundle. A new build gets its own generation so an
existing server is never overwritten. These generated bundles can be removed
with the server stopped and are recreated on the next start.

macOS chooses which app receives media commands. Starting playback in another
app can move control there; vtamp does not globally intercept or monopolize the
keys. Merely starting the server or restoring a paused session does not publish
a Now Playing item. Start playback in vtamp to make it eligible. Pausing retains
the item; stopping, clearing the queue, or shutting down the server removes it.

This integration is on by default for the regular macOS server. Set
`VTAMP_MEDIA_KEYS=0` **when starting the server** to disable it:

```sh
vtamp server stop
VTAMP_MEDIA_KEYS=0 vtamp server start
```

With `VTAMP_HOME` set, it defaults off to keep test instances from taking media
commands. Set `VTAMP_MEDIA_KEYS=1` to explicitly enable it for such an instance.
Only `0` and `1` are accepted; an invalid value disables integration with a
warning in the server log. Changing the environment or rebuilding requires a
server restart; reattaching a TUI does not change a running server's integration.
If keys do not work, check which app macOS is controlling and inspect `server.log`
for `macOS media controls registered`. Normal CLI playback remains available if
the desktop integration cannot initialize.

## Keys

| Key | Action |
| --- | --- |
| `q`, `Esc`, `Ctrl+C` | Detach the interface (`Esc` clears an applied search first) |
| `Space` | Play / pause |
| `n` / `b` | Next / previous track |
| `←` / `→` | Seek backward / forward 10 seconds |
| `+` / `-` | Volume up / down 5 percentage points |
| `s` | Toggle shuffle |
| `R` | Cycle repeat: off → all → one |
| `Tab`, `Ctrl-W w`, `Ctrl-W Ctrl-W` | Switch library / queue focus |
| `j` / `k`, `↓` / `↑` | Move selection |
| `PageDown` / `PageUp`, `Ctrl-F` / `Ctrl-B` | Move selection down / up by ten entries |
| `gg` / `G` | Select the first / last entry in the focused list; Library jumps across pages in the current search results |
| `/` | Start a blank title/artist/album search; Enter applies (empty clears), Esc keeps the current filter. Outside the prompt, `Esc` clears the filter |
| `Ctrl-U` | Clear the text in a search, folder, or track-editor field |
| `a` | Add a folder, stream URL, or M3U/PLS channel list |
| `r` | Rescan registered folders |
| `[` / `]` | Previous / next library page (200 tracks) |
| `Enter` | Play a library track, reusing a queue entry if present; or play the selected queue entry |
| `Ctrl+Enter` | Play the selected Library track without adding it to Queue |
| `e` | Append a library track without interrupting playback; duplicates allowed |
| `x` / `d` | Remove the selected queue entry; Library `d` unregisters a stream after confirmation |
| `J` / `K` | Move the selected queue entry down / up |
| `v` | Toggle the read-only audio spectrum |
| `t` | Preview and choose a color theme |
| `?` | Show key reference |

Help scrolls with `↑`/`↓` or `j`/`k`; `PageUp`/`PageDown` and `Ctrl-B`/`Ctrl-F`
move by a page, and `Home`/`End` jump to the top/bottom. Press `Esc`, `q`, or `?`
to close help. Scroll hints and position stay visible when content overflows;
when everything fits, only the close hint is shown.

When playing a library track, vtamp reuses the current queue entry if it matches, otherwise the first matching entry from the top. It appends only when the track is absent. Existing duplicates stay in place; use `e` to add another copy intentionally.

**Ctrl+Enter** in Library plays a track without adding it to Queue. Now Playing
shows **NO QUEUE**. Queue contents, its playback cursor, and pending play-next
entries stay intact. When the track finishes, or you press `n`, playback continues
with the next queued entry after the saved cursor; with no cursor, it starts at the
front. Explicit play-next entries and shuffle retain their usual priority. If
there is no next entry, playback stops. Repeat-one repeats the direct track;
repeat-all cycles the queue, or the direct track when the queue is empty. `b`
restarts the direct track. Clearing Queue does not stop a direct track.

Ctrl+Enter requires a terminal that reports modified Enter separately. vtamp
requests this mode from Ghostty/Kitty or tmux without changing saved terminal
settings. Terminals that send plain Enter retain the ordinary Enter behavior;
the CLI option below is available independently of keyboard support.
Inside tmux, `extended-keys` must be enabled for Ctrl+Enter to reach the app.

Attaching during queued playback focuses Queue and scrolls to the current entry. Direct playback, paused, or stopped sessions open on Library. Later playback updates and automatic reconnections preserve your navigation. If the saved spectrum view would hide Queue, it starts hidden for this attachment without changing `ui.json`; press `v` to show it.

Short panes (12–27 rows, at least 72 columns wide) show two columns: now playing on the left, and Library or Queue on the right. The cover sits above the track details and scales to the available space; `Tab` switches the right-hand list. With 28 or more rows, now playing returns to the top, with Library and Queue below (both visible from 100 columns). Narrower panes keep the stacked layout. Below 40 columns or 12 rows the UI shows a compact size notice and still allows detaching.

## See the music

Press **`v`** for a live audio spectrum: bass on the left, treble on the right,
with green, yellow, and red height zones and falling peak markers. Colors follow
your theme. This is a visualization, not an equalizer; it never changes the sound.
Analysis uses the decoded signal before app volume, so the bars also move when muted.
Mono and stereo are supported; multichannel files visualize the front stereo pair.

In short panes, the spectrum replaces the Library/Queue area. **`Tab`** returns to
the previous list; **`/`** returns to Library and opens a blank search. Hidden lists
cannot be played or edited with selection keys. At 28+ rows and 72+ columns, the
spectrum occupies the right half of Now Playing while lists remain available below.
Narrower panes use the list area. **`v`** closes it in either layout.

The view starts off and remembers your choice in `ui.json`. Each attached TUI has
its own visibility; toggling one does not change another. Analysis runs only while
a spectrum view is subscribed, and slow displays drop old frames instead of
holding up playback. Pause, stop, and missing data let the bars settle to zero.
Animation stays at 20 fps while needed; settled bars stop the animation timer,
and unchanged screens send no terminal updates.
After upgrading from a server without spectrum support, restart the server with
the new binary and reattach. Client and server must use the same protocol version.

## Make it yours

vtamp opens in **Catppuccin Mocha**, with a peach accent and quiet, dark panels. Press **`t`** to preview all nine themes: Catppuccin Mocha, Catppuccin Latte (light), Rosé Pine, Gruvbox, Tokyo Night, Nord, Dracula, Kanagawa, and the original green Classic.

Use `↑` / `↓` or `j` / `k` to preview the whole interface. `Enter` saves; `Esc` or `q` cancels and restores the previous theme. Playback continues while you browse. The picker stays in the list area so the player and album cover remain visible. Album art keeps its original colors; the no-cover illustration follows the theme.

```sh
vtamp theme list                       # Names and stable CLI identifiers.
vtamp theme current --json             # Read the saved default.
vtamp theme set rose-pine               # Save a default without starting the server.
vtamp --theme catppuccin-latte           # Override just this attachment.
vtamp attach --theme gruvbox
```

The picker applies the saved choice to its own TUI immediately. Other open TUIs keep their current appearance; new attachments use the saved default. `--theme` overrides the saved preference for one attachment and does not write settings. Choosing a theme with the picker and pressing `Enter` explicitly saves it, including when launched with `--theme`.

Preferences live in `ui.json` beside `state.db` (or `$VTAMP_HOME/ui.json`). No server restart is needed. Missing preferences use Mocha. Invalid or unreadable preferences show a TUI warning and fall back to Mocha (or the explicit `--theme` override); they are not overwritten until you explicitly save. `theme current` reports a settings error instead of guessing, and `theme set NAME` can repair invalid contents. All theme commands support `--json` and run without connecting to the playback server.

See [theme variants and palette credits](docs/themes.md). Custom palettes and automatic OS light/dark switching are not part of this release.

## Covers, terminals, and tmux

Album covers render as high-resolution pixel images through Sixel or Kitty graphics when supported. vtamp detects graphics support when you attach. Inside tmux, it first checks native Sixel support in **both tmux and the terminals attached to the pane's session**. Otherwise, it queries the outer terminal for Kitty graphics support. Ghostty + tmux uses this Kitty path for high-resolution covers. Color halfblocks are the fallback when neither graphics path is available.

| Option | Rendering |
| --- | --- |
| `--art auto` | Native Sixel or detected Kitty graphics in tmux; detected graphics protocol elsewhere; halfblocks as fallback |
| `--art halfblocks` | Unicode upper/lower blocks with foreground and background colors |
| `--art sixel` | Force native Sixel graphics when support is known but automatic detection fails |
| `--art kitty` | Explicit Kitty graphics protocol; requires terminal/multiplexer support |
| `--art none` | Hide album art |

```sh
vtamp                     # Automatically select pixel graphics when supported.
vtamp --art sixel
vtamp --art halfblocks
vtamp --art kitty
```

Sixel is sent directly to the current terminal or tmux pane, without passthrough wrapping, so a Sixel-enabled tmux can manage the image across pane redraws and window switches. This requires native Sixel support in both tmux and its client terminal; tmux's own Sixel response alone is insufficient. Each terminal probe waits at most 250 ms. Explicit `--art sixel` still queries cell dimensions, with a 10×20-pixel estimate if the terminal supplies none.

If forced Sixel shows `SIXEL IMAGE` or rows of `+`, tmux is substituting its text placeholder because its client cannot render Sixel. Use `--art auto` to select a compatible protocol. Building tmux with Sixel support does not add that protocol to the outer terminal. Ghostty supports the [Kitty graphics protocol](https://ghostty.org/docs/features), which automatic mode detects through tmux passthrough.

For Kitty graphics, vtamp temporarily enables `allow-passthrough on` **only for its own pane**, and restores the previous setting on normal detach or failed detection. Existing `on`/`all` settings are preserved. Global options and configuration files are never changed. Pixel uploads use passthrough; Unicode placeholders let tmux keep the image positioned with its cells. Automatic Kitty detection runs only in the active pane because terminal replies are routed there; launch from the pane you are using, or explicitly select `--art kitty` for a known-compatible setup.

Halfblocks need no graphics passthrough. Use `--art halfblocks` if graphics are unavailable in your terminal or multiplexer version. For true color, configure your terminal and tmux for RGB color if necessary.

Artwork comes from the embedded front cover first, then the first embedded picture, then `cover.jpg`, `cover.png`, `cover.jpeg`, `folder.jpg`, `folder.png`, `Folder.jpg`, or `Cover.jpg` beside the audio. Artwork is cached at up to 512 pixels on the long side and keeps its own shape, as imported YouTube thumbnails do; the player sizes the cover area to the image and scales the artwork to fill it, so a wide thumbnail is drawn in full without bars or losing pixels. Missing or undecodable art uses a built-in image. Image decoding, resizing, and Sixel encoding run outside the UI input loop.

## tmux status bar

Keep the current track visible after detaching the TUI:

```text
▶ Training Montage · 0:56 / 3:39    22:41 30-Sep-26
```

The optional plugin displays the title and elapsed / total time. Pausing keeps the
track visible with `Ⅱ`; stopping playback, clearing the current track, or shutting
down the server hides the segment. It inherits your status-bar colors and needs no
Nerd Font, jq, Python, or additional background service.

Install the latest vtamp binary with [Homebrew or from source](#install),
then add this to `~/.tmux.conf`, **after any theme or other status-bar settings**:

```tmux
set -g status-interval 1
set -g status-right-length 120
set -g status-right '#{vtamp} %H:%M %d-%b-%y'
run-shell '/absolute/path/to/vtamp/vtamp.tmux'
```

Replace the path with your source checkout and reload with `tmux source-file ~/.tmux.conf`.
You can put `#{vtamp}` anywhere in your existing `status-right` or `status-left`
instead of replacing its contents. The plugin only substitutes that placeholder;
it does not change your colors, keys, refresh interval, or status-bar length.
Repeated loading does not insert duplicate segments.

For [TPM](https://github.com/tmux-plugins/tpm), use `set -g @plugin 'rath/vtamp'`
instead of the `run-shell` line above, before your existing TPM initialization.
Press your tmux prefix followed by `I` to install the plugin. TPM downloads the
repository but does not build Rust code: if vtamp is not installed yet, run
`brew install rath/tap/vtamp` or `cargo install --locked --path ~/.tmux/plugins/vtamp`
(adjust for a custom TPM directory).

Optional settings, placed before loading the plugin:

```tmux
# Executable path only, not a shell command. Useful if tmux has an older PATH.
set -g @vtamp-bin '/absolute/path/to/vtamp'
set -g @vtamp-max-width 50
set -g @vtamp-show-artist off
```

Without `@vtamp-bin`, the plugin searches tmux's PATH, then `~/.cargo/bin/vtamp`.
The width includes the indicator and times (20–200 terminal cells, default 50).
Long titles are shortened with `…`, preserving whole Unicode graphemes and the
times. Set `@vtamp-show-artist on` to include the artist after the title. Missing
binaries produce an empty segment; diagnose installation with `vtamp --version`.

The underlying command is also available directly:

```sh
vtamp tmux status
vtamp tmux status --max-width 70 --show-artist
vtamp tmux status --json
```

It reads the existing server with a 500 ms deadline and never starts one. An
unreachable, busy, incompatible, or unresponsive server produces an empty line
instead of leaving stale text or errors in the bar. JSON output uses the usual
response envelope with `data.text`; both modes return tmux-escaped text, so a
literal `#` in metadata becomes `##`. Use `vtamp status --json` for raw metadata
or connection diagnostics. `VTAMP_HOME` is supported; for tmux jobs, set it in
the tmux server environment with `tmux set-environment -g VTAMP_HOME /absolute/path`.

## Commands

Run `vtamp --help` or `vtamp COMMAND --help` for argument details. All non-TUI commands accept `--json` before or after the command.

| Command | Behavior |
| --- | --- |
| `play [PATH...]` | Resume, or append paths and play the first new entry |
| `play --track ID` | Play a library track, reusing a queue entry if present; append only if absent |
| `play --no-queue --track ID`, `play --no-queue FILE` | Play one library track or audio file outside Queue, then continue the existing queue |
| `play --queue-item ID` | Play an existing queue entry |
| `pause`, `resume`, `toggle` | Playback state controls; pause/resume are idempotent |
| `stop`, `stop --after-current` | Stop now or when the selected track ends |
| `sleep set 30m`, `sleep status`, `sleep cancel` | Set, inspect, or cancel a server-owned stop reservation |
| `next`, `prev` | Move to the next or previous track |
| `seek 90`, `seek +10`, `seek -10` | Absolute or relative seconds; decimals supported |
| `volume [0..100]` | Read or set volume |
| `shuffle on\|off` | Shuffle without repeating entries within a traversal |
| `repeat off\|one\|all` | Repeat mode; repeat-one affects natural endings, not manual skipping |
| `queue list [--offset N] [--limit N]` | List queue entries; optional pagination includes a queue revision |
| `queue add --tracks ID... [--after-current]` | Atomically add library tracks, optionally ahead of shuffle |
| `queue edit --file FILE [--dry-run]` | Atomically apply add/remove/move operations, protecting the current entry |
| `queue add PATH...`, `queue add --track ID` | Append without starting playback |
| `queue remove ID` | Remove a queue entry |
| `queue move ID INDEX` | Move an entry to a **zero-based** destination index |
| `queue clear` | Stop playback and empty the queue |
| `library add PATH`, `library remove PATH` | Register/unregister a directory; never delete music files |
| `library scan [--wait] [--timeout 60s]` | Rescan and optionally await a report; add/remove also accept wait options |
| `library scan-status JOB_ID` | Inspect a running or recent scan |
| `library cover refresh [TRACK_ID\|all] [--wait]` | Re-fetch thumbnails and rebuild square covers; every managed YouTube import unless a track ID is given |
| `library cover status JOB_ID` | Inspect a cover refresh job; reports live in server memory only |
| `library track ID` | Read one indexed track |
| `library list [--offset N] [--limit N]` | List indexed tracks, default 200, maximum 1000 per page |
| `library search [QUERY] [--title TEXT] [--artist TEXT] [--album TEXT] [--exact] [--exclude TEXT]` | Combine normalized field filters and exclusions; supports pagination |
| `library roots` | Show registered roots |
| `status` | Current playback state and queue |
| `now` | Current track, remaining time, and settings without the full queue |
| `tmux status [--max-width N] [--show-artist]` | One tmux-safe now-playing line; empty when stopped or unavailable |
| `watch` | Initial state, state changes, progress heartbeats, and library events |
| `server start\|status\|stop` | Explicit server lifecycle |
| `doctor` | Paths, connectivity, terminal environment, and default output device |
| `theme list\|current\|set NAME` | List themes, read the saved default, or save it for future attachments |

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
{"version":6,"ok":true,"data":{"scanning":true,"job_id":"SCAN_JOB_ID"}}
```

```json
{"version":6,"ok":false,"error":{"code":"server_unavailable","message":"Cannot connect to vtamp…"}}
```

`status` returns `queue`, `current_id`, `status`, `position_ms`, `volume`, `shuffle`, `repeat`, `revision`, `queue_revision`, `play_next`, `scheduled_stop`, `scanning`, and `last_error`. Each queue entry contains `id` and `track`; each track includes its library ID, path, title, artist, album, track number, duration, and optional local cover path. `current_id` identifies a **queue entry**, not a library track. It is null before a current entry is selected. A stopped player may still have a selected entry.

`library list` and `library search` return `{ "tracks": [...], "total": N, "offset": N }`. The default query page is bounded; use `--offset` to retrieve subsequent pages.

`watch --json` emits one response envelope per line (NDJSON), starting with a `state` event. Later events are `state`, `progress`, `library_changed`, `scan_completed`, and `shutdown`:

```json
{"version":6,"ok":true,"data":{"event":"progress","data":{"position_ms":102000,"revision":7}}}
```

State events contain the full state; progress events update position for their matching state revision. Heartbeats occur about once a second, including while paused. A slow subscriber gets a fresh state after event-buffer lag. `Ctrl+C` stops watching without stopping playback.

Exit status is **0** for success, **1** for a connection or operation failure, and **2** for invalid CLI arguments. JSON errors go to stdout with the same envelope. Normal logs go to stderr; server logs go to a file. Current error codes include `invalid_arguments`, `server_unavailable`, `server_busy`, `version_mismatch`, `invalid_request`, `timeout`, `operation_failed`, and `client_error`. New agent operations also return specific codes such as `track_not_found`, `queue_item_not_found`, `current_item_protected`, `queue_conflict`, `request_id_conflict`, `request_log_full`, `scan_in_progress`, `scan_not_found`, `scan_failed`, `wait_timeout`, and `no_active_track`. Optional `error.details` holds machine-readable context. Parse the code; the human message can change. A timeout or disconnect during a mutation has an unknown outcome: inspect the queue or state before retrying an operation such as `next` or `queue add`.

### Find music and line up what plays next

Agents can find tracks, line up what plays next, and edit the queue without
interrupting the current song. All commands below support `--json`; IDs in
examples are placeholders to replace with IDs returned by vtamp.

```sh
vtamp now --json
vtamp library search --artist "DAY6" --title "HAPPY" --exact --json
vtamp library search 'love' --exclude 'live' --limit 20 --json
vtamp library track TRACK_ID --json
vtamp queue add --tracks TRACK_A TRACK_B --after-current --json
vtamp queue list --offset 0 --limit 20 --json
```

`now` returns `current` (a queue entry, or null), `status`, `position_ms`,
`duration_ms`, `remaining_ms`, `volume`, `shuffle`, `repeat`, `queue_length`,
`revision`, `queue_revision`, `scheduled_stop`, and `last_error`. It does not
return the full queue or start a server. A stopped player may retain a current
entry. Without one, the time fields are zero.

Field filters (`--title`, `--artist`, `--album`) combine with AND and use the
same Unicode normalization and lowercasing as ordinary search. Matching is by
substring unless `--exact` is present; exact matching applies only to field
filters and requires at least one. The optional positional query searches the
combined title/artist/album text. Each `--exclude TEXT` removes matches from
that combined text. This is metadata search, not mood or audio analysis.

`--tracks` adds library tracks as one batch, including intentional duplicates.
`--after-current` also works with a single `--track`: it inserts after the current
entry and schedules the additions in the supplied order ahead of shuffle.
A newer play-next batch goes ahead of older pending batches. Repeat-one still
repeats the current song; manual `next` or disabling repeat-one reaches the
pending tracks. With no current entry, insertion starts at the front without
starting playback. Explicit play-next entries survive server restart; ordinary
moves and shuffle switches do not reorder them. Directly playing or removing an
entry takes it out of the pending list. Path imports do not support this option.

`queue list` without pagination keeps its original array response. Supplying
`--offset` or `--limit` returns `{items, total, offset, queue_revision}` instead;
page size defaults to 200 and is capped at 1000.

### Edit the queue in one operation

Save an edit document as `edits.json`:

```json
{
  "operations": [
    {"op": "add", "track_ids": ["TRACK_A", "TRACK_B"], "after_current": true},
    {"op": "remove", "queue_item_ids": ["QUEUE_ENTRY_X"]},
    {"op": "move", "queue_item_id": "QUEUE_ENTRY_Y", "index": 0}
  ]
}
```

```sh
vtamp queue edit --file edits.json --dry-run --json
vtamp queue edit --file edits.json --if-queue-revision 42 --request-id edit-001 --json
# --file - reads the JSON document from standard input.
```

Operations run in document order against a candidate queue. Additions default to
the end; choose either `after_current: true` or a zero-based `index` to insert
elsewhere. Remove and move use **queue entry IDs**, not library IDs. Documents
accept 1–1000 operations and the final queue remains bounded to 10,000 entries
at every step. Unknown fields and invalid references are rejected.

The entire edit is validated and saved before the live queue changes. A failure
leaves the queue and playback untouched. Batch edits cannot delete the current
entry, including while paused or stopped; use the explicit `queue remove` or
`queue clear` command for that. Moving the current entry keeps its position and
playback state. Responses contain `applied`, previous/new `queue_revision`, and
per-operation `changes`, including new queue entry IDs. Dry-run IDs are provisional
and are not reserved; a dry run writes nothing and does not start a server.

`queue_revision` changes with queue membership/order, pending play-next entries,
or the current entry ID. Volume and elapsed time do not change it. Use the
revision returned by `now` or paginated `queue list` as an optional precondition;
`queue_conflict` means reread the queue and reconsider the edit.

`--request-id` is supported by `queue edit` and `queue add --tracks`. A successful
edit and its receipt are stored in one transaction. Retrying the **same ID and
same parsed edit/precondition** returns the original response without applying it
again, including after a server restart. Replay happens before revision checking;
it does not undo later edits. Reusing the ID with different content returns
`request_id_conflict`. IDs allow 1–128 ASCII letters, digits, `-`, `_`, `.`, or `:`.

Receipts last **24 hours**. At most 10,000 unexpired receipts are retained; a full
log rejects new keyed edits with `request_log_full` rather than evicting a valid
receipt. After expiry, an ID is new again. Dry runs cannot use request IDs.
Playback commands such as `next` and relative `seek`, legacy single-track adds,
and path imports do not have this retry guarantee: inspect state after an
unknown outcome before retrying.

### Wait for scans and inspect results

```sh
vtamp library add ~/Music --wait --timeout 60s --json
vtamp library scan --wait --timeout 60s --json
vtamp library scan-status JOB_ID --json
```

`library add`, `remove`, and `scan` normally acknowledge startup with
`{scanning: true, job_id: "…"}`. `--wait` waits for that job, leaving the server
free to handle playback. Its default timeout is 60 seconds; `s`, `m`, and `h`
units are supported up to 24 hours. `--timeout` requires `--wait`.

Jobs report `running`, `completed`, `failed`, or `interrupted`. A completed
summary counts `added`, `updated`, `removed`, `unchanged`, and `warning_count`,
with up to 100 path/message warning details. A warning does not make an otherwise
completed scan fail. Unavailable roots retain their existing catalog entries.
The latest 100 finished jobs are retained across restarts; an unfinished job
becomes `interrupted` on startup.

`wait_timeout` stops waiting without cancelling the job; its `error.details.job_id`
can be passed to `scan-status`. `scan_in_progress` identifies the already running
job in the same way. `watch --json` also emits `scan_completed` with the terminal
job result; `library_changed` still signals a successful catalog update.

### Let the server handle bedtime

```sh
vtamp stop --after-current --json
vtamp sleep set 30m --json
vtamp sleep status --json
vtamp sleep cancel --json
```

There is one scheduled stop: a new reservation replaces the old one. After-current
requires a playing or paused entry, takes effect at its natural end even with
repeat-one enabled, and is cancelled when the selected entry changes. A sleep
duration is a positive integer with `s`, `m`, or `h`, up to 24 hours. Its wall-clock
deadline includes time spent paused or asleep; after system sleep, the server
stops at its next tick if overdue. Changing tracks does not reset the deadline.

Both modes stop playback and reset position while keeping the queue. Stopping,
cancelling, or restarting the server clears the reservation. Closing the CLI or
TUI does not. `scheduled_stop` is null or an object with `kind: "after_current"`
and `queue_item_id`, or `kind: "deadline"` and `deadline_ms` (Unix milliseconds).

### Updating from protocol 1, 2, 3, or 4

This build uses **protocol 6** and migrates the library to **database version 6**
when the new server starts. Stop an older running server using its matching old
binary before starting the new binary, then reattach TUIs. Restart restores the
selected track paused and clears stop reservations. Track IDs, queue entries,
position, volume, and play-next entries are preserved. Older binaries cannot
open the migrated database.

Optional native radio verification uses an isolated, muted server and generated
silence, including HTTP redirects, token renewal, deliberate network failure,
pause, restart, and sleep deadlines:

```sh
cargo build --locked --release
python3 scripts/check-radio.py
```

This check requires macOS audio access and installed FFmpeg **only to generate
test fixtures**. It contacts a temporary localhost server, not public stations.
Radio playback itself has no FFmpeg dependency.

## Storage and troubleshooting

On macOS, persistent data lives under `~/Library/Application Support/vtamp/`; cover thumbnails are under `~/Library/Caches/vtamp/covers/`. The control socket is `/tmp/vtamp-<uid>/control.sock`. Directories are private to the current user. A held advisory lock ensures one server, and a later launch recovers stale sockets left by crashes.

`ui.json` holds client theme and spectrum preferences, saved independently of the playback server. `state.db` holds the library, session, metadata overrides, and background job reports. Installed-tool import settings live in `imports.json`; shared LLM settings live in `llm.json`. Use `vtamp llm setup`, `llm status`, or `llm test` to configure and check an optional provider, even without yt-dlp; see [LLM configuration](docs/llm.md). `server.log` holds diagnostics; a log larger than 5 MiB is rotated at the next server start. Run `vtamp doctor --json` for the exact paths and device information on your machine.

For isolated development or independent test instances, set an absolute, short `VTAMP_HOME`:

```sh
VTAMP_HOME=/tmp/vtamp-dev cargo run -- server start
VTAMP_HOME=/tmp/vtamp-dev cargo run -- status --json
VTAMP_HOME=/tmp/vtamp-dev cargo run -- server stop
```

This places data, covers, and the runtime socket under that directory. Keep the complete socket path under macOS's 104-byte limit. All clients for an instance must use the same `VTAMP_HOME`.

- **No sound:** inspect `vtamp doctor`, system output selection, player volume, and `last_error`. Sandboxed development shells may not see CoreAudio devices even when decoding succeeds. Run the built binary in a normal terminal for output-device access.
- **A changed output device:** vtamp follows the system default output, including AirPods-to-speaker changes. It checks the device every 500 ms and reopens playback at the saved position, preserving volume, queue, and pause state. Stream errors or three seconds without playback progress also trigger recovery. If an output is temporarily unavailable, it retries once a second; `last_error` explains the wait. Pause and stop remain available during recovery.
- **Brief playback interruptions:** decoding runs ahead on a separate worker, with about half a second of PCM buffered for ordinary outputs and a fixed memory limit. The output callback consumes prepared samples; it does not read or decode files. If decoding falls behind, it emits silence without advancing the song position. `Audio decode buffer underrun` in `server.log` records the number of starvation episodes and silence duration, aggregated at most once every five seconds and on track cleanup. This diagnoses vtamp's PCM supply, not every CoreAudio or Bluetooth interruption. Output-open logs include the stream configuration.
- **Idle audio and power:** pause and stop release the audio output and decoder worker. Resume reopens the current default output at the saved position; paused seeking does not open a device. The OS chooses its output buffer size. Spectrum analysis sleeps without viewers or while paused, and unchanged inactive frames are not repeatedly transmitted. These reduce unnecessary work; battery-life improvements depend on the device and workload.
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

With tmux and Python 3 installed, `python3 -m unittest discover -s scripts -p 'test_*.py'`
also checks screenshot publication and the status-bar plugin. Plugin tests use
private tmux servers and a pseudo-terminal to check actual rendering, reloads,
quoted paths, options, and clearing stale text without touching your session.

Optional local-media verification, without redistributing your music:

```sh
VTAMP_TEST_MUSIC_DIR=/path/to/m4a/files cargo test --test media -- --ignored
```

An optional muted output test checks stream reopening, pause/stop resource release, resume, and seeking on a real audio device. Supply a track at least 15 seconds long and run outside an audio-restricted sandbox. Device changes and stream errors are injected; the test does not change your system output or physically disconnect headphones.

```sh
VTAMP_TEST_AUDIO_FILE=/path/to/track.m4a cargo test --lib audio::tests::real_output -- --ignored
```

The implementation separates the queue state machine (`engine`), the rodio adapter (`audio`), metadata/catalog work (`library`, `store`), local transport (`wire`, `client`, `daemon`), platform paths (`platform`), and human/CLI interfaces (`tui`, `cli`). The server alone owns authoritative state and SQLite writes. Scans and artwork work run separately from playback control. See [the protocol notes](docs/protocol.md) for low-level integration.

On macOS, vtamp uses Apple's built-in AudioToolbox decoder for AAC playback to
reduce the patent-licensing concerns associated with distributing an AAC codec
implementation. No AAC software decoder is bundled in the macOS build. ALAC, MP3,
FLAC, WAV, and Vorbis continue to use Symphonia; rodio handles playback, volume,
and output recovery.

The decoder worker also converts samples to the output format, then supplies a
bounded PCM queue. Buffering tests deliberately block the producer to check
continued consumption, underrun recovery, exact sample ordering, and position
accounting. Native decoder disposal never runs in the output callback.

The synthesized AAC, ALAC, and WAV fixtures cover decoder selection, stereo
samples, EOF, and AAC seeking without opening an output device. Native AAC tests
need access to macOS codec services: an execution sandbox can block those services
even though no sound is played. Run the same tests outside that sandbox rather
than treating such a failure as an unsupported file or silently skipping it.

On macOS, `media_controls` runs a windowless AppKit loop on the server's main
thread and bridges system commands to the existing bounded player queue. Metadata
updates are coalesced; artwork is prepared by a separate worker. Ordinary CLI/TUI
clients do not initialize this integration. `build.rs` embeds the application
identity in the executable, including binaries installed with Cargo.
`media_controls/app_bundle.rs` packages that identity and the embedded ICNS for
the server, before AppKit starts. If preparation fails, the server logs a warning
and continues with ordinary playback and the available media controls.

The approved icon source is `assets/icon.png`; its generation prompt is in
`assets/icon-prompt.txt`. Run `python3 scripts/build-icons.py` on macOS to regenerate
the web PNG sizes and `assets/vtamp.icns`. These generated assets are committed;
building or installing vtamp does not require image-generation tools.
For a visual icon check, use a private `VTAMP_HOME` on the normal filesystem
(for example a short directory under `target/`): Launch Services may leave
bundles under `/tmp` with a generic icon even when registration succeeds.

To check real system media-key routing in an interactive macOS desktop session:

```sh
cargo build --locked --release
python3 scripts/check-media-keys.py
```

This opt-in check generates two silent WAV files, starts a muted private server,
and posts actual system media-key events. It also checks detach when tmux is
available, natural track advancement, and paused restoration after restart.
The test helper needs Accessibility permission for the invoking terminal to
**post** keys; the player does not need that permission to receive media commands.
The test temporarily becomes a Now Playing source. Avoid starting playback in
other apps during the check, since macOS decides where global media keys go.
It cleans up its own server and tmux socket without editing your library.
Inspect Control Center separately for visual cover verification; this script
does not prove that the OS rendered artwork or expose a seek slider on every OS.

To preview the landing page:

```sh
python3 -m http.server 8765 --directory site
```

Open `http://localhost:8765`. No build step or external network requests are needed. The hero cycles through actual Ghostty + tmux captures of vtamp in Catppuccin Mocha: a wide view, a compact Library, and a compact Queue. Select a layout or pause the slideshow; open an image at full resolution to inspect the terminal text and album art. Reduced-motion preferences disable automatic rotation.

### Refresh the screenshots

On macOS, install Ghostty at `/Applications/Ghostty.app`, tmux, Python 3, and the Xcode Command Line Tools (for `swiftc`), alongside the Rust toolchain. Allow screen recording for the terminal running the command in **System Settings → Privacy & Security → Screen & System Audio Recording**. Select a track with album art in your regular vtamp instance, then run:

```sh
python3 scripts/capture-site.py
```

The script builds the current release and replaces all three PNGs in `site/screenshots/`. It takes a read-only snapshot of your library, queue, current track, and position, then starts a paused copy under a temporary `VTAMP_HOME`. Each capture uses a dedicated Ghostty window and private tmux server. Your existing playback, theme preference, and working panes stay as they are. The script restores your previous application after opening the capture windows and captures each window directly. It checks the saved pixels for visible artwork; if Ghostty has not painted a background window, it briefly brings that window forward and redraws before retrying.

The presets are 120 × 28 cells (Wide) and 100 × 24 cells (Compact Library and Compact Queue), all in Catppuccin Mocha. Captures include the real Ghostty window frame and high-resolution Kitty artwork selected by vtamp's normal automatic detection. The script inherits your Ghostty font configuration, removes `NO_COLOR` from the capture environment, and pins both tmux and PTY dimensions, waits for a real image upload, and verifies visible cover pixels before accepting the image. If a preset cannot fit on your display, reduce Ghostty's configured font size or use a larger display.

To inspect a new set before replacing the website images:

```sh
python3 scripts/capture-site.py --output /tmp/vtamp-screenshots
```

An existing `VTAMP_HOME` selects the source instance; the script always uses a separate home for its copy. Missing tools, missing artwork, graphics-detection failures, and capture errors leave the existing image set in place. Temporary servers and windows are cleaned up on success, errors, Ctrl-C, and SIGTERM. A graphics-detection failure saves diagnostics from the isolated window under `/tmp/vtamp-capture-failed-*`; remove that directory when finished investigating. It may contain song titles and local paths.

Review the resulting images before committing: screenshots show the selected library's actual song titles and album covers. Music files, source artwork, and databases are never copied into the site. The checked-in captures use the maintainer's selected library; depicted album artwork belongs to its respective owners and is not covered by vtamp's MIT license.

Capture-script checks (no GUI or personal music required):

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
```

## Contributing and license

Small, focused changes are welcome. Include the behavior you changed, a reproducible example for bugs, and relevant validation. Avoid bundling personal music, private paths, cache files, or standalone album artwork. Changes to the curated website captures should follow the screenshot workflow above. Keep platform-specific work behind the existing boundaries.

vtamp is [MIT licensed](LICENSE). Its Rust dependencies retain their own licenses. The bundled Space Grotesk font is distributed under the [SIL Open Font License](site/fonts/OFL.txt). vtamp is an independent project inspired by the experience of classic desktop players, not an affiliation with Winamp.
