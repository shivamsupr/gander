# gander dev tasks — run `just` to list, `just <recipe>` to invoke.
# Requires: cargo, ffmpeg/ffprobe + a backend on PATH for the live/run recipes.

# Show available recipes.
default:
    @just --list

# Debug build.
build:
    cargo build

# Optimized release build → target/release/gander.
release:
    cargo build --release

# Install the `gander` binary to ~/.local/bin (must be on PATH), as the README says.
# NOT `cargo install`: that targets ~/.cargo/bin, which sits earlier on most PATHs
# and would silently shadow a binary installed the documented way.
# The rm is required on macOS: overwriting a signed binary in place breaks its
# signature and the kernel then SIGKILLs it.
install: release
    rm -f ~/.local/bin/gander
    install -m 755 target/release/gander ~/.local/bin/gander
    @[ "$(uname)" = "Darwin" ] && codesign --force --sign - ~/.local/bin/gander || true
    @gander --version

# Run gander with arbitrary args (debug build).
#   just run image.png --output-format json
run *ARGS:
    cargo run --quiet -- {{ARGS}}

# Deterministic test suite (no live backends, no network).
test:
    cargo test --bin gander

# Live backend smoke (real agy; #[ignore]d by default). Needs agy logged in.
test-live:
    cargo test --bin gander -- --ignored agy_smoke --nocapture

# Format the code.
fmt:
    cargo fmt

# CI-style checks: formatting + lints + tests (mirrors .github/workflows/ci.yml).
ci:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test --bin gander

# Lint with clippy (warnings as errors).
clippy:
    cargo clippy --all-targets -- -D warnings

# Describe a file with the debug binary, JSON envelope. Extra flags pass through.
#   just describe path/to/img.png --backend codex
describe SOURCE *ARGS:
    cargo run --quiet -- "{{SOURCE}}" --output-format json {{ARGS}}

# Health-probe the backends + ffmpeg.
check:
    cargo run --quiet -- --check

# Browse the cache (read-only). Extra flags pass through.
#   just recall --kind image --limit 5
recall *ARGS:
    cargo run --quiet -- recall {{ARGS}}

# Inspect or reset persisted defaults: `path` | `show` | `clear`.
#   just config path   ·   just config clear
config *ARGS:
    cargo run --quiet -- config {{ARGS}}

# Inspect or clear the result cache.
#   just cache path                     # print the DB path
#   just cache clear                    # forget ALL cached assets
#   just cache clear path/to/file.png   # forget one asset
cache *ARGS:
    cargo run --quiet -- cache {{ARGS}}

# Re-run the interactive first-run picker (rewrites the config file).
reconfigure:
    cargo run --quiet -- --reconfigure

# Static musl release build for Linux artifacts (needs `cross` / Docker).
#   just musl x86_64-unknown-linux-musl
musl TARGET="x86_64-unknown-linux-musl":
    cross build --release --target {{TARGET}}

# Remove build artifacts.
clean:
    cargo clean
