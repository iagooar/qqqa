use httpmock::Method::POST;
use httpmock::MockServer;
use qqqa::ai::{ChatClient, Msg, SseDecoder};
use serde_json::Value;
use std::collections::HashMap;
use std::net::TcpListener;

fn sandbox_blocks_binding() -> bool {
    TcpListener::bind("127.0.0.1:0").is_err()
}

fn delta(text: &str) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{"content":text}}]})
    )
}

const FINISH_STOP: &str = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";

async fn stream(
    server: &MockServer,
    content_type: &str,
    body: String,
) -> (anyhow::Result<qqqa::ai::StreamSummary>, String) {
    server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", content_type)
            .body(body);
    });
    let client =
        ChatClient::new(server.base_url(), "test".into(), HashMap::new(), None, None).unwrap();
    let messages = [Msg {
        role: "user",
        content: "hi",
    }];
    let mut acc = String::new();
    let res = client
        .chat_stream_messages("model-x", &messages, false, |t| acc.push_str(t))
        .await;
    (res, acc)
}

#[test]
fn sse_decoder_follows_the_event_stream_rules() {
    let mut d = SseDecoder::default();
    let mut events =
        d.push(b"data:no-space\n\ndata: one\r\ndata: two\r\n\r\n: comment\n\nevent: x\ndata: last");
    events.extend(d.finish());
    assert_eq!(events, vec!["no-space", "one\ntwo", "last"]);
}

#[test]
fn sse_decoder_handles_any_split_point() {
    let input = "data: {\"a\":\"ż\"}\n\ndata:b\r\n\r\n";
    for cut in 0..=input.len() {
        let mut d = SseDecoder::default();
        let mut events = d.push(&input.as_bytes()[..cut]);
        events.extend(d.push(&input.as_bytes()[cut..]));
        events.extend(d.finish());
        assert_eq!(events, vec!["{\"a\":\"ż\"}", "b"], "cut at {cut}");
    }
}

#[tokio::test]
async fn stream_accepts_data_lines_without_a_space() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    let body = "data:{\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata:[DONE]\n\n";
    let (res, acc) = stream(&server, "text/event-stream", body.to_string()).await;
    res.unwrap();
    assert_eq!(acc, "hello");
}

#[tokio::test]
async fn stream_error_event_is_an_error() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    let body = delta("partial ")
        + ": OPENROUTER PROCESSING\n\n"
        + "data: {\"error\":{\"message\":\"upstream overloaded\",\"code\":502}}\n\n";
    let (res, acc) = stream(&server, "text/event-stream", body).await;
    let err = res.unwrap_err().to_string();
    assert!(err.contains("upstream overloaded"), "{err}");
    assert_eq!(acc, "partial ");
}

#[tokio::test]
async fn stream_finish_reason_error_is_an_error() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    let body = delta("x") + "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"error\"}]}\n\n";
    let (res, _) = stream(&server, "text/event-stream", body).await;
    assert!(res.is_err());
}

#[tokio::test]
async fn stream_that_ends_early_is_an_error() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    let (res, acc) = stream(&server, "text/event-stream", delta("cut off")).await;
    assert!(res.unwrap_err().to_string().contains("ended before"));
    assert_eq!(acc, "cut off");
}

#[tokio::test]
async fn stream_reports_the_finish_reason() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    let body = delta("a")
        + "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n"
        + "data: [DONE]\n\n";
    let (res, _) = stream(&server, "text/event-stream", body).await;
    assert_eq!(res.unwrap().finish_reason.as_deref(), Some("length"));
}

#[tokio::test]
async fn stream_joins_multi_line_data() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    let body = "data: {\"choices\":[{\"delta\":\ndata: {\"content\":\"joined\"}}]}\n\n".to_string()
        + FINISH_STOP;
    let (res, acc) = stream(&server, "text/event-stream", body).await;
    res.unwrap();
    assert_eq!(acc, "joined");
}

#[tokio::test]
async fn stream_falls_back_to_a_json_body() {
    if sandbox_blocks_binding() {
        return;
    }
    let server = MockServer::start();
    let body = r#"{"choices":[{"message":{"content":"whole answer"},"finish_reason":"stop"}]}"#;
    let (res, acc) = stream(&server, "application/json", body.to_string()).await;
    assert_eq!(res.unwrap().finish_reason.as_deref(), Some("stop"));
    assert_eq!(acc, "whole answer");
}

#[tokio::test]
async fn token_caps_have_defaults_and_profile_overrides() {
    if sandbox_blocks_binding() {
        return;
    }
    let base = ChatClient::new("http://x".into(), "k".into(), HashMap::new(), None, None).unwrap();
    assert_eq!(base.max_tokens_for("gpt-4.1-mini"), 4096);
    assert_eq!(base.max_tokens_for("gpt-5-mini"), 16384);
    let one = ChatClient::new("http://x".into(), "k".into(), HashMap::new(), None, None)
        .unwrap()
        .with_max_tokens(Some(1000), None);
    assert_eq!(one.max_tokens_for("gpt-4.1-mini"), 1000);
    assert_eq!(one.max_tokens_for("o3-mini"), 1000);
    let both = ChatClient::new("http://x".into(), "k".into(), HashMap::new(), None, None)
        .unwrap()
        .with_max_tokens(Some(1000), Some(30000));
    assert_eq!(both.max_tokens_for("o3-mini"), 30000);

    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/chat/completions").matches(|req| {
            let body: Value = serde_json::from_slice(req.body.as_ref().unwrap()).unwrap();
            body.get("max_completion_tokens").and_then(Value::as_u64) == Some(30000)
        });
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"choices":[{"message":{"content":"ok"}}]}"#);
    });
    let client = ChatClient::new(server.base_url(), "k".into(), HashMap::new(), None, None)
        .unwrap()
        .with_max_tokens(Some(1000), Some(30000));
    assert_eq!(
        client.chat_once("o3-mini", "hi", false).await.unwrap(),
        "ok"
    );
    mock.assert();
}
