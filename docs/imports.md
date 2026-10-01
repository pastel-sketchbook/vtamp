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
```

All commands support the global `--json` flag. A waited partial/failed/cancelled
job returns a nonzero exit status with its report in the JSON error's `details`.
Status queries never auto-start a server. A retry creates a new job containing
unfinished retryable videos only. There is one download worker, at most 32
queued/running jobs, and 100 retained terminal reports. Restart marks unfinished
jobs interrupted; explicit retry is required. Completed tracks remain available.

In the TUI, press `a` and paste a URL into the existing add prompt. Playlist URLs
open a preview; Enter confirms all entries, Esc closes it. Press `i` for job
progress, `j`/`k` to select a job, `[`/`]` to select an item, PgUp/PgDn to scroll,
`c` to cancel, or `r` to retry. Playback controls remain available after closing
the overlay. Detaching the TUI leaves the job running.

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
needed. Thumbnail images are fitted into a padded 512×512 JPEG without stretching
or cutting off their edges. Missing/failed artwork does not fail the audio import.
Completed files live under the data directory's `imports/youtube/VIDEO_ID/` with
`audio.m4a`, optional `cover.jpg`, and `source.json`. Temporary files stay under
`imports/.staging/` and are excluded from scans. Publication waits for scans and
catalog changes; retry can recover a completed directory after a failed database
commit. Library deduplication does not remove intentional queue duplicates.

## Optional LLM cleanup

`vtamp llm setup` offers `api`, `codex`, `claude`, and `rules`, with `api` selected
by default in the setup prompt. Choose `rules` to disable LLM cleanup and use
built-in metadata rules. Without setup, vtamp continues to use `rules` and never
selects an LLM automatically.
`vtamp llm status` reports configuration and tool availability;
`vtamp llm test` explicitly tests the selected provider with bundled sample
metadata. Missing tools, authentication failures, timeouts, invalid JSON or
unsupported options fall back to code rules with an item warning. No model or
reasoning effort is guessed. Structured title/artist fields remain authoritative.

The API adapter uses an OpenAI-compatible **Chat Completions** base URL, a model
ID, and optional reasoning effort. Enter `default` to omit reasoning effort. It
requests JSON Schema output and retries once without the schema parameter if the
endpoint rejects that parameter. Only bounded metadata is sent, never audio,
browser cookies, or local file paths. API calls have a 60-second deadline and
can be cancelled with the job. Model output must supply evidence from the input;
unsupported fields fall back independently to code rules.

API keys can be stored in macOS Keychain (`vtamp.metadata`, scoped to the data
directory), referenced by an environment variable, or omitted for an unauthenticated
local endpoint. Environment variables must exist in the **server process** when
it starts; changing your shell does not change an already running server.
`imports.json` contains references, never the API key. Configuration is captured
for each new job; changing it does not change jobs already queued or running.

The CLI adapters use the installed tool's own login, an isolated temporary working
directory, structured JSON output, and restricted tool/settings options. vtamp
does not copy their tokens or configuration. Codex must support `exec` with
`--output-schema`, `--ephemeral`, and `--ignore-user-config`; Claude must support
`--safe-mode`, `--json-schema`, and `--tools`. Older versions fall back to code
rules. The selected tool may use its normal subscription or API billing.

For automation, edit the private `imports.json` in the data directory. For example:

```json
{
  "youtube": {
    "yt_dlp": "/opt/homebrew/bin/yt-dlp",
    "ffmpeg": "/opt/homebrew/bin/ffmpeg",
    "ffprobe": "/opt/homebrew/bin/ffprobe",
    "chrome_cookies": false
  },
  "llm": {
    "provider": "api",
    "endpoint": "http://localhost:1234/v1",
    "model": "your-model-id",
    "effort": null,
    "key_env": "VTAMP_LLM_API_KEY"
  }
}
```

Remove `key_env` for an endpoint without authentication. Set `provider` to `rules`
to disable LLM requests. Client preferences remain separate in `ui.json`.

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
