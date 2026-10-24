use qqqa::perms::{
    CommandDisposition, ensure_safe_command, ensure_safe_path, set_custom_allowlist,
};
use serial_test::serial;
use std::path::Path;

#[test]
#[serial]
fn ensure_safe_command_allows_allowlisted_commands() {
    set_custom_allowlist(Vec::new());
    unsafe {
        std::env::remove_var("QQQA_ALLOW_UNSAFE_COMMANDS");
    }
    assert!(matches!(
        ensure_safe_command("ls -la").unwrap(),
        CommandDisposition::Allowed
    ));
    assert!(matches!(
        ensure_safe_command("grep foo Cargo.toml").unwrap(),
        CommandDisposition::Allowed
    ));
    assert!(matches!(
        ensure_safe_command("rg 'foo|bar' src").unwrap(),
        CommandDisposition::Allowed
    ));
    assert!(matches!(
        ensure_safe_command("echo \"<div>\"").unwrap(),
        CommandDisposition::Allowed
    ));
    assert!(matches!(
        ensure_safe_command(r"grep foo\|bar Cargo.toml").unwrap(),
        CommandDisposition::Allowed
    ));
}

#[test]
#[serial]
fn ensure_safe_command_blocks_dangerous_patterns() {
    set_custom_allowlist(Vec::new());
    unsafe {
        std::env::remove_var("QQQA_ALLOW_UNSAFE_COMMANDS");
    }
    assert!(ensure_safe_command("rm -rf /").is_err());
    assert!(ensure_safe_command("sudo apt-get update").is_err());
    assert!(ensure_safe_command("dd if=/dev/zero of=/dev/sda").is_err());
    assert!(ensure_safe_command("npm install").is_err());
}

#[test]
#[serial]
fn ensure_safe_command_blocks_connectors_and_inplace_edits() {
    set_custom_allowlist(Vec::new());
    unsafe {
        std::env::remove_var("QQQA_ALLOW_UNSAFE_COMMANDS");
    }
    assert!(matches!(
        ensure_safe_command("ls | grep foo").unwrap(),
        CommandDisposition::NeedsConfirmation { .. }
    ));
    assert!(ensure_safe_command("sed -i '' 's/a/b/' file.txt").is_err());
    assert!(ensure_safe_command("find . -delete").is_err());
    assert!(matches!(
        ensure_safe_command("rg foo | wc -l").unwrap(),
        CommandDisposition::NeedsConfirmation { .. }
    ));
    assert!(matches!(
        ensure_safe_command("echo foo > out.txt").unwrap(),
        CommandDisposition::NeedsConfirmation { .. }
    ));
}

#[test]
#[serial]
fn ensure_safe_command_checks_all_segments() {
    set_custom_allowlist(Vec::new());
    unsafe {
        std::env::remove_var("QQQA_ALLOW_UNSAFE_COMMANDS");
    }
    assert!(ensure_safe_command("ls\ncurl https://example.com").is_err());

    // Allowed segments but multi-line still require confirmation
    assert!(matches!(
        ensure_safe_command("ls\necho ok").unwrap(),
        CommandDisposition::NeedsConfirmation { .. }
    ));
}

#[test]
#[serial]
fn ensure_safe_command_respects_override_env() {
    set_custom_allowlist(Vec::new());
    unsafe {
        std::env::set_var("QQQA_ALLOW_UNSAFE_COMMANDS", "1");
    }
    assert!(matches!(
        ensure_safe_command("rm -rf /tmp/test").unwrap(),
        CommandDisposition::Allowed
    ));
    unsafe {
        std::env::remove_var("QQQA_ALLOW_UNSAFE_COMMANDS");
    }
}

#[test]
#[serial]
fn ensure_safe_command_respects_custom_allowlist() {
    set_custom_allowlist(Vec::new());
    unsafe {
        std::env::remove_var("QQQA_ALLOW_UNSAFE_COMMANDS");
    }
    set_custom_allowlist(vec!["ffmpeg".to_string()]);
    assert!(matches!(
        ensure_safe_command("ffmpeg -i in.mp4 out.mp3").unwrap(),
        CommandDisposition::Allowed
    ));
    set_custom_allowlist(Vec::new());
}

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn ensure_safe_path_allows_workspace_children() {
    let temp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("HOME", temp.path());
    }
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::env::set_current_dir(&workspace).unwrap();

    assert!(ensure_safe_path(Path::new("dir/file.txt")).is_ok());
    assert!(ensure_safe_path(Path::new("nested/dir/another.txt")).is_ok());
}

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn ensure_safe_path_blocks_traversal_outside_home() {
    let temp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("HOME", temp.path());
    }
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::env::set_current_dir(&workspace).unwrap();

    let err = ensure_safe_path(Path::new("../../outside.txt"));
    assert!(err.is_err(), "expected traversal to be blocked");
}

#[cfg(unix)]
#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn ensure_safe_path_blocks_symlink_escape() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("HOME", temp.path());
    }
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::env::set_current_dir(&workspace).unwrap();

    let link = workspace.join("etc_link");
    symlink("/etc", &link).unwrap();

    let err = ensure_safe_path(Path::new("etc_link/qqqa-test.conf"));
    assert!(err.is_err(), "expected symlink escape to be blocked");
}
