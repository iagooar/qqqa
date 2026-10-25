# Contributing & Release Guide

Thanks for your interest in qqqa. This doc is for contributors and for anyone cutting releases.

## How to Contribute

- Issues
  - Report bugs and feature requests via GitHub Issues. Include repro steps, logs, and environment info when possible.
- Pull Requests
  - Small, focused diffs are best. Describe the change, rationale, and how to verify.
  - Keep style consistent with the repo. Prefer `anyhow` in app code, `thiserror` for library errors.
  - Update docs/tests when behavior changes, and add a line to the `Unreleased` part of `CHANGELOG.md` for anything users will notice. Avoid committing secrets.
  - A new OpenAI-compatible provider usually needs no code change: it is a `model_providers` entry in the user's config (see the README). Built-in providers are kept to a small set.
- Development basics
  - Rust 1.89 or newer (`rust-version` in `Cargo.toml`).
  - Build: `cargo build` (or `cargo build --release`)
  - Run: `cargo run --bin qq -- --help` and `cargo run --bin qa -- --help`
  - Test: `cargo test`. End-to-end tests in `tests/e2e_tests.rs` run the real binaries against a local mock server, so no API key is needed.
- Before you push, run what CI runs (it fails on any warning):
  ```
  cargo fmt --all --check
  cargo clippy --locked --all-targets -- -D warnings
  cargo test --locked
  ```
  CI runs the tests on Linux, macOS and Windows.
- `Cargo.lock` is committed. Use `--locked` so builds match it; run `cargo update` in a separate change when you want newer dependencies.

## Building Releases

We keep prebuilt binaries in `target/releases/` inside the repo and retain ~3 latest versions; this directory is already gitignored, so the tarballs stay local and never bloat the repo. The `scripts/release.sh` helper builds and packages everything and prints next steps. It never commits, tags or pushes.

Prerequisites
- A clean working tree on a commit whose CI is green.
- Rust 1.89 or newer; `rustup` available to add targets (the script adds missing ones).
- Cross toolchains for the default targets (on macOS via Homebrew):
  - Linux MUSL: `brew install filosottile/musl-cross/musl-cross` (provides `x86_64-linux-musl-gcc` and `aarch64-linux-musl-gcc`). Homebrew ignores formulae from untrusted taps, so trust it once: `brew trust --formula filosottile/musl-cross/musl-cross`.
  - Windows x86_64: `brew install mingw-w64` (provides `x86_64-w64-mingw32-gcc`).
  - Windows arm64: an [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) bundle in `/opt/llvm-mingw` (provides `aarch64-w64-mingw32-clang`; the script adds `/opt/llvm-mingw/bin` to `PATH`).
- Smoke-test the release build against real providers before tagging (at least the default profile, one CLI provider and one `qa` tool call).

Steps (example for 1.2.0)
```
# 1. Move the Unreleased notes in CHANGELOG.md under "## [1.2.0] - <date>".

# 2. Bump the version, build and package all targets.
#    Updates Cargo.toml and Cargo.lock, writes target/releases/v1.2.0/.
scripts/release.sh v1.2.0

# 3. Commit (through a PR), then tag the merged commit.
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "release: v1.2.0"
git tag -a v1.2.0 <merge-commit-sha> -m "qqqa v1.2.0"
git push origin v1.2.0

# 4. Rebuild from the tagged commit, then publish the GitHub release
#    with the CHANGELOG section as notes.
git checkout v1.2.0
scripts/release.sh v1.2.0 "$(git rev-parse HEAD)"
gh release create v1.2.0 --verify-tag --latest \
  --title "v1.2.0 - <summary>" --notes-file <changelog-section.md> \
  target/releases/v1.2.0/qqqa-v1.2.0-*.tar.gz \
  target/releases/v1.2.0/qqqa-v1.2.0-*.zip \
  target/releases/v1.2.0/manifest.json
```

Passing a SHA as the second argument only adds it to `manifest.json` and to the printed tag command.

Limit targets when you only want a subset (e.g., during local smoke tests). Required targets (both Linux MUSL builds by default) are always added; set `REQUIRED_TARGETS=" "` to skip them:
```
TARGETS="aarch64-apple-darwin" REQUIRED_TARGETS=" " scripts/release.sh v1.2.0
```

What the script does
- Bumps the version in `Cargo.toml` (normalized to SemVer) and qqqa's own entry in `Cargo.lock`.
- Builds `qq` and `qa` with `--locked` for macOS (x86_64/arm64), Linux MUSL (x86_64/arm64), and Windows (x86_64 GNU + arm64 gnullvm) by default; override `TARGETS` if needed.
- Packages `qqqa-v<version>-<target>.tar.gz` (`.zip` for Windows) under `target/releases/v<version>/` with README and LICENSE.
- Packs `qqqa-v<version>-src.tar.gz` with `git archive`, so it holds only tracked files (untracked local folders never end up in it).
- Writes `target/releases/v<version>/manifest.json` with the SHA-256 of every artifact.
- Prints a checklist covering the commit, the tag and `gh release create`.

Homebrew
- `brew install qqqa` installs from homebrew-core, which picked up v1.1.0 from the GitHub release within hours. After a release, check `brew info qqqa` shows the new version; if it does not after a day or two, open a version-bump PR on [homebrew-core](https://github.com/Homebrew/homebrew-core) (`brew bump-formula-pr qqqa --tag v<version>`).

Notes
- Windows artifacts use MSVC targets on Windows hosts. When cross-compiling from macOS/Linux, the x86_64 build uses MinGW-w64 (`x86_64-pc-windows-gnu`) and the arm64 build uses llvm-mingw (`aarch64-pc-windows-gnullvm`).

## Code of Conduct

Be respectful, constructive, and considerate. We value practical collaboration and helpful reviews.
