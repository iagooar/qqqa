use anyhow::{Result, anyhow};
use fs_err as fs;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Cap read_file size to 1 MiB
pub const READ_FILE_MAX_BYTES: usize = 1024 * 1024;

const SAFE_COMMANDS: &[&str] = &[
    "awk", "cat", "cut", "df", "du", "env", "echo", "find", "grep", "head", "ls", "pwd", "rg",
    "sed", "sort", "stat", "tail", "tree", "uniq", "wc",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandDisposition {
    Allowed,
    NeedsConfirmation { reason: &'static str },
}

#[derive(Debug)]
pub struct CommandNotAllowedError {
    pub program: String,
}

impl CommandNotAllowedError {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl std::fmt::Display for CommandNotAllowedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Command '{}' is not in the qa allowlist", self.program)
    }
}

impl std::error::Error for CommandNotAllowedError {}

fn custom_allowlist() -> &'static Mutex<HashSet<String>> {
    static CUSTOM: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CUSTOM.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn set_custom_allowlist(commands: Vec<String>) {
    let mut set = custom_allowlist().lock().unwrap();
    set.clear();
    for cmd in commands {
        set.insert(cmd);
    }
}

/// Ensure a path is within HOME or current working directory.
pub fn ensure_safe_path(p: &Path) -> Result<()> {
    let resolved = resolve_path(p)?;
    let allowed_roots = gather_allowed_roots()?;

    if !is_within_allowed(&resolved, &allowed_roots) {
        return Err(anyhow!(
            "Refusing to access path outside home or cwd: {}",
            resolved.display()
        ));
    }

    if let Some(existing) = deepest_existing_ancestor(&resolved) {
        let canonical = fs::canonicalize(&existing)?;
        if !is_within_allowed(&canonical, &allowed_roots) {
            return Err(anyhow!(
                "Refusing to access path outside home or cwd: {}",
                resolved.display()
            ));
        }
    }

    Ok(())
}

/// Very conservative safety checks for shell commands.
pub fn ensure_safe_command(cmd: &str) -> Result<CommandDisposition> {
    if allow_unsafe_commands() {
        return Ok(CommandDisposition::Allowed);
    }

    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("Blocked empty command"));
    }

    let needs_confirmation = contains_control_operators(trimmed);

    let segments = split_command_segments(trimmed);
    let mut saw_segment = false;
    for segment in segments {
        let seg_trimmed = segment.trim();
        if seg_trimmed.is_empty() {
            continue;
        }
        saw_segment = true;
        let seg_tokens = shell_words::split(seg_trimmed)
            .map_err(|_| anyhow!("Unable to safely parse command; refusing to execute"))?;
        if seg_tokens.is_empty() {
            continue;
        }
        let program = seg_tokens[0].as_str();
        let builtin_allowed = SAFE_COMMANDS.contains(&program);
        let custom_allowed = is_custom_allowed(program);

        if !builtin_allowed && !custom_allowed {
            return Err(CommandNotAllowedError::new(program).into());
        }

        if builtin_allowed && !custom_allowed {
            enforce_program_specific_rules(program, &seg_tokens)?;
        }
    }

    if !saw_segment {
        return Err(anyhow!("Blocked empty command"));
    }

    let lower = cmd.to_lowercase();
    // Block obviously destructive global wipes
    if lower.contains("rm -rf /") || lower.contains("rm -rf -- /") {
        return Err(anyhow!("Blocked dangerous command pattern: rm -rf /"));
    }
    // Discourage sudo without user interaction context
    if lower.contains("sudo ") {
        return Err(anyhow!(
            "Blocked command using sudo (not allowed by default)"
        ));
    }
    // Block direct disk wiping
    if lower.contains("mkfs") || lower.contains("dd if=") {
        return Err(anyhow!("Blocked potentially destructive disk operation"));
    }
    if needs_confirmation {
        Ok(CommandDisposition::NeedsConfirmation {
            reason: "Command uses shell control operators (pipelines, redirection, chaining).",
        })
    } else {
        Ok(CommandDisposition::Allowed)
    }
}

fn allow_unsafe_commands() -> bool {
    std::env::var("QQQA_ALLOW_UNSAFE_COMMANDS")
        .map(|v| !v.trim().is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

fn contains_control_operators(cmd: &str) -> bool {
    let mut chars = cmd.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape_next = false;

    while let Some(c) = chars.next() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match c {
            '\\' => {
                if !in_single {
                    escape_next = true;
                }
            }
            '\'' => {
                if !in_double {
                    in_single = !in_single;
                }
            }
            '"' => {
                if !in_single {
                    in_double = !in_double;
                }
            }
            '\n' => {
                if !in_single && !in_double {
                    return true;
                }
            }
            '`' => {
                if !in_single && !in_double {
                    return true;
                }
            }
            '$' if !in_single => {
                if let Some('(') = chars.peek().copied() {
                    return true;
                }
            }
            '&' | '|' | ';' | '>' | '<' if !in_single && !in_double => match c {
                '&' => {
                    return true;
                }
                '|' => {
                    return true;
                }
                ';' => return true,
                '>' => return true,
                '<' => return true,
                _ => {}
            },
            _ => {}
        }
    }

    false
}

fn split_command_segments(cmd: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut chars = cmd.char_indices().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape_next = false;

    while let Some((idx, ch)) = chars.next() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match ch {
            '\\' => {
                if !in_single {
                    escape_next = true;
                }
            }
            '\'' => {
                if !in_double {
                    in_single = !in_single;
                }
            }
            '"' => {
                if !in_single {
                    in_double = !in_double;
                }
            }
            '\n' if !in_single && !in_double => {
                if idx >= start {
                    segments.push(cmd[start..idx].to_string());
                }
                start = idx + ch.len_utf8();
            }
            '|' | '&' | ';' if !in_single && !in_double => {
                let op_char = ch;
                let mut op_end = idx + ch.len_utf8();
                if let Some(&(_, next_ch)) = chars.peek()
                    && next_ch == op_char
                {
                    chars.next();
                    op_end += next_ch.len_utf8();
                }
                if idx >= start {
                    segments.push(cmd[start..idx].to_string());
                }
                start = op_end;
            }
            _ => {}
        }
    }

    if start <= cmd.len() {
        segments.push(cmd[start..].to_string());
    }

    segments
}

fn is_custom_allowed(program: &str) -> bool {
    custom_allowlist().lock().unwrap().contains(program)
}

fn enforce_program_specific_rules(program: &str, tokens: &[String]) -> Result<()> {
    match program {
        "find" => {
            if tokens.iter().any(|t| {
                matches!(
                    t.as_str(),
                    "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
                )
            }) {
                return Err(anyhow!("Blocked find command with delete/exec actions"));
            }
        }
        "sed" => {
            if tokens.iter().any(|t| t == "-i" || t.starts_with("-i")) {
                return Err(anyhow!("Blocked sed -i edits from qa for safety"));
            }
        }
        "awk" | "cat" | "cut" | "df" | "du" | "env" | "grep" | "head" | "ls" | "pwd" | "rg"
        | "sort" | "stat" | "tail" | "tree" | "uniq" | "wc" => {}
        _ => {}
    }
    Ok(())
}

pub(crate) fn resolve_path(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(normalize_path(p))
    } else {
        let cwd = std::env::current_dir()?;
        Ok(normalize_path(&cwd.join(p)))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(seg) => normalized.push(seg),
        }
    }
    normalized
}

fn gather_allowed_roots() -> Result<Vec<PathBuf>> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    if let Some(home) = dirs::home_dir() {
        push_unique(&mut roots, &mut seen, home.clone());
        if let Ok(canon) = fs::canonicalize(&home) {
            push_unique(&mut roots, &mut seen, canon);
        }
    }

    let cwd = std::env::current_dir()?;
    push_unique(&mut roots, &mut seen, cwd.clone());
    if let Ok(canon) = fs::canonicalize(&cwd) {
        push_unique(&mut roots, &mut seen, canon);
    }

    Ok(roots)
}

fn push_unique(roots: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        roots.push(path);
    }
}

fn is_within_allowed(path: &Path, allowed: &[PathBuf]) -> bool {
    allowed.iter().any(|root| path.starts_with(root))
}

fn deepest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    while !current.as_os_str().is_empty() {
        if current.exists() {
            return Some(current.to_path_buf());
        }
        current = match current.parent() {
            Some(parent) => parent,
            None => Path::new(""),
        };
    }
    None
}
