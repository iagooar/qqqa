use anyhow::{Context, Result, anyhow};
use clap::{ArgAction, Parser};
use qqqa::ai::{
    AssistantReply, ChatClient, CliCompletionRequest, DEFAULT_REQUEST_TIMEOUT_SECS, Msg,
    run_cli_completion,
};
use qqqa::config::{Config, InitExistsError, ProviderConnection};
use qqqa::history::read_recent_history;
use qqqa::perms;
use qqqa::prompt::{build_qa_system_prompt, build_qa_user_message, coalesce_prompt_inputs};
use qqqa::shell::{ShellKind, detect_shell, shell_hint_for_prompt};
use qqqa::tools::execute_command::CommandOutput;
use qqqa::tools::{Reply, ToolCall, parse_reply_text, resolve_tool_call};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// qa — single-step agent that may use one tool
#[derive(Debug, Parser)]
#[command(name = "qa", disable_colored_help = false, version, about)]
struct Cli {
    /// Initialize or reinitialize configuration (~/.qq/config.json) and exit
    #[arg(long = "init", action = ArgAction::SetTrue)]
    init: bool,
    /// Profile name from config to use
    #[arg(short = 'p', long = "profile")]
    profile: Option<String>,

    /// Override the resolved provider base URL (e.g., http://localhost:11434/v1)
    #[arg(long = "api-base")]
    api_base: Option<String>,

    /// Override the model ID from profile
    #[arg(short = 'm', long = "model")]
    model: Option<String>,

    /// Temperature for this run (overrides profile/default)
    #[arg(long = "temperature")]
    temperature: Option<f32>,

    /// Disable terminal history context
    #[arg(short = 'n', long = "no-history", action = ArgAction::SetTrue)]
    no_history: bool,

    /// Ignore stdin even when it is a pipe or a file (for loops and wrappers)
    #[arg(long = "no-stdin", action = ArgAction::SetTrue)]
    no_stdin: bool,
    /// Include terminal history context even if disabled in config
    #[arg(long = "history", action = ArgAction::SetTrue, conflicts_with = "no_history")]
    history: bool,

    /// Verbose internal logs
    #[arg(short = 'd', long = "debug", action = ArgAction::SetTrue)]
    debug: bool,

    /// Auto-approve command execution (use with caution)
    #[arg(short = 'y', long = "yes", action = ArgAction::SetTrue)]
    yes: bool,

    /// Disable emojis going forward (persists to config)
    #[arg(long = "no-fun", action = ArgAction::SetTrue)]
    no_fun: bool,

    /// The task to perform. If omitted, stdin must be piped.
    #[arg(trailing_var_arg = true)]
    task: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.init {
        match Config::init_interactive(cli.debug) {
            Ok(path) => {
                if cli.debug {
                    eprintln!("[debug] Initialized config at {}", path.display());
                }
            }
            Err(e) => match e.downcast::<InitExistsError>() {
                Ok(init_err) => {
                    qqqa::outln!(
                        "Keeping your existing config at {}.\nMove or remove that file if you want to rerun --init.",
                        init_err.path.display()
                    );
                }
                Err(e) => return Err(e),
            },
        }
        return Ok(());
    }

    // Handle stdin piping for extra context.
    let stdin_block = if cli.no_stdin {
        None
    } else {
        qqqa::input::read_piped_stdin()?
    };

    let prepared = coalesce_prompt_inputs(cli.task.join(" "), stdin_block);
    if prepared.question.trim().is_empty() {
        return Err(anyhow!("No input provided. Pass a task or pipe stdin."));
    }
    let qqqa::prompt::PromptInputs {
        question: task,
        stdin_block,
    } = prepared;

    let (mut cfg, path) = Config::load_or_init(cli.debug)?;
    perms::set_custom_allowlist(cfg.command_allowlist());
    if cli.no_fun {
        cfg = Config::update(&path, cli.debug, |c| c.no_emoji = Some("true".to_string()))?;
        if cli.debug {
            eprintln!(
                "[debug] Disabled emojis in system prompt (persisted at {}).",
                path.display()
            );
        }
    }
    let cfg_dir = path.parent();
    let mut eff = match cfg.resolve_profile(cli.profile.as_deref(), cli.model.as_deref(), cfg_dir) {
        Ok(eff) => eff,
        Err(e) => {
            let msg = e.to_string();
            let mut out = msg.clone();
            if msg.contains("Missing API key") {
                out.push_str(
                    "\n\nFix it quickly:\n- Run `qa --init` and choose provider; optionally paste the API key.\n- Or export an env var, e.g.\n    export OPENROUTER_API_KEY=... # OpenRouter (default)\n    export GROQ_API_KEY=...       # Groq\n    export OPENAI_API_KEY=...     # OpenAI\n    export ANTHROPIC_API_KEY=...  # Anthropic (Claude)",
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
    let os_details = os_info::get();
    let os_type = os_details.os_type();
    let shell_kind = detect_shell(os_type);
    if cli.debug {
        eprintln!("[debug] Inferred shell: {}", shell_kind.display_name(),);
    }

    let mut system_prompt = build_qa_system_prompt();
    if cfg.no_emoji_enabled() {
        system_prompt.push_str("\nHard rule: You MUST NOT use emojis anywhere in the response.\n");
    }
    if let Some(suffix) = &eff.prompt_suffix {
        system_prompt.push('\n');
        system_prompt.push_str(suffix);
    }
    let shell_hint = shell_hint_for_prompt(shell_kind);
    let user_msg = build_qa_user_message(
        Some(os_type),
        Some(shell_hint),
        &history,
        stdin_block.as_deref(),
        &task,
    );

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
            .with_temperature(eff.temperature, eff.temperature.is_some())
            .with_max_tokens(eff.max_tokens, eff.reasoning_max_tokens)
            .with_extra_body(conn.extra_body.clone()),
        ),
        ProviderConnection::Cli(_) => None,
    };
    // Provide tool specs so the API can emit structured tool_calls instead of erroring.
    let tools_spec = serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a UTF-8 text file up to a safe size.",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "json",
                "description": r#"Wrap another tool call as { "tool": string, "arguments": object }."#,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "tool": {"type": "string"},
                        "arguments": {"type": "object"}
                    },
                    "required": ["tool", "arguments"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Create or overwrite a UTF-8 text file.",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "execute_command",
                "description": "Run a safe shell command in the workspace/home.",
                "parameters": {
                    "type": "object",
                    "properties": {"command": {"type": "string"}, "cwd": {"type": "string"}},
                    "required": ["command"]
                }
            }
        }
    ]);

    let assistant_reply = match (&eff.connection, &http_client) {
        (ProviderConnection::Http(_), Some(client)) => {
            client
                .chat_once_messages_with_tools(
                    &eff.model,
                    &[
                        Msg {
                            role: "system",
                            content: &system_prompt,
                        },
                        Msg {
                            role: "user",
                            content: &user_msg,
                        },
                    ],
                    tools_spec,
                    cli.debug,
                )
                .await?
        }
        (ProviderConnection::Cli(cli_conn), _) => {
            let timeout = cli_conn
                .request_timeout_secs
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS));
            let text = run_cli_completion(CliCompletionRequest {
                engine: cli_conn.engine,
                binary: &cli_conn.binary,
                base_args: &cli_conn.base_args,
                system_prompt: &system_prompt,
                user_prompt: &user_msg,
                model: &eff.model,
                reasoning_effort: eff.reasoning_effort.as_deref(),
                debug: cli.debug,
                timeout,
            })
            .await?;
            AssistantReply::Content(text)
        }
        _ => unreachable!("Provider/client mismatch"),
    };

    let reply = match assistant_reply {
        AssistantReply::ToolCall {
            name,
            arguments_json,
        } => {
            if cli.debug {
                eprintln!("[debug] tool call '{}' args: {}", name, arguments_json);
            }
            resolve_tool_call(&name, serde_json::Value::String(arguments_json))?
        }
        AssistantReply::Content(assistant) => {
            if cli.debug {
                eprintln!("[debug] assistant raw: {}", assistant);
            }
            // A bare or fenced `{ "tool": .., "arguments": .. }` is a tool call; else the answer.
            parse_reply_text(&assistant)?
        }
    };
    let exit_code = match reply {
        Reply::Tool(call) => run_tool(call, cli.yes, cli.debug, shell_kind, &mut cfg, &path).await,
        Reply::Text(text) => {
            qqqa::outln!("{}", text.trim_end());
            0
        }
    };

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

/// Run one tool call and return qa's exit code: the command's own exit code
/// for `execute_command`, 1 when a tool fails, else 0. Results go to stdout;
/// errors and the command's exit status go to stderr.
async fn run_tool(
    call: ToolCall,
    auto_yes: bool,
    debug: bool,
    shell: ShellKind,
    cfg: &mut Config,
    cfg_path: &Path,
) -> i32 {
    match call {
        ToolCall::ReadFile(args) => match qqqa::tools::read_file::run(args) {
            Ok(content) => {
                print_tool_result("read_file", &content);
                0
            }
            Err(e) => print_tool_error("read_file", &e.to_string()),
        },
        ToolCall::WriteFile(args) => match qqqa::tools::write_file::run(args) {
            Ok(summary) => {
                print_tool_result("write_file", &summary);
                0
            }
            Err(e) => print_tool_error("write_file", &e.to_string()),
        },
        ToolCall::ExecuteCommand(args) => {
            match run_execute_command_with_allowlist(args, auto_yes, debug, shell, cfg, cfg_path)
                .await
            {
                Ok(output) => {
                    eprintln!("[tool:execute_command] exit status {}", output.exit_code);
                    output.exit_code
                }
                Err(e) => print_tool_error("execute_command", &e.to_string()),
            }
        }
    }
}

async fn run_execute_command_with_allowlist(
    args: qqqa::tools::execute_command::Args,
    auto_yes: bool,
    debug: bool,
    shell: ShellKind,
    cfg: &mut Config,
    cfg_path: &Path,
) -> Result<CommandOutput> {
    let mut base_dir = std::env::current_dir().context("Failed to read current directory")?;
    if let Ok(canon) = base_dir.canonicalize() {
        base_dir = canon;
    }

    let original_args = args;
    loop {
        let mut stream_printer = |chunk: qqqa::tools::execute_command::StreamChunk| match chunk.kind
        {
            qqqa::tools::execute_command::StreamKind::Stdout => {
                let mut handle = std::io::stdout();
                let _ = handle.write_all(chunk.data);
                let _ = handle.flush();
            }
            qqqa::tools::execute_command::StreamKind::Stderr => {
                let mut handle = std::io::stderr();
                let _ = handle.write_all(chunk.data);
                let _ = handle.flush();
            }
        };
        let on_chunk: Option<
            &mut dyn for<'chunk> FnMut(qqqa::tools::execute_command::StreamChunk<'chunk>),
        > = Some(&mut stream_printer);
        let exec_args = sanitize_execute_args(original_args.clone(), &base_dir, debug);
        match qqqa::tools::execute_command::run(exec_args, auto_yes, debug, shell, on_chunk).await {
            Ok(output) => return Ok(output),
            Err(err) => {
                if let Some(program) = err
                    .downcast_ref::<perms::CommandNotAllowedError>()
                    .map(|e| e.program.clone())
                {
                    if !std::io::stdin().is_terminal() {
                        return Err(err);
                    }
                    if prompt_add_command_to_allowlist(&program)? {
                        let mut inserted = false;
                        *cfg = Config::update(cfg_path, debug, |c| {
                            inserted = c.add_command_to_allowlist(&program);
                        })?;
                        perms::set_custom_allowlist(cfg.command_allowlist());
                        if inserted {
                            if debug {
                                eprintln!("[debug] Added '{}' to qa command allowlist", program);
                            } else {
                                eprintln!("Added '{}' to qa command allowlist.", program);
                            }
                        }
                        continue;
                    } else {
                        return Err(err);
                    }
                } else {
                    return Err(err);
                }
            }
        }
    }
}

fn prompt_add_command_to_allowlist(program: &str) -> Result<bool> {
    use std::io::{self, Write};

    eprint!(
        "Command '{}' is not in the qa allowlist. Add it now and retry? [y/N]: ",
        program
    );
    io::stderr().flush().context("Failed to flush prompt")?;
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("Failed to read response")?;
    let choice = line.trim().to_ascii_lowercase();
    Ok(choice == "y" || choice == "yes")
}

fn sanitize_execute_args(
    args: qqqa::tools::execute_command::Args,
    base_dir: &Path,
    debug: bool,
) -> qqqa::tools::execute_command::Args {
    let (sanitized_path, fell_back) = sanitize_cwd_path(args.cwd.as_deref(), base_dir);
    let sanitized_str = sanitized_path.to_string_lossy().to_string();

    if fell_back && debug {
        if let Some(original) = args.cwd.as_deref() {
            eprintln!(
                "[debug] Ignoring requested working directory '{}' and using {} instead",
                original, sanitized_str
            );
        } else {
            eprintln!(
                "[debug] Using current working directory {} for execute_command",
                sanitized_str
            );
        }
    }

    qqqa::tools::execute_command::Args {
        command: args.command,
        cwd: Some(sanitized_str),
    }
}

fn sanitize_cwd_path(requested: Option<&str>, base_dir: &Path) -> (PathBuf, bool) {
    let base = base_dir.to_path_buf();
    let Some(raw) = requested else {
        return (base, false);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (base, false);
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        if let Ok(canon) = std::fs::canonicalize(path)
            && canon.starts_with(base_dir)
        {
            return (canon, false);
        }
        return (base, true);
    }

    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(seg) => relative.push(seg),
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return (base, true);
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return (base, false);
    }
    (base_dir.join(relative), false)
}

fn print_tool_result(tool: &str, result: &str) {
    qqqa::outln!("[tool:{}]", tool);
    qqqa::outln!("{}", result.trim_end());
}

/// Report a failed tool on stderr; returns qa's exit code for it.
fn print_tool_error(tool: &str, err: &str) -> i32 {
    eprintln!("[tool:{}:error] {}", tool, err);
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn sanitize_cwd_allows_relative_subdir() {
        let dir = tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("sub/child")).unwrap();

        let args = qqqa::tools::execute_command::Args {
            command: "pwd".into(),
            cwd: Some("sub/child".into()),
        };

        let sanitized = sanitize_execute_args(args, &base, false);
        let cwd_path = Path::new(sanitized.cwd.as_ref().unwrap());
        assert_eq!(cwd_path, base.join("sub/child"));
    }

    #[test]
    fn sanitize_cwd_rejects_parent_dirs() {
        let dir = tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();

        let (result, fell_back) = sanitize_cwd_path(Some("../outside"), &base);
        assert_eq!(result, base);
        assert!(fell_back);
    }

    #[test]
    fn sanitize_cwd_rejects_absolute_outside() {
        let dir = tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().canonicalize().unwrap();

        let (result, fell_back) =
            sanitize_cwd_path(Some(outside_path.to_string_lossy().as_ref()), &base);
        assert_eq!(result, base);
        assert!(fell_back);
    }

    #[test]
    fn sanitize_cwd_accepts_absolute_inside_base() {
        let dir = tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let nested = base.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let (result, fell_back) = sanitize_cwd_path(Some(nested.to_string_lossy().as_ref()), &base);
        assert_eq!(result, nested);
        assert!(!fell_back);
    }
}
