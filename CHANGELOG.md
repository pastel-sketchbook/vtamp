# Changelog

## [Unreleased]

### Added

- Portable Library tarballs with downloaded YouTube audio/video, covers,
  metadata edits and radio registrations. `library export --include-local`
  also includes local originals; default exports preserve external references.
- `library import`, `--dry-run`, and `library archive-status` validate and merge
  archives while keeping existing tracks, Queue and playback intact. Matching
  external files reconnect, and interrupted publication is recovered on startup.

### Upgrade notes

- Protocol 10 adds archive restoration commands; database version remains 6.
  Restart an older server with matching binaries before using the new commands.

### Fixed

- Archive export, restore and dry-run now report stages, item counts and byte
  progress on stderr, including updates within large files. Restore progress is
  also available through `library archive-status`.
- Exported audio, video and cover files use readable artist/title names with
  collision suffixes, so a normally extracted tarball is useful without vtamp.

## [0.4.0] — 2026-10-03

Changes since v0.3.0. Optional YouTube video now plays inside the terminal, with
pane-local fullscreen, lower graphics overhead, and stable seeking and input.

### Added

- Optional YouTube video downloads up to 480p. The TUI asks for Audio only or
  Audio + video for each import; the CLI accepts `library add URL --video`.
  Existing audio imports can gain video without changing track identity or
  Queue. Failed or cancelled video downloads preserve successfully imported audio.
- Terminal video playback on macOS with installed FFmpeg/FFprobe and Kitty or
  Sixel graphics. Video follows the server's audio; `w` toggles video/cover,
  and `F` fills the current terminal pane with elapsed / total time below.
  `F` or Esc returns. Audio keeps playing when the TUI detaches.
- Video defaults to 15 fps; `VTAMP_VIDEO_FPS=8` reduces terminal CPU usage.
  Kitty uploads use capability-detected compression and terminal-side scaling,
  including fullscreen. Frame buffers and image IDs remain bounded.
- Spectrum styles. `V` switches the visible spectrum between `bars`, `gradient`,
  `mono`, `mirror`, `dots`, and `waterfall`; the status row names the new style
  and `ui.json` remembers it as `spectrum_style` (older files default to `bars`).
  Every style uses the active theme's colors. The waterfall scrolls one row per
  analysis frame, freezes while paused, and resets on a track change.
- Deleting managed YouTube downloads: `d` in Library opens a confirmation, and
  `vtamp library delete TRACK_ID` removes the audio, video, cover, source metadata,
  and catalog entry. Local originals and tracks in Queue or playback are refused.

### Fixed

- Pasted YouTube playlist URLs open the playlist preview instead of importing
  only the selected video.
- Idle or paused spectrum streams stay connected without repeated reconnects.
- Video fullscreen exits cleanly on track transitions, and seeks retain the
  last displayed frame until the target is ready instead of flashing the cover.
- Text input cursors stay at the field during video redraws, including Kitty
  passthrough through tmux to Ghostty.
- Relay mode spectrum frames now carry the remote's `current_id`, so an attached
  TUI shows the spectrum of the audio the relay plays instead of dropping every
  frame.

### Upgrade notes

- Protocol is **9**; database version remains **6**, so upgrading from v0.3.0
  needs no database migration. Clients and servers must use the same protocol.
- Before upgrading v0.3.0, stop its server with the matching installed binary:
  `vtamp server stop`. Then run `brew update && brew upgrade vtamp` (or replace
  the binary) and start vtamp again. The saved session is restored paused.
  Installing or rebuilding does not replace a running server.
- Video is optional and requires installed `yt-dlp`, FFmpeg, and FFprobe for
  imports; tools are detected at runtime and are not installed automatically.
  Playback is local to the macOS TUI. Linux remains a headless server without
  device playback, relay, media keys, radio, or terminal video.
- Prebuilt downloads cover Apple Silicon macOS and ARM64 Linux with glibc 2.39+
  (tested on Ubuntu 24.04). Intel Macs and other Linux architectures build from source.

## [0.3.0] — 2026-10-02

Changes since v0.2.0. This release makes searching and managing the TUI queue
easier and fixes live-radio media keys and slow LLM CLI startup.

### Added

- `/` searches the focused list: title, artist, and album in Library, or the
  entries already in Queue. Queue filtering is instant; Library search follows
  typing after a 150 ms debounce. Enter keeps the draft, and Esc restores the
  previous filter and Library page. A filtered queue retains original positions
  and disables `J`/`K` reordering.
- `A` in Library appends every matching track across all result pages in one
  atomic edit, up to the 10,000-entry queue limit. It skips tracks already in
  Queue, reports added and skipped counts, and preserves playback, shuffle, and
  navigation. Repeating `A` does not add another copy; `e` still allows duplicates.
- `X` in Queue opens a confirmation with the current entry count before emptying
  it. Enter confirms and Esc cancels. Clearing stops queued playback; a direct
  track playing outside Queue continues.
- `zz` selects the now-playing queue entry from either panel and centers it when
  the list ends leave room. It clears a queue filter that hides the entry.
- `>` and `<` are next/previous shortcuts alongside `n` and `b`.

### Changed

- Library and Queue appear side by side from 90 columns, down from 100, in panes
  with at least 28 rows. The compact player layout remains unchanged.
- Search, folder, and radio channel-name prompts are centered over the browser
  area and sized to their labels, keeping the player visible.
- Queue-all notices suggest only remaining actions: shuffle when it is off, play
  while stopped, or resume while paused.
- Live radio's cover slot reads **Live stream** instead of **No album art**.

### Fixed

- macOS media keys can skip a live station while it is still connecting. Now
  Playing publishes the selected station before its first audio frame; local
  files still wait for audio output.
- Optional Codex and Claude CLI providers allow 15 seconds, up from 5, for each
  `--help` or `--version` probe. Slow first launches no longer trigger an early
  built-in-rules fallback. The model request deadline remains 60 seconds.

### Upgrade notes

- Protocol remains **7** and database version remains **6**; upgrading from
  v0.2.0 requires no database migration.
- Upgrade Homebrew with `brew update && brew upgrade vtamp`, then reattach the
  TUI to use the new interface. To pick up server-side media-key and LLM fixes,
  run `vtamp server stop` when convenient, then start vtamp again. Installation
  never replaces a running server; a restart restores the session paused.
- Prebuilt downloads cover Apple Silicon macOS and ARM64 Linux with glibc 2.39+
  (tested on Ubuntu 24.04). Linux remains a headless server only, without device
  playback, relay, media keys, or radio. Intel Macs and other Linux architectures
  build from source.

[Unreleased]: https://github.com/rath/vtamp/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/rath/vtamp/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/rath/vtamp/compare/v0.2.0...v0.3.0
