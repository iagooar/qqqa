//! End-to-end runs of the real `qq` and `qa` binaries against a mock
//! OpenAI-compatible server, with a temporary HOME.
use assert_cmd::cargo::cargo_bin_cmd;
use httpmock::Method::POST;
use httpmock::MockServer;
use std::net::TcpListener;
use std::path::Path;
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
