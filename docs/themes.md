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

The client stores `{"theme":"catppuccin-mocha"}` in `ui.json` under the data directory (`~/Library/Application Support/vtamp/` on macOS, or `$VTAMP_HOME`). Writes use a private temporary file and atomic rename. The last completed explicit save wins. The file is independent of the server's database and wire protocol.

Missing files use Mocha. Invalid files are left intact and produce a warning on attach; explicitly saving a theme replaces them. A failed save leaves the preview open, with retry and cancel controls. CLI `theme current` returns a settings error for an invalid file; `theme list` remains available and `theme set` can repair it.

`theme list --json` returns the usual versioned response envelope with `data.themes`, an array of `{id, name, mode}` objects. `theme current --json` returns `data.theme` and `data.path`; `theme set NAME --json` also includes `data.applies_to: "future_attachments"`. Invalid names exit 2; settings errors exit 1.

The theme selected in one TUI takes effect there immediately. Other attached TUIs retain their theme. Nothing watches or synchronizes theme preferences between running clients. A theme command does not need a running server or a server restart.
