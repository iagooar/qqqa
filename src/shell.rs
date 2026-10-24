use os_info::Type as OsType;

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
pub fn detect_shell(os_type: OsType) -> ShellKind {
    match os_type {
        OsType::Windows => detect_windows_shell(),
        _ => ShellKind::Posix,
    }
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

    #[test]
    fn non_windows_defaults_to_posix_even_when_prompt_true() {
        assert_eq!(detect_shell(OsType::Linux), ShellKind::Posix);
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
