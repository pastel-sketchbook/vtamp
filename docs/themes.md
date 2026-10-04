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

## Preferences and JSON

The client stores `{"theme":"catppuccin-mocha","spectrum":false,"spectrum_style":"bars"}` in `ui.json` under the data directory (`~/Library/Application Support/vtamp/` on macOS, or `$VTAMP_HOME`). Writes use a private temporary file and atomic rename. The last completed explicit save wins. The file is independent of the server's database and wire protocol. Older files
without `spectrum` default to false, and files without `spectrum_style` default
to `bars`. Theme saves preserve spectrum visibility and style, and spectrum
toggles and style changes preserve the saved theme (including during a
`--theme` override). A spectrum toggle or style change applies to the current
attachment even if saving fails; neither replaces an invalid settings file. An
unknown `spectrum_style` value is a settings error like an unknown theme: attach
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
| `radial` | Rays around a ring of braille dots; the ring widens on strong bass | spectrum low → middle → high, border (ring) |
| `fire` | Flames on half-block pixels fed by each column's level | canvas → spectrum high → middle → text; on light themes canvas → middle → high |
| `ridge` | Stacked lines of recent frames; nearer lines hide farther ones, which fade | spectrum gradient faded toward canvas |
| `sparks` | The bars, with sparks thrown up when a band jumps | spectrum low / middle / high; sparks text → middle → high |

See `DESIGN.md` for the geometry of each style and the rules for pause, seek,
and track changes.

Missing files use Mocha. Invalid files are left intact and produce a warning on attach; explicitly saving a theme replaces them. A failed save leaves the preview open, with retry and cancel controls. CLI `theme current` returns a settings error for an invalid file; `theme list` remains available and `theme set` can repair it.

`theme list --json` returns the usual versioned response envelope with `data.themes`, an array of `{id, name, mode}` objects. `theme current --json` returns `data.theme` and `data.path`; `theme set NAME --json` also includes `data.applies_to: "future_attachments"`. Invalid names exit 2; settings errors exit 1.

The theme selected in one TUI takes effect there immediately. Other attached TUIs retain their theme. Nothing watches or synchronizes theme preferences between running clients. A theme command does not need a running server or a server restart.
