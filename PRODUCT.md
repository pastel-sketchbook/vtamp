# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

The marketing surface is web. The product itself is a native Rust terminal application, macOS first, primarily used inside tmux in Ghostty or kitty.

## Stack

Rust, ratatui, crossterm, rodio. The approved landing page stack is static HTML/CSS/JS.

## Users

People who spend their working day in a terminal and want local music without permanently occupying a pane. AI agents and scripts are secondary clients through a documented CLI.

## Product Purpose

Keep music playing independently of its interface. Launching vtamp starts a per-user server if needed and attaches the TUI; closing the TUI leaves playback running.

## Positioning

Virtual terminal meets Winamp: a local music player with the lifecycle of a detachable terminal session.

## Capabilities and Constraints

Folder-based local library, search, queue editing, album art, machine-readable CLI, persistent session. A server restart restores the session paused. No streaming, account, login service, or permanent pane required. English-only first release. macOS is the supported platform; isolate platform dependencies for later Linux support.

## Brand Commitments

Classic Winamp inspiration is approved for both TUI and website: dense charcoal panels, green playback displays, compact controls, legible type. Name: vtamp. Voice: direct, practical, slightly nostalgic.

## Evidence on Hand

Local m4a files kept outside the repository may be used for development verification only. They and their cover art must not be redistributed. Public demonstrations use authored sample metadata and art. No published package, remote repository, endorsements, or usage metrics exist yet.

## Product Principles

- Playback belongs to the server; the interface is disposable.
- Terminal space is working space.
- Every important action is available without a TUI.
- Graceful text-based cover art is better than fragile terminal graphics.

