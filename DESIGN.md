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

On the first connection of a TUI attachment, active playback focuses Queue and
selects the current queue entry, scrolling it into view. Paused/stopped sessions
keep Library focus. Subsequent state updates and automatic reconnections preserve
the user's focus and selection. If the saved spectrum view would replace the
list, hide it for this attachment without changing the saved preference.
Tab, Ctrl-W w, and Ctrl-W Ctrl-W switch Library/Queue. Both Ctrl-W sequences
follow Tab's behavior when returning from the spectrum, and do not switch panels
inside prompts or overlays. Any other intervening key cancels the prefix.

Library Ctrl+Enter plays outside the queue; Enter retains its existing queue
behavior. Mark direct playback with `NO QUEUE` in the Now Playing border and keep
Library focus on attachment. Do not mark a queued copy as currently playing.
Preserve the help dialog's line count when documenting the alternate play action.

The website is a Persuade surface. Lead with “Music stays. Your terminal moves on.” and a full-width gallery of the real terminal interface. Follow with the detachable lifecycle, executable CLI examples, and source installation instructions. The headline and introduction share a row above the gallery. Show Wide, Compact Library, and Compact Queue in a fixed image area without cropping; provide full-resolution links. Named layout controls and a pause button accompany a six-second rotation. Hover, keyboard focus, and offscreen state pause rotation; reduced motion disables automatic rotation and fading. Without JavaScript, the first screenshot remains visible.

The lifecycle section also introduces the optional tmux status-bar plugin with a labeled text example, a copyable status format, and a link to installation instructions. The example places music after the pane title without a clock or date; on narrow screens, the title truncates while playback times remain visible.

A macOS feature row pairs Now Playing copy with a static F7/F8/F9 keyboard reference. These are labeled keys, not clickable playback controls. Match the neighboring tmux feature’s spacing and stack copy above the keys on mobile.

The CLI section introduces agent workflows in its existing two-column layout. Copyable examples show current-track JSON, field search, and a sleep timer; a compact protocol-5 response excerpt avoids a full queue dump. Link to the README for atomic-edit and retry details. Long commands wrap beside reachable copy buttons, including on mobile.

## Audio spectrum

The read-only spectrum extends the existing TUI. `v` toggles it and saves the
client preference (default off). Bars run from low to high frequencies, using
fixed height zones: green below 55%, yellow up to 80%, and red above. Each theme
provides three spectrum roles; Latte uses darker inks. Unicode eighth blocks and
briefly held falling peaks animate at 20 Hz. There are no EQ sliders or L/R meters.

At 28+ rows and 72+ columns, use the right half of Now Playing for the spectrum,
with a cover and compact metadata/controls on the left. Keep the lists below.
Otherwise replace the browser area, preserving its selection and scroll. Tab
returns to the previous list; slash returns to Library with a blank search draft.
Disable hidden-list actions. Help/theme overlays obscure the spectrum and suspend
its subscription. Labels remain neutral and the axes read LOW / HIGH. Paused,
stopped, or stale data settles to zero instead of showing decorative motion.
Once bars and peaks settle, suspend the animation timer until fresh audio or a
view change needs it. Do not send terminal output for unchanged frames. Keep
input and artwork completion immediate, and preserve progress and notice expiry.

## Shapes

Compact square corners and fine panel borders. TUI overlays use solid panel backgrounds, not transparency over album art.

The approved V-meter app icon has five lime bars forming a V silhouette on a charcoal rounded-square tile. Its rounded shape and illuminated finish belong to the identity asset; they do not change the surrounding interface's shapes or palette.

## Components

The V-meter source is `assets/icon.png`. Use derived PNGs for the website header/footer mark (`site/mark.png`), favicons (`site/favicon-32.png`, `site/favicon-64.png`), and touch icon (`site/apple-touch-icon.png`); use `assets/vtamp.icns` for macOS app identity. Preserve the approved artwork across these sizes. The brand icon identifies vtamp and never replaces album covers.

The theme picker opens with `t`, previews with arrows or j/k, saves with Enter, and restores the opening theme on Esc/q. It scrolls at small sizes. The theme picker stays within the browser area, keeping the player and album art visible throughout preview. It uses the full browser height on very small panes and scrolls its choices. Help and import overlays hide pixel art when they overlap it and restore it on close. Bottom search/folder prompts keep the cover visible. `/` starts a blank search draft; Enter applies it (empty clears the filter), while Esc keeps the current filter. Outside the prompt, Esc clears an applied filter before it detaches. Text fields show the real terminal cursor at the caret so input methods (for example, Korean) compose inside the field; Ctrl-U clears the field. Theme selection is client-local; saved preferences apply to future attachments.

Help scrolls through wrapped text at small pane sizes. When scrolling is needed,
keep scroll/page controls, close keys, and the visible row range in a fixed
two-line footer. When all text fits, show only a one-line close hint. Arrow keys and
j/k scroll; PageUp/PageDown or Ctrl-B/F page; Home/End jump. Esc/q/? close help,
other playback/list keys stay inside the modal, and reopening starts at the top.

Keyboard focus is explicit; reduced motion removes web transitions. The public screenshots use the maintainer’s selected real library and original album colors. Refresh them with `scripts/capture-site.py`, using isolated playback and terminal sessions.

## Do's and Don'ts

- Do resolve every UI color through a semantic palette role.
- Do keep theme changes independent of playback and server state.
- Do verify pixel graphics in the actual terminal; text captures cannot prove image rendering.
- Don't use color as the only indicator of selection, playback, or errors.
- Don't introduce invented metrics or release claims.

## Optional import controls

Show these only after server-side detection of an installed yt-dlp. The existing
`a` bottom prompt accepts a folder or URL; a playlist opens a preview with an
explicit Enter confirmation. Single videos start without a mandatory metadata
form. `i` opens jobs, `m` edits title/artist/album, and `o`/`O` open video/channel links.
Album is optional: an empty value hides the album and its separator in Library
and omits the album line in Now Playing. The editor uses Tab/Shift-Tab to change
fields, Ctrl-U to clear, and Enter to save. Keep the active field visible even
when earlier values wrap in a small terminal.
Imports shows a selectable list of source titles and labeled states, newest
first, above the selected import's details. Keep titles to one line in the list;
show the full source title and any different saved title with explicit labels
below. Lead details with the outcome in plain language, such as "Added 1 track
to Library." Results omit zero counters and completed transfers omit stale
percentages. Offer cancel only while running, retry only for unfinished imports,
and track navigation only for multi-track imports. Successful imports need no
repair action. Paint the full overlay with the theme's panel background.
Keep the list and key hints visible while PgUp/PgDn scroll the details. On small
terminals show fewer rows while keeping the selection in view. j/k selects jobs,
brackets select tracks within a playlist, c cancels and r retries.
Opening `i` reveals the active job shown in the bottom status line, or
the newest job when all have finished, starting at its first item. While the
dialog is open, retain the job the user is reading by identity as new jobs arrive.
List replies must not roll back newer progress or erase newly observed jobs.
When a newly observed import finishes, focus Library and reveal its first
successfully added track, once per job. Locate the correct page by identity;
clear a search only if it hides the track. Defer this while a dialog or prompt is
open, and let explicit browsing cancel a pending jump. Historical completions
on attachment/reconnection and jobs with no additions must not move selection.
Do not play or enqueue the revealed track.
Keep hints reachable at 40×12. Import/edit/preview overlays hide pixel
covers and restore them on close; the bottom add prompt keeps the cover visible.
Use existing semantic palette roles and English copy. No installation prompts,
integration placeholders, or related help are shown when yt-dlp is absent.
