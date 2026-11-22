# gander

`gander` looks at one local media file and tells you what is in it.

Give it an image, a video, or an audio file. You get back a **transcript**, if there is
speech, and a **structured description**.

It runs no model itself: no weights, no API keys, no network calls of its own. It drives
an agentic CLI that you already have and are logged into: `agy`, `claude`, or `codex`.
`ffmpeg` and `ffprobe` do the media work.

Every result is cached in local SQLite, keyed by content hash. Ask about the same file
again and the answer is instant and free.

- **Who calls it:** any agent harness that can run a shell command. No service, no
  socket, no daemon.
- **Backends:** `agy` (image, audio, video; needs a PTY), `claude` (image only),
  `codex` (`codex exec --yolo`, same range as agy). One adapter trait, so you can add more.

## Why the name?

"To take a gander" means "to take a look". That is the whole job.

## Build

```sh
just release        # → target/release/gander  (one binary, about 6 MB)
just test           # unit tests, no live backends
```

SQLite is built into the binary (`rusqlite` `bundled`), so you need no libsqlite at
runtime. For a static Linux (musl) build, use Linux CI or
[`cross`](https://github.com/cross-rs/cross) (`just musl`).

## Dependencies

**To build:** the Rust toolchain. [`just`](https://github.com/casey/just) is optional,
and only used for the dev recipes.

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust
brew install just        # or: cargo install just  (optional)
```

**To run:** these binaries, on your `PATH` or named by the `GANDER_*_BIN` env vars.

| Binary | Purpose | Install |
|---|---|---|
| `ffmpeg`, `ffprobe` | probe, segment, pull frames + audio | `brew install ffmpeg` · `apt install ffmpeg` |
| `agy` and/or `claude` and/or `codex` | the analysis backend. You need **at least one** | each is its own CLI. Install and log in separately |

Each backend logs in through its own CLI. **gander never sees your credentials.**

## Install

Build it, then copy it onto your `PATH`. macOS and Linux:

```sh
just release
install -m 755 target/release/gander ~/.local/bin/gander        # just for you
sudo install -m 755 target/release/gander /usr/local/bin/gander # or for everyone
gander --version
```

You only need `sudo` for the second one, because root owns `/usr/local/bin`.

## Quickstart

```sh
gander --check                            # see which backends answer
gander --models                           # see which models each backend offers
gander photo.jpg                          # describe → text for people
gander clip.mp4 --output-format json      # describe → JSON envelope
gander recall                             # browse what you already analysed
```

## Testing

```sh
just test           # unit tests, about 1 second, no backends. CI gates on this.
just test-live      # optional. Makes one real agy call.
```

Run `just` with no arguments to list every dev recipe.

## Health checks

gander depends on CLIs that change without notice. Two checks tell you what works.

| Command | What it sends | What it proves | Cost |
|---|---|---|---|
| `gander --check` | one text prompt to each backend, on a fixed model | the backend is installed and logged in | 1 call per backend |
| `gander --check --deep` | a real image, audio file, and video, through your configured primary and fallback | each media type gives a correct answer | 1 call per media type |

**`--check` does not test media and does not test your configured model.** A green
row there can sit next to dead audio. Use `--check --deep` for that.

### The deep check

```sh
gander --check --deep                 # image, audio, and video
gander --check --deep=audio           # one media type
gander --check --deep --backend codex --model gpt-6.2-sol --fallback-model none
```

```
image    ● ok    agy/gemini-4-neon-medium  33.5s
audio    ● ok    agy/gemini-4-neon-medium  59.8s
video    ✖ fail  claude/claude-sonnet-5-5  14.0s
         no transcript: the backend that answered did not process the audio
ffprobe  ● ok    /opt/homebrew/bin/ffprobe
ffmpeg   ● ok    /opt/homebrew/bin/ffmpeg
```

Three small files are built into the binary (see [`assets/probe`](assets/probe)). The
deep check does not trust `status`. It reads the answer:

- **image:** the answer must contain the number shown in the picture.
- **audio:** the transcript must match the spoken sentence.
- **video:** both.

This matters because a failure can look like success. When the audio path of one backend
dies and a fallback with no audio support answers, a normal call returns `partial` with a
confident description and no transcript. The deep check reports that as a failure.

- It uses the same primary and fallback as a real call: flags, then `GANDER_*` env, then
  the config file.
- It never reads or writes your cache.
- It retries each probe once, because one empty answer from a backend is common.
- It exits `0` when every probe passes, and `4` when one fails.
- `claude` cannot hear. With only `claude`, the audio and video probes always fail. Use
  `--deep=image`.

To run it on a schedule and get an alert, see [`contrib`](contrib).

### More about `--check`

- It makes one real backend call each, with the full report prompt. It allows 120
  seconds per backend. agy takes 25 to 45 seconds, so a full check takes about a minute.
- It always probes a **fixed model**: `flash` on agy, `sonnet` on claude, and the codex
  default.
- A failed row prints the reason after the latency: `auth`, `timeout`, `empty`,
  `model_unknown`, or `fatal`. `timeout` means the probe ran out of time. It does not
  mean a broken login.

Long videos are slow. A CHUNKED run makes one call per chunk, plus one merge call, so a
clip over 60 seconds can take a few minutes.

## For agent harnesses

`gander` is made to be called from a script. This is the contract:

- **Always pass `--output-format json`.** stdout is then one JSON object, and
  [every key is always there](#output-envelope---output-formatjson). The picker and all
  messages go to **stderr**, so stdout stays clean JSON.
- **Read the exit code first, then `status`.** Never read the prose on stdout. Careful:
  **`partial` also exits `0`**, so check `status` and `warnings` to tell it from `ok`.
  See [Exit codes](#exit-codes).
- **It never stops to ask you anything.** With no TTY, gander does not prompt and does
  not write `config.toml`. Pin it with `--backend` and `--model`, or the `GANDER_*` env vars.
- **Running it again is free.** The same file is an instant cache hit. Use `--force` to
  compute it again, and `--allowed-root` for paths you do not trust.
- **Choose the model yourself.** `gander --models --output-format json` lists what each
  backend offers right now. Pass any of those names to `--model`. gander never swaps
  your model for another one.
- **When a call fails, read `error`.** It names the next command to run:
  `gander --models` when the backend does not know the model, a different `--backend`
  when the CLI is not installed, and `gander --check --deep` for everything else. See [Health checks](#health-checks).
- **`--check` is not free, and is not a config test.** It proves a login. For a real
  test of image, audio, and video, run `gander --check --deep --output-format json`.

```sh
gander SOURCE --output-format json             # describe one local file → envelope
gander recall --query Q --output-format json   # full-text search the cache
gander recall --output-format json             # browse or filter past results
gander cache clear [SOURCE]                    # forget everything, or one file
gander --check                                 # which backends and ffmpeg answer
gander --check --deep --output-format json     # do image, audio, and video work?
gander --models --output-format json           # which models can I pass?
```

`recall` and `cache` never call a backend. They read the local cache, so they are free
and instant.

## Usage

```sh
gander SOURCE [options]                 # describe one local file
gander recall [filters]                 # read the cache (no model call)
gander config {path,show,clear}         # look at or reset saved defaults
gander cache path                       # print the cache DB path
gander cache clear [SOURCE]             # forget everything, or one file
gander --check                          # check the backends and ffmpeg
gander --check --deep[=KINDS]           # run real media through your backends
gander --models                         # list the models each backend offers
gander --version
```

### Describe

| Flag | Default | Meaning |
|---|---|---|
| `SOURCE` | — | path to one local image, video, or audio file. No URLs |
| `--output-format {raw,json}` | `raw` | `json` prints the envelope |
| `--model NAME` | `pro` | primary model: a gander alias, or any name the backend accepts. See [Models](#models) |
| `--backend {agy,claude,codex}` | `agy` | primary backend |
| `--fallback-model NAME` | `flash` | model for the second try. `none` switches the second try off |
| `--fallback-backend {…,none}` | `agy` | backend for the second try |
| `--no-transcript` | off | do not transcribe speech |
| `--no-translate` | off | no English translation block |
| `--max-frames N` | `12` | frames per clip or chunk, evenly spaced (kept within `[1,64]`) |
| `--fps RATE` | — | sample frames at a fixed rate, capped by `--max-frames` |
| `--chunk-length S` | `60` | length of each segment in the chunked tier |
| `--max-chunks N` | `8` | limit on chunks. Over the limit, segments get longer |
| `--max-duration S` | unset | reject videos longer than S |
| `--force` | off | ignore the cache and compute again |
| `--keep-temp` | off | keep the temp working folder (path goes to stderr) |
| `--allowed-root DIR` | — | only allow a SOURCE inside DIR |
| `--db PATH` | `~/.gander/media.db` | use a different cache DB |
| `--timeout S` | `540` | wall-clock seconds per backend |
| `--reconfigure` | — | run the first-run setup again. **It rewrites `config.toml` from scratch, so any comments in it are lost** |
| `--no-config` | off | ignore the config file for this run |
| `--check` | — | check the backends and ffmpeg. SOURCE is ignored |
| `--deep[=KINDS]` | — | with `--check`: run real media through your backends. KINDS is any of `image,audio,video` |
| `--models` | — | list the models each backend offers. SOURCE is ignored |
| `-V`, `--version` | — | print the version and exit |
| `-h`, `--help` | — | print help |

### Primary and fallback

gander tries twice. Each try is one `(backend, model)` pair.

The primary runs first. If it returns capacity (429), a timeout, a temporary error, an
unknown model, or an answer that is empty or unreadable, gander drops to the fallback.

**An auth error is different: it stops everything at once, with no fallback.** A broken
login means analysis is dead, not slower.

`--fallback-model none` or `--fallback-backend none` switches the second try off. A model
that belongs to a different backend, such as `--backend agy --model sonnet`, is a usage
error (`2`).

Each try is recorded in `backend.attempts` with an `error_class`:

| `error_class` | Meaning |
|---|---|
| `capacity` | rate limit or quota (429) |
| `timeout` | gander stopped the call at `--timeout` |
| `transient` | a temporary network or server error |
| `auth` | the backend is not logged in. No fallback |
| `model_unknown` | the backend does not know the model name |
| `not_installed` | the backend's CLI is not on `PATH` |
| `empty` | the backend answered, but with no result block |
| `fatal` | anything else. The detail has the end of the output |

### Models

**gander does not decide which models exist.** The list of a backend depends on its
version and on your account. So `--model` goes to the backend as you typed it, and the
backend accepts or rejects it.

```sh
gander --models                        # every backend
gander --models --backend agy          # one backend
gander --models --output-format json   # for agents
```

```
codex  (live list from the backend)
  default                        unknown    the model set in the backend's own config (default)
  gpt-6.2-sol                    pro
  gpt-6.2-astra                  above-pro
  gpt-6.1-terra                  mid
  ...
```

`--models` asks each backend for its list (`agy models`, `codex debug models`). `claude`
has no such command, so gander shows its aliases only. You can still pass any claude
model id.

**Aliases.** gander has a few short names of its own:

| Backend | Alias | Runs |
|---|---|---|
| `agy` | `pro` (default) | `gemini-4-neon-medium` |
| `agy` | `flash` | `gemini-4-hydrogen-medium` |
| `claude` | `sonnet` (default), `haiku`, `opus`, `fable` | the matching claude model id |
| `codex` | `default` (default) | no model flag: codex uses the model in your codex config |

The agy aliases point at models that your plan may not have. If `pro` fails with
`model_unknown`, pick a name from `gander --models --backend agy` and save it with
`gander --reconfigure`.

**When a backend renames a model**, the call fails with `error_class: model_unknown`, and
`error` tells you to run `gander --models`. No new gander build is needed: pass the new
name.

**Classes.** `--models` adds a class to each model: `above-pro`, `pro`, `mid`, `flash`,
or `unknown`. The backends publish no such label. The class is a rough guide to help an
agent choose, taken from the family word in the model name:

| Class | Family words |
|---|---|
| `above-pro` | `fable`, `astra` |
| `pro` | `neon`, `opus`, `sol` |
| `mid` | `terra`, `carbon` |
| `flash` | `hydrogen`, `luna`, `flash` |

Change or extend this in `~/.gander/config.toml`. Your entries win over the built-in ones:

```toml
[classes]
mid = ["sonnet", "argon"]
flash = ["haiku"]
```

### Video tiers (picked automatically from the duration)

| Duration | Tier | What runs |
|---|---|---|
| `< 30s` | DIRECT | the whole file goes to the backend |
| `30–60s` | SINGLE-BATCH | ffmpeg pulls frames and audio → one backend call |
| `> 60s` | CHUNKED | stream-copy segments → one call per chunk → a fixed merge, then one text call for the prose (agy `flash`, or your primary on another backend) |

### Recall

```
gander recall [--query Q] [--keyword K] [--text T] [--rating {keep,review,cull}]
              [--language L] [--kind {image,video,audio}] [--min-people N]
              [--min-duration S] [--has-transcript|--no-transcript]
              [--has-audio|--no-audio] [--chunked] [--include-failed] [--all-versions]
              [--order-by {updated_at,created_at,rating,people_count,duration_seconds}]
              [--asc] [--limit N] [--db PATH] [--output-format {raw,json}]
```

`--query` is full-text search (SQLite FTS5, ranked by BM25, porter-stemmed) over the
summary, description, transcript, English translation, keywords, and filename. The best
match comes first, unless you pass `--order-by`. Each row shows the asset's
`source_path` and a `match_context` snippet, so you can see why it matched.

FTS5 syntax works as-is (`steel OR crane`, `transcript:prueba`, `weld*`). A string that
is not valid FTS5 is retried as plain quoted words.

```sh
gander recall --query "steel beam"        # ranked full-text search
gander recall --query worker --kind video --rating keep
```

### Config and cache subcommands

```
gander config path | show | clear        # ~/.gander/config.toml
gander cache  path                       # print the cache DB path
gander cache  clear [SOURCE] [--db PATH] # forget everything, or one file
```

## Output envelope (`--output-format=json`)

One object. **Every key is always there.** Anything that does not apply is `null`, `""`,
`0`, or `[]`.

```jsonc
{
  "status": "ok",                  // "ok" | "partial" | "failed"
  "error": null, "warnings": [], "parse_ok": true,
  "media_kind": "image", "content_sha256": "…", "cached": false,
  "summary": "…", "description": "**Scene:** …",
  "transcript": null, "language": null, "english_translation": null,
  "structured": { "rating": "keep", "people_count": 0, "keywords": […], … },
  "media": { "duration": null, "wxh": "2048x2048", "has_audio": false, … },
  "backend": { "model_used": "…", "backend_used": "agy", "attempts": [ … ] },
  "schema_version": "2026-06-08.1", "tool_version": "0.1.0"
}
```

## Exit codes

| Code | Meaning |
|---|---|
| `0` | ok, or partial |
| `1` | unexpected |
| `2` | usage error |
| `3` | input error (`failed`) |
| `4` | backend or auth failure |

**`partial` also exits `0`.** Read `status` and `warnings` to tell it from `ok`.

## Configuration

For each setting, this order wins:
**flag > `GANDER_*` env > `~/.gander/config.toml` > built-in default.**

The first time you run gander in a real terminal, an arrow-key picker (↑/↓, then Enter)
asks for your primary and fallback defaults. It asks for the backend first, then for a
model from that backend's live list, so the pair is always valid. A fallback backend of `none`
skips the model question. Answers go to `~/.gander/config.toml`. The picker draws on
stderr, so it never touches the JSON on stdout. A run with no terminal never asks and
never writes.

`--reconfigure` runs the picker again. **It rewrites `config.toml` from scratch, so any
comments you put in that file are lost.** Your `[classes]` table is kept. `--no-config` ignores the file for one run.

Env vars: `GANDER_DB_PATH`, `GANDER_AGY_BIN` / `GANDER_CLAUDE_BIN` / `GANDER_CODEX_BIN`,
`GANDER_FFPROBE_BIN` / `GANDER_FFMPEG_BIN`, `GANDER_ALLOWED_ROOT`,
`GANDER_MODEL_DEFAULT` / `GANDER_BACKEND_DEFAULT` /
`GANDER_FALLBACK_MODEL_DEFAULT` / `GANDER_FALLBACK_BACKEND_DEFAULT`,
`GANDER_PRINT_TIMEOUT_S`, `GANDER_CHUNK_LEN_S` / `GANDER_MAX_CHUNKS`,
`GANDER_MAX_DURATION_S`, `GANDER_MAX_FRAMES` / `GANDER_FRAME_FPS`.

## Security

gander does not trust `SOURCE`:

- symlinks and `..` are resolved first
- URLs, NUL bytes, and files that are not regular files are rejected
- paths outside `--allowed-root` are rejected
- backend arguments are passed as an argv vector, never through a shell
- frames and standalone images have EXIF and GPS stripped (`-map_metadata -1`)
- your source file is never changed
- the cache DB is `0600`

> **Warning:** the backends run with their permission checks off:
> `--dangerously-skip-permissions` (agy), `bypassPermissions` (claude), and `--yolo`,
> which means `danger-full-access` (codex). This is on purpose, so one call needs no
> interaction. Use `--allowed-root`, and only run against files you trust.
