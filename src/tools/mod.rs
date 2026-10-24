use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};

pub mod execute_command;
pub mod read_file;
pub mod write_file;

#[derive(Debug, Deserialize)]
pub struct ToolEnvelope {
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug)]
pub enum ToolCall {
    ReadFile(read_file::Args),
    WriteFile(write_file::Args),
    ExecuteCommand(execute_command::Args),
}

/// What qa should do with a model reply.
#[derive(Debug)]
pub enum Reply {
    /// Run this tool.
    Tool(ToolCall),
    /// Print this text as the answer.
    Text(String),
}

/// Wrappers (`json` tool, `{tool, arguments}`) nested deeper than this are refused.
const MAX_WRAPPER_DEPTH: usize = 8;

/// Fields a model uses to explain, in a `json` call, why it does not run a tool.
const EXPLANATION_FIELDS: &[&str] = &[
    "reason",
    "explanation",
    "message",
    "content",
    "text",
    "answer",
];

/// Resolve a named tool call from an API `tool_calls` entry or a plain-JSON reply.
///
/// - `arguments` may be an object or a JSON string (the OpenAI shape).
/// - The `json` tool and `{"tool": .., "arguments": ..}` wrappers are unwrapped.
/// - A `json` call that carries only an explanation becomes [`Reply::Text`].
pub fn resolve_tool_call(name: &str, arguments: Value) -> Result<Reply> {
    let mut name = name.to_string();
    let mut args = arguments;
    for _ in 0..MAX_WRAPPER_DEPTH {
        if let Value::String(raw) = &args {
            args = if raw.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(raw)
                    .with_context(|| format!("Failed to parse {} arguments: {}", name, raw))?
            };
        }
        if args.is_null() {
            args = json!({});
        }
        let wrapped_tool = args.get("tool").and_then(Value::as_str);
        if let Some(inner) = wrapped_tool
            && (name == "json" || args.get("arguments").is_some())
        {
            let inner = inner.to_string();
            args = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
            name = inner;
            continue;
        }
        return match name.as_str() {
            "read_file" => Ok(Reply::Tool(ToolCall::ReadFile(parse_args(&name, args)?))),
            "write_file" => Ok(Reply::Tool(ToolCall::WriteFile(parse_args(&name, args)?))),
            "execute_command" => Ok(Reply::Tool(ToolCall::ExecuteCommand(parse_args(
                &name, args,
            )?))),
            "json" => EXPLANATION_FIELDS
                .iter()
                .find_map(|key| args.get(*key).and_then(Value::as_str))
                .map(|text| Reply::Text(text.to_string()))
                .ok_or_else(|| anyhow!("The model called the `json` tool without a `tool` field.")),
            other => Err(anyhow!("Unknown tool call: {}", other)),
        };
    }
    Err(anyhow!(
        "Tool call wrappers are nested more than {} levels deep.",
        MAX_WRAPPER_DEPTH
    ))
}

fn parse_args<T: serde::de::DeserializeOwned>(name: &str, args: Value) -> Result<T> {
    serde_json::from_value(args).map_err(|e| anyhow!("Failed to parse {} args: {}", name, e))
}

/// Interpret a plain-text model reply (CLI providers, or HTTP content).
///
/// A reply that is one JSON object with a `tool` field, bare or inside a
/// Markdown code fence, is a tool call. Anything else is the answer text.
pub fn parse_reply_text(text: &str) -> Result<Reply> {
    let body = strip_code_fence(text.trim());
    if !body.starts_with('{') {
        return Ok(Reply::Text(text.to_string()));
    }
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Ok(Reply::Text(text.to_string()));
    };
    let Some(tool) = value.get("tool").and_then(Value::as_str) else {
        return Ok(Reply::Text(text.to_string()));
    };
    let arguments = value.get("arguments").cloned().unwrap_or_else(|| json!({}));
    resolve_tool_call(tool, arguments)
}

/// Remove one surrounding Markdown code fence (```` ``` ```` or ```` ```json ````).
fn strip_code_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let Some(newline) = rest.find('\n') else {
        return text;
    };
    let inner = &rest[newline + 1..];
    inner.trim_end().strip_suffix("```").unwrap_or(inner).trim()
}

/// Try to parse a tool call JSON from assistant content.
pub fn parse_tool_call(json_text: &str) -> Result<ToolCall> {
    let env: ToolEnvelope = serde_json::from_str(json_text)
        .with_context(|| "Assistant response was not a tool JSON object")?;
    match resolve_tool_call(&env.tool, env.arguments)? {
        Reply::Tool(call) => Ok(call),
        Reply::Text(_) => Err(anyhow!("Assistant response was not a tool call")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(reply: Result<Reply>) -> ToolCall {
        match reply.expect("resolves") {
            Reply::Tool(call) => call,
            Reply::Text(t) => panic!("expected a tool call, got text {t:?}"),
        }
    }

    fn read_path(call: ToolCall) -> String {
        match call {
            ToolCall::ReadFile(args) => args.path,
            other => panic!("expected read_file, got {other:?}"),
        }
    }

    #[test]
    fn arguments_may_be_a_string_or_an_object() {
        let s = resolve_tool_call("read_file", json!(r#"{"path":"a.txt"}"#));
        let o = resolve_tool_call("read_file", json!({"path": "a.txt"}));
        assert_eq!(read_path(tool(s)), "a.txt");
        assert_eq!(read_path(tool(o)), "a.txt");
    }

    #[test]
    fn json_wrapper_and_nested_wrappers_are_unwrapped() {
        let inner = json!({"tool": "read_file", "arguments": {"path": "a.txt"}});
        let nested = json!({"tool": "json", "arguments": inner.clone()});
        assert_eq!(read_path(tool(resolve_tool_call("json", inner))), "a.txt");
        assert_eq!(read_path(tool(resolve_tool_call("json", nested))), "a.txt");
        let legacy = json!({"tool": "execute_command", "arguments": {"command": "ls"}});
        match tool(resolve_tool_call("execute_command", legacy)) {
            ToolCall::ExecuteCommand(args) => assert_eq!(args.command, "ls"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn json_call_with_an_explanation_is_text() {
        match resolve_tool_call("json", json!({"reason": "cannot do that"})).unwrap() {
            Reply::Text(t) => assert_eq!(t, "cannot do that"),
            other => panic!("{other:?}"),
        }
        assert!(resolve_tool_call("json", json!({})).is_err());
    }

    #[test]
    fn unknown_tools_and_endless_wrappers_are_errors() {
        assert!(resolve_tool_call("delete_all", json!({})).is_err());
        let mut v = json!({"tool": "read_file", "arguments": {"path": "a"}});
        for _ in 0..20 {
            v = json!({"tool": "json", "arguments": v});
        }
        assert!(resolve_tool_call("json", v).is_err());
    }

    #[test]
    fn reply_text_accepts_bare_and_fenced_json() {
        let bare = r#"{"tool":"read_file","arguments":{"path":"a.txt"}}"#;
        let fenced = "```json\n{\"tool\":\"read_file\",\"arguments\":{\"path\":\"a.txt\"}}\n```";
        let plain_fence = "```\n{\"tool\":\"read_file\",\"arguments\":{\"path\":\"a.txt\"}}\n```\n";
        for text in [bare, fenced, plain_fence] {
            assert_eq!(read_path(tool(parse_reply_text(text))), "a.txt", "{text:?}");
        }
    }

    #[test]
    fn reply_text_without_a_tool_object_is_text() {
        for text in [
            "Use ls -la to list files.",
            "{\"note\": \"no tool here\"}",
            "```sh\nls -la\n```",
            "{ not json",
        ] {
            match parse_reply_text(text).unwrap() {
                Reply::Text(t) => assert_eq!(t, text),
                other => panic!("{text:?} gave {other:?}"),
            }
        }
    }
}
