# Changelog

Dates are YYYY-MM-DD. The newest entry is first.

## Unreleased

### Added

- `gander --models` lists the models each backend offers, with a class per model. It
  asks the backends for their live lists (`agy models`, `codex debug models`).
- `gander --check --deep[=KINDS]` runs a bundled image, audio file, and video through
  your configured primary and fallback, and checks the content of each answer.
- `error_class: model_unknown`, for a model name the backend does not know. Before, such
  a call was reported as `empty`.
- `error_class: not_installed`, for a backend whose CLI is not on `PATH`.
- A failed call now names the next command to run in `error`.
- A `[classes]` table in `~/.gander/config.toml` to change the class of a model family.
- The claude alias `fable`.
- `contrib/`: a watch script, and launchd and systemd files that run the deep check on a
  schedule.

### Changed

- **`--model` and `--fallback-model` accept any name.** They were a fixed list in the
  binary, so every rename by a backend needed a new gander build. Now the name goes to
  the backend as typed. A name that belongs to a different backend is still a usage error.
- **The codex default passes no model flag.** codex then uses the model from your own
  codex config. The old default, `gpt-5.5`, is no longer offered by codex, and every
  codex call failed.
- The agy aliases: `pro` runs `gemini-4-neon-medium` and `flash` runs
  `gemini-4-hydrogen-medium`. Both ran Gemini 3.8 Flash (Medium) before.
- The claude aliases `sonnet`, `haiku`, and `opus` run the 5-5 model ids.
- The first-run picker offers each backend's live model list.
- `--reconfigure` keeps the `[classes]` table.

### Background

Why these changes exist. The backends are separate products, and they change without
notice:

- 2026-08: one backend's audio path returned nothing for hours, while images worked and
  `--check` stayed green. A fallback with no audio support answered `partial`, which
  callers read as success. This is why the deep check reads the transcript and does not
  trust `status`.
- 2026-10: agy removed its Pro models and renamed Flash. Every agy call failed until
  gander was rebuilt with the new names, and the failure showed as `empty`. This is why
  model names now pass through, and why `model_unknown` exists.
- 2026-10: the per-call timeout went from 300 to 540 seconds. The same model answered a
  3 second audio file in 40 seconds on one run and in almost 5 minutes on the next.
- 2026-11: codex stopped offering all three models that gander listed. This is why the
  codex default no longer names a model.

## 0.1.0

The first version: describe an image, a video, or an audio file through `agy`, `claude`,
or `codex`, with a primary and a fallback, a local SQLite cache, and `recall` search.
