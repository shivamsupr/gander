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
just test           # 63 tests, no live backends
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
gander photo.jpg                          # describe → text for people
gander clip.mp4 --output-format json      # describe → JSON envelope
gander recall                             # browse what you already analysed
```

## Testing

```sh
just test           # 63 tests, about 1 second, no backends. CI gates on this.
just test-live      # optional. Makes one real agy call.
```

`gander --check` costs more than it looks:

- It makes one real backend call each, with the full report prompt. It allows 120
  seconds per backend (`CHECK_TIMEOUT_S`). agy takes 25 to 45 seconds, so a full check
  takes about a minute.
- It always probes a **fixed model**: agy→flash, claude→sonnet, codex→gpt-5.5. A green
  `agy` row does not prove that your configured `pro` model works.
- A failed row prints the reason after the latency: `auth`, `timeout`, `empty`, or
  `fatal`. `timeout` means the probe ran out of time. It does not mean a broken login.

Long videos are slow. A CHUNKED run makes one call per chunk, plus one merge call, so a
clip over 60 seconds can take a few minutes.

Run `just` with no arguments to list every dev recipe.

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
- **`--check` is not free, and is not a config test.** See [Testing](#testing). For one
  machine-readable probe, run `gander --check --backend agy --output-format json` and
  read `error_class`.

```sh
gander SOURCE --output-format json             # describe one local file → envelope
gander recall --query Q --output-format json   # full-text search the cache
gander recall --output-format json             # browse or filter past results
gander cache clear [SOURCE]                    # forget everything, or one file
gander --check                                 # which backends and ffmpeg answer
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
gander --version
```

### Describe

| Flag | Default | Meaning |
|---|---|---|
| `SOURCE` | — | path to one local image, video, or audio file. No URLs |
| `--output-format {raw,json}` | `raw` | `json` prints the envelope |
| `--model {pro,flash,sonnet,haiku,opus,gpt-5.5,gpt-5.4,gpt-5.4-mini}` | `pro` | primary model |
| `--backend {agy,claude,codex}` | `agy` | primary backend |
| `--fallback-model {…,none}` | `flash` | model for the second try (same list, plus `none`) |
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
| `--timeout S` | `300` | wall-clock seconds per backend |
| `--reconfigure` | — | run the first-run setup again. **It rewrites `config.toml` from scratch, so any comments in it are lost** |
| `--no-config` | off | ignore the config file for this run |
| `--check` | — | check the backends and ffmpeg. SOURCE is ignored |
| `-V`, `--version` | — | print the version and exit |
| `-h`, `--help` | — | print help |

### Primary and fallback

gander tries twice. Each try is one `(backend, model)` pair.

The primary runs first. If it returns capacity (429), a timeout, a temporary error, or
an answer that is empty or unreadable, gander drops to the fallback.

**An auth error is different: it stops everything at once, with no fallback.** A broken
login means analysis is dead, not slower.

`--fallback-model none` or `--fallback-backend none` switches the second try off. A pair
that does not go together, such as `--backend agy --model sonnet`, is a usage error (`2`).

### Video tiers (picked automatically from the duration)

| Duration | Tier | What runs |
|---|---|---|
| `< 30s` | DIRECT | the whole file goes to the backend |
| `30–60s` | SINGLE-BATCH | ffmpeg pulls frames and audio → one backend call |
| `> 60s` | CHUNKED | stream-copy segments → one call per chunk → a fixed merge, then one Flash call for the prose |

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
model that belongs to it, so the pair is always valid. A fallback backend of `none`
skips the model question. Answers go to `~/.gander/config.toml`. The picker draws on
stderr, so it never touches the JSON on stdout. A run with no terminal never asks and
never writes.

`--reconfigure` runs the picker again. **It rewrites `config.toml` from scratch, so any
comments you put in that file are lost.** `--no-config` ignores the file for one run.

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
