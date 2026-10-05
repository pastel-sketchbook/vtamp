# Color themes

Press `t` in the TUI to preview themes, `Enter` to save, or `Esc` / `q` to cancel. Use `vtamp theme list` to list identifiers and `vtamp theme set NAME` to set the default for future attachments. `vtamp --theme NAME` is an attachment-only override. Theme commands accept `--json` and never start the playback server.

| Name | Identifier | Variant / source |
| --- | --- | --- |
| Catppuccin Mocha | `catppuccin-mocha` | [Mocha](https://catppuccin.com/palette/), peach accent; default |
| Catppuccin Latte | `catppuccin-latte` | [Latte](https://catppuccin.com/palette/), light |
| Rosé Pine | `rose-pine` | [Base](https://github.com/rose-pine/neovim/blob/main/lua/rose-pine/palette.lua) |
| Gruvbox | `gruvbox` | [Dark Medium](https://github.com/morhetz/gruvbox/blob/master/colors/gruvbox.vim) |
| Tokyo Night | `tokyo-night` | [Night](https://github.com/folke/tokyonight.nvim) |
| Nord | `nord` | [Nord](https://github.com/nordtheme/nord) |
| Dracula | `dracula` | [Dracula](https://github.com/dracula/dracula-theme) |
| Kanagawa | `kanagawa` | [Wave](https://github.com/rebelot/kanagawa.nvim/blob/master/lua/kanagawa/colors.lua) |
| Classic | `classic` | Original vtamp green palette |

Thanks to these projects and their authors for their palettes. vtamp maps their colors to terminal-specific roles rather than reproducing an editor's syntax highlighting. For readable small text, Latte's mauve, warning, and error roles are darkened; Nord, Dracula, Gruvbox, and Classic use brighter error colors, and Dracula uses a brighter neutral for secondary text. Kanagawa uses peach red for errors. The exact role values live in `src/theme.rs`.

Body and secondary text have at least 4.5:1 contrast on normal, panel, and selected-row backgrounds. Accent and status text meet the same threshold on normal and panel backgrounds. Selection and playback also have text markers. Swatches and inactive decorative borders are not text or focus indicators.

## Custom themes

Add a JSON file to the data directory's `themes/` folder. On macOS this is
`~/Library/Application Support/vtamp/themes/`; with `VTAMP_HOME` it is
`$VTAMP_HOME/themes/`. There is no registration index or rebuild. The filename
without `.json` is the stable theme ID: `my-pastel.json` becomes `my-pastel`.
Use lowercase ASCII letters, digits, and single separating hyphens. Built-in IDs
are reserved, including when installing with `--replace`.

The file must be UTF-8 JSON, at most 64 KiB, with this complete version-1 format:

```json
{
  "version": 1,
  "name": "My Pastel",
  "mode": "dark",
  "colors": {
    "bg": "#121218",
    "panel": "#18181e",
    "selection": "#28283c",
    "text": "#ffffff",
    "muted": "#9a9aa5",
    "accent": "#00d9ff",
    "border": "#555555",
    "warning": "#00d9ff",
    "error": "#ff5050",
    "spectrum": ["#00c850", "#00d9ff", "#ff5050"]
  }
}
```

`name` is a nonempty display name without control characters. `mode` is `dark`
or `light`. Every color is an explicit `#RRGGBB`; all roles are required, and
unknown fields are errors. `spectrum` contains low, middle, and high colors in
that order. Themes do not execute code, inherit from other themes, or change
layout, terminal fonts, or original album artwork.

Install files from any local directory, or copy them into `themes/` yourself:

```sh
vtamp theme install /path/to/my-pastel.json
vtamp theme list
vtamp --theme my-pastel                # Preview this attachment only.
vtamp theme set my-pastel              # Save for future attachments.
vtamp theme install /path/to/my-pastel.json --replace
```

Installation validates every input and destination collision before writing.
An identical existing file is unchanged; a different file requires `--replace`.
Duplicate IDs in a batch and non-regular destinations are rejected. Each file
is published atomically; an I/O failure can leave earlier files installed, and
the error identifies those IDs. Installation does not change `ui.json`, create
a database, or start the server. Edit or remove installed files directly; no
uninstall command is needed.

The CLI reads the folder for each theme command. Each TUI reads it on attachment,
then keeps its own palette snapshot; reattach after adding, editing, or deleting
a file. The `t` picker shows built-ins in their existing order followed by custom
IDs in alphabetical order, with the same preview/save/cancel controls. There is
no automatic file watching or cross-client synchronization.

Malformed, unreadable, or conflicting files are skipped with a path and reason;
other themes remain available. Low text contrast and a mode that disagrees with
the canvas/text brightness produce warnings but do not prevent using the theme.
The 4.5:1 text contrast guarantee applies to the built-in and supplied Pastel
palettes; user palettes are under their author's control. CLI warnings go to
stderr, or `data.warnings` (`{path, message}` objects) with `--json`; the TUI shows
them in its status message. `theme list` provides the full diagnostics.

A saved theme whose file is missing or invalid falls back to Mocha with a warning
on attachment. An explicit `--theme` must resolve successfully before the client
starts a server. Neither case rewrites settings. Spectrum and video preference
changes retain the saved ID even when its file is missing; saving a different
theme repairs that selection while preserving those preferences.

### Pastel examples and credits

The sixteen JSON files in [`themes/pastel/`](../themes/pastel/) are adapted from
[pastel-sketchbook's vtamp commit a64c16f8](https://github.com/pastel-sketchbook/vtamp/commit/a64c16f88aa65a4d6afd38f9756cefae9746c3d9).
Credit: pastel-sketchbook, porting the pastel-market palettes. The names, IDs,
nine UI colors, and three spectrum colors are unchanged from that commit.
The original port tunes concrete RGB values for terminal text contrast.

The collection has dark and light variants of Default, Gruvbox, Solarized, Ayu,
Flexoki, Zoegi, FFE, and Postrboard. It is an optional example collection, not
compiled into the built-in list:

```sh
# From a source checkout after building:
target/release/vtamp theme install themes/pastel/*.json
target/release/vtamp --theme pastel-default
target/release/vtamp --theme pastel-default-light
```

Homebrew also installs these files under its package share directory:

```sh
vtamp theme install "$(brew --prefix vtamp)/share/vtamp/themes/pastel/"*.json
```

Installation adds sixteen choices, for twenty-five total. It preserves the
saved default. Use `t` and Enter or `theme set` only when you want to save one.

## Preferences and JSON

The client stores `{"theme":"catppuccin-mocha","spectrum":false,"spectrum_style":"bars"}` in `ui.json` under the data directory (`~/Library/Application Support/vtamp/` on macOS, or `$VTAMP_HOME`). Writes use a private temporary file and atomic rename. The last completed explicit save wins. The file is independent of the server's database and wire protocol. Older files
without `spectrum` default to false, and files without `spectrum_style` default
to `bars`. Theme saves preserve spectrum visibility and style, and spectrum
toggles and style changes preserve the saved theme (including during a
`--theme` override). A spectrum toggle or style change applies to the current
attachment even if saving fails; neither replaces an invalid settings file. An
unknown `spectrum_style` value is a settings error like an unavailable theme: attach
warns, and saving a theme with `t` repairs the file. The three decorative
spectrum roles use theme-specific low/middle/high colors, with darker colors in
Latte to remain visible on its light background.

## Spectrum styles

Press `V` while the spectrum is shown to switch to the next style. Styles take
every color from the active theme's roles, so they need no per-theme tuning.

| Identifier | Draws | Roles used |
| --- | --- | --- |
| `bars` | Vertical bars with green, yellow, and red height zones and falling peaks (default) | spectrum low / middle / high |
| `gradient` | The same bars blended smoothly from bottom to top | spectrum low → middle → high |
| `mono` | The same bars in one color; peaks in the text color | accent, text |
| `mirror` | Bars growing up and down from a center line | spectrum low / middle / high |
| `dots` | One dot per row, like a segmented meter; unlit dots are faint | spectrum low / middle / high, selection |
| `waterfall` | A scrolling history with the newest frame at the bottom | spectrum gradient |
| `radial` | Petals around a ring of braille dots; sudden rises swell a glowing core and send waves outward | spectrum low → middle → high (petals, peaks), text (petal tips, core center), border → accent (ring), accent (core, waves) |
| `fire` | Flames on half-block pixels fed by each column's level | canvas → spectrum high → middle → text; on light themes canvas → middle → high |
| `ridge` | Stacked lines of recent frames; nearer lines hide farther ones, which fade | spectrum gradient faded toward canvas |
| `sparks` | The bars, with sparks thrown up when a band jumps | spectrum low / middle / high; sparks text → middle → high |
| `squares` | Whole-cell segments and held peaks | spectrum gradient; selection for unlit dots |
| `smooth` | Filled braille curve | spectrum low / middle / high by height |
| `trail` | Six recent frame tops, newest drawn last | spectrum zones faded toward canvas by age |
| `stereo` | Independent L/R meters around the center | spectrum zones by distance from center; muted channel labels |

Frequency labels and channel labels use the muted role.

See `DESIGN.md` for the geometry of each style and the rules for pause, seek,
and track changes.

Missing preference files use Mocha. Invalid preference files are left intact and produce a warning on attach; explicitly saving a theme replaces them. A failed save leaves the preview open, with retry and cancel controls. CLI `theme current` returns a settings error for an invalid file; `theme list` remains available and `theme set` can repair it.

`theme list --json` returns the usual versioned response envelope with `data.themes`, an array of `{id, name, mode}` objects. `theme current --json` returns `data.theme` and `data.path`; `theme set NAME --json` also includes `data.applies_to: "future_attachments"`. Invalid names exit 2; settings errors exit 1.

`theme install FILE... --json` returns `data.installed`, `data.unchanged`,
`data.warnings`, and `data.path` (the theme directory). Invalid definitions and
installation/settings errors exit 1; unknown CLI theme IDs exit 2. Existing
`theme list/current/set` fields remain unchanged.

The theme selected in one TUI takes effect there immediately. Other attached TUIs retain their theme. Nothing watches or synchronizes theme preferences between running clients. A theme command does not need a running server or a server restart.
