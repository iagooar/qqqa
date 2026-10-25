/// Shell flavors that qq/qa can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// POSIX `sh -lc`, default on Unix.
    Posix,
    /// `cmd.exe /C`, default on Windows when unspecified.
    CmdExe,
    /// Windows PowerShell (`powershell.exe`/`pwsh`).
    PowerShell,
}

impl ShellKind {
    pub fn display_name(self) -> &'static str {
        match self {
            ShellKind::Posix => "POSIX sh",
            ShellKind::CmdExe => "Windows cmd.exe",
            ShellKind::PowerShell => "Windows PowerShell",
        }
    }
}

/// Infer the shell flavor based on the current OS and environment.
pub fn detect_shell() -> ShellKind {
    if cfg!(windows) {
        detect_windows_shell()
    } else {
        ShellKind::Posix
    }
}

/// The operating system name for the prompt, without starting any process:
/// the build target, and on Linux the distribution's `NAME` from
/// `/etc/os-release` (so the model can tell apt from dnf).
pub fn os_name() -> String {
    match std::env::consts::OS {
        "macos" => "macOS".to_string(),
        "windows" => "Windows".to_string(),
        "linux" => std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|text| os_release_name(&text))
            .unwrap_or_else(|| "Linux".to_string()),
        other => other.to_string(),
    }
}

/// The `NAME` value of an os-release file, unquoted.
fn os_release_name(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("NAME="))
        .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|v| !v.is_empty())
}

fn detect_windows_shell() -> ShellKind {
    let shell_env = std::env::var_os("SHELL").map(|val| val.to_string_lossy().to_ascii_lowercase());
    let has_prompt = std::env::var_os("PROMPT").is_some();
    let has_pwsh_markers = std::env::var_os("POWERSHELL_DISTRIBUTION_CHANNEL").is_some()
        || std::env::var_os("WT_PROFILE_ID").is_some()
        || std::env::var_os("PSMODULEPATH").is_some();
    classify_windows_shell(shell_env, has_prompt, has_pwsh_markers)
}

fn classify_windows_shell(
    shell_env_lower: Option<String>,
    has_prompt: bool,
    has_pwsh_markers: bool,
) -> ShellKind {
    if let Some(lower) = shell_env_lower
        && (lower.contains("bash") || lower.contains("sh"))
    {
        return ShellKind::Posix;
    }

    if !has_prompt && has_pwsh_markers {
        ShellKind::PowerShell
    } else {
        ShellKind::CmdExe
    }
}

pub fn shell_hint_for_prompt(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Posix => "POSIX-compatible sh",
        ShellKind::CmdExe => "Windows cmd.exe",
        ShellKind::PowerShell => "Windows PowerShell",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn non_windows_defaults_to_posix() {
        assert_eq!(detect_shell(), ShellKind::Posix);
    }

    #[test]
    fn os_release_name_is_read_and_unquoted() {
        let text = "PRETTY_NAME=\"Ubuntu 24.04 LTS\"\nNAME=\"Ubuntu\"\nID=ubuntu\n";
        assert_eq!(os_release_name(text).as_deref(), Some("Ubuntu"));
        assert_eq!(
            os_release_name("NAME=Arch Linux\n").as_deref(),
            Some("Arch Linux")
        );
        assert_eq!(os_release_name("ID=x\n"), None);
    }

    #[test]
    fn os_name_is_never_empty() {
        assert!(!os_name().is_empty());
    }

    #[test]
    fn windows_posix_when_shell_env_mentions_bash() {
        let result = classify_windows_shell(Some("/usr/bin/bash".into()), false, false);
        assert_eq!(result, ShellKind::Posix);
    }

    #[test]
    fn windows_powershell_when_only_ps_markers_present() {
        let result = classify_windows_shell(None, false, true);
        assert_eq!(result, ShellKind::PowerShell);
    }

    #[test]
    fn windows_cmd_when_prompt_present_no_ps_markers() {
        let result = classify_windows_shell(None, true, false);
        assert_eq!(result, ShellKind::CmdExe);
    }
}
