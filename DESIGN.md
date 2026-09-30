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

The website keeps phosphor green (#b4f676), warm off-white (#eeeae0), muted sage (#b0bdab), and amber (#efbc72). Its hero shows real Ghostty + tmux screenshots in Catppuccin Mocha.

## Typography

TUI typography follows the user's terminal font. Bold weight and text markers establish hierarchy. Use self-hosted Space Grotesk for web headings/body and the system monospace for commands and player data.

## Layout

The TUI is an Operate surface: queue, library, current track, timing, and key hints have priority. At 12–27 rows and at least 72 columns, the player and focused browser share one row. The player takes 40% of the width, bounded to 30–44 columns; Library or Queue takes the rest and switches with Tab. The player stacks a centered cover above metadata, progress, and two rows of controls. Cover size follows available height while reserving readable controls. At 28 or more rows, now playing sits above the lists; from 100 columns, both lists are visible below it. Panes narrower than 72 columns keep the stacked layout and use a compact now-playing summary at 12–13 rows. Minimum usable size is 40 × 12 cells. Cover art gives context without dominating the queue.

The website is a Persuade surface. Lead with “Music stays. Your terminal moves on.” and a full-width gallery of the real terminal interface. Follow with the detachable lifecycle, executable CLI examples, and source installation instructions. The headline and introduction share a row above the gallery. Show Wide, Compact Library, and Compact Queue in a fixed image area without cropping; provide full-resolution links. Named layout controls and a pause button accompany a six-second rotation. Hover, keyboard focus, and offscreen state pause rotation; reduced motion disables automatic rotation and fading. Without JavaScript, the first screenshot remains visible.

The lifecycle section also introduces the optional tmux status-bar plugin with a labeled text example, a copyable status format, and a link to installation instructions. The example places music after the pane title without a clock or date; on narrow screens, the title truncates while playback times remain visible.

A macOS feature row pairs Now Playing copy with a static F7/F8/F9 keyboard reference. These are labeled keys, not clickable playback controls. Match the neighboring tmux feature’s spacing and stack copy above the keys on mobile.

## Shapes

Compact square corners and fine panel borders. TUI overlays use solid panel backgrounds, not transparency over album art.

The approved V-meter app icon has five lime bars forming a V silhouette on a charcoal rounded-square tile. Its rounded shape and illuminated finish belong to the identity asset; they do not change the surrounding interface's shapes or palette.

## Components

The V-meter source is `assets/icon.png`. Use derived PNGs for the website header/footer mark (`site/mark.png`), favicons (`site/favicon-32.png`, `site/favicon-64.png`), and touch icon (`site/apple-touch-icon.png`); use `assets/vtamp.icns` for macOS app identity. Preserve the approved artwork across these sizes. The brand icon identifies vtamp and never replaces album covers.

The theme picker opens with `t`, previews with arrows or j/k, saves with Enter, and restores the opening theme on Esc/q. It scrolls at small sizes. During overlays, pixel art is hidden and restored on close so graphics cannot cover dialog text. Selection is client-local; saved preferences apply to future attachments.

Keyboard focus is explicit; reduced motion removes web transitions. The public screenshots use the maintainer’s selected real library and original album colors. Refresh them with `scripts/capture-site.py`, using isolated playback and terminal sessions.

## Do's and Don'ts

- Do resolve every UI color through a semantic palette role.
- Do keep theme changes independent of playback and server state.
- Do verify pixel graphics in the actual terminal; text captures cannot prove image rendering.
- Don't use color as the only indicator of selection, playback, or errors.
- Don't introduce invented metrics or release claims.
