use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::json;

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
        let mut buffer = String::new();
        let mut assistant = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("failed to read response stream")?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some((split, width)) = find_sse_boundary(&buffer) {
                let event = buffer[..split].to_string();
                buffer = buffer[(split + width)..].to_string();
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
