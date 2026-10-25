# Changelog

All notable changes to qqqa. Versions follow [Semantic Versioning](https://semver.org/).

## [1.1.0] - 2026-10-25

A correctness release: qq and qa now show exactly what the model wrote, report every failure with a non-zero exit code, and behave in pipes and scripts. The config file holds only your own changes, and the default models are newer and faster.

### Changes you may notice

- **qa exit codes.** After a command, qa prints `[tool:execute_command] exit status N` on stderr and exits with the command's own code N (128 + N for a signal). A failed or unknown tool prints its error on stderr and exits 1. Before, qa always exited 0.
- **qq fails loudly.** qq exits non-zero when the provider reports an error in the stream, when the stream ends early, when the answer is empty, or when it stops at the output limit. Whatever arrived is still printed first.
- **Config file format.** The file now holds only what differs from the built-in defaults. A file from an older release still works and is shortened the next time qqqa saves it (for example after `qq --no-fun`). Values you never set now follow the built-in defaults, so new providers, profiles and models reach you without editing the file.
- **New default models.** The default `openrouter` profile uses `qwen/qwen3.8-27b` routed to Cerebras (was `openai/gpt-4.1-nano`). New `luna` (`openai/gpt-6-luna`) and `haiku` (`anthropic/claude-haiku-5.5`) profiles. `openai` uses `gpt-6-luna`; `claude_cli` uses `claude-haiku-5-5`.
- **Piped input.** qq and qa read stdin only when it is a pipe or a file, up to 256 KiB (longer input is cut with a notice). Binary input is refused. Use the new `--no-stdin` inside `while read` loops.
- **Output limit.** The fixed 800-token cap is gone: 4096 tokens by default, 16384 for reasoning models, configurable per profile with `max_tokens` and `reasoning_max_tokens`.
- **Requirements.** Building from source needs Rust 1.89 or newer. The Claude Code profile needs a Claude Code release that supports `--tools`.

### Added

- Per-provider `headers` and `extra_body`, and per-profile `extra_body`, for example OpenRouter provider routing (#10).
- Per-profile and global `system_prompt` / `system_prompt_file` to replace qq's built-in system prompt (#16).
- `--no-stdin` for qq and qa.
- `max_tokens` and `reasoning_max_tokens` per profile.

### Fixed

- **Rendering.** Text between `<` and `>` that is not a known tag is now shown (`sort < in > out`, `kill -9 <PID>`, `<(…)`); it was silently deleted. A stray `<` no longer stalls streaming, and repeated first tokens (`-`, `-force`) are no longer dropped.
- **Streaming.** Server-sent events follow the spec: `data:` without a space and multi-line data are accepted, and a plain JSON reply to a streaming request works.
- **qa tool calls.** Replies inside a Markdown code fence, `arguments` sent as an object, a missing `type`, and the `json` wrapper on CLI providers are accepted. An explanation sent through the `json` tool is printed instead of failing.
- **Config writes.** Saves are atomic (temp file, mode 0600, rename) and locked, so a crash cannot leave an empty file and two runs cannot lose each other's change. Unknown keys are kept.
- **Model parameters.** `vendor/` prefixes are understood (`openai/gpt-5-mini`); GPT-5 and later and the o-series get `max_completion_tokens` and no temperature; `minimal` effort goes only to models that accept it; a configured `reasoning_effort` is always sent.
- **Codex** receives `-m` and `model_override`.
- **CLI providers** (Codex, Claude Code, Gemini CLI): one timeout covers the whole run, large prompts cannot deadlock, the provider's own error is shown instead of "Broken pipe", and a timeout or Ctrl-C ends everything the provider started. Ctrl-C exits with 130 in every phase, including qa's confirmation prompt.
- **Claude Code** gets the prompt on stdin (large input works) and runs with no tools and no MCP servers, which makes each request about 74 KB smaller.
- `qq … | head` exits quietly instead of panicking; other stdout write errors exit 1.
- `--no-stream` no longer waits up to 300 ms for the loading animation, and the animation is not drawn into redirected stderr.
- The Gemini profiles from 1.0.1 now reach existing configs (#43).

### Changed

- The OS name is read without starting processes (`os_info` removed, with `plist`, `quick-xml` and `time`).
- `Cargo.lock` is committed and release builds use `--locked`; `atty` is replaced by `std::io::IsTerminal`.
- CI runs rustfmt, clippy (`-D warnings`), and tests on Linux, macOS and Windows.

## [1.0.1]

Suffix prompts, Gemini CLI support, XDG config directory.

[1.1.0]: https://github.com/iagooar/qqqa/compare/v1.0.1...v1.1.0
[1.0.1]: https://github.com/iagooar/qqqa/compare/v1.0.0...v1.0.1
