# gander

`gander` is a one-shot CLI that takes a **local media file** (image / video / audio)
and returns a **transcript** (when speech is present) plus a **structured
description**, by driving an **agentic CLI backend** (`agy` / `claude` / `codex`)
under the hood. It persists every result in SQLite keyed by content hash and prints a
single **JSON envelope** to stdout.

It runs no model itself: no weights, no API keys, no local inference, no networking of
its own. `ffmpeg`/`ffprobe` handle probing/segmentation/extraction;
`agy`/`claude`/`codex`/`ffmpeg`/`ffprobe` are external binaries discovered at runtime.

- **Consumers:** any agent harness that can run a shell command shells out to `gander`
  and reads the JSON. No service, no socket, no daemon.
- **Backends:** `agy` (full vision+audio+video, PTY), `claude` (vision-only floor),
  `codex` (`codex exec --yolo`, agy-class). Pluggable via one adapter trait.

## Why "gander"?

To **take a gander** is to take a look — and that is the whole job: hand it a file, it
takes a gander and tells you what it sees.

## Build

```sh
cargo build --release        # → target/release/gander  (single ~6 MB binary)
cargo test                   # 52 deterministic tests, no live backends
```

The binary statically bundles SQLite (`rusqlite` `bundled`), so there is no libsqlite
runtime dependency. A fully static musl build for Linux release artifacts is best done
on Linux CI or via [`cross`](https://github.com/cross-rs/cross) (the bundled SQLite C
sources need a musl cross-toolchain).

## Requirements

On `PATH` (or pointed to via `GANDER_*_BIN` env vars): `ffmpeg`, `ffprobe`, and at
least one backend — `agy`, `claude`, or `codex`. Each backend handles its own login
through its own CLI; gander never sees credentials.

## Testing

**Deterministic suite** (fast, no backends, no network) — parsing, the merge fold,
db round-trip, source validation, prompts, ffmpeg planning, envelope shape:

```sh
cargo test --bin gander          # ~52 tests, ~1s; this is what CI gates on
```

**Live backend smoke** (real agy) is `#[ignore]`d so it never runs by accident:

```sh
cargo test --bin gander -- --ignored agy_smoke --nocapture
```

**Manual end-to-end** against the release binary (use a throwaway `--db` so you don't
touch `~/.gander/media.db`):

```sh
cargo build --release
B=./target/release/gander

$B image.png --output-format json                 # describe → JSON envelope
$B image.png --backend codex --fallback-backend none   # force one backend

$B image.png --db /tmp/t.db                        # cold (calls the backend)
$B image.png --db /tmp/t.db                        # warm  → cached:true, $0

$B recall --db /tmp/t.db                           # browse the cache (no model call)
$B --check                                         # health-probe backends + ffmpeg
```

**Exit-code contract:**

```sh
$B good.png; echo $?                               # 0  ok/partial
$B --backend agy --model sonnet x.png; echo $?     # 2  usage (mismatch)
$B /does/not/exist; echo $?                        # 3  input
```

Notes: `--check` may report agy as *down* — a probe artifact, since the no-media health
prompt does not elicit agy's sentinel block; agy works fine in real describe runs.
Video CHUNKED runs are slow (one backend call per chunk plus a synthesis call), so a
>60s clip can take a few minutes.

## Usage

```sh
gander SOURCE [options]                 # describe one local file
gander recall [filters]                 # read-only cache browse (no model call)
gander config {path,show,clear}         # inspect / reset persisted defaults
gander cache path                       # print the cache DB path
gander cache clear [SOURCE]             # forget all assets, or just one file
gander --check                          # health-probe the backends + ffmpeg
gander --version
```

### Describe

| Flag | Default | Meaning |
|---|---|---|
| `SOURCE` | — | local path to one image / video / audio file (no URLs) |
| `--output-format {raw,json}` | `raw` | `json` emits the canonical envelope |
| `--model {pro,flash,sonnet,haiku,opus,gpt-5.5,gpt-5.4,gpt-5.4-mini}` | `pro` | primary model |
| `--backend {agy,claude,codex}` | `agy` | primary backend |
| `--fallback-model {…,none}` | `flash` | model for the fallback attempt (same set + `none`) |
| `--fallback-backend {…,none}` | `agy` | backend for the fallback attempt |
| `--no-transcript` | off | skip speech transcription |
| `--no-translate` | off | no English translation block |
| `--max-frames N` | `12` | evenly-spaced frames per clip/chunk (clamped `[1,64]`) |
| `--fps RATE` | — | fixed-rate frame sampling, capped by `--max-frames` |
| `--chunk-length S` | `60` | segment length for the chunked tier |
| `--max-chunks N` | `20` | cap on chunks; over-limit ⇒ widen segment length |
| `--max-duration S` | unset | hard-reject videos longer than S |
| `--force` | off | ignore any cached row and recompute |
| `--keep-temp` | off | keep the per-call temp workdir (path on stderr) |
| `--allowed-root DIR` | — | restrict SOURCE to paths under DIR |
| `--db PATH` | `~/.gander/media.db` | override the cache DB location |
| `--timeout S` | `300` | per-backend wall-clock seconds |
| `--reconfigure` | — | re-run the interactive first-run setup |
| `--no-config` | off | ignore the persisted config file for this run |

### Primary → fallback

Two attempts, each an explicit `(backend, model)` pair. The primary runs first; on
capacity(429)/timeout/transient/empty/unparseable it **demotes** to the fallback; an
**auth** signal **aborts** immediately (no fallback). `--fallback-model none` (or
`--fallback-backend none`) disables the second attempt. A backend/model mismatch
(e.g. `--backend agy --model sonnet`) is a usage error (exit `2`).

### Video tiers (chosen automatically from duration)

| Duration | Tier | What runs |
|---|---|---|
| `< 30s` | DIRECT | whole file to the backend |
| `30–60s` | SINGLE-BATCH | ffmpeg frames+audio → one backend call |
| `> 60s` | CHUNKED | stream-copy segments → per-chunk call → deterministic merge + one Flash prose-synthesis call |

### Recall

```
gander recall [--keyword K] [--text T] [--rating {keep,review,cull}] [--language L]
              [--kind {image,video,audio}] [--min-people N] [--min-duration S]
              [--has-transcript|--no-transcript] [--has-audio|--no-audio] [--chunked]
              [--include-failed] [--all-versions]
              [--order-by {updated_at,created_at,rating,people_count,duration_seconds}]
              [--asc] [--limit N] [--output-format {raw,json}]
```

## Output envelope (`--output-format=json`)

One object; **every key always present** (`null`/`""`/`0`/`[]` for N/A):

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

`0` ok/partial · `2` usage error · `3` input error (`failed`) · `4` backend/auth
failure · `1` unexpected. **`partial` exits `0`** — read the JSON `status`/`warnings`.

## Configuration

Per-setting precedence: **flag > `GANDER_*` env > `~/.gander/config.toml` > built-in.**

On the first interactive run (a real TTY), gander shows an **arrow-key picker** (↑/↓,
Enter) for the primary and fallback defaults — **backend first, then a model scoped to
that backend** (so the pair is always valid) — and saves them to
`~/.gander/config.toml`. A fallback backend of `none` skips the fallback model question.
The picker renders to stderr, so it never pollutes the stdout envelope. A non-interactive
run (the agent path) never prompts and never writes. `--reconfigure` re-runs the picker;
`--no-config` ignores the file for one run.

Env vars: `GANDER_DB_PATH`, `GANDER_AGY_BIN` / `GANDER_CLAUDE_BIN` / `GANDER_CODEX_BIN`,
`GANDER_FFPROBE_BIN` / `GANDER_FFMPEG_BIN`, `GANDER_ALLOWED_ROOT`,
`GANDER_MODEL_DEFAULT` / `GANDER_BACKEND_DEFAULT` /
`GANDER_FALLBACK_MODEL_DEFAULT` / `GANDER_FALLBACK_BACKEND_DEFAULT`,
`GANDER_PRINT_TIMEOUT_S`, `GANDER_CHUNK_LEN_S` / `GANDER_MAX_CHUNKS`,
`GANDER_MAX_DURATION_S`, `GANDER_MAX_FRAMES` / `GANDER_FRAME_FPS`.

## Security

`SOURCE` is treated as untrusted: symlinks/`..` are resolved first; URLs, NUL bytes,
non-regular files, and paths outside `--allowed-root` are rejected. Backend args are
passed as argv vectors (no shell). Frames and standalone images are EXIF/GPS-stripped
(`-map_metadata -1`). The source file is never mutated. The cache DB is `0600`.

> **Footgun:** the backends run with `--dangerously-skip-permissions` (agy),
> `bypassPermissions` (claude), and `--yolo` = `danger-full-access` (codex). These are
> on by design so the one-shot call is non-interactive — use `--allowed-root` and run
> against trusted inputs.
