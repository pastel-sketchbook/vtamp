# Changelog

## [Unreleased]

### Added

- Spectrum styles. `V` switches the visible spectrum between `bars`, `gradient`,
  `mono`, `mirror`, `dots`, and `waterfall`; the status row names the new style
  and `ui.json` remembers it as `spectrum_style` (older files default to `bars`).
  Every style uses the active theme's colors. The waterfall scrolls one row per
  analysis frame, freezes while paused, and resets on a track change. No server
  or protocol change; attached TUIs pick it up on reattach.

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

[0.3.0]: https://github.com/rath/vtamp/compare/v0.2.0...v0.3.0
