use qqqa::history::read_recent_history;
use qqqa::prompt::{build_qa_system_prompt, build_qa_user_message, build_qq_prompt};

use serial_test::serial;

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn history_reader_prefers_existing_files_and_limits() {
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("HOME", dir.path());
    }

    // Create zsh history with a couple entries
    std::fs::write(
        dir.path().join(".zsh_history"),
        ": 1696971992:0;qq alpha\n\
: 1696971993:0;qa beta\n\
: 1696971994:0;ls -la\n\
: 1696971995:0;qq gamma\n\
: 1696971996:0;qa delta\n\
: 1696971997:0;git status\n\
: 1696971998:0;qq epsilon\n\
: 1696971999:0;qa zeta\n\
: 1696972000:0;qq eta\n\
: 1696972001:0;qa theta\n\
: 1696972002:0;qq iota\n\
: 1696972003:0;qa kappa\n\
: 1696972004:0;qq lambda\n",
    )
    .unwrap();

    let got = read_recent_history(10, true);
    assert_eq!(
        got,
        vec![
            "qa beta",
            "qq gamma",
            "qa delta",
            "qq epsilon",
            "qa zeta",
            "qq eta",
            "qa theta",
            "qq iota",
            "qa kappa",
            "qq lambda"
        ]
    );
}

#[test]
#[serial]
fn prompt_builders_include_sections() {
    let hist = vec!["cmd1".to_string(), "cmd2".to_string()];
    let stdin_block = Some("input text\nline2");
    let shell_hint = Some("POSIX sh");
    let qq = build_qq_prompt(
        Some(os_info::get().os_type()),
        shell_hint,
        &hist,
        stdin_block,
        "What is Rust?",
    );
    assert!(qq.contains("Terminal History"));
    assert!(qq.contains("Input from pipe:"));
    assert!(qq.contains("Question: What is Rust?"));

    let sys = build_qa_system_prompt();
    assert!(sys.contains("Available tools"));
    let user = build_qa_user_message(
        Some(os_info::get().os_type()),
        shell_hint,
        &hist,
        stdin_block,
        "Do the thing",
    );
    assert!(user.contains("Question: Do the thing"));
}

#[test]
fn coalesce_prompt_inputs_uses_piped_text_when_args_empty() {
    let prepared = qqqa::prompt::coalesce_prompt_inputs(
        String::new(),
        Some("Show me the full contents of this directory\n".to_string()),
    );
    let qqqa::prompt::PromptInputs {
        question,
        stdin_block,
    } = prepared;
    assert_eq!(question, "Show me the full contents of this directory");
    assert!(stdin_block.is_none());

    let shell_hint = Some("POSIX sh");
    let prompt = qqqa::prompt::build_qq_prompt(
        Some(os_info::get().os_type()),
        shell_hint,
        &[],
        stdin_block.as_deref(),
        &question,
    );
    assert!(prompt.contains("Question: Show me the full contents of this directory"));
    assert!(!prompt.contains("Input from pipe:"));
}
