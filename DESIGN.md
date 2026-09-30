---
name: vtamp
description: A detachable terminal music player with a palette of your own.
colors:
  tui-bg: "#1e1e2e"
  tui-panel: "#181825"
  tui-selection: "#313244"
  tui-text: "#cdd6f4"
  tui-muted: "#bac2de"
  tui-accent: "#fab387"
  tui-border: "#585b70"
  tui-warning: "#f9e2af"
  tui-error: "#f38ba8"
  web-bg: "#171c18"
  web-accent: "#b4f676"
  web-text: "#eeeae0"
  web-panel: "#222a23"
  web-deep: "#101610"
  web-muted: "#b0bdab"
  web-line: "#46513f"
  web-warning: "#efbc72"
---

# vtamp visual system

## Overview

Classic Winamp, rendered as a useful terminal instrument. The TUI defaults to warm Catppuccin Mocha; users can preview and save any of nine palettes. The website retains its existing charcoal and green identity.

## Colors

TUI tokens above describe the default Mocha palette. `src/theme.rs` defines all nine palettes through the same semantic roles. Text and metadata stay neutral; accent identifies focus, current playback, and progress. Selected rows have their own background and a `›` marker; current playback has a `▶` marker. Warnings and errors remain labeled in text. Theme roles cover overlays, empty states, artwork padding, and the generated no-cover illustration. Original album artwork is never recolored.

The registry includes Catppuccin Mocha and Latte, Rosé Pine, Gruvbox Dark Medium, Tokyo Night Night, Nord, Dracula, Kanagawa Wave, and Classic. Body and secondary text must reach 4.5:1 against canvas, panel, and selected-row backgrounds. Accent, warning, and error text must reach 4.5:1 on canvas and panel backgrounds. Palette origins and contrast adaptations are documented in `docs/themes.md`.

The website keeps phosphor green (#b4f676), warm off-white (#eeeae0), muted sage (#b0bdab), and amber (#efbc72). Its green player simulation is explicitly labeled Classic.

## Typography

TUI typography follows the user's terminal font. Bold weight and text markers establish hierarchy. Use self-hosted Space Grotesk for web headings/body and the system monospace for commands and player data.

## Layout

The TUI is an Operate surface: queue, library, current track, timing, and key hints have priority. At 100 columns and above, Library and Queue sit side by side; narrower panes show the focused list. Minimum usable size is 40 × 12 cells, with a compact now-playing summary at 12–13 rows. Cover art gives context without dominating the queue.

The website is a Persuade surface. Lead with “Music stays. Your terminal moves on.” and a working, explicitly simulated detach demonstration. Follow with the detachable lifecycle, executable CLI examples, and source installation instructions. A broad typographic headline is balanced by a dense, mechanically precise player.

## Shapes

Compact square corners and fine panel borders. TUI overlays use solid panel backgrounds, not transparency over album art.

## Components

The theme picker opens with `t`, previews with arrows or j/k, saves with Enter, and restores the opening theme on Esc/q. It scrolls at small sizes. During overlays, pixel art is hidden and restored on close so graphics cannot cover dialog text. Selection is client-local; saved preferences apply to future attachments.

Keyboard focus is explicit; reduced motion removes web transitions. All public music names and covers are authored demo content.

## Do's and Don'ts

- Do resolve every UI color through a semantic palette role.
- Do keep theme changes independent of playback and server state.
- Do verify pixel graphics in the actual terminal; text captures cannot prove image rendering.
- Don't use color as the only indicator of selection, playback, or errors.
- Don't introduce invented metrics or release claims.
