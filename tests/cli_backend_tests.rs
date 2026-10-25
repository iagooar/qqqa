#![cfg(unix)]

use anyhow::Result;
use qqqa::ai::{CliCompletionRequest, run_cli_completion};
use qqqa::config::CliEngine;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::thread;
use std::time::Duration as StdDuration;
use tempfile::tempdir;
use tokio::time::{Duration, sleep};

fn read_args(path: &Path) -> Vec<String> {
    let data = fs::read(path).expect("args file");
    data.split(|b| *b == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect()
}

fn write_executable_script(path: &Path, contents: &str) {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .expect("create script");
    file.write_all(contents.as_bytes()).expect("write script");
    file.sync_all().ok();
    drop(file);

    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();

    // Give the filesystem a brief moment to release the executable so execve won't race with writes.
    thread::sleep(StdDuration::from_millis(50));
}

async fn run_cli_completion_with_retry<'a, F>(mut make_req: F) -> Result<String>
where
    F: FnMut() -> CliCompletionRequest<'a>,
{
    const MAX_ATTEMPTS: usize = 3;
    let mut attempt = 0;
    loop {
        match run_cli_completion(make_req()).await {
            Ok(text) => return Ok(text),
            Err(err) => {
                if should_retry_etxtbsy(&err) && attempt < MAX_ATTEMPTS {
                    attempt += 1;
                    sleep(Duration::from_millis(25 * attempt as u64)).await;
                    continue;
                }
                return Err(err);
            }
        }
    }
}

#[cfg(unix)]
fn should_retry_etxtbsy(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io_err| io_err.raw_os_error() == Some(libc::ETXTBSY))
}

#[cfg(not(unix))]
fn should_retry_etxtbsy(_: &anyhow::Error) -> bool {
    false
}

#[tokio::test]
async fn run_cli_completion_returns_agent_message_from_script() {
    let dir = tempdir().unwrap();
    let script_path = dir.path().join("fake_codex");
    let script = r#"#!/bin/sh
printf '%s\n' '{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}'
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"hello"}}'
"#;
    write_executable_script(&script_path, script);

    let text = run_cli_completion_with_retry(|| CliCompletionRequest {
        engine: CliEngine::Codex,
        binary: script_path.to_str().unwrap(),
        base_args: &[],
        system_prompt: "SYSTEM",
        user_prompt: "USER",
        model: "gpt-5",
        model_explicit: false,
        reasoning_effort: Some("minimal"),
        debug: true,
        timeout: Duration::from_secs(5),
    })
    .await
    .expect("cli run succeeds");

    assert_eq!(text, "hello");
}

#[tokio::test]
async fn run_cli_completion_writes_tagged_prompts_to_stdin() {
    let dir = tempdir().unwrap();
    let script_path = dir.path().join("fake_codex");
    let args_dump = dir.path().join("args.txt");
    let prompt_dump = dir.path().join("prompt.txt");
    let script = format!(
        r#"#!/bin/sh
set -eu
DIR="$(dirname "$0")"
printf '%s\0' "$@" > "{args}"
cat > "{prompt}"
printf '%s\n' '{{"type":"item.completed","item":{{"type":"agent_message","text":"ok"}}}}'
"#,
        args = args_dump.display(),
        prompt = prompt_dump.display()
    );
    write_executable_script(&script_path, &script);

    let base_args = vec!["exec".to_string()];
    let text = run_cli_completion_with_retry(|| CliCompletionRequest {
        engine: CliEngine::Codex,
        binary: script_path.to_str().unwrap(),
        base_args: &base_args,
        system_prompt: "SYSTEM",
        user_prompt: "USER",
        model: "gpt-5",
        model_explicit: false,
        reasoning_effort: Some("low"),
        debug: false,
        timeout: Duration::from_secs(5),
    })
    .await
    .expect("cli run succeeds");

    assert_eq!(text.trim(), "ok");

    let args = read_args(&args_dump);
    let expected: Vec<String> = vec![
        "exec",
        "--skip-git-repo-check",
        "--json",
        "-c",
        "model_reasoning_effort=low",
        "-c",
        "sandbox_mode=read-only",
        "-",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(args, expected);

    let prompt_contents = fs::read_to_string(&prompt_dump).expect("prompt file");
    let expected_prompt =
        "<system-prompt>\nSYSTEM\n</system-prompt>\n\n<user-prompt>\nUSER\n</user-prompt>\n";
    assert_eq!(prompt_contents, expected_prompt);
}

#[tokio::test]
async fn run_cli_completion_invokes_gemini_with_expected_args() {
    let dir = tempdir().unwrap();
    let script_path = dir.path().join("fake_gemini");
    let args_dump = dir.path().join("gemini_args.txt");
    let prompt_dump = dir.path().join("gemini_prompt.txt");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\0' "$@" > "{args}"
cat > "{prompt}"
printf '%s\n' '{{"session_id":"test","response":"gemini ok"}}'
"#,
        args = args_dump.display(),
        prompt = prompt_dump.display()
    );
    write_executable_script(&script_path, &script);

    let text = run_cli_completion_with_retry(|| CliCompletionRequest {
        engine: CliEngine::Gemini,
        binary: script_path.to_str().unwrap(),
        base_args: &[],
        system_prompt: "SYSTEM",
        user_prompt: "USER",
        model: "gemini-3-flash-preview",
        model_explicit: false,
        reasoning_effort: None,
        debug: false,
        timeout: Duration::from_secs(5),
    })
    .await
    .expect("cli run succeeds");

    assert_eq!(text.trim(), "gemini ok");

    let args = read_args(&args_dump);
    let expected: Vec<String> = vec![
        "-o",
        "json",
        "--skip-trust",
        "--approval-mode",
        "plan",
        "-m",
        "gemini-3-flash-preview",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(args, expected);

    let prompt_contents = fs::read_to_string(&prompt_dump).expect("prompt file");
    let expected_prompt =
        "<system-prompt>\nSYSTEM\n</system-prompt>\n\n<user-prompt>\nUSER\n</user-prompt>\n";
    assert_eq!(prompt_contents, expected_prompt);
}

#[tokio::test]
async fn run_cli_completion_invokes_claude_with_expected_args() {
    let dir = tempdir().unwrap();
    let script_path = dir.path().join("fake_claude");
    let args_dump = dir.path().join("claude_args.txt");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\0' "$@" > "{args}"
printf '%s' '{{"type":"result","subtype":"success","result":"<cmd>echo hi</cmd>"}}'
"#,
        args = args_dump.display()
    );
    write_executable_script(&script_path, &script);

    let text = run_cli_completion(CliCompletionRequest {
        engine: CliEngine::Claude,
        binary: script_path.to_str().unwrap(),
        base_args: &[],
        system_prompt: "SYSTEM",
        user_prompt: "USER",
        model: "claude-haiku-4-5",
        model_explicit: false,
        reasoning_effort: None,
        debug: false,
        timeout: Duration::from_secs(5),
    })
    .await
    .expect("cli run succeeds");

    assert_eq!(text, "<cmd>echo hi</cmd>");

    let args = read_args(&args_dump);
    let expected: Vec<String> = vec![
        "-p",
        "--output-format",
        "json",
        "--model",
        "claude-haiku-4-5",
        "--append-system-prompt",
        "<system-prompt>\nSYSTEM\n</system-prompt>",
        "--disallowed-tools",
        "Bash(*) Edit",
        "--",
        "<user-prompt>\nUSER\n</user-prompt>",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(args, expected);
}

async fn codex_args(model_explicit: bool) -> Vec<String> {
    let dir = tempdir().unwrap();
    let script_path = dir.path().join("fake_codex_args");
    let args_dump = dir.path().join("codex_args.txt");
    let script = format!(
        r#"#!/bin/sh
printf '%s\0' "$@" > "{args}"
cat > /dev/null
printf '%s\n' '{{"type":"item.completed","item":{{"type":"agent_message","text":"ok"}}}}'
"#,
        args = args_dump.display()
    );
    write_executable_script(&script_path, &script);
    run_cli_completion_with_retry(|| CliCompletionRequest {
        engine: CliEngine::Codex,
        binary: script_path.to_str().unwrap(),
        base_args: &[],
        system_prompt: "SYSTEM",
        user_prompt: "USER",
        model: "o3",
        model_explicit,
        reasoning_effort: None,
        debug: false,
        timeout: Duration::from_secs(5),
    })
    .await
    .expect("cli run succeeds");
    read_args(&args_dump)
}

#[tokio::test]
async fn codex_gets_the_model_only_when_the_user_chose_it() {
    let chosen = codex_args(true).await;
    let pos = chosen.iter().position(|a| a == "-m").expect("-m passed");
    assert_eq!(chosen[pos + 1], "o3");
    let default = codex_args(false).await;
    assert!(!default.iter().any(|a| a == "-m"), "{default:?}");
}

fn codex_request<'a>(
    binary: &'a str,
    prompt: &'a str,
    timeout_secs: u64,
) -> CliCompletionRequest<'a> {
    CliCompletionRequest {
        engine: CliEngine::Codex,
        binary,
        base_args: &[],
        system_prompt: "SYSTEM",
        user_prompt: prompt,
        model: "gpt-5",
        model_explicit: false,
        reasoning_effort: None,
        debug: false,
        timeout: Duration::from_secs(timeout_secs),
    }
}

const AGENT_OK: &str =
    r#"printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"ok"}}'"#;

#[tokio::test]
async fn cli_that_writes_before_reading_a_large_prompt_does_not_deadlock() {
    let dir = tempdir().unwrap();
    let script = dir.path().join("fake_codex_chatty");
    write_executable_script(
        &script,
        &format!("#!/bin/sh\nhead -c 262144 /dev/zero >&2\ncat > /dev/null\n{AGENT_OK}\n"),
    );
    let prompt = "x".repeat(300_000);
    let started = std::time::Instant::now();
    let text =
        run_cli_completion_with_retry(|| codex_request(script.to_str().unwrap(), &prompt, 10))
            .await
            .expect("no deadlock");
    assert_eq!(text, "ok");
    assert!(started.elapsed() < StdDuration::from_secs(8));
}

#[tokio::test]
async fn cli_that_exits_without_reading_shows_its_own_error() {
    let dir = tempdir().unwrap();
    let script = dir.path().join("fake_codex_logged_out");
    write_executable_script(&script, "#!/bin/sh\necho 'Not logged in' >&2\nexit 1\n");
    let prompt = "x".repeat(1_000_000);
    let err =
        run_cli_completion_with_retry(|| codex_request(script.to_str().unwrap(), &prompt, 10))
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("Not logged in"), "{err}");
    assert!(!err.contains("Broken pipe"), "{err}");
}

#[tokio::test]
async fn cli_helper_holding_the_pipes_does_not_extend_the_wait() {
    let dir = tempdir().unwrap();
    let script = dir.path().join("fake_codex_helper");
    write_executable_script(
        &script,
        &format!("#!/bin/sh\ncat > /dev/null\n{AGENT_OK}\nsleep 30 &\nexit 0\n"),
    );
    let started = std::time::Instant::now();
    let text = run_cli_completion_with_retry(|| codex_request(script.to_str().unwrap(), "hi", 20))
        .await
        .expect("returns after the grace period");
    assert_eq!(text, "ok");
    assert!(
        started.elapsed() < StdDuration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn cli_timeout_kills_the_whole_process_group() {
    let dir = tempdir().unwrap();
    let script = dir.path().join("fake_codex_slow");
    let pid_file = dir.path().join("grandchild.pid");
    write_executable_script(
        &script,
        &format!(
            "#!/bin/sh\nsleep 47 &\necho $! > \"{}\"\nsleep 30\n",
            pid_file.display()
        ),
    );
    let started = std::time::Instant::now();
    // 3 s leaves the script time to start even when tests run in parallel.
    let err = run_cli_completion_with_retry(|| codex_request(script.to_str().unwrap(), "hi", 3))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("timed out"), "{err}");
    assert!(started.elapsed() < StdDuration::from_secs(10));
    let pid: i32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    sleep(Duration::from_millis(200)).await;
    // SAFETY: signal 0 only checks whether the process exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "grandchild {pid} survived the timeout");
}
