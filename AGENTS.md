# Repository Guidelines

## Project Structure & Module Organization
- Source lives in `src/` with two binaries: `src/bin/qq.rs` and `src/bin/qa.rs`.
- Core modules: `ai.rs` (HTTP client, SSE parsing, CLI backends), `config.rs` (layered config, atomic locked saves), `prompt.rs`, `history.rs`, `perms.rs`, `formatting.rs` (tag renderer, stdout helpers), `shell.rs` (shell and OS detection), `input.rs` (piped stdin), `interrupt.rs` (process-wide Ctrl-C), `clipboard.rs`.
- Tools for the agent live in `src/tools/` (`read_file.rs`, `write_file.rs`, `execute_command.rs`).
- Integration tests live in `tests/`; `tests/e2e_tests.rs` runs the real binaries against an `httpmock` server. Build artifacts are in `target/`.
- Architecture notes: `src/lib.rs` re-exports the runtime modules and `src/tools/mod.rs` centralizes tool parsing/dispatch; refer to `README.md` for the high-level architecture and behavior overview.

## Build, Test, and Development Commands
- Build: `cargo build` (debug) or `cargo build --release`.
- Run qq: `cargo run --bin qq -- --help` or `cargo run --bin qq -- "your question"`.
- Run qa (agent): `cargo run --bin qa -- "your task"` (adds tool execution).
- Tests: `cargo test --locked` (runs all in `tests/` and unit tests; no API key needed).
- Before pushing, run what CI enforces: `cargo fmt --all --check` and `cargo clippy --locked --all-targets -- -D warnings`. CI tests on Linux, macOS and Windows.
- Rust 1.89 or newer (`rust-version`). `Cargo.lock` is committed; keep builds `--locked`.
- Debug logs: `RUST_LOG=debug cargo run --bin qq -- -d "..."`.

## Coding Style & Naming Conventions
- Rust 2024 edition; format with `rustfmt` (4‑space indents). Clippy warnings fail CI.
- Naming: `snake_case` for functions/modules, `PascalCase` for types, `SCREAMING_SNAKE_CASE` for consts.
- Errors: favor `anyhow` for app layers and `thiserror` for library error types; use `?` over `unwrap()` in binaries.
- Keep modules focused and functions small; prefer pure helpers in `src/` over ad‑hoc logic in bins.

## Testing Guidelines
- Integration tests in `tests/*.rs`; unit tests colocated with modules.
- Name tests descriptively: `test_<behavior>()`; table‑driven where useful.
- Mock HTTP with `httpmock`; use `tempfile` for fs tests; avoid global state.
- Run: `cargo test`; add edge‑case coverage for config permissions and tool safety.
- Test what users see: assert on printed output and exit codes (see `tests/e2e_tests.rs`), not only on internal return values.
- Tests that point `HOME` at a temp dir must be skipped on Windows (`dirs::home_dir` ignores `HOME` there); see the existing `cfg_attr(windows, ignore = ...)`.

## Commit & Pull Request Guidelines
- Commits: imperative, scoped messages (e.g., `feat: add qa execute_command safety checks`).
- PRs: include a clear description, reproduction/verification steps, relevant logs, and screenshots when output formatting changes.
- Link issues, keep diffs minimal, and update docs/tests when behavior changes. Add user-visible changes to the `Unreleased` section of `CHANGELOG.md`.

## Security & Configuration Tips
- Config path: `~/.qq/config.json` (or `$XDG_CONFIG_HOME/qq/config.json` when no legacy file exists); permissions dir `700`, file `600`. The file holds only overrides: defaults are built into `Config::default()` and merged at load. Save through `Config::save` (atomic) or `Config::update` (locked read-modify-write), never by writing the file directly.
- API keys via env: `OPENROUTER_API_KEY` (default), `OPENAI_API_KEY`, `GROQ_API_KEY`, `GEMINI_API_KEY`, `ANTHROPIC_API_KEY` (placeholder provider); never commit secrets.
- Agent (`qa`): print proposed commands, require confirmation unless `--yes`; avoid destructive patterns and paths outside the workspace/home.
- Command allowlist: default safe commands are bundled. Approving a blocked binary persists it under `command_allowlist` in `~/.qq/config.json` for future runs.
