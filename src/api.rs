use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
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
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self { http, config })
    }

    pub async fn stream_chat<F>(&self, messages: &[Message], mut on_delta: F) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        let url = format!(
            "{}/chat/completions",
            self.config.api_base.trim_end_matches('/')
        );

        let body = json!({
            "model": normalize_model(&self.config.model),
            "stream": true,
            "messages": messages,
        });

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
        let mut assistant = String::with_capacity(4096);

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("failed to read response stream")?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some((split, width)) = find_sse_boundary(&buffer) {
                let event: String = buffer.drain(..split).collect();
                buffer.drain(..width);
                if consume_sse_event(&event, &mut assistant, &mut on_delta)? {
                    return Ok(assistant);
                }
            }
        }

        if !buffer.trim().is_empty() {
            let _ = consume_sse_event(&buffer, &mut assistant, &mut on_delta)?;
        }

        Ok(assistant)
    }

    pub async fn begin_device_login(&self) -> Result<DeviceAuthorization> {
        let url = format!(
            "{}/auth/device/code",
            self.config.api_base.trim_end_matches('/')
        );

        let response = self
            .http
            .post(url)
            .json(&json!({
                "client_id": "codeus-cli",
                "client_name": "Codeus CLI",
                "scope": "chat account usage",
            }))
            .send()
            .await
            .context("failed to start browser login")?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "browser login is not available from the proxy ({status}): {}",
                text.trim()
            ));
        }

        let value: Value =
            serde_json::from_str(&text).context("failed to parse browser login response")?;
        parse_device_authorization(value)
    }

    pub async fn poll_device_token(&self, device_code: &str) -> Result<DeviceTokenStatus> {
        let url = format!(
            "{}/auth/device/token",
            self.config.api_base.trim_end_matches('/')
        );

        let response = self
            .http
            .post(url)
            .json(&json!({
                "client_id": "codeus-cli",
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                "device_code": device_code,
            }))
            .send()
            .await
            .context("failed to poll browser login")?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));

        if status.is_success() {
            return Ok(DeviceTokenStatus::Authorized(parse_token_set(value)?));
        }

        let error = value
            .get("error")
            .or_else(|| value.get("code"))
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
        self.get_json_from_candidates(&["account", "me", "auth/session"])
            .await
    }

    pub async fn usage(&self) -> Result<Value> {
        self.get_json_from_candidates(&["usage", "account/usage"])
            .await
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
        let url = format!(
            "{}/{}",
            self.config.api_base.trim_end_matches('/'),
            path.trim_start_matches('/')
        );

        let response = self.http.get(url).send().await.context("request failed")?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let detail = text.trim();
            if status == StatusCode::UNAUTHORIZED {
                bail!("not signed in; run `codeus login`");
            }
            bail!(
                "proxy returned {status}: {}",
                if detail.is_empty() { path } else { detail }
            );
        }

        serde_json::from_str(&text).with_context(|| format!("failed to parse {path} response"))
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
            ],
        ),
        expires_in: u64_field(&value, &["expires_in", "expiresIn"]).unwrap_or(600),
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
        expires_in: u64_field(&value, &["expires_in", "expiresIn"]),
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

fn find_sse_boundary(buffer: &str) -> Option<(usize, usize)> {
    match (buffer.find("\r\n\r\n"), buffer.find("\n\n")) {
        (Some(crlf), Some(lf)) if crlf < lf => Some((crlf, 4)),
        (Some(_), Some(lf)) => Some((lf, 2)),
        (Some(crlf), None) => Some((crlf, 4)),
        (None, Some(lf)) => Some((lf, 2)),
        (None, None) => None,
    }
}

fn consume_sse_event<F>(event: &str, assistant: &mut String, on_delta: &mut F) -> Result<bool>
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
            assistant.push_str(delta);
            on_delta(delta)?;
        }
    }

    Ok(false)
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
}
