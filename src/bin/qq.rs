use anyhow::{Result, anyhow};
use clap::{ArgAction, Parser};
use qqqa::ai::{
    ChatClient, CliCompletionRequest, DEFAULT_REQUEST_TIMEOUT_SECS, Msg, run_cli_completion,
    run_cli_completion_streaming,
};
use qqqa::clipboard;
use qqqa::config::{Config, InitExistsError, ProviderConnection};
use qqqa::formatting::{
    StreamingFormatter, print_assistant_text, print_stream_token, render_xmlish_to_ansi,
    start_loading_animation,
};
use qqqa::history::read_recent_history;
use qqqa::prompt::{build_qq_system_prompt, build_qq_user_message, coalesce_prompt_inputs};
use qqqa::shell::{detect_shell, shell_hint_for_prompt};
use std::ffi::OsString;
use std::io::Write as _;
use std::io::{Read, Stdin};
use std::time::Duration;

/// qq — ask an LLM assistant a question
///
/// - Reads optional terminal history and piped stdin as context.
/// - Sends a single user prompt to an OpenAI-compatible endpoint.
#[derive(Debug, Parser)]
#[command(name = "qq", disable_colored_help = false, version, about)]
struct Cli {
    /// Initialize or reinitialize configuration (~/.qq/config.json) and exit
    #[arg(long = "init", action = ArgAction::SetTrue)]
    init: bool,

    /// Disable emojis going forward (persists to config)
    #[arg(long = "no-fun", action = ArgAction::SetTrue)]
    no_fun: bool,

    /// Persistently enable automatic copying of the first <cmd> block
    #[arg(long = "enable-auto-copy", action = ArgAction::SetTrue, conflicts_with = "disable_auto_copy")]
    enable_auto_copy: bool,

    /// Persistently disable automatic copying of the first <cmd> block
    #[arg(long = "disable-auto-copy", action = ArgAction::SetTrue, conflicts_with = "enable_auto_copy")]
    disable_auto_copy: bool,
    /// Profile name from config to use
    #[arg(short = 'p', long = "profile")]
    profile: Option<String>,

    /// Override the resolved provider base URL (e.g., http://localhost:11434/v1)
    #[arg(long = "api-base")]
    api_base: Option<String>,

    /// Override the model ID from profile
    #[arg(short = 'm', long = "model")]
    model: Option<String>,

    /// Disable terminal history context
    #[arg(short = 'n', long = "no-history", action = ArgAction::SetTrue)]
    no_history: bool,
    /// Include terminal history context even if disabled in config
    #[arg(long = "history", action = ArgAction::SetTrue, conflicts_with = "no_history")]
    history: bool,

    /// Disable streaming and wait for the full response before printing
    #[arg(long = "no-stream", action = ArgAction::SetTrue)]
    no_stream: bool,

    /// Temperature for this run (overrides profile/default)
    #[arg(long = "temperature")]
    temperature: Option<f32>,

    /// Auto-copy the first recommended command for this run
    #[arg(
        long = "copy-command",
        alias = "cc",
        visible_alias = "cc",
        action = ArgAction::SetTrue,
        conflicts_with = "no_copy_command"
    )]
    copy_command: bool,

    /// Disable auto-copying for this run (alias: --ncc, -ncc)
    #[arg(
        long = "no-copy-command",
        alias = "ncc",
        visible_alias = "ncc",
        action = ArgAction::SetTrue,
        conflicts_with = "copy_command"
    )]
    no_copy_command: bool,

    /// Print raw text (no formatting)
    #[arg(short = 'r', long = "raw", action = ArgAction::SetTrue)]
    raw: bool,

    /// Verbose internal logs
    #[arg(short = 'd', long = "debug", action = ArgAction::SetTrue)]
    debug: bool,

    /// The question to ask (free text). If omitted, stdin must be piped.
    #[arg(trailing_var_arg = true)]
    question: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse_from(normalized_cli_args());
    configure_color_output();

    // Run interactive init if requested.
    if cli.init {
        match Config::init_interactive(cli.debug) {
            Ok(path) => {
                if cli.debug {
                    eprintln!("[debug] Initialized config at {}", path.display());
                }
            }
            Err(e) => match e.downcast::<InitExistsError>() {
                Ok(init_err) => {
                    println!(
                        "Keeping your existing config at {}.\nMove or remove that file if you want to rerun --init.",
                        init_err.path.display()
                    );
                }
                Err(e) => return Err(e),
            },
        }
        return Ok(());
    }

    let config_flags_requested = cli.no_fun || cli.enable_auto_copy || cli.disable_auto_copy;
    if config_flags_requested {
        persist_config_flags(&cli)?;
    }

    // Detect piped stdin and read it if present.
    let stdin_is_tty = atty::is(atty::Stream::Stdin);
    let stdin_block = if !stdin_is_tty {
        Some(read_all_stdin(std::io::stdin())?)
    } else {
        None
    };

    let prepared = coalesce_prompt_inputs(cli.question.join(" "), stdin_block);
    if prepared.question.trim().is_empty() {
        if config_flags_requested {
            return Ok(());
        }
        return Err(anyhow!(
            "No input provided. Pass a question or pipe stdin (e.g., `ls -la | qq explain`)."
        ));
    }
    let qqqa::prompt::PromptInputs {
        question,
        stdin_block,
    } = prepared;

    // Load config and resolve profile/model.
    let (cfg, cfg_path) = Config::load_or_init(cli.debug)?;
    let cfg_dir = cfg_path.parent();
    let copy_enabled = if cli.copy_command {
        true
    } else if cli.no_copy_command {
        false
    } else {
        cfg.copy_first_command_enabled()
    };
    let mut eff = match cfg.resolve_profile(cli.profile.as_deref(), cli.model.as_deref(), cfg_dir) {
        Ok(eff) => eff,
        Err(e) => {
            let msg = e.to_string();
            let mut out = msg.clone();
            if msg.contains("Missing API key") {
                out.push_str(
                    "\n\nFix it quickly:\n- Run `qq --init` and choose provider; optionally paste the API key.\n- Or export an env var, e.g.\n    export OPENROUTER_API_KEY=... # OpenRouter (default)\n    export GROQ_API_KEY=...  # Groq\n    export OPENAI_API_KEY=... # OpenAI",
                );
            }
            return Err(anyhow!(out));
        }
    };
    if let Some(base) = cli.api_base.as_deref() {
        if let Some(http) = eff.http_mut() {
            http.base_url = base.to_string();
        } else {
            return Err(anyhow!(
                "--api-base override is only supported for HTTP providers (current: '{}').",
                eff.provider_key
            ));
        }
    }
    if let Some(temp) = cli.temperature {
        eff.temperature = Some(temp);
    }
    if cli.debug {
        match &eff.connection {
            ProviderConnection::Http(conn) => {
                eprintln!(
                    "[debug] Using provider='{}' base_url='{}' model='{}'",
                    eff.provider_key, conn.base_url, eff.model
                );
            }
            ProviderConnection::Cli(conn) => {
                eprintln!(
                    "[debug] Using provider='{}' cli_binary='{}' model='{}'",
                    eff.provider_key, conn.binary, eff.model
                );
            }
        }
    }

    // Read terminal history unless disabled.
    let include_history = if cli.no_history {
        false
    } else if cli.history {
        true
    } else {
        cfg.history_enabled()
    };
    let history = if include_history {
        read_recent_history(10, cli.debug)
    } else {
        Vec::new()
    };

    // Build system + user messages for formatting/topic control.
    let mut system = build_qq_system_prompt();
    if cfg.no_emoji_enabled() {
        system.push_str("\nHard rule: You MUST NOT use emojis anywhere in the response.\n");
    }
    if let Some(suffix) = &eff.prompt_suffix {
        system.push('\n');
        system.push_str(suffix);
    }
    let os_details = os_info::get();
    let os_type = os_details.os_type();
    let shell_kind = detect_shell(os_type);
    if cli.debug {
        eprintln!("[debug] Inferred shell: {}", shell_kind.display_name(),);
    }
    let shell_hint = shell_hint_for_prompt(shell_kind);
    let user = build_qq_user_message(
        Some(os_type),
        Some(shell_hint),
        &history,
        stdin_block.as_deref(),
        &question,
    );

    // Prepare backend-specific client.
    let http_client = match &eff.connection {
        ProviderConnection::Http(conn) => Some(
            ChatClient::new(
                conn.base_url.clone(),
                conn.api_key.clone(),
                conn.headers.clone(),
                conn.tls.as_ref(),
                conn.request_timeout_secs.map(Duration::from_secs),
            )?
            .with_reasoning_effort(eff.reasoning_effort.clone())
            .with_temperature(eff.temperature, eff.temperature.is_some()),
        ),
        ProviderConnection::Cli(_) => None,
    };

    let messages = [
        Msg {
            role: "system",
            content: &system,
        },
        Msg {
            role: "user",
            content: &user,
        },
    ];

    match (&eff.connection, &http_client) {
        (ProviderConnection::Http(_), Some(client)) => {
            if !cli.no_stream {
                if cli.raw {
                    println!("");
                    let mut raw_buffer = String::new();
                    client
                        .chat_stream_messages(&eff.model, &messages, cli.debug, |tok| {
                            raw_buffer.push_str(tok);
                            print_stream_token(tok);
                        })
                        .await?;
                    println!();
                    maybe_copy_first_command(&raw_buffer, copy_enabled, cli.raw, cli.debug);
                } else {
                    println!("");
                    let mut buf = String::new();
                    let mut formatter = StreamingFormatter::new();
                    let mut writer = PrettyStreamWriter::new();
                    client
                        .chat_stream_messages(&eff.model, &messages, cli.debug, |tok| {
                            buf.push_str(tok);
                            if let Some(delta) = formatter.push(tok) {
                                writer.write(&delta);
                            }
                        })
                        .await?;
                    if let Some(tail) = formatter.flush() {
                        if !tail.is_empty() {
                            writer.write(&tail);
                        }
                    }
                    println!("");
                    maybe_copy_first_command(&buf, copy_enabled, cli.raw, cli.debug);
                }
            } else {
                let loading = start_loading_animation();
                let full = client
                    .chat_once_messages(&eff.model, &messages, cli.debug)
                    .await?;
                drop(loading);
                println!("");
                print_assistant_text(&full, cli.raw);
                maybe_copy_first_command(&full, copy_enabled, cli.raw, cli.debug);
            }
        }
        (ProviderConnection::Cli(cli_conn), _) => {
            let timeout = cli_conn
                .request_timeout_secs
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS));
            let make_request = || CliCompletionRequest {
                engine: cli_conn.engine,
                binary: &cli_conn.binary,
                base_args: &cli_conn.base_args,
                system_prompt: &system,
                user_prompt: &user,
                model: &eff.model,
                reasoning_effort: eff.reasoning_effort.as_deref(),
                debug: cli.debug,
                timeout,
            };

            let streaming_enabled = !cli.no_stream && cli_conn.engine.supports_streaming();

            if streaming_enabled {
                println!("");
                if cli.raw {
                    let mut collected = String::new();
                    let fallback = run_cli_completion_streaming(make_request(), |tok| {
                        collected.push_str(tok);
                        print_stream_token(tok);
                    })
                    .await?;
                    if collected.is_empty() {
                        collected = fallback;
                    }
                    println!("");
                    maybe_copy_first_command(&collected, copy_enabled, cli.raw, cli.debug);
                } else {
                    let mut formatter = StreamingFormatter::new();
                    let mut writer = PrettyStreamWriter::new();
                    let mut collected = String::new();
                    let fallback = run_cli_completion_streaming(make_request(), |tok| {
                        collected.push_str(tok);
                        if let Some(delta) = formatter.push(tok) {
                            writer.write(&delta);
                        }
                    })
                    .await?;
                    if let Some(tail) = formatter.flush() {
                        if !tail.is_empty() {
                            writer.write(&tail);
                        }
                    }
                    if collected.is_empty() {
                        collected = fallback;
                    }
                    println!("");
                    maybe_copy_first_command(&collected, copy_enabled, cli.raw, cli.debug);
                }
            } else {
                if !cli.no_stream && cli.debug {
                    eprintln!(
                        "[debug] CLI provider '{}' does not support streaming; buffering output.",
                        eff.provider_key
                    );
                }
                let loading = start_loading_animation();
                let response = run_cli_completion(make_request()).await?;
                drop(loading);
                println!("");
                print_assistant_text(&response, cli.raw);
                maybe_copy_first_command(&response, copy_enabled, cli.raw, cli.debug);
            }
        }
        _ => unreachable!("Provider/client mismatch"),
    }

    Ok(())
}

/// Read the entire stdin into a string. We do this synchronously before async work
/// begins to keep things simple and robust.
fn read_all_stdin(mut stdin: Stdin) -> Result<String> {
    let mut buf = String::new();
    stdin.read_to_string(&mut buf)?;
    Ok(buf)
}

fn normalized_cli_args() -> Vec<OsString> {
    normalize_ncc(std::env::args_os())
}

fn normalize_ncc<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    args.into_iter()
        .enumerate()
        .map(|(idx, arg)| {
            if idx > 0 && arg == OsString::from("-ncc") {
                OsString::from("--ncc")
            } else {
                arg
            }
        })
        .collect()
}

struct PrettyStreamWriter {
    newline_run: usize,
}

impl PrettyStreamWriter {
    fn new() -> Self {
        Self { newline_run: 0 }
    }

    fn write(&mut self, text: &str) {
        let filtered = self.filter(text);
        if filtered.is_empty() {
            return;
        }
        print!("{}", filtered);
        let _ = std::io::stdout().flush();
    }

    fn filter(&mut self, text: &str) -> String {
        let mut out = String::new();
        for ch in text.chars() {
            match ch {
                '\r' => continue,
                '\n' => {
                    if self.newline_run < 2 {
                        out.push('\n');
                    }
                    if self.newline_run < 2 {
                        self.newline_run += 1;
                    }
                }
                _ => {
                    out.push(ch);
                    self.newline_run = 0;
                }
            }
        }
        out
    }
}

fn maybe_copy_first_command(text: &str, enabled: bool, raw_output: bool, debug: bool) {
    if !enabled {
        return;
    }
    let Some(command) = extract_first_command(text) else {
        if debug {
            eprintln!("[debug] No <cmd> block found to copy.");
        }
        return;
    };
    match clipboard::copy_to_clipboard(&command) {
        Ok(()) => print_copy_notice(raw_output),
        Err(err) => {
            eprintln!("Failed to copy first command to clipboard: {}", err);
        }
    }
}

fn extract_first_command(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("<cmd>")?;
    let after_start = start + 5;
    let closing_rel = lower[after_start..].find("</cmd>")?;
    let end = after_start + closing_rel;
    let raw = &text[after_start..end];
    let normalized = raw
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<br>", "\n");
    let unescaped = unescape_entities(&normalized);
    let trimmed = unescaped.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn unescape_entities(input: &str) -> String {
    input
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn print_copy_notice(raw_output: bool) {
    println!("");
    if raw_output {
        println!("<info>Copied first command to clipboard</info>");
    } else {
        println!(
            "{}",
            render_xmlish_to_ansi("<info>Copied first command to clipboard</info>")
        );
    }
}

fn persist_config_flags(cli: &Cli) -> Result<()> {
    let (mut cfg, path) = Config::load_or_init(cli.debug)?;
    let mut changed = false;

    if cli.no_fun {
        let desired = Some("true".to_string());
        if cfg.no_emoji != desired {
            cfg.no_emoji = desired;
            changed = true;
        }
        if cli.debug {
            eprintln!(
                "[debug] Disabled emojis in system prompt (persisted at {}).",
                path.display()
            );
        }
    }

    if cli.enable_auto_copy {
        if !cfg.copy_first_command_enabled() {
            cfg.set_copy_first_command(true);
            changed = true;
        }
        println!("Auto-copy first command: enabled (persisted).");
    }

    if cli.disable_auto_copy {
        if cfg.copy_first_command_enabled() {
            cfg.set_copy_first_command(false);
            changed = true;
        }
        println!("Auto-copy first command: disabled (persisted).");
    }

    if changed {
        cfg.save(&path, cli.debug)?;
    }

    Ok(())
}

fn configure_color_output() {
    let stdout_is_tty = atty::is(atty::Stream::Stdout);
    let enable_color = if env_var_truthy("CLICOLOR_FORCE") {
        true
    } else if std::env::var_os("NO_COLOR").is_some() || env_var_falsey("CLICOLOR") {
        false
    } else {
        stdout_is_tty
    };
    qqqa::formatting::set_color_output_enabled(enable_color);
}

fn env_var_truthy(name: &str) -> bool {
    matches!(env_var_bool(name), Some(true))
}

fn env_var_falsey(name: &str) -> bool {
    matches!(env_var_bool(name), Some(false))
}

fn env_var_bool(name: &str) -> Option<bool> {
    let raw = std::env::var(name).ok()?;
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_first_command_unescapes_entities_and_br() {
        let input = "<cmd>echo foo&amp;&lt;bar&gt;<br/>ls</cmd><cmd>pwd</cmd>";
        let extracted = extract_first_command(input).expect("should find command");
        assert_eq!(extracted, "echo foo&<bar>\nls");
    }

    #[test]
    fn extract_first_command_returns_none_when_missing() {
        assert!(extract_first_command("<info>No commands here</info>").is_none());
    }

    #[test]
    fn normalize_ncc_rewrites_short_flag() {
        let args = vec![
            OsString::from("qq"),
            OsString::from("-ncc"),
            OsString::from("status"),
        ];
        let normalized = normalize_ncc(args);
        assert_eq!(normalized[1], OsString::from("--ncc"));
    }

    #[test]
    fn pretty_stream_writer_limits_blank_lines() {
        let mut writer = PrettyStreamWriter::new();
        let first = writer.filter("foo\n\n\nbar\r\n\n");
        assert_eq!(first, "foo\n\nbar\n\n");
        // Additional newlines without intervening text should be dropped.
        let second = writer.filter("\n\n\nbaz");
        assert_eq!(second, "baz");
    }
}
