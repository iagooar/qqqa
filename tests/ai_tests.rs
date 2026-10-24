use httpmock::Method::POST;
use httpmock::MockServer;
use httpmock::prelude::HttpMockRequest;
use qqqa::ai::{AssistantReply, ChatClient, Msg};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::TcpListener;
use std::time::{Duration, Instant};

fn sandbox_blocks_binding() -> bool {
    TcpListener::bind("127.0.0.1:0").is_err()
}

#[tokio::test]
async fn chat_once_non_streaming_parses_response() {
    if sandbox_blocks_binding() {
        eprintln!("[skip] sandbox blocks binding to 127.0.0.1; skipping httpmock test");
        return;
    }
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"choices":[{"message":{"content":"Hello world"}}]}"#);
    });

    let client =
        ChatClient::new(server.base_url(), "test".into(), HashMap::new(), None, None).unwrap();
    let got = client.chat_once("model-x", "Hi", true).await.unwrap();
    assert_eq!(got, "Hello world");
    mock.assert();
}

#[tokio::test]
async fn chat_stream_streams_tokens() {
    if sandbox_blocks_binding() {
        eprintln!("[skip] sandbox blocks binding to 127.0.0.1; skipping httpmock test");
        return;
    }
    let server = MockServer::start();
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"He\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"llo\"}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let mock = server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(sse_body);
    });

    let client =
        ChatClient::new(server.base_url(), "test".into(), HashMap::new(), None, None).unwrap();
    let mut acc = String::new();
    client
        .chat_stream("model-x", "Hi", true, |tok| acc.push_str(tok))
        .await
        .unwrap();
    assert_eq!(acc, "Hello");
    mock.assert();
}

#[tokio::test]
async fn chat_once_uses_new_parameters_for_new_models() {
    if sandbox_blocks_binding() {
        eprintln!("[skip] sandbox blocks binding to 127.0.0.1; skipping httpmock test");
        return;
    }
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/chat/completions")
            .matches(|req: &HttpMockRequest| {
                let body = req
                    .body
                    .as_ref()
                    .expect("expected request body for new model");
                let payload: Value =
                    serde_json::from_slice(body).expect("request body should be valid JSON");
                assert!(payload.get("max_completion_tokens").is_some());
                assert!(payload.get("max_tokens").is_none());
                assert!(payload.get("temperature").is_none());
                assert_eq!(
                    payload.get("reasoning_effort").and_then(|v| v.as_str()),
                    Some("minimal")
                );
                true
            });
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"choices":[{"message":{"content":"ok"}}]}"#);
    });

    let client =
        ChatClient::new(server.base_url(), "test".into(), HashMap::new(), None, None).unwrap();
    let got = client.chat_once("gpt-5-mini", "Hi", false).await.unwrap();
    assert_eq!(got, "ok");
    mock.assert();
}

#[tokio::test]
async fn chat_once_uses_legacy_parameters_for_old_models() {
    if sandbox_blocks_binding() {
        eprintln!("[skip] sandbox blocks binding to 127.0.0.1; skipping httpmock test");
        return;
    }
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/chat/completions")
            .matches(|req: &HttpMockRequest| {
                let body = req
                    .body
                    .as_ref()
                    .expect("expected request body for legacy model");
                let payload: Value =
                    serde_json::from_slice(body).expect("request body should be valid JSON");
                assert_eq!(
                    payload.get("max_tokens").and_then(|v| v.as_u64()),
                    Some(4096)
                );
                assert!(payload.get("max_completion_tokens").is_none());
                let temp = payload
                    .get("temperature")
                    .and_then(|v| v.as_f64())
                    .expect("temperature present for legacy model");
                assert!((temp - 0.15).abs() < 1e-6);
                assert!(payload.get("reasoning_effort").is_none());
                true
            });
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"choices":[{"message":{"content":"ok"}}]}"#);
    });

    let client =
        ChatClient::new(server.base_url(), "test".into(), HashMap::new(), None, None).unwrap();
    let got = client.chat_once("gpt-4.1-mini", "Hi", false).await.unwrap();
    assert_eq!(got, "ok");
    mock.assert();
}

#[tokio::test]
async fn chat_once_defaults_temperature_for_o1_models() {
    if sandbox_blocks_binding() {
        eprintln!("[skip] sandbox blocks binding to 127.0.0.1; skipping httpmock test");
        return;
    }
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/chat/completions")
            .matches(|req: &HttpMockRequest| {
                let body = req
                    .body
                    .as_ref()
                    .expect("expected request body for o1 model");
                let payload: Value =
                    serde_json::from_slice(body).expect("request body should be valid JSON");
                assert!(payload.get("max_completion_tokens").is_some());
                let temp = payload
                    .get("temperature")
                    .and_then(|v| v.as_f64())
                    .expect("temperature present for o1 model");
                assert!((temp - 0.15).abs() < 1e-6);
                true
            });
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"choices":[{"message":{"content":"ok"}}]}"#);
    });

    let client =
        ChatClient::new(server.base_url(), "test".into(), HashMap::new(), None, None).unwrap();
    let got = client.chat_once("o1-preview", "Hi", false).await.unwrap();
    assert_eq!(got, "ok");
    mock.assert();
}

#[tokio::test]
async fn chat_once_forces_temperature_for_gpt5_overrides() {
    if sandbox_blocks_binding() {
        eprintln!("[skip] sandbox blocks binding to 127.0.0.1; skipping httpmock test");
        return;
    }
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/chat/completions")
            .matches(|req: &HttpMockRequest| {
                let body = req
                    .body
                    .as_ref()
                    .expect("expected request body for gpt-5 override");
                let payload: Value =
                    serde_json::from_slice(body).expect("request body should be valid JSON");
                assert_eq!(
                    payload.get("temperature").and_then(|v| v.as_f64()),
                    Some(1.0)
                );
                true
            });
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"choices":[{"message":{"content":"ok"}}]}"#);
    });

    let client = ChatClient::new(server.base_url(), "test".into(), HashMap::new(), None, None)
        .unwrap()
        .with_temperature(Some(0.42), true);
    let got = client.chat_once("gpt-5-mini", "Hi", false).await.unwrap();
    assert_eq!(got, "ok");
    mock.assert();
}

#[tokio::test]
async fn chat_once_respects_reasoning_override_when_configured() {
    if sandbox_blocks_binding() {
        eprintln!("[skip] sandbox blocks binding to 127.0.0.1; skipping httpmock test");
        return;
    }
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/chat/completions")
            .matches(|req: &HttpMockRequest| {
                let body = req
                    .body
                    .as_ref()
                    .expect("expected request body for override model");
                let payload: Value =
                    serde_json::from_slice(body).expect("request body should be valid JSON");
                assert_eq!(
                    payload.get("reasoning_effort").and_then(|v| v.as_str()),
                    Some("high")
                );
                true
            });
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"choices":[{"message":{"content":"ok"}}]}"#);
    });

    let client = ChatClient::new(server.base_url(), "test".into(), HashMap::new(), None, None)
        .unwrap()
        .with_reasoning_effort(Some("high".to_string()));
    let got = client.chat_once("gpt-5-mini", "Hi", false).await.unwrap();
    assert_eq!(got, "ok");
    mock.assert();
}

#[tokio::test]
async fn chat_client_sends_custom_headers() {
    if sandbox_blocks_binding() {
        eprintln!("[skip] sandbox blocks binding to 127.0.0.1; skipping httpmock test");
        return;
    }
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/chat/completions")
            .header("HTTP-Referer", "https://github.com/iagooar/qqqa")
            .header("X-Title", "qqqa");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"choices":[{"message":{"content":"ok"}}]}"#);
    });

    let mut headers = HashMap::new();
    headers.insert(
        "HTTP-Referer".to_string(),
        "https://github.com/iagooar/qqqa".to_string(),
    );
    headers.insert("X-Title".to_string(), "qqqa".to_string());

    let client = ChatClient::new(server.base_url(), "test".into(), headers, None, None).unwrap();
    let got = client.chat_once("model-x", "Hi", false).await.unwrap();
    assert_eq!(got, "ok");
    mock.assert();
}

#[tokio::test]
async fn chat_client_respects_timeout_override() {
    if sandbox_blocks_binding() {
        eprintln!("[skip] sandbox blocks binding to 127.0.0.1; skipping httpmock test");
        return;
    }
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .delay(Duration::from_millis(200))
            .body(r#"{"choices":[{"message":{"content":"too slow"}}]}"#);
    });

    let client = ChatClient::new(
        server.base_url(),
        "test".into(),
        HashMap::new(),
        None,
        Some(Duration::from_millis(50)),
    )
    .unwrap();
    let start = Instant::now();
    let err = client.chat_once("model-x", "Hi", false).await.unwrap_err();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(200),
        "request was not capped by timeout: {:?}",
        elapsed
    );
    let timed_out = err.chain().any(|cause| {
        let msg = cause.to_string();
        msg.contains("deadline") || msg.contains("timed out")
    });
    assert!(timed_out, "unexpected error chain: {err:?}");
    mock.assert();
}

#[tokio::test]
async fn chat_once_messages_with_tools_handles_legacy_function_call() {
    if sandbox_blocks_binding() {
        eprintln!("[skip] sandbox blocks binding to 127.0.0.1; skipping httpmock test");
        return;
    }
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .body(
                r#"{"choices":[{"message":{"function_call":{"name":"read_file","arguments":"{\"path\":\"/tmp/file.txt\"}"}}}]}"#,
            );
    });

    let client =
        ChatClient::new(server.base_url(), "test".into(), HashMap::new(), None, None).unwrap();
    let reply = client
        .chat_once_messages_with_tools(
            "model-x",
            &[
                Msg {
                    role: "system",
                    content: "You are a helper",
                },
                Msg {
                    role: "user",
                    content: "Read a file",
                },
            ],
            json!([]),
            false,
        )
        .await
        .unwrap();

    match reply {
        AssistantReply::ToolCall {
            name,
            arguments_json,
        } => {
            assert_eq!(name, "read_file");
            assert!(arguments_json.contains("/tmp/file.txt"));
        }
        AssistantReply::Content(other) => {
            panic!("expected tool call, got content: {}", other);
        }
    }

    mock.assert();
}
