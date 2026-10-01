# Optional installed-tool imports

vtamp remains a local music player. If an executable `yt-dlp` is available on
PATH (or at a configured absolute path), it also offers YouTube audio imports.
Without it, related CLI help, TUI controls, and diagnostic messages are hidden.
vtamp never installs tools, enables cookies, or selects an LLM automatically.
There is no separate Cargo feature or special build.

## Add music

Audio extraction requires existing `yt-dlp`, `ffmpeg`, and `ffprobe` executables.
An installed Deno runtime is passed to yt-dlp when available. Tool requirements
can change with YouTube; `vtamp youtube status` reports detected versions and
paths without testing authentication. `vtamp youtube setup` can save explicit
absolute paths when the playback server's PATH differs from your shell.

```sh
vtamp library add 'https://www.youtube.com/watch?v=VIDEO_ID' --preview
vtamp library add 'https://www.youtube.com/watch?v=VIDEO_ID' --wait
vtamp library add --clipboard
vtamp library add 'https://www.youtube.com/playlist?list=PLAYLIST_ID'
```

Replace `VIDEO_ID` and `PLAYLIST_ID` with the IDs you want to import.

The clipboard option reads one URL from the macOS clipboard. Quote URLs in a
shell, especially when they contain `&`. A watch URL with a `list` parameter
imports only that video; add `--playlist` to import the whole list. A playlist
URL imports the list. Lists are limited to 10,000 entries. Unavailable videos
produce item failures; other videos continue. Live/upcoming broadcasts are
rejected. Existing source video IDs with present audio files are skipped.

`--preview` extracts metadata and checks existing entries if a server is already
running. It does not download audio, create jobs, persist settings, or start the
server. `--json` includes every preview entry; ordinary output shows the first 50.
Metadata cleanup can contact the explicitly configured LLM provider during a
single-video preview.

Imports run in the playback server, independent of attached clients. By default
the CLI prints a job ID and returns; `--wait` shows stages, item counts, bytes,
speed, ETA, and elapsed time. `--wait --timeout 5m` limits waiting. A timeout or
Ctrl+C stops waiting only; use `import-cancel` to cancel the job.

```sh
vtamp library imports
vtamp library import-status JOB_ID --offset 0 --limit 20
vtamp library import-cancel JOB_ID
vtamp library import-retry JOB_ID
vtamp library cover refresh [TRACK_ID|all]
vtamp library cover status JOB_ID
```

All commands support the global `--json` flag. A waited partial/failed/cancelled
job returns a nonzero exit status with its report in the JSON error's `details`.
Status queries never auto-start a server. A retry creates a new job containing
unfinished retryable videos only. There is one download worker, at most 32
queued/running jobs, and 100 retained terminal reports. Restart marks unfinished
jobs interrupted; explicit retry is required. Completed tracks remain available.

`library cover refresh` re-fetches the thumbnail of every managed YouTube import
and rewrites its `cover.jpg` with the current thumbnail pipeline, so imports from
an older pipeline can be repaired without re-importing audio. Omitting the track
or passing `all` refreshes the whole set; a track ID limits the run to that one
track. One refresh runs at a time; `--wait` follows it, and
`--wait --timeout 60s` limits waiting without cancelling the work. Covers are
written only for files under the managed `imports/` directory; other library
tracks are counted as skipped. A track that has no thumbnail yet (or a failed
one) gains a cover when the refresh succeeds, and tracks whose cover bytes are
already current are left untouched. The job report lives in server memory:
restarting the server loses it, and re-running the command is safe.

In the TUI, press `a` and paste a URL into the existing add prompt. Playlist URLs
open a preview; Enter confirms all entries, Esc closes it. Press `i` for the
import history: each row shows a source title and its status, with the selected
import's results and full title below. Use `j`/`k` to select a job, `[`/`]` to
select a track within a playlist, PgUp/PgDn to scroll the details,
`c` to cancel, or `r` to retry. Enter closes the dialog, selects the track shown
in the details in Library, and plays it; while that page is still loading, it
plays the import's first added track. A track that is not in Library yet, or an
import that added nothing, only reports that and leaves the dialog open.
Playback controls remain available after closing the overlay. Each time you open
`i`, it starts at the active job shown in the bottom status line, or the newest
job when all are finished, with item paging reset. New arrivals preserve the job
you are reading while the dialog is open. Detaching the TUI leaves the job
running.

When an import finishes while the TUI is attached, Library focuses and selects
the first successfully added track, scrolling to it even on another page. For a
playlist this happens once, when the whole job finishes. A search that hides the
track is cleared; matching searches remain. Open dialogs and prompts defer the
selection until they close. Explicit browsing while the destination loads takes
precedence. Jobs that add no tracks and historical jobs on attachment do not move
the selection. Playback and Queue are unchanged.

## Metadata and source links

Structured music metadata takes precedence. Otherwise conservative code rules
recognize forms such as `Artist - Title` and `Artist 'Title'`. For example,
`Artist A + Artist B 'Song title'` becomes **Song title** by **Artist A, Artist B**.
Ambiguous titles stay intact; the uploader is not automatically treated as the
performer.

Single-video imports accept `--title` and `--artist`. To correct an existing
track, press `m` in the TUI to edit Title, Artist, and Album (Tab/Shift-Tab switches
fields, Ctrl-U clears a field, Enter saves), or run:

```sh
vtamp library edit TRACK_ID --title 'Song title' --artist 'Performers'
vtamp library edit TRACK_ID --album 'Album title'
vtamp library edit TRACK_ID --album ''
vtamp library retag TRACK_ID
```

Edits are database overrides, preserved by rescans and retagging. They do not
rewrite audio files. Retag reruns the configured automatic cleanup against saved
source metadata while retaining overrides. Queued copies update without changing
their order, identities, playback position, or queue revision.

Albums are optional. Imports use the source's structured album when available,
otherwise the audio file's album tag; no album name is guessed. Leave Album blank
to hide it in Now Playing and Library, including the separator after the artist.
Clearing an album is a saved override, so rescans and retagging keep it empty.
Older `Unknown album` placeholders are cleared automatically on server upgrade.

Imported tracks retain their original title, video ID/URL, channel identity and
link, bounded description, and any structured music metadata. Press `o` for the
selected track's video or `O` for its channel in the system browser. Track JSON
contains this information in `source` (`provider: youtube`).

Downloads select audio only, preferring m4a; FFmpeg extracts/converts to m4a when
needed. Thumbnails are stored at up to 512 pixels on the long side, keeping the
image's own shape like album art: nothing is cropped or padded on disk, and the
player sizes its cover area to the image so a wide thumbnail is drawn in full.
Missing/failed artwork does not fail the audio import.
Completed files live under the data directory's `imports/youtube/VIDEO_ID/` with
`audio.m4a`, optional `cover.jpg`, and `source.json`. Temporary files stay under
`imports/.staging/` and are excluded from scans. Publication waits for scans and
catalog changes; retry can recover a completed directory after a failed database
commit. Library deduplication does not remove intentional queue duplicates.

## Optional LLM inference

Imports use conservative built-in rules by default. If a provider is configured
in the shared `llm.json`, they can use it to extract title and artist information.
See [LLM configuration and connection tests](llm.md) for setup, credentials, API,
and installed CLI options. Setup and connection tests are independent of imports.

Structured title/artist fields remain authoritative. Only bounded source metadata
is sent for inference, never audio, browser cookies, or local file paths. Model
output must supply evidence from the input; unsupported fields fall back
independently to built-in rules. Missing tools, authentication failures, timeouts,
invalid JSON, and unsupported options produce an item warning and use the rules.
Requests can be cancelled with the import job. Each new job captures both import
and LLM configuration, so changing settings does not affect queued/running jobs.

The [extraction prompt](../src/prompts/music_metadata.txt) asks for concise titles,
removing upload promotion and duplicate romanization while preserving the original
language and recording versions. It distinguishes cover performers from original
artists and keeps primary/featured artists separate from accompaniment, backing
vocals and production credits. Instrumental soloists and explicitly co-billed
musicians remain artists. A medley or full session stays one recording, rather
than being named after its first song. Whole series episodes retain the artist
name that identifies the episode, even when it also appears in Artist. Korean
episode titles shorten `Artist의 Series를 라이브로!` to `Artist의 Series 라이브`
without an additional `(Live)` suffix. Individual song clips keep their song
title instead of being named after the whole episode. Examples use fictional
names and songs. Channel names alone are never performer evidence. Evidence must
quote one supplied metadata field; model inference can still be wrong, so manual edits
remain available.

Updating the prompt requires rebuilding and restarting the playback server.
Existing tracks keep their metadata until explicitly retagged; saved manual
overrides remain in place. A single-video `--preview` uses the invoked CLI binary
and can check extraction without downloading or changing the library.

Import options remain separate in `imports.json`. For automation, for example:

```json
{
  "youtube": {
    "yt_dlp": "/opt/homebrew/bin/yt-dlp",
    "ffmpeg": "/opt/homebrew/bin/ffmpeg",
    "ffprobe": "/opt/homebrew/bin/ffprobe",
    "chrome_cookies": false
  }
}
```

## Optional Chrome cookies

`vtamp youtube setup` offers **Use Chrome cookies?**, defaulting to no, and an
optional profile such as `Default` or `Profile 1`. When explicitly enabled,
vtamp passes `--cookies-from-browser chrome[:PROFILE]` to yt-dlp. yt-dlp reads
the browser profile using its own implementation; vtamp does not export or store
cookies. A blank profile lets yt-dlp choose its default profile. macOS may ask for
Keychain access, and browser permissions, profiles, and extractor support can
affect availability. A logged-in browser alone does not enable this option.
`youtube status` checks executables only; an explicit preview/import exercises
the selected authentication path. Disable `chrome_cookies` to return to public
unauthenticated extraction.
