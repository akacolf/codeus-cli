use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChatToolCall>>,
}

impl Message {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ChatToolCall>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: String::new(),
            tool_call_id: None,
            tool_calls: Some(tool_calls),
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ChatCompletion {
    pub content: String,
    pub tool_calls: Vec<ChatToolCall>,
    pub usage: Option<UsageStats>,
}

#[derive(Debug, Clone, Default)]
pub struct UsageStats {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub api_base: String,
    pub model: String,
    pub token: Option<String>,
}

#[derive(Clone)]
pub struct CodeusClient {
    http: reqwest::Client,
    config: ClientConfig,
}

#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: Option<String>,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TokenSet {
    pub session_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum DeviceTokenStatus {
    Authorized(TokenSet),
    Pending,
    SlowDown,
    Denied(String),
    Expired,
}

impl CodeusClient {
    pub fn new(config: ClientConfig) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if let Some(token) = config
            .token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
        {
            let value = format!("Bearer {}", token.trim());
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&value).context("invalid session token header")?,
            );
        }

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self { http, config })
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    pub async fn stream_chat<F>(&self, messages: &[Message], on_delta: F) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        Ok(self
            .stream_chat_with_tools(messages, false, on_delta)
            .await?
            .content)
    }

    pub async fn stream_chat_completion<F>(
        &self,
        messages: &[Message],
        mut on_delta: F,
    ) -> Result<ChatCompletion>
    where
        F: FnMut(&str) -> Result<()>,
    {
        self.stream_chat_with_tools(messages, true, &mut on_delta)
            .await
    }

    async fn stream_chat_with_tools<F>(
        &self,
        messages: &[Message],
        include_tools: bool,
        mut on_delta: F,
    ) -> Result<ChatCompletion>
    where
        F: FnMut(&str) -> Result<()>,
    {
        let url = format!(
            "{}/chat/completions",
            self.config.api_base.trim_end_matches('/')
        );

        let mut body = json!({
            "model": normalize_model(&self.config.model),
            "stream": true,
            "messages": messages,
        });
        if include_tools {
            body["tools"] = local_tool_schemas();
            body["tool_choice"] = json!("auto");
        }

        let response = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .context("request failed")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!("proxy returned {status}: {}", text.trim()));
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::with_capacity(8192);
        let mut completion = StreamingCompletion::default();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("failed to read response stream")?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some((split, width)) = find_sse_boundary(&buffer) {
                let event: String = buffer.drain(..split).collect();
                buffer.drain(..width);
                if consume_sse_event(&event, &mut completion, &mut on_delta)? {
                    return Ok(completion.finish());
                }
            }
        }

        if !buffer.trim().is_empty() {
            let _ = consume_sse_event(&buffer, &mut completion, &mut on_delta)?;
        }

        Ok(completion.finish())
    }

    pub async fn begin_device_login(&self) -> Result<DeviceAuthorization> {
        let body = json!({
            "client_id": "codeus-cli",
            "client_name": "Codeus CLI",
            "scope": "chat account usage",
            "accountId": null,
        });

        let mut last_404 = String::new();
        for path in ["device/link/init", "auth/device/code"] {
            let url = self.url(path);
            let response = self
                .http
                .post(url)
                .json(&body)
                .send()
                .await
                .context("failed to start browser login")?;

            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if status == StatusCode::NOT_FOUND {
                last_404 = text.trim().to_string();
                continue;
            }
            if !status.is_success() {
                return Err(anyhow!(
                    "browser login is not available from the proxy ({status}): {}",
                    text.trim()
                ));
            }

            let value: Value =
                serde_json::from_str(&text).context("failed to parse browser login response")?;
            let mut auth = parse_device_authorization(value)?;
            auth.verification_uri = with_cli_client_hint(&auth.verification_uri);
            auth.verification_uri_complete = auth
                .verification_uri_complete
                .map(|url| with_cli_client_hint(&url));
            return Ok(auth);
        }

        Err(anyhow!(
            "browser login route returned 404. The QC website exposes /device/link/init and /device/link/poll through /v1 rewrites; verify the CLI is pointed at https://api.quantumclub.sh/v1 or deploy those routes on the configured API host: {last_404}"
        ))
    }

    pub async fn poll_device_token(&self, device_code: &str) -> Result<DeviceTokenStatus> {
        let body = json!({
            "client_id": "codeus-cli",
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "deviceCode": device_code,
            "device_code": device_code,
        });

        let mut last_not_found = String::new();
        for path in ["device/link/poll", "auth/device/token"] {
            let response = self
                .http
                .post(self.url(path))
                .json(&body)
                .send()
                .await
                .context("failed to poll browser login")?;

            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            let value: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));

            if status.is_success() {
                if value.get("status").and_then(Value::as_str) == Some("pending") {
                    return Ok(DeviceTokenStatus::Pending);
                }
                let token = parse_token_set(value).with_context(|| {
                    "browser approval completed, but the QC API did not return a CLI session token"
                })?;
                return Ok(DeviceTokenStatus::Authorized(token));
            }

            if status == StatusCode::ACCEPTED {
                return Ok(DeviceTokenStatus::Pending);
            }
            if status == StatusCode::GONE {
                return match value.get("status").and_then(Value::as_str) {
                    Some("denied") => Ok(DeviceTokenStatus::Denied(
                        "browser login was denied".to_string(),
                    )),
                    _ => Ok(DeviceTokenStatus::Expired),
                };
            }
            if status == StatusCode::NOT_FOUND {
                last_not_found = text.trim().to_string();
                continue;
            }

            return parse_device_token_error(status, &text, value);
        }

        Err(anyhow!("browser login failed (404): {last_not_found}"))
    }

    pub async fn refresh_session(&self, refresh_token: &str) -> Result<TokenSet> {
        let url = format!(
            "{}/auth/refresh",
            self.config.api_base.trim_end_matches('/')
        );

        let response = self
            .http
            .post(url)
            .json(&json!({
                "client_id": "codeus-cli",
                "refresh_token": refresh_token,
            }))
            .send()
            .await
            .context("failed to refresh Codeus session")?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "session refresh failed ({status}): {}",
                text.trim()
            ));
        }

        let value: Value =
            serde_json::from_str(&text).context("failed to parse session refresh response")?;
        parse_token_set(value)
    }

    pub async fn account(&self) -> Result<Value> {
        self.get_json_from_candidates(&["auth/whoami", "account", "me", "auth/session"])
            .await
    }

    pub async fn usage(&self) -> Result<Value> {
        self.get_json_from_candidates(&["balance", "usage", "account/usage"])
            .await
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.config.api_base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    async fn get_json_from_candidates(&self, paths: &[&str]) -> Result<Value> {
        let mut last_error = None;
        for path in paths {
            match self.get_json(path).await {
                Ok(value) => return Ok(value),
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("no API paths configured")))
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let response = self
            .http
            .get(self.url(path))
            .send()
            .await
            .context("request failed")?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let detail = text.trim();
            if status == StatusCode::UNAUTHORIZED {
                bail!("not signed in; run `codeus` to connect your Quantum Club account");
            }
            bail!(
                "proxy returned {status}: {}",
                if detail.is_empty() { path } else { detail }
            );
        }

        serde_json::from_str(&text).with_context(|| format!("failed to parse {path} response"))
    }
}

fn parse_device_token_error(
    status: StatusCode,
    text: &str,
    value: Value,
) -> Result<DeviceTokenStatus> {
    let error = value
        .get("error")
        .or_else(|| value.get("code"))
        .or_else(|| value.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown_error");
    let description = value
        .get("error_description")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(error)
        .to_string();

    match error {
        "authorization_pending" | "pending" => Ok(DeviceTokenStatus::Pending),
        "slow_down" => Ok(DeviceTokenStatus::SlowDown),
        "access_denied" | "denied" => Ok(DeviceTokenStatus::Denied(description)),
        "expired_token" | "expired" => Ok(DeviceTokenStatus::Expired),
        _ => Err(anyhow!(
            "browser login failed ({status}): {}",
            if text.trim().is_empty() {
                description
            } else {
                text.trim().to_string()
            }
        )),
    }
}

fn parse_device_authorization(value: Value) -> Result<DeviceAuthorization> {
    let device_code = string_field(&value, &["device_code", "deviceCode"])
        .context("browser login response did not include a device code")?;
    let verification_uri = string_field(
        &value,
        &[
            "verification_uri",
            "verification_url",
            "verificationUrl",
            "linkUrl",
            "url",
        ],
    )
    .context("browser login response did not include a verification URL")?;

    Ok(DeviceAuthorization {
        device_code,
        user_code: string_field(&value, &["user_code", "userCode"]),
        verification_uri,
        verification_uri_complete: string_field(
            &value,
            &[
                "verification_uri_complete",
                "verification_url_complete",
                "verificationUrlComplete",
                "complete_url",
                "linkUrl",
            ],
        ),
        expires_in: expires_in_field(&value).unwrap_or(600),
        interval: u64_field(&value, &["interval"]).unwrap_or(5).max(1),
        message: string_field(&value, &["message"]),
    })
}

fn parse_token_set(value: Value) -> Result<TokenSet> {
    let session_token = string_field(
        &value,
        &["session_token", "sessionToken", "access_token", "token"],
    )
    .context("browser login response did not include a session token")?;

    Ok(TokenSet {
        session_token,
        refresh_token: string_field(&value, &["refresh_token", "refreshToken"]),
        expires_in: expires_in_field(&value),
    })
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn u64_field(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
}

fn expires_in_field(value: &Value) -> Option<u64> {
    if let Some(expires_in) = u64_field(value, &["expires_in", "expiresIn"]) {
        return Some(expires_in);
    }

    let expires_at = u64_field(value, &["expires_at", "expiresAt"])?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    if expires_at > 10_000_000_000 {
        return Some(
            expires_at
                .saturating_sub(now_ms)
                .saturating_div(1000)
                .max(1),
        );
    }

    let now_secs = now_ms / 1000;
    Some(expires_at.saturating_sub(now_secs).max(1))
}

fn with_cli_client_hint(url: &str) -> String {
    if url.contains("client=cli") {
        return url.to_string();
    }

    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}client=cli")
}

fn find_sse_boundary(buffer: &str) -> Option<(usize, usize)> {
    match (buffer.find("\r\n\r\n"), buffer.find("\n\n")) {
        (Some(crlf), Some(lf)) if crlf < lf => Some((crlf, 4)),
        (Some(_), Some(lf)) => Some((lf, 2)),
        (Some(crlf), None) => Some((crlf, 4)),
        (None, Some(lf)) => Some((lf, 2)),
        (None, None) => None,
    }
}

#[derive(Debug, Default)]
struct StreamingCompletion {
    content: String,
    tool_calls: Vec<StreamingToolCall>,
    usage: Option<UsageStats>,
}

#[derive(Debug, Default, Clone)]
struct StreamingToolCall {
    id: String,
    kind: String,
    name: String,
    arguments: String,
}

impl StreamingCompletion {
    fn finish(self) -> ChatCompletion {
        ChatCompletion {
            content: self.content,
            tool_calls: self
                .tool_calls
                .into_iter()
                .filter(|call| !call.name.is_empty())
                .map(|call| ChatToolCall {
                    id: call.id,
                    kind: if call.kind.is_empty() {
                        "function".to_string()
                    } else {
                        call.kind
                    },
                    function: ChatToolFunction {
                        name: call.name,
                        arguments: call.arguments,
                    },
                })
                .collect(),
            usage: self.usage,
        }
    }
}

fn consume_sse_event<F>(
    event: &str,
    completion: &mut StreamingCompletion,
    on_delta: &mut F,
) -> Result<bool>
where
    F: FnMut(&str) -> Result<()>,
{
    for line in event.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            return Ok(true);
        }
        if data.is_empty() {
            continue;
        }

        let parsed: serde_json::Value =
            serde_json::from_str(data).context("failed to parse stream event")?;
        if let Some(delta) = parsed
            .pointer("/choices/0/delta/content")
            .and_then(|value| value.as_str())
        {
            completion.content.push_str(delta);
            on_delta(delta)?;
        }
        if let Some(tool_calls) = parsed
            .pointer("/choices/0/delta/tool_calls")
            .and_then(|value| value.as_array())
        {
            for tool_call in tool_calls {
                merge_streaming_tool_call(completion, tool_call);
            }
        }

        // Parse usage stats if present
        if let Some(usage) = parsed.get("usage") {
            completion.usage = Some(UsageStats {
                prompt_tokens: usage.get("prompt_tokens").and_then(|v| v.as_u64()),
                completion_tokens: usage.get("completion_tokens").and_then(|v| v.as_u64()),
                total_tokens: usage.get("total_tokens").and_then(|v| v.as_u64()),
            });
        }
    }

    Ok(false)
}

fn merge_streaming_tool_call(completion: &mut StreamingCompletion, value: &Value) {
    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    while completion.tool_calls.len() <= index {
        completion.tool_calls.push(StreamingToolCall::default());
    }

    let target = &mut completion.tool_calls[index];
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        target.id.push_str(id);
    }
    if let Some(kind) = value.get("type").and_then(Value::as_str) {
        target.kind.push_str(kind);
    }
    if let Some(name) = value.pointer("/function/name").and_then(Value::as_str) {
        target.name.push_str(name);
    }
    if let Some(arguments) = value.pointer("/function/arguments").and_then(Value::as_str) {
        target.arguments.push_str(arguments);
    }
}

fn local_tool_schemas() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "list_files",
                "description": "List files under the active workspace, skipping common generated directories.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a UTF-8-ish text file under the active workspace.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search_text",
                "description": "Search for literal text under the active workspace.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "replace_in_file",
                "description": "Replace one exact text snippet in a workspace file. The old_text must occur exactly once.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_text": { "type": "string" },
                        "new_text": { "type": "string" }
                    },
                    "required": ["path", "old_text", "new_text"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "apply_patch",
                "description": "Apply a structured workspace patch. Validate all operations before writing. Operations are add, delete, or replace.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "operations": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "op": { "type": "string", "enum": ["add", "delete", "replace"] },
                                    "path": { "type": "string" },
                                    "content": { "type": "string" },
                                    "old_text": { "type": "string" },
                                    "new_text": { "type": "string" }
                                },
                                "required": ["op", "path"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["operations"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "git_status",
                "description": "Inspect git status for the active workspace.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "git_diff",
                "description": "Inspect git diff stat for the active workspace.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Run a local command only if it matches an approved argv prefix.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        }
    ])
}

fn normalize_model(model: &str) -> &str {
    match model {
        "sonnet-4.6" => "anthropic/claude-sonnet-4.6",
        "haiku-4.5" => "anthropic/claude-haiku-4.5",
        "gemini-3-flash" => "google/gemini-3-flash",
        "gpt-5-mini" => "openai/gpt-5-mini",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_authorization_aliases() {
        let auth = parse_device_authorization(json!({
            "deviceCode": "device-123",
            "userCode": "ABCD-EFGH",
            "verificationUrl": "https://quantumclub.sh/device",
            "verificationUrlComplete": "https://quantumclub.sh/device?code=ABCD-EFGH",
            "expiresIn": "900",
            "interval": 3
        }))
        .expect("device authorization should parse");

        assert_eq!(auth.device_code, "device-123");
        assert_eq!(auth.user_code.as_deref(), Some("ABCD-EFGH"));
        assert_eq!(
            auth.verification_uri_complete.as_deref(),
            Some("https://quantumclub.sh/device?code=ABCD-EFGH")
        );
        assert_eq!(auth.expires_in, 900);
        assert_eq!(auth.interval, 3);
    }

    #[test]
    fn parses_website_device_link_response() {
        let auth = parse_device_authorization(json!({
            "deviceCode": "device-123",
            "userCode": "ABCD2345",
            "linkUrl": "https://quantumclub.sh/account/link?code=ABCD2345",
            "pollUrl": "https://quantumclub.sh/api/codeus/v1/device/link/poll",
            "expiresAt": 4_102_444_800_000_u64,
            "interval": 2
        }))
        .expect("website device link response should parse");

        assert_eq!(auth.device_code, "device-123");
        assert_eq!(auth.user_code.as_deref(), Some("ABCD2345"));
        assert_eq!(
            auth.verification_uri,
            "https://quantumclub.sh/account/link?code=ABCD2345"
        );
        assert_eq!(auth.interval, 2);
        assert!(auth.expires_in > 0);
    }

    #[test]
    fn appends_cli_client_hint_to_link_url() {
        assert_eq!(
            with_cli_client_hint("https://quantumclub.sh/account/link?code=ABCD2345"),
            "https://quantumclub.sh/account/link?code=ABCD2345&client=cli"
        );
        assert_eq!(
            with_cli_client_hint("https://quantumclub.sh/account/link?code=ABCD2345&client=cli"),
            "https://quantumclub.sh/account/link?code=ABCD2345&client=cli"
        );
    }

    #[test]
    fn parses_token_set_aliases() {
        let token = parse_token_set(json!({
            "access_token": "session-123",
            "refreshToken": "refresh-123",
            "expiresIn": 3600
        }))
        .expect("token set should parse");

        assert_eq!(token.session_token, "session-123");
        assert_eq!(token.refresh_token.as_deref(), Some("refresh-123"));
        assert_eq!(token.expires_in, Some(3600));
    }

    #[test]
    fn parses_website_session_token_response() {
        let token = parse_token_set(json!({
            "status": "approved",
            "sessionToken": "session-123",
            "expiresAt": 4_102_444_800_000_u64
        }))
        .expect("website session response should parse");

        assert_eq!(token.session_token, "session-123");
        assert!(token.expires_in.is_some());
    }

    #[test]
    fn parses_streamed_tool_call_deltas() {
        let mut completion = StreamingCompletion::default();
        let mut deltas = String::new();
        let event = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\""}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"README.md\"}"}}]}}]}

data: [DONE]"#;

        let done = consume_sse_event(event, &mut completion, &mut |delta| {
            deltas.push_str(delta);
            Ok(())
        })
        .unwrap();
        let finished = completion.finish();

        assert!(done);
        assert!(deltas.is_empty());
        assert_eq!(finished.tool_calls.len(), 1);
        assert_eq!(finished.tool_calls[0].id, "call_1");
        assert_eq!(finished.tool_calls[0].function.name, "read_file");
        assert_eq!(
            finished.tool_calls[0].function.arguments,
            r#"{"path":"README.md"}"#
        );
    }
}
