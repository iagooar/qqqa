use chrono::Utc;

/// Normalized inputs for building prompts.
pub struct PromptInputs {
    pub question: String,
    pub stdin_block: Option<String>,
}

/// Merge CLI question/task text with optional piped stdin.
///
/// - If CLI arguments are present (non-empty after trimming), they win and any
///   piped text is preserved as extra context.
/// - Otherwise, a non-empty piped block becomes the question/task and is NOT
///   echoed as separate context to avoid duplication.
pub fn coalesce_prompt_inputs(args_question: String, stdin_block: Option<String>) -> PromptInputs {
    if args_question.trim().is_empty()
        && let Some(block) = stdin_block
    {
        let trimmed = block.trim();
        if !trimmed.is_empty() {
            return PromptInputs {
                question: trimmed.to_string(),
                stdin_block: None,
            };
        }
        return PromptInputs {
            question: args_question,
            stdin_block: Some(block),
        };
    }

    PromptInputs {
        question: args_question,
        stdin_block,
    }
}

/// Build the final prompt for `qq` per spec.
/// Sections:
/// - Header with date/time + OS
/// - Optional terminal history (last commands)
/// - Optional input from pipe
/// - Trailing line: `Question: <text>`
pub fn build_qq_prompt(
    os: Option<&str>,
    shell_hint: Option<&str>,
    history: &[String],
    stdin_block: Option<&str>,
    question: &str,
) -> String {
    let now = Utc::now();
    let os_name = os.map(str::to_string).unwrap_or_else(crate::shell::os_name);

    let mut out = String::new();
    out.push_str(&format!(
        "Timestamp (UTC): {}\nOS: {}\n",
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        os_name
    ));
    if let Some(shell) = shell_hint {
        out.push_str(&format!("Default Shell: {}\n", shell));
    }
    out.push('\n');

    if !history.is_empty() {
        out.push_str("Terminal History (last commands):\n");
        for line in history {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }

    if let Some(block) = stdin_block
        && !block.trim().is_empty()
    {
        out.push_str("Input from pipe:\n");
        out.push_str(block);
        if !block.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("Question: ");
    out.push_str(question);
    out
}

/// System prompt for `qq`: restrict topics to terminal/dev and enforce XML-tag formatting.
pub fn build_qq_system_prompt() -> String {
    let mut s = String::new();
    s.push_str("Balance quick answer with just enough thinking.\n");
    s.push_str("You are a terminal assistant. Help users ONLY with command-line, programming, system administration, and technical computing tasks.\n\n");
    s.push_str("TOPIC RESTRICTIONS:\n");
    s.push_str("- ONLY answer questions about: terminal commands, shell scripting, file operations, system administration, programming, development tools, git, network tools, text processing, etc.\n");
    s.push_str("- REFUSE to answer: philosophical questions, general knowledge, personal advice, non-technical topics, creative writing, etc.\n");
    s.push_str("- For off-topic questions, kindly refuse and then provide fun computer/Unix/Linux trivia with wit and humor that's somehow related to their question if possible. ALWAYS use this EXACT format with XML tags:\n");
    s.push_str("  <warn>I can only help with terminal and technical tasks, but here's some fun tech trivia...</warn><br/>\n");
    s.push_str("  <info>Your witty, reality-based computer/Unix/Linux fact goes here</info>\n\n");
    s.push_str(
        "CRITICAL: ALWAYS wrap trivia text in <info></info> tags for proper cyan formatting!\n\n",
    );
    s.push_str("ANSWER THE USER'S QUESTION DIRECTLY. For \"convert mp4 to mp3\", provide ffmpeg commands. For \"list files\", provide ls commands. Give the actual commands they need to run.\n\n");
    s.push_str("- Start with 1-3 commands (1-3 commands should generally be enough to solve the user's problem)\n");
    s.push_str("FORMAT using XML tags:\n");
    s.push_str("<cmd>command to run</cmd> = terminal commands (green)\n");
    s.push_str("<bold>heading</bold> = headings and emphasis\n");
    s.push_str("<info>tip</info> = tips and notes (cyan)\n");
    s.push_str("<file>path</file> = file/directory paths (magenta)\n");
    s.push_str("<warn>warning</warn> = warnings (yellow)\n");
    s.push_str("<br/> = line break (use for spacing between sections)\n\n");
    s.push_str("CRITICAL SPACING RULES:\n");
    s.push_str("- Use <br/> tags for line breaks between different sections\n");
    s.push_str("- Add <br/> after each command block\n");
    s.push_str("- Add <br/> before and after explanatory text\n");
    s.push_str("- Example format:\n");
    s.push_str("  <bold>Heading</bold><br/>\n");
    s.push_str("  <cmd>command1</cmd><br/>\n");
    s.push_str("  <cmd>command2</cmd><br/>\n");
    s.push_str("  <br/>\n");
    s.push_str("  <info>Explanation or tip here</info>\n\n");
    s.push_str("RULES:\n");
    s.push_str("- Provide REAL WORKING COMMANDS the user can copy and run\n");
    s.push_str("- Example: User asks \"convert mp4 to mp3\" → Provide: <cmd>ffmpeg -i input.mp4 output.mp3</cmd>\n");
    s.push_str("- NOT: <cmd>qq something</cmd> (don't tell them to run qq again!)\n");
    s.push_str("- Escape XML chars: & -> &amp; < -> &lt; > -> &gt;\n");
    // Line repeated on purpose - it works for some smaller models to reiterate this rule - DO NOT REMOVE
    s.push_str("- Start with 1-3 commands (1-3 commands should generally be enough to solve the user's problem)\n");
    s.push_str("- Be concise and practical\n");
    s.push_str("- Prefer tools that are typically pre-installed on macOS/Linux (POSIX utilities like ls, find, grep, awk, sed, xargs, tar, curl, ssh). Optimize for portability.\n");
    s.push_str("- You MAY suggest non-default tools (e.g., ripgrep, fd, bat, tree) only if they are notably better; when you do, include an install hint for the detected OS (e.g., <cmd>brew install ripgrep</cmd> on macOS, <cmd>sudo apt-get install ripgrep</cmd> on Debian/Ubuntu) and ALSO provide a built-in alternative.\n");
    s.push_str("- Tailor flags/commands to the provided Operating System AND Default Shell in the user message (POSIX sh vs Windows cmd.exe vs Windows PowerShell behave differently).\n");
    s
}

/// User prompt for `qq` per the documented template (timestamp/OS/history/stdin/question).
pub fn build_qq_user_message(
    os: Option<&str>,
    shell_hint: Option<&str>,
    history: &[String],
    stdin_block: Option<&str>,
    question: &str,
) -> String {
    let now = Utc::now();
    let os_name = os.map(str::to_string).unwrap_or_else(crate::shell::os_name);

    let mut out = String::new();
    out.push_str(&format!(
        "Current date/time: {}\n",
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));
    out.push_str(&format!("Operating System: {}\n", os_name));
    if let Some(shell) = shell_hint {
        out.push_str(&format!("Default Shell: {}\n", shell));
    }
    out.push('\n');

    if !history.is_empty() {
        out.push_str("[Recent terminal commands for context (sanitized for privacy):\n");
        for (i, line) in history.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, line));
        }
        out.push_str("]\n\n");
    }

    if let Some(block) = stdin_block
        && !block.trim().is_empty()
    {
        out.push_str("[Input from pipe:\n");
        out.push_str(block);
        if !block.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("]\n\n");
    }

    out.push_str("Question: ");
    out.push_str(question);
    out
}

/// Build system and user messages for `qa`.
/// The system prompt instructs the assistant to either:
/// - Return a normal helpful answer as plain text, or
/// - Return ONLY a JSON object of the form:
///   { "tool": "read_file"|"write_file"|"execute_command", "arguments": { ... } }
///
/// The CLI enforces a single tool call and will not loop.
pub fn build_qa_system_prompt() -> String {
    let mut s = String::new();
    s.push_str("Balance quick answer with just enough thinking.\n");
    s.push_str("You are a careful CLI agent with a single tool-call step.\n");
    s.push_str("When the request needs a file or a command, issue exactly one tool call, returned as a JSON object with this shape:\n");
    s.push_str("{ \"tool\": string, \"arguments\": object }\n\n");
    s.push_str("Available tools and JSON argument schemas:\n");
    s.push_str("- read_file: { \"path\": string }\n");
    s.push_str("- write_file: { \"path\": string, \"content\": string }\n");
    s.push_str("- execute_command: { \"command\": string, \"cwd?\": string }\n\n");
    s.push_str("Rules:\n");
    s.push_str("- Single step: at most one tool call.\n");
    s.push_str("- When the user asks you to run or inspect something, you must call the appropriate tool (usually execute_command) to gather real output; if running a command would be unsafe or impossible, reply in plain text and explain why.\n");
    s.push_str("- If using a tool, return ONLY the JSON object (no prose) and fill the function parameters exactly as declared (no extra wrapper objects).\n");
    s.push_str("- Prefer safe, non-destructive commands.\n");
    s
}

/// Build the user message for `qa`: includes timestamp, OS, optional history and stdin context, plus the task.
pub fn build_qa_user_message(
    os: Option<&str>,
    shell_hint: Option<&str>,
    history: &[String],
    stdin_block: Option<&str>,
    task: &str,
) -> String {
    build_qq_prompt(os, shell_hint, history, stdin_block, task)
}
