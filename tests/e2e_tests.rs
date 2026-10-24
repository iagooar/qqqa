//! End-to-end runs of the real `qq` and `qa` binaries against a mock
//! OpenAI-compatible server, with a temporary HOME.
//! Unix only: on Windows `dirs::home_dir` ignores HOME, so the binaries would
//! read and write the real user profile.
#![cfg(unix)]
use assert_cmd::cargo::cargo_bin_cmd;
use httpmock::Method::POST;
use httpmock::MockServer;
use std::io::Read;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::tempdir;

fn sandbox_blocks_binding() -> bool {
    TcpListener::bind("127.0.0.1:0").is_err()
}

fn sse(tokens: &[&str]) -> String {
    let mut body: String = tokens
        .iter()
        .map(|t| {
            format!(
                "data: {}\n\n",
                serde_json::json!({"choices":[{"delta":{"content":t}}]})
            )
        })
        .collect();
    body.push_str(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    );
    body
}

fn mock_reply(server: &MockServer, content_type: &str, body: &str) {
    server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", content_type)
            .body(body);
    });
}

fn qq(home: &Path, server: &MockServer) -> assert_cmd::Command {
    let mut cmd = cargo_bin_cmd!("qq");
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", "")
        .env("OPENROUTER_API_KEY", "test")
        .env_remove("CLICOLOR_FORCE")
        .current_dir(home)
        .args(["-n", "--api-base", &server.base_url()]);
    cmd
}

#[test]
fn qq_streams_formatted_answer_and_keeps_angle_text() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    mock_reply(
        &server,
        "text/event-stream",
        &sse(&[
            "Run <cmd>kill -9 <PI",
            "D></cmd> and <cmd>sort < a > b</cmd>",
        ]),
    );
    let home = tempdir().unwrap();
    let out = qq(home.path(), &server)
        .arg("how")
        .write_stdin("")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Run kill -9 <PID> and sort < a > b"),
        "{stdout:?}"
    );
}

#[test]
fn qq_no_stream_prints_answer() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    mock_reply(
        &server,
        "application/json",
        r#"{"choices":[{"message":{"content":"<cmd>ls</cmd>"},"finish_reason":"stop"}]}"#,
    );
    let home = tempdir().unwrap();
    let out = qq(home.path(), &server)
        .args(["--no-stream", "-r", "list"])
        .write_stdin("")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("<cmd>ls</cmd>"));
}

#[test]
fn qq_fails_on_stream_error_event() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    mock_reply(
        &server,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\ndata: {\"error\":{\"message\":\"overloaded\"}}\n\n",
    );
    let home = tempdir().unwrap();
    let out = qq(home.path(), &server)
        .args(["-r", "x"])
        .write_stdin("")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("overloaded"));
}

#[test]
fn qq_fails_when_answer_is_truncated() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    mock_reply(
        &server,
        "application/json",
        r#"{"choices":[{"message":{"content":"Use <cmd>tar -czf"},"finish_reason":"length"}]}"#,
    );
    let home = tempdir().unwrap();
    let out = qq(home.path(), &server)
        .args(["--no-stream", "x"])
        .write_stdin("")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("output limit"), "{stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("tar -czf"));
}

#[test]
fn qq_fails_on_empty_answer() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    mock_reply(
        &server,
        "application/json",
        r#"{"choices":[{"message":{"content":null},"finish_reason":"stop"}]}"#,
    );
    let home = tempdir().unwrap();
    let out = qq(home.path(), &server)
        .args(["--no-stream", "x"])
        .write_stdin("")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("empty answer"));
}

#[test]
fn qq_into_closed_pipe_exits_quietly() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    let lines: Vec<String> = (0..2000).map(|i| format!("line {i}\n")).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    mock_reply(&server, "text/event-stream", &sse(&refs));
    let home = tempdir().unwrap();
    let mut child = Command::new(assert_cmd::cargo::cargo_bin!("qq"))
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", "")
        .env("OPENROUTER_API_KEY", "test")
        .current_dir(home.path())
        .args(["-n", "-r", "--api-base", &server.base_url(), "x"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = [0u8; 16];
    child
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut first)
        .unwrap();
    drop(child.stdout.take()); // like `| head -c 16`
    let status = child.wait().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(status.success(), "status {status:?}, stderr {stderr}");
}

#[test]
fn qa_runs_read_file_tool_call() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    mock_reply(
        &server,
        "application/json",
        r#"{"choices":[{"message":{"content":null,"tool_calls":[{"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"note.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
    );
    let home = tempdir().unwrap();
    std::fs::write(home.path().join("note.txt"), "hello from file").unwrap();
    let out = cargo_bin_cmd!("qa")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", "")
        .env("OPENROUTER_API_KEY", "test")
        .current_dir(home.path())
        .args(["-n", "--api-base", &server.base_url(), "read note.txt"])
        .write_stdin("")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[tool:read_file]") && stdout.contains("hello from file"),
        "{stdout}"
    );
}

#[test]
fn qa_fails_when_tool_call_is_truncated() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    mock_reply(
        &server,
        "application/json",
        r#"{"choices":[{"message":{"content":"{\"tool\":\"write_file\",\"argum"},"finish_reason":"length"}]}"#,
    );
    let home = tempdir().unwrap();
    let out = cargo_bin_cmd!("qa")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", "")
        .env("OPENROUTER_API_KEY", "test")
        .current_dir(home.path())
        .args(["-n", "--api-base", &server.base_url(), "write a file"])
        .write_stdin("")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("output limit"));
}

#[cfg(target_os = "linux")]
#[test]
fn qq_reports_other_stdout_write_errors() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    mock_reply(&server, "text/event-stream", &sse(&["answer"]));
    let home = tempdir().unwrap();
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let out = Command::new(assert_cmd::cargo::cargo_bin!("qq"))
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", "")
        .env("OPENROUTER_API_KEY", "test")
        .current_dir(home.path())
        .args(["-n", "-r", "--api-base", &server.base_url(), "x"])
        .stdin(Stdio::null())
        .stdout(full)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("failed to write output"));
}

/// What one qq run with a given stdin did.
struct StdinRun {
    status: std::process::ExitStatus,
    stderr: String,
    /// Requests whose body contained the needle.
    needle_hits: usize,
    /// All requests.
    total_hits: usize,
}

fn qq_with_stdin(stdin: Stdio, extra: &[&str], needle: &str) -> StdinRun {
    let server = MockServer::start();
    let with_needle = server.mock(|when, then| {
        when.method(POST)
            .path("/chat/completions")
            .body_contains(needle);
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(sse(&["ok"]));
    });
    let any = server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(sse(&["ok"]));
    });
    let home = tempdir().unwrap();
    let base = server.base_url();
    let mut args = vec!["-n", "-r"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--api-base", &base, "question"]);
    let out = Command::new(assert_cmd::cargo::cargo_bin!("qq"))
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", "")
        .env("OPENROUTER_API_KEY", "test")
        .current_dir(home.path())
        .args(&args)
        .stdin(stdin)
        .output()
        .unwrap();
    StdinRun {
        status: out.status,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        needle_hits: with_needle.hits(),
        total_hits: with_needle.hits() + any.hits(),
    }
}

fn stdin_file(contents: &[u8]) -> (tempfile::TempDir, Stdio) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("in.txt");
    std::fs::write(&path, contents).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    (dir, Stdio::from(file))
}

#[test]
fn qq_reads_stdin_from_a_file() {
    if sandbox_blocks_binding() {
        return;
    }
    let (_dir, stdin) = stdin_file(b"MARKER-FROM-FILE\n");
    let run = qq_with_stdin(stdin, &[], "MARKER-FROM-FILE");
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!(run.needle_hits, 1);
}

#[test]
fn qq_no_stdin_ignores_piped_input() {
    if sandbox_blocks_binding() {
        return;
    }
    let (_dir, stdin) = stdin_file(b"MARKER-IGNORED\n");
    let run = qq_with_stdin(stdin, &["--no-stdin"], "MARKER-IGNORED");
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!((run.needle_hits, run.total_hits), (0, 1));
}

#[test]
fn qq_does_not_read_dev_null_or_wait_on_it() {
    if sandbox_blocks_binding() {
        return;
    }
    let run = qq_with_stdin(Stdio::null(), &[], "Input from pipe");
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!((run.needle_hits, run.total_hits), (0, 1));
}

#[test]
fn qq_no_stdin_does_not_wait_on_an_open_pipe() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    mock_reply(&server, "text/event-stream", &sse(&["ok"]));
    let home = tempdir().unwrap();
    let mut child = Command::new(assert_cmd::cargo::cargo_bin!("qq"))
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", "")
        .env("OPENROUTER_API_KEY", "test")
        .current_dir(home.path())
        .args([
            "-n",
            "-r",
            "--no-stdin",
            "--api-base",
            &server.base_url(),
            "x",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Hold the write end open and never write, as a wrapper or editor might.
    let _held_stdin = child.stdin.take();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("qq --no-stdin waited on an open stdin pipe");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(status.success());
}

#[test]
fn qq_cuts_long_stdin_with_a_notice() {
    if sandbox_blocks_binding() {
        return;
    }
    let max = qqqa::input::MAX_STDIN_BYTES;
    let big = vec![b'x'; max + 50_000];
    let (_dir, stdin) = stdin_file(&big);
    let too_long = "x".repeat(max + 1);
    let run = qq_with_stdin(stdin, &[], &too_long);
    assert!(run.status.success(), "{}", run.stderr);
    assert!(
        run.stderr.contains("only the first 256 KiB"),
        "{}",
        run.stderr
    );
    assert_eq!((run.needle_hits, run.total_hits), (0, 1));
}

#[test]
fn qq_rejects_binary_stdin() {
    if sandbox_blocks_binding() {
        return;
    }
    let (_dir, stdin) = stdin_file(b"\x7fELF\x00\x02\x01");
    let run = qq_with_stdin(stdin, &[], "ELF");
    assert!(!run.status.success());
    assert!(run.stderr.contains("binary"), "{}", run.stderr);
    assert_eq!(run.total_hits, 0);
}
