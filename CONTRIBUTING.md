# Contributing

Thank you for helping. This file tells you how to build, test, and change gander.

## Set up

You need the Rust toolchain. [`just`](https://github.com/casey/just) is optional.

```sh
just build     # or: cargo build
just test      # or: cargo test --bin gander
just ci        # format check, clippy, and tests. CI runs the same three.
```

The unit tests need no backend and no network. They take about one second.

## Before you open a pull request

1. Run `just ci`. It must pass.
2. Add a test for the change. Most modules have a `tests` block at the end of the file.
3. Update `README.md` if a flag, an output field, or an exit code changes.
4. Add a line to `CHANGELOG.md` under "Unreleased".

## Changes that touch a backend

The unit tests cannot see a real backend. If your change touches `src/backend/`,
`src/models.rs`, or the prompt, also run the real checks. They use your own logins and
make real calls:

```sh
just check             # one text prompt per backend
just check-deep        # the bundled image, audio, and video
just models            # the live model lists
```

Say in the pull request which backends you ran, and paste the output.

## Where things are

| Path | What it holds |
|---|---|
| `src/main.rs`, `src/cli.rs` | the command line, dispatch, and exit codes |
| `src/core.rs`, `src/video.rs` | the describe flow and the three video tiers |
| `src/backend/` | one adapter per backend, the two runners, error classes, and the fallback ladder |
| `src/models.rs` | aliases, the built-in and live model lists, and classes |
| `src/check.rs`, `src/deep.rs` | `--check` and `--check --deep` |
| `src/prompt.rs`, `src/parse.rs` | the prompt, and the parser for the answer |
| `src/db.rs` | the SQLite cache and `recall` |
| `assets/probe/` | the three files of the deep check, and how to build them again |
| `contrib/` | files that run the deep check on a schedule |

## Common changes

### A backend renamed its models

Users do not need a new build: `--model` passes any name through. Update the built-in
snapshot anyway, so that the offline list and the aliases stay correct:

1. Run `gander --models` and compare it with the lists in `src/models.rs`.
2. Update the lists. If an alias target is gone, update `PRO` or `FLASH` in
   `src/config.rs`.
3. If the new model family has a clear class, add its family word to `DEFAULT_CLASSES`.
4. Run `just check-deep` with the changed alias.

### A backend changed how it reports an unknown model

Add its wording to `model_unknown_re` in `src/backend/mod.rs`, and add the real output to
the test next to it.

### Add a backend

1. Add `src/backend/<name>.rs` and implement the `AgentBackend` trait. `codex.rs` is the
   shortest example.
2. Add the backend to `BackendKind`, `backend_for`, and the `Backend` enums in `src/cli.rs`.
3. Add its aliases and its built-in list to `src/models.rs`. If it has a command that
   lists models, add a parser to `live`.
4. Run `just check-deep --backend <name>`.

## Style

- Run `cargo fmt`. Clippy warnings fail CI.
- Write comments that explain why, not what. If a rule in the code came from a real
  failure, say what failed.
- Keep the JSON envelope stable. Agents parse it. A new key needs a new
  `SCHEMA_VERSION`.

## Security

gander runs the backends with their permission checks off (see "Security" in the
README). Please report a security problem in private to the repository owner, not in a
public issue.
