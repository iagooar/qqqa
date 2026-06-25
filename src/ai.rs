use crate::config::{CliEngine, ResolvedTlsConfig};
use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use fs_err as fs;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Certificate, Client, RequestBuilder};
use rustls_pemfile::certs;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::time::Duration;

const DEFAULT_MAX_COMPLETION_TOKENS: u32 = 800;
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 180;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;
const DEFAULT_TEMPERATURE: f32 = 0.15;

fn format_api_error(status: reqwest::StatusCode, body: &str, base_url: &str) -> anyhow::Error {
    let mut message = format!("API error ({}): {}", status, body);
    if status.as_u16() == 402 && base_url.contains("openrouter.ai") {
        message.push_str(
            "\n\nOpenRouter hint: many models require a positive credit balance even when using free variants. \
Try a model slug ending in :free (for example openrouter/free), or add credits at \
https://openrouter.ai/settings/credits . See https://openrouter.ai/docs/faq for free-tier limits.",
        );
    }
    anyhow!(message)
}

/// Minimal OpenAI-compatible chat streaming delta payload
#[derive(Debug, Deserialize)]
struct ChatStreamChunkChoiceDelta {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatStreamChunkChoice {
    delta: Option<ChatStreamChunkChoiceDelta>,
    #[allow(dead_code)]
    index: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ChatStreamChunk {
    choices: Vec<ChatStreamChunkChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

// Types for tool-aware chat responses
#[derive(Debug, Deserialize)]
struct ToolFunctionSpecResp {
    name: String,
    // OpenAI-compatible APIs return arguments as a JSON string
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ToolCallResp {
    #[serde(rename = "type")]
    _type: String,
    function: ToolFunctionSpecResp,
}

#[derive(Debug, Deserialize)]
struct ChatMessageWithTools {
    #[serde(default)]
    content: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallResp>>,
    #[serde(default)]
    function_call: Option<ToolFunctionSpecResp>,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceWithTools {
    message: ChatMessageWithTools,
}

#[derive(Debug, Deserialize)]
struct ChatResponseWithTools {
    choices: Vec<ChatChoiceWithTools>,
}

/// A simplified representation of the assistant's first choice.
pub enum AssistantReply {
    Content(String),
    ToolCall {
        name: String,
        arguments_json: String,
    },
}

#[derive(Clone, Copy)]
enum TemperatureDirective {
    Omit,
    Send(f32),
}

pub struct ChatClient {
    client: Client,
    base_url: String,
    api_key: String,
    reasoning_effort: Option<String>,
    temperature_override: Option<f32>,
    temperature_user_override: bool,
    default_headers: HeaderMap,
}

impl ChatClient {
    pub fn new(
        base_url: String,
        api_key: String,
        headers: HashMap<String, String>,
        tls: Option<&ResolvedTlsConfig>,
        request_timeout: Option<Duration>,
    ) -> Result<Self> {
        // Use rustls for TLS; set useful timeouts for robustness.
        let timeout = request_timeout.unwrap_or(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS));
        let mut builder = Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS));
        if let Some(tls_cfg) = tls {
            for cert in load_root_certificates(&tls_cfg.ca_bundle_path)? {
                builder = builder.add_root_certificate(cert);
            }
        }
        let client = builder.build()?;
        let mut default_headers = HeaderMap::new();
        for (name, value) in headers {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("Invalid header name '{}': must be ASCII", name))?;
            let header_value = HeaderValue::from_str(&value)
                .with_context(|| format!("Invalid header value for '{}': {}", name, value))?;
            default_headers.insert(header_name, header_value);
        }
        Ok(Self {
            client,
            base_url,
            api_key,
            reasoning_effort: None,
            temperature_override: None,
            temperature_user_override: false,
            default_headers,
        })
    }

    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<String>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    pub fn with_temperature(mut self, temperature: Option<f32>, user_provided: bool) -> Self {
        self.temperature_override = temperature;
        self.temperature_user_override = user_provided && temperature.is_some();
        self
    }

    fn request_builder(&self) -> RequestBuilder {
        let mut builder = self.client.post(self.chat_url());
        if !self.default_headers.is_empty() {
            builder = builder.headers(self.default_headers.clone());
        }
        builder.bearer_auth(&self.api_key)
    }

    fn is_new_style_model(model: &str) -> bool {
        let lower = model.to_ascii_lowercase();
        const PREFIXES: [&str; 3] = ["gpt-5", "o1", "o3"];
        PREFIXES.iter().any(|prefix| lower.starts_with(prefix))
    }

    fn is_gpt5_model(model: &str) -> bool {
        model.to_ascii_lowercase().starts_with("gpt-5")
    }

    fn max_tokens_param(model: &str) -> &'static str {
        if Self::is_new_style_model(model) {
            "max_completion_tokens"
        } else {
            "max_tokens"
        }
    }

    fn temperature_directive(&self, model: &str) -> TemperatureDirective {
        if Self::is_gpt5_model(model) {
            if self.temperature_override.is_some() {
                TemperatureDirective::Send(1.0)
            } else {
                TemperatureDirective::Omit
            }
        } else {
            TemperatureDirective::Send(self.temperature_override.unwrap_or(DEFAULT_TEMPERATURE))
        }
    }

    fn default_reasoning_effort(model: &str) -> Option<&'static str> {
        if Self::is_gpt5_model(model) {
            Some("minimal")
        } else {
            None
        }
    }

    fn apply_model_defaults(&self, body: &mut Value, model: &str, max_tokens: u32, debug: bool) {
        if let Some(obj) = body.as_object_mut() {
            obj.remove("max_tokens");
            obj.remove("max_completion_tokens");
            obj.insert(Self::max_tokens_param(model).to_string(), json!(max_tokens));
            match self.temperature_directive(model) {
                TemperatureDirective::Send(value) => {
                    obj.insert("temperature".into(), json!(value));
                }
                TemperatureDirective::Omit => {
                    obj.remove("temperature");
                }
            }
            if debug
                && Self::is_gpt5_model(model)
                && self.temperature_user_override
                && self.temperature_override.is_some()
            {
                eprintln!(
                    "[warn] Overriding requested temperature to 1.0 for GPT-5 model '{}'.",
                    model
                );
            }
            let reasoning = if Self::default_reasoning_effort(model).is_some() {
                self.reasoning_effort
                    .as_deref()
                    .or_else(|| Self::default_reasoning_effort(model))
            } else {
                None
            };
            if let Some(effort) = reasoning {
                obj.insert("reasoning_effort".into(), json!(effort));
            } else {
                obj.remove("reasoning_effort");
            }
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Non-streaming chat completion: returns the full assistant message.
    pub async fn chat_once(&self, model: &str, prompt: &str, debug: bool) -> Result<String> {
        let mut body = json!({
            "model": model,
            "messages": [
                {"role": "user", "content": prompt}
            ]
        });
        self.apply_model_defaults(&mut body, model, DEFAULT_MAX_COMPLETION_TOKENS, debug);
        if debug {
            let bytes = serde_json::to_vec(&body).unwrap();
            eprintln!("[debug] POST {} ({} bytes)", self.chat_url(), bytes.len());
        }
        let resp = self
            .request_builder()
            .json(&body)
            .send()
            .await
            .with_context(|| "HTTP request failed")?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(format_api_error(status, &text, &self.base_url));
        }
        let parsed: ChatResponse = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse chat response JSON: {}", text))?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No choices in response"))?;
        Ok(choice.message.content)
    }

    /// Non-streaming chat completion using explicit messages.
    pub async fn chat_once_messages(
        &self,
        model: &str,
        messages: &[Msg<'_>],
        debug: bool,
    ) -> Result<String> {
        let mut body = json!({
            "model": model,
            "messages": messages
        });
        self.apply_model_defaults(&mut body, model, DEFAULT_MAX_COMPLETION_TOKENS, debug);
        if debug {
            let bytes = serde_json::to_vec(&body).unwrap();
            eprintln!("[debug] POST {} ({} bytes)", self.chat_url(), bytes.len());
        }
        let resp = self
            .request_builder()
            .json(&body)
            .send()
            .await
            .with_context(|| "HTTP request failed")?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(format_api_error(status, &text, &self.base_url));
        }
        let parsed: ChatResponse = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse chat response JSON: {}", text))?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No choices in response"))?;
        Ok(choice.message.content)
    }

    /// Non-streaming chat completion allowing tool specs; returns either content or a tool call.
    pub async fn chat_once_messages_with_tools(
        &self,
        model: &str,
        messages: &[Msg<'_>],
        tools: serde_json::Value,
        debug: bool,
    ) -> Result<AssistantReply> {
        let mut body = json!({
            "model": model,
            "messages": messages,
            "tools": tools
        });
        self.apply_model_defaults(&mut body, model, DEFAULT_MAX_COMPLETION_TOKENS, debug);
        if debug {
            let bytes = serde_json::to_vec(&body).unwrap();
            eprintln!("[debug] POST {} ({} bytes)", self.chat_url(), bytes.len());
        }
        let resp = self
            .request_builder()
            .json(&body)
            .send()
            .await
            .with_context(|| "HTTP request failed")?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(format_api_error(status, &text, &self.base_url));
        }

        // Try to parse as tool-aware response first
        let parsed_tools: ChatResponseWithTools = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse tool-aware chat response JSON: {}", text))?;
        let choice = parsed_tools
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No choices in response"))?;

        if let Some(calls) = choice.message.tool_calls {
            if let Some(first) = calls.into_iter().next() {
                return Ok(AssistantReply::ToolCall {
                    name: first.function.name,
                    arguments_json: first.function.arguments,
                });
            }
        } else if let Some(func) = choice.message.function_call {
            return Ok(AssistantReply::ToolCall {
                name: func.name,
                arguments_json: func.arguments,
            });
        }
        let content = choice.message.content.unwrap_or_default();
        Ok(AssistantReply::Content(content))
    }

    /// Streaming chat completion. Calls `on_token` for each token/delta of content.
    pub async fn chat_stream<F>(
        &self,
        model: &str,
        prompt: &str,
        debug: bool,
        mut on_token: F,
    ) -> Result<()>
    where
        F: FnMut(&str),
    {
        let mut body = json!({
            "model": model,
            "messages": [
                {"role": "user", "content": prompt}
            ],
            "stream": true
        });
        self.apply_model_defaults(&mut body, model, DEFAULT_MAX_COMPLETION_TOKENS, debug);
        if debug {
            let bytes = serde_json::to_vec(&body).unwrap();
            eprintln!(
                "[debug] POST {} ({} bytes, stream)",
                self.chat_url(),
                bytes.len()
            );
        }
        let resp = self
            .request_builder()
            .json(&body)
            .send()
            .await
            .with_context(|| "HTTP request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format_api_error(status, &text, &self.base_url));
        }

        // The OpenAI-compatible API returns text/event-stream with lines prefixed by "data: ".
        // We read chunks and split by newlines; we accumulate and parse JSON lines.
        let mut stream = resp.bytes_stream();
        let mut buffer = Vec::<u8>::new();

        while let Some(item) = stream.next().await {
            let chunk: Bytes = item?;
            buffer.extend_from_slice(&chunk);
            // Process complete lines
            while let Some(pos) =
                find_double_newline(&buffer).or_else(|| find_single_newline(&buffer))
            {
                let line = buffer.drain(..=pos).collect::<Vec<u8>>();
                let s = String::from_utf8_lossy(&line);
                for raw in s.split('\n') {
                    let data = raw.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if let Some(rest) = data.strip_prefix("data: ") {
                        if rest == "[DONE]" {
                            return Ok(());
                        }
                        if let Ok(parsed) = serde_json::from_str::<ChatStreamChunk>(rest) {
                            for c in parsed.choices.into_iter() {
                                if let Some(delta) = c.delta {
                                    if let Some(token) = delta.content {
                                        on_token(&token);
                                    }
                                }
                            }
                        } else if debug {
                            eprintln!("[debug] Unparsed stream line: {}", rest);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Streaming chat completion with explicit messages (supports system+user for qq).
    pub async fn chat_stream_messages<F>(
        &self,
        model: &str,
        messages: &[Msg<'_>],
        debug: bool,
        mut on_token: F,
    ) -> Result<()>
    where
        F: FnMut(&str),
    {
        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": true
        });
        self.apply_model_defaults(&mut body, model, DEFAULT_MAX_COMPLETION_TOKENS, debug);
        if debug {
            let bytes = serde_json::to_vec(&body).unwrap();
            eprintln!(
                "[debug] POST {} ({} bytes, stream)",
                self.chat_url(),
                bytes.len()
            );
        }
        let resp = self
            .request_builder()
            .json(&body)
            .send()
            .await
            .with_context(|| "HTTP request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format_api_error(status, &text, &self.base_url));
        }

        // Reuse the same SSE parsing as non-message streaming
        let mut stream = resp.bytes_stream();
        let mut buffer = Vec::<u8>::new();

        while let Some(item) = stream.next().await {
            let chunk: Bytes = item?;
            buffer.extend_from_slice(&chunk);
            while let Some(pos) =
                find_double_newline(&buffer).or_else(|| find_single_newline(&buffer))
            {
                let line = buffer.drain(..=pos).collect::<Vec<u8>>();
                let s = String::from_utf8_lossy(&line);
                for raw in s.split('\n') {
                    let data = raw.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if let Some(rest) = data.strip_prefix("data: ") {
                        if rest == "[DONE]" {
                            return Ok(());
                        }
                        if let Ok(parsed) = serde_json::from_str::<ChatStreamChunk>(rest) {
                            for c in parsed.choices.into_iter() {
                                if let Some(delta) = c.delta {
                                    if let Some(token) = delta.content {
                                        on_token(&token);
                                    }
                                }
                            }
                        } else if debug {
                            eprintln!("[debug] Unparsed stream line: {}", rest);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

fn load_root_certificates(path: &Path) -> Result<Vec<Certificate>> {
    let data = fs::read(path)
        .with_context(|| format!("Reading TLS certificate(s) at {}", path.display()))?;
    let mut cursor = Cursor::new(&data);
    let mut parsed = Vec::new();
    for pem in certs(&mut cursor) {
        let der =
            pem.with_context(|| format!("Parsing PEM certificate from {}", path.display()))?;
        parsed.push(
            Certificate::from_der(der.as_ref())
                .with_context(|| format!("Parsing PEM certificate from {}", path.display()))?,
        );
    }
    if !parsed.is_empty() {
        return Ok(parsed);
    }

    if let Ok(cert) = Certificate::from_pem(&data) {
        return Ok(vec![cert]);
    }

    let cert = Certificate::from_der(&data)
        .with_context(|| format!("Parsing DER certificate from {}", path.display()))?;
    Ok(vec![cert])
}

/// Minimal typed message for multi-message calls.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Msg<'a> {
    pub role: &'a str,
    pub content: &'a str,
}

fn find_double_newline(buf: &[u8]) -> Option<usize> {
    // Find position to cut at a blank line (\n\n). Return index of the second newline.
    buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 1)
}

fn find_single_newline(buf: &[u8]) -> Option<usize> {
    buf.iter().position(|&b| b == b'\n')
}

mod cli_backend {
    use super::*;
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::process::Command;

    #[derive(Debug)]
    pub struct CliCompletionRequest<'a> {
        pub engine: CliEngine,
        pub binary: &'a str,
        pub base_args: &'a [String],
        pub system_prompt: &'a str,
        pub user_prompt: &'a str,
        pub model: &'a str,
        pub reasoning_effort: Option<&'a str>,
        pub debug: bool,
        pub timeout: Duration,
    }

    pub async fn run_cli_completion(req: CliCompletionRequest<'_>) -> Result<String> {
        match req.engine {
            CliEngine::Codex => run_codex(req).await,
            CliEngine::Claude => run_claude(req).await,
        }
    }

    pub async fn run_cli_completion_streaming<F>(
        req: CliCompletionRequest<'_>,
        on_token: F,
    ) -> Result<String>
    where
        F: FnMut(&str) + Send,
    {
        match req.engine {
            CliEngine::Claude => run_claude_streaming(req, on_token).await,
            CliEngine::Codex => Err(anyhow!(
                "CLI provider '{}' does not support streaming",
                req.binary
            )),
        }
    }

    async fn run_codex(req: CliCompletionRequest<'_>) -> Result<String> {
        let mut cmd = Command::new(req.binary);
        if req.base_args.is_empty() {
            cmd.arg("exec");
        } else {
            cmd.args(req.base_args);
        }
        let has_skip_flag = req
            .base_args
            .iter()
            .any(|arg| arg == "--skip-git-repo-check");
        if !has_skip_flag {
            cmd.arg("--skip-git-repo-check");
        }
        cmd.arg("--json");
        let reasoning = req.reasoning_effort.unwrap_or("low");
        cmd.arg("-c");
        cmd.arg(format!("model_reasoning_effort={}", reasoning));
        cmd.arg("-c");
        cmd.arg("sandbox_mode=read-only");
        cmd.arg("-");

        let prompt = format!(
            "{}\n\n{}\n",
            tagged_system_prompt(req.system_prompt),
            tagged_user_prompt(req.user_prompt)
        );

        if req.debug {
            eprintln!(
                "[debug] Running CLI provider '{}' with args: {:?}",
                req.binary, cmd
            );
        }

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn CLI provider '{}'. Is it installed and on your PATH?",
                    req.binary
                )
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .context("Writing prompt to CLI provider stdin")?;
        }

        let output = wait_child_output_with_timeout(child, req.timeout, req.binary).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(anyhow!(
                "CLI provider '{}' exited with status {}.{}{}",
                req.binary,
                output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                if stdout.trim().is_empty() {
                    "".to_string()
                } else {
                    format!("\nstdout: {}", stdout.trim())
                },
                if stderr.trim().is_empty() {
                    "".to_string()
                } else {
                    format!("\nstderr: {}", stderr.trim())
                }
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_codex_response(&stdout)
    }

    async fn run_claude(req: CliCompletionRequest<'_>) -> Result<String> {
        let mut cmd = build_claude_command(&req, false);

        if req.debug {
            eprintln!(
                "[debug] Running CLI provider '{}' with args: {:?}",
                req.binary, cmd
            );
        }

        let child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn CLI provider '{}'. Is it installed and on your PATH?",
                    req.binary
                )
            })?;

        let output = wait_child_output_with_timeout(child, req.timeout, req.binary).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(anyhow!(
                "CLI provider '{}' exited with status {}.{}{}",
                req.binary,
                output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                if stdout.trim().is_empty() {
                    "".to_string()
                } else {
                    format!("\nstdout: {}", stdout.trim())
                },
                if stderr.trim().is_empty() {
                    "".to_string()
                } else {
                    format!("\nstderr: {}", stderr.trim())
                }
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_claude_response(&stdout)
    }

    async fn run_claude_streaming<F>(
        req: CliCompletionRequest<'_>,
        mut on_token: F,
    ) -> Result<String>
    where
        F: FnMut(&str) + Send,
    {
        let mut cmd = build_claude_command(&req, true);

        if req.debug {
            eprintln!(
                "[debug] Running CLI provider '{}' with args: {:?}",
                req.binary, cmd
            );
        }

        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn CLI provider '{}'. Is it installed and on your PATH?",
                    req.binary
                )
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("CLI provider '{}' did not expose stdout", req.binary))?;
        let mut stderr = child.stderr.take().map(|stderr| {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf).await.ok();
                String::from_utf8_lossy(&buf).trim().to_string()
            })
        });

        let mut reader = BufReader::new(stdout).lines();
        let mut aggregated = String::new();
        let mut fallback = None;
        let deadline = tokio::time::sleep(req.timeout);
        tokio::pin!(deadline);

        loop {
            let next = tokio::select! {
                line = reader.next_line() => line?,
                _ = &mut deadline => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    let _ = take_stderr_output(&mut stderr).await;
                    return Err(timeout_error(req.binary, req.timeout));
                }
            };

            match next {
                Some(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match parse_claude_stream_line(trimmed) {
                        Ok((token, result_text)) => {
                            if let Some(t) = token {
                                if !t.is_empty() {
                                    aggregated.push_str(&t);
                                    on_token(&t);
                                }
                            }
                            if let Some(res) = result_text {
                                if !res.trim().is_empty() {
                                    fallback = Some(res);
                                }
                            }
                        }
                        Err(e) => {
                            if req.debug {
                                eprintln!(
                                    "[debug] Failed to parse Claude stream line '{}': {}",
                                    trimmed, e
                                );
                            }
                        }
                    }
                }
                None => break,
            }
        }

        let status = tokio::select! {
            status = child.wait() => status?,
            _ = &mut deadline => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                let _ = take_stderr_output(&mut stderr).await;
                return Err(timeout_error(req.binary, req.timeout));
            }
        };
        let stderr = take_stderr_output(&mut stderr).await;

        if !status.success() {
            return Err(anyhow!(
                "CLI provider '{}' exited with status {}.{}",
                req.binary,
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!("\nstderr: {}", stderr)
                }
            ));
        }

        if aggregated.is_empty() {
            if let Some(res) = fallback {
                return Ok(res);
            }
            return Err(anyhow!("CLI provider returned no stream tokens."));
        }

        Ok(aggregated)
    }

    fn build_claude_command(req: &CliCompletionRequest<'_>, streaming: bool) -> Command {
        let mut cmd = Command::new(req.binary);
        if !req.base_args.is_empty() {
            cmd.args(req.base_args.iter().filter(|arg| !arg.trim().is_empty()));
        }
        cmd.arg("-p");
        if streaming {
            cmd.arg("--verbose");
            cmd.arg("--output-format");
            cmd.arg("stream-json");
            cmd.arg("--include-partial-messages");
        } else {
            cmd.arg("--output-format");
            cmd.arg("json");
        }
        if !req.model.trim().is_empty() {
            cmd.arg("--model");
            cmd.arg(req.model);
        }
        cmd.arg("--append-system-prompt");
        cmd.arg(tagged_system_prompt(req.system_prompt));
        cmd.arg("--disallowed-tools");
        cmd.arg("Bash(*) Edit");
        cmd.arg("--");
        cmd.arg(tagged_user_prompt(req.user_prompt));
        cmd
    }

    async fn wait_child_output_with_timeout(
        mut child: tokio::process::Child,
        timeout: Duration,
        binary: &str,
    ) -> Result<std::process::Output> {
        const POLL_INTERVAL_MS: u64 = 25;

        let stdout_task = child.stdout.take().map(spawn_output_reader);
        let stderr_task = child.stderr.take().map(spawn_output_reader);

        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);

        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            tokio::select! {
                _ = &mut deadline => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Err(timeout_error(binary, timeout));
                }
                _ = tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)) => {}
            }
        };

        let stdout = match stdout_task {
            Some(handle) => handle.await??,
            None => Vec::new(),
        };
        let stderr = match stderr_task {
            Some(handle) => handle.await??,
            None => Vec::new(),
        };

        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }

    fn spawn_output_reader<R>(stream: R) -> tokio::task::JoinHandle<Result<Vec<u8>>>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await?;
            Ok(buf)
        })
    }

    async fn take_stderr_output(stderr: &mut Option<tokio::task::JoinHandle<String>>) -> String {
        if let Some(handle) = stderr.take() {
            match handle.await {
                Ok(text) => text,
                Err(_) => String::new(),
            }
        } else {
            String::new()
        }
    }

    fn timeout_error(binary: &str, timeout: Duration) -> anyhow::Error {
        anyhow!(
            "CLI provider '{}' timed out after {}s",
            binary,
            timeout.as_secs()
        )
    }

    #[derive(Debug, Deserialize)]
    struct CodexEvent {
        #[serde(rename = "type")]
        event_type: String,
        #[serde(default)]
        item: Option<CodexItem>,
    }

    #[derive(Debug, Deserialize)]
    struct CodexItem {
        #[serde(rename = "type")]
        item_type: String,
        #[serde(default)]
        text: Option<String>,
    }

    fn parse_codex_response(stdout: &str) -> Result<String> {
        let mut messages = Vec::new();
        for line in stdout.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<CodexEvent>(trimmed) {
                Ok(event) => {
                    if event.event_type == "item.completed" {
                        if let Some(item) = event.item {
                            if item.item_type == "agent_message" {
                                if let Some(text) = item.text {
                                    if !text.trim().is_empty() {
                                        messages.push(text);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    continue;
                }
            }
        }
        if messages.is_empty() {
            return Err(anyhow!("CLI provider returned no agent_message text."));
        }
        Ok(messages.join("\n\n"))
    }

    fn parse_claude_response(stdout: &str) -> Result<String> {
        let value = parse_single_json_value(stdout)
            .with_context(|| "CLI provider returned non-JSON output")?;

        if let Some(result_field) = value.get("result") {
            if let Some(text) = extract_text(result_field) {
                return Ok(text);
            }
        }

        if let Some(text) = extract_text(&value) {
            return Ok(text);
        }

        Err(anyhow!(
            "CLI provider returned JSON without a usable message."
        ))
    }

    #[derive(Debug, Deserialize)]
    struct ClaudeStreamEnvelope {
        #[serde(rename = "type")]
        event_type: String,
        #[serde(default)]
        event: Option<ClaudeStreamEvent>,
        #[serde(default)]
        result: Option<Value>,
    }

    #[derive(Debug, Deserialize)]
    struct ClaudeStreamEvent {
        #[serde(rename = "type")]
        event_type: String,
        #[serde(default)]
        delta: Option<ClaudeStreamDelta>,
    }

    #[derive(Debug, Deserialize)]
    struct ClaudeStreamDelta {
        #[serde(default)]
        text: Option<String>,
    }

    fn parse_claude_stream_line(
        line: &str,
    ) -> Result<(Option<String>, Option<String>), serde_json::Error> {
        let env: ClaudeStreamEnvelope = serde_json::from_str(line)?;
        let mut token = None;
        let mut result_text = None;

        if env.event_type == "stream_event" {
            if let Some(event) = env.event {
                if event.event_type == "content_block_delta" {
                    if let Some(delta) = event.delta {
                        if let Some(text) = delta.text {
                            if !text.is_empty() {
                                token = Some(text);
                            }
                        }
                    }
                }
            }
        } else if env.event_type == "result" {
            if let Some(result) = env.result {
                if let Some(text) = extract_text(&result) {
                    result_text = Some(text);
                }
            }
        }

        Ok((token, result_text))
    }

    fn parse_single_json_value(stdout: &str) -> Result<Value> {
        let trimmed = stdout.trim();
        if !trimmed.is_empty() {
            if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                return Ok(value);
            }
        }

        for line in stdout.lines().rev() {
            let candidate = line.trim();
            if candidate.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(candidate) {
                return Ok(value);
            }
        }

        Err(anyhow!("Unable to parse CLI JSON output"))
    }

    fn extract_text(value: &Value) -> Option<String> {
        match value {
            Value::String(s) => {
                if s.trim().is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            }
            Value::Array(items) => {
                let mut parts = Vec::new();
                for item in items {
                    if let Some(text) = extract_text(item) {
                        parts.push(text);
                    }
                }
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("\n\n"))
                }
            }
            Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(|t| t.as_str()) {
                    if !text.trim().is_empty() {
                        return Some(text.to_string());
                    }
                }
                for key in ["content", "messages", "output_text", "result"] {
                    if let Some(val) = map.get(key) {
                        if let Some(text) = extract_text(val) {
                            return Some(text);
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn tagged_system_prompt(prompt: &str) -> String {
        format!("<system-prompt>\n{}\n</system-prompt>", prompt)
    }

    fn tagged_user_prompt(prompt: &str) -> String {
        format!("<user-prompt>\n{}\n</user-prompt>", prompt)
    }

    #[cfg(test)]
    pub(super) fn parse_codex_response_for_test(input: &str) -> Result<String> {
        parse_codex_response(input)
    }

    #[cfg(test)]
    pub(super) fn parse_claude_response_for_test(input: &str) -> Result<String> {
        parse_claude_response(input)
    }

    #[cfg(test)]
    pub(super) fn parse_claude_stream_line_for_test(
        line: &str,
    ) -> Result<(Option<String>, Option<String>), serde_json::Error> {
        parse_claude_stream_line(line)
    }
}

pub use cli_backend::{CliCompletionRequest, run_cli_completion, run_cli_completion_streaming};

#[cfg(test)]
mod tests {
    use super::cli_backend::{
        parse_claude_response_for_test, parse_claude_stream_line_for_test,
        parse_codex_response_for_test,
    };
    use super::load_root_certificates;
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn format_api_error_adds_openrouter_hint_for_402() {
        let err = super::format_api_error(
            reqwest::StatusCode::PAYMENT_REQUIRED,
            r#"{"error":{"message":"Insufficient credits"}}"#,
            "https://openrouter.ai/api/v1",
        );
        let msg = err.to_string();
        assert!(msg.contains(":free"));
        assert!(msg.contains("openrouter.ai/settings/credits"));
    }

    #[test]
    fn format_api_error_omits_openrouter_hint_for_other_providers() {
        let err = super::format_api_error(
            reqwest::StatusCode::PAYMENT_REQUIRED,
            r#"{"error":{"message":"Insufficient credits"}}"#,
            "https://api.openai.com/v1",
        );
        assert_eq!(
            err.to_string(),
            r#"API error (402 Payment Required): {"error":{"message":"Insufficient credits"}}"#
        );
    }

    #[test]
    fn load_root_certificates_supports_multiple_pem_entries() {
        let bundle_dir = tempdir().unwrap();
        let CertifiedKey { cert: cert_a, .. } =
            generate_simple_self_signed(["a.lan".into()]).unwrap();
        let CertifiedKey { cert: cert_b, .. } =
            generate_simple_self_signed(["b.lan".into()]).unwrap();
        let pem = format!("{}\n{}", cert_a.pem(), cert_b.pem());
        let path = bundle_dir.path().join("bundle.pem");
        fs::write(&path, pem).unwrap();

        let certs = load_root_certificates(&path).expect("certs load");
        assert_eq!(certs.len(), 2);
    }

    #[test]
    fn load_root_certificates_handles_der_files() {
        let dir = tempdir().unwrap();
        let CertifiedKey { cert, .. } = generate_simple_self_signed(["der.test".into()]).unwrap();
        let der = cert.der().as_ref().to_vec();
        let path = dir.path().join("bundle.der");
        fs::write(&path, der).unwrap();

        let certs = load_root_certificates(&path).expect("certs load");
        assert_eq!(certs.len(), 1);
    }

    #[test]
    fn codex_parser_returns_last_agent_message() {
        let payload = r#"{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"Reasoning"}}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"First"}}
{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"Second"}}"#;
        let merged = payload.replace('\n', "\n");
        let parsed = parse_codex_response_for_test(&merged).expect("parse");
        assert_eq!(parsed, "First\n\nSecond");
    }

    #[test]
    fn claude_parser_reads_result_string() {
        let payload = r#"{"type":"result","subtype":"success","result":"<cmd>echo hi</cmd>"}"#;
        let parsed = parse_claude_response_for_test(payload).expect("parse");
        assert_eq!(parsed, "<cmd>echo hi</cmd>");
    }

    #[test]
    fn claude_parser_walks_nested_messages() {
        let payload = r#"{"type":"result","subtype":"success","result":{"messages":[{"content":[{"type":"text","text":"Hello"},{"type":"text","text":"World"}]}]}}"#;
        let parsed = parse_claude_response_for_test(payload).expect("parse");
        assert_eq!(parsed, "Hello\n\nWorld");
    }

    #[test]
    fn claude_stream_parser_emits_deltas() {
        let payload = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"<cmd>echo hi</cmd>"}}}"#;
        let (token, result) = parse_claude_stream_line_for_test(payload).expect("parse");
        assert_eq!(token.as_deref(), Some("<cmd>echo hi</cmd>"));
        assert!(result.is_none());
    }

    #[test]
    fn claude_stream_parser_handles_result_objects() {
        let payload = r#"{"type":"result","result":{"messages":[{"content":[{"type":"text","text":"All done"}]}]}}"#;
        let (token, result) = parse_claude_stream_line_for_test(payload).expect("parse");
        assert!(token.is_none());
        assert_eq!(result.as_deref(), Some("All done"));
    }
}
