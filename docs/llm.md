# Optional LLM provider

LLM configuration is shared by features that make model requests. It is available
without yt-dlp and does not start the playback server.

```sh
vtamp llm setup
vtamp llm status
vtamp llm test
```

`setup` offers `api`, `codex`, `claude`, and `none`, defaulting to `api` in the
prompt. Choose `none` to disable LLM requests. Without configuration, the provider
is `none`; vtamp never selects a model or enables an LLM automatically.

The setup saves the provider, endpoint or executable, model ID, optional reasoning
effort, and credential references in the private `llm.json` file. Its location is
the data directory (`~/Library/Application Support/vtamp/` on macOS, or
`$VTAMP_HOME`). Import options remain in `imports.json`; client preferences remain
in `ui.json`. Run setup again to save an existing provider in this separate file.

After saving, setup offers **Test connection?** and describes the request first.
The test asks only for `{"ok":true}`. It does not read the library or send song,
channel, video, or other user content. `llm test` runs the same request explicitly.
A successful result includes `tested: true` and `response: {"ok":true}`; an
unexpected response or provider failure returns an error. With `none`, it returns
`tested: false` and `enabled: false` without sending anything. This checks a model
request, not the quality of a feature's results.

`status` reports saved settings and, for a CLI provider, the selected executable's
availability and version. It does not contact a model or test authentication.
`status` and `test` accept `--json`; interactive setup does not.

## API and credentials

The API provider uses an OpenAI-compatible **Chat Completions** base URL, a required
model ID, and optional reasoning effort. Enter `default` to omit reasoning effort.
Requests use JSON Schema output, with one retry without the schema parameter if
the endpoint rejects that parameter. Requests have a 60-second deadline and a
1 MiB response limit.

API keys can use macOS Keychain (`vtamp.llm`, scoped to the data directory), an
environment variable reference, or no authentication for a local endpoint.
`llm.json` never stores the key itself. Environment variables must exist in the
process sending the request: the CLI for `llm test`, or the playback server for
background features. Changing a shell does not change an already running server.

For automation, edit `llm.json` directly:

```json
{
  "provider": "api",
  "endpoint": "http://localhost:1234/v1",
  "model": "your-model-id",
  "effort": null,
  "key_env": "VTAMP_LLM_API_KEY"
}
```

Omit `key_env` for an endpoint without authentication. To disable requests, save
`{"provider":"none"}`. New requests load the saved settings; work already queued
uses its captured configuration.

## Installed CLI providers

`codex` and `claude` use the selected installed executable and its own login. A
blank model ID uses the CLI's default. Each request runs in an isolated temporary
working directory with structured JSON output and restricted tool/settings
options. vtamp does not copy tokens or configuration.

Codex must support `exec` with `--output-schema`, `--ephemeral`, and
`--ignore-user-config`; Claude must support `--safe-mode`, `--json-schema`, and
`--tools`. Missing tools, unsupported options, or authentication failures produce
errors. The selected tool may use its normal subscription or API billing.
