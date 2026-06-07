use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::{header, Response, StatusCode};
use serde_json::{json, Map, Value};
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

const BODY_LIMIT: usize = 4 * 1024 * 1024;
const DEVICE_CODE_PATHS: &[&str] = &["device/link/init", "oauth/device/code", "device/code"];
const DEVICE_TOKEN_PATHS: &[&str] = &["device/link/poll", "oauth/token", "device/token"];
const REFRESH_PATHS: &[&str] = &["oauth/token", "refresh"];

#[derive(Debug)]
struct ProxyError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ProxyError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

struct UpstreamText {
    status: reqwest::StatusCode,
    headers: reqwest::header::HeaderMap,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatAuth {
    ServerToken(String),
    UserBearer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatUpstream {
    name: &'static str,
    url: String,
    auth: ChatAuth,
}

pub async fn chat_completions(request: Request) -> Response<Body> {
    respond(chat_completions_inner(request).await)
}

pub async fn device_code(request: Request) -> Response<Body> {
    respond(device_code_inner(request).await)
}

pub async fn device_token(request: Request) -> Response<Body> {
    respond(device_token_inner(request).await)
}

pub async fn refresh(request: Request) -> Response<Body> {
    respond(refresh_inner(request).await)
}

pub async fn account(request: Request) -> Response<Body> {
    respond(account_inner(request).await)
}

pub async fn usage(request: Request) -> Response<Body> {
    respond(usage_inner(request).await)
}

pub async fn fallback() -> Response<Body> {
    json_response(
        StatusCode::NOT_FOUND,
        json!({
            "error": "not_found",
            "error_description": "No Codeus API route matched this request.",
        }),
    )
}

async fn chat_completions_inner(request: Request) -> Result<Response<Body>, ProxyError> {
    let user_token = bearer_token(&request);
    let body = read_json(request).await?;
    let upstreams = chat_upstream_candidates(user_token.is_some())?;
    let response = post_chat_with_fallback(upstreams, user_token, body).await?;

    Ok(proxy_response(response))
}

async fn device_code_inner(request: Request) -> Result<Response<Body>, ProxyError> {
    let auth_urls = configured_url_candidates(
        "CODEUS_DEVICE_CODE_URL",
        "CODEUS_AUTH_BASE",
        DEVICE_CODE_PATHS,
    )?;
    let mut payload = object_body(read_json(request).await?);
    payload.insert(
        "client_id".to_string(),
        Value::String(
            optional_env("CODEUS_OAUTH_CLIENT_ID").unwrap_or_else(|| "codeus-cli".into()),
        ),
    );
    payload.insert(
        "scope".to_string(),
        Value::String(
            optional_env("CODEUS_OAUTH_SCOPE").unwrap_or_else(|| "chat account usage".into()),
        ),
    );

    let response = post_json_with_404_fallback(auth_urls, Value::Object(payload)).await?;
    let status = response.status;
    let headers = response.headers;
    let text = response.text;
    if !status.is_success() {
        return Ok(text_response(status_code(status), headers, text));
    }

    let upstream = parse_upstream_json(&text)?;
    if upstream.get("deviceCode").and_then(Value::as_str).is_some() {
        return Ok(json_response(
            StatusCode::OK,
            json!({
                "device_code": upstream.get("deviceCode"),
                "user_code": upstream.get("userCode"),
                "verification_uri": upstream.get("linkUrl"),
                "verification_uri_complete": upstream.get("linkUrl"),
                "expires_in": expires_in(upstream.get("expiresAt")),
                "interval": upstream.get("interval"),
            }),
        ));
    }

    Ok(json_response(StatusCode::OK, upstream))
}

async fn device_token_inner(request: Request) -> Result<Response<Body>, ProxyError> {
    let auth_urls = configured_url_candidates(
        "CODEUS_DEVICE_TOKEN_URL",
        "CODEUS_AUTH_BASE",
        DEVICE_TOKEN_PATHS,
    )?;
    let mut payload = object_body(read_json(request).await?);
    payload.insert(
        "client_id".to_string(),
        Value::String(
            optional_env("CODEUS_OAUTH_CLIENT_ID").unwrap_or_else(|| "codeus-cli".into()),
        ),
    );
    if let Some(device_code) = payload
        .get("deviceCode")
        .cloned()
        .or_else(|| payload.get("device_code").cloned())
    {
        payload.insert("deviceCode".to_string(), device_code);
    }

    let response = post_json_with_404_fallback(auth_urls, Value::Object(payload)).await?;
    let status = response.status;
    let headers = response.headers;
    let text = response.text;
    let upstream = if text.trim().is_empty() {
        json!({})
    } else {
        parse_upstream_json(&text)?
    };

    if status.as_u16() == 202 || upstream.get("status").and_then(Value::as_str) == Some("pending") {
        return Ok(oauth_error(
            "authorization_pending",
            "Waiting for browser approval.",
        ));
    }
    if status.as_u16() == 410 || upstream.get("status").and_then(Value::as_str) == Some("expired") {
        return Ok(oauth_error("expired_token", "Browser login expired."));
    }
    if upstream.get("status").and_then(Value::as_str) == Some("denied") {
        return Ok(oauth_error("access_denied", "Browser login was denied."));
    }
    if !status.is_success() {
        return Ok(text_response(status_code(status), headers, text));
    }
    if let Some(session_token) = upstream.get("sessionToken").and_then(Value::as_str) {
        return Ok(json_response(
            StatusCode::OK,
            json!({
                "access_token": session_token,
                "session_token": session_token,
                "token_type": "Bearer",
                "expires_in": expires_in(upstream.get("expiresAt")),
            }),
        ));
    }

    Ok(json_response(StatusCode::OK, upstream))
}

async fn refresh_inner(request: Request) -> Result<Response<Body>, ProxyError> {
    let auth_urls =
        configured_url_candidates("CODEUS_REFRESH_URL", "CODEUS_AUTH_BASE", REFRESH_PATHS)?;
    let mut payload = object_body(read_json(request).await?);
    payload.insert(
        "client_id".to_string(),
        Value::String(
            optional_env("CODEUS_OAUTH_CLIENT_ID").unwrap_or_else(|| "codeus-cli".into()),
        ),
    );

    proxy_json_post_candidates(auth_urls, Value::Object(payload)).await
}

async fn account_inner(request: Request) -> Result<Response<Body>, ProxyError> {
    let account_url = configured_account_url("CODEUS_ACCOUNT_URL", "account")?;
    let token = bearer_token(&request).ok_or_else(|| {
        ProxyError::new(
            StatusCode::UNAUTHORIZED,
            "not_authenticated",
            "Missing Codeus session token.",
        )
    })?;

    proxy_get(account_url, token).await
}

async fn usage_inner(request: Request) -> Result<Response<Body>, ProxyError> {
    let usage_url = configured_account_url("CODEUS_USAGE_URL", "usage")?;
    let token = bearer_token(&request).ok_or_else(|| {
        ProxyError::new(
            StatusCode::UNAUTHORIZED,
            "not_authenticated",
            "Missing Codeus session token.",
        )
    })?;

    proxy_get(usage_url, token).await
}

async fn proxy_json_post_candidates(
    urls: Vec<String>,
    body: Value,
) -> Result<Response<Body>, ProxyError> {
    let response = post_json_with_404_fallback(urls, body).await?;
    Ok(text_response(
        status_code(response.status),
        response.headers,
        response.text,
    ))
}

async fn post_json_with_404_fallback(
    urls: Vec<String>,
    body: Value,
) -> Result<UpstreamText, ProxyError> {
    let client = reqwest::Client::new();
    let last_index = urls.len().saturating_sub(1);

    for (index, url) in urls.into_iter().enumerate() {
        let response = client
            .post(url)
            .header(header::CONTENT_TYPE.as_str(), "application/json")
            .json(&body)
            .send()
            .await
            .map_err(upstream_request_error)?;

        let status = response.status();
        let headers = response.headers().clone();
        let text = response.text().await.map_err(upstream_request_error)?;
        if status != reqwest::StatusCode::NOT_FOUND || index == last_index {
            return Ok(UpstreamText {
                status,
                headers,
                text,
            });
        }
    }

    Err(ProxyError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "missing_server_config",
        "No upstream URLs were configured.",
    ))
}

async fn post_chat_with_fallback(
    upstreams: Vec<ChatUpstream>,
    user_token: Option<String>,
    body: Value,
) -> Result<reqwest::Response, ProxyError> {
    let client = reqwest::Client::new();
    let last_index = upstreams.len().saturating_sub(1);
    let mut last_failure = None;

    for (index, upstream) in upstreams.into_iter().enumerate() {
        let mut builder = client
            .post(&upstream.url)
            .header(header::CONTENT_TYPE.as_str(), "application/json");

        match &upstream.auth {
            ChatAuth::ServerToken(token) => {
                builder = builder.bearer_auth(token);
                if let Some(user_token) = user_token.as_ref() {
                    builder = builder.header("x-codeus-session-token", user_token);
                }
            }
            ChatAuth::UserBearer => {
                if let Some(user_token) = user_token.as_ref() {
                    builder = builder.bearer_auth(user_token);
                }
            }
        }

        let response = match builder.json(&body).send().await {
            Ok(response) => response,
            Err(error) if index < last_index => {
                last_failure = Some(format!("{} request failed: {error}", upstream.name));
                continue;
            }
            Err(error) => return Err(upstream_request_error(error)),
        };

        if response.status().is_success()
            || index == last_index
            || !should_fallback_chat_status(response.status())
        {
            return Ok(response);
        }

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        last_failure = Some(format!(
            "{} returned {status}: {}",
            upstream.name,
            text.trim()
        ));
    }

    Err(ProxyError::new(
        StatusCode::BAD_GATEWAY,
        "upstream_request_failed",
        last_failure.unwrap_or_else(|| "No chat upstreams were available.".to_string()),
    ))
}

async fn proxy_get(url: String, token: String) -> Result<Response<Body>, ProxyError> {
    let response = reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(upstream_request_error)?;

    Ok(proxy_response(response))
}

fn proxy_response(response: reqwest::Response) -> Response<Body> {
    let mut builder = Response::builder().status(status_code(response.status()));
    for (key, value) in response.headers() {
        if key.as_str().eq_ignore_ascii_case("content-encoding") {
            continue;
        }
        if let Ok(value) = value.to_str() {
            builder = builder.header(key.as_str(), value);
        }
    }
    builder = builder.header(header::CACHE_CONTROL, "no-store");

    builder
        .body(Body::from_stream(response.bytes_stream()))
        .unwrap_or_else(|_| internal_error("failed to build upstream response"))
}

fn text_response(
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    text: String,
) -> Response<Body> {
    let mut builder = Response::builder().status(status);
    for (key, value) in headers.iter() {
        if key.as_str().eq_ignore_ascii_case("content-encoding") {
            continue;
        }
        if let Ok(value) = value.to_str() {
            builder = builder.header(key.as_str(), value);
        }
    }
    builder = builder.header(header::CACHE_CONTROL, "no-store");
    builder
        .body(Body::from(text))
        .unwrap_or_else(|_| internal_error("failed to build upstream response"))
}

async fn read_json(request: Request) -> Result<Value, ProxyError> {
    let bytes = to_bytes(request.into_body(), BODY_LIMIT)
        .await
        .map_err(|_| {
            ProxyError::new(
                StatusCode::BAD_REQUEST,
                "invalid_body",
                "Request body could not be read.",
            )
        })?;

    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }

    serde_json::from_slice(&bytes).map_err(|_| {
        ProxyError::new(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "Request body must be valid JSON.",
        )
    })
}

fn require_env(name: &'static str) -> Result<String, ProxyError> {
    optional_env(name).ok_or_else(|| {
        ProxyError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "missing_server_config",
            format!("Missing required server environment variable: {name}."),
        )
    })
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn configured_url_candidates(
    override_name: &str,
    base_name: &'static str,
    paths: &[&str],
) -> Result<Vec<String>, ProxyError> {
    if let Some(url) = optional_env(override_name) {
        return Ok(vec![url]);
    }

    let base = require_env(base_name)?;
    Ok(paths.iter().map(|path| upstream_url(&base, path)).collect())
}

fn chat_upstream_candidates(user_token_available: bool) -> Result<Vec<ChatUpstream>, ProxyError> {
    chat_upstream_candidates_from_lookup(user_token_available, optional_env)
}

fn chat_upstream_candidates_from_lookup<F>(
    user_token_available: bool,
    mut lookup: F,
) -> Result<Vec<ChatUpstream>, ProxyError>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut candidates = Vec::new();

    let vercel_token = lookup("AI_GATEWAY_API_KEY").or_else(|| lookup("VERCEL_OIDC_TOKEN"));
    if let Some(token) = vercel_token {
        let url = lookup("CODEUS_VERCEL_AI_GATEWAY_CHAT_COMPLETIONS_URL").unwrap_or_else(|| {
            upstream_url(
                &lookup("CODEUS_VERCEL_AI_GATEWAY_BASE")
                    .or_else(|| lookup("AI_GATEWAY_BASE_URL"))
                    .unwrap_or_else(|| "https://ai-gateway.vercel.sh/v1".to_string()),
                "chat/completions",
            )
        });
        candidates.push(ChatUpstream {
            name: "vercel_ai_gateway",
            url,
            auth: ChatAuth::ServerToken(token),
        });
    }

    if let Some(url) = lookup("CODEUS_CHAT_COMPLETIONS_URL")
        .or_else(|| lookup("CODEUS_CHAT_BASE").map(|base| upstream_url(&base, "chat/completions")))
    {
        let auth = lookup("CODEUS_CHAT_API_KEY")
            .map(ChatAuth::ServerToken)
            .unwrap_or(ChatAuth::UserBearer);
        candidates.push(ChatUpstream {
            name: "llm_gateway",
            url,
            auth,
        });
    }

    if candidates.is_empty() {
        let detail = if user_token_available {
            "A user session token is present, but no OpenAI-compatible chat gateway URL is configured."
        } else {
            "No user session token or OpenAI-compatible chat gateway URL is configured."
        };
        return Err(ProxyError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "missing_server_config",
            format!("{detail} Configure AI_GATEWAY_API_KEY/VERCEL_OIDC_TOKEN for Vercel AI Gateway or CODEUS_CHAT_BASE/CODEUS_CHAT_COMPLETIONS_URL for the LLM gateway."),
        ));
    }

    Ok(candidates)
}

fn configured_account_url(override_name: &str, path: &str) -> Result<String, ProxyError> {
    if let Some(url) = optional_env(override_name) {
        return Ok(url);
    }

    let base = optional_env("CODEUS_ACCOUNT_BASE")
        .map(Ok)
        .unwrap_or_else(|| require_env("CODEUS_AUTH_BASE"))?;
    Ok(upstream_url(&base, path))
}

fn bearer_token(request: &Request) -> Option<String> {
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .trim();
    let (scheme, token) = authorization.split_once(char::is_whitespace)?;
    if scheme.eq_ignore_ascii_case("bearer") {
        let token = token.trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
    }
    None
}

fn upstream_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn object_body(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn parse_upstream_json(text: &str) -> Result<Value, ProxyError> {
    serde_json::from_str(text).map_err(|_| {
        ProxyError::new(
            StatusCode::BAD_GATEWAY,
            "invalid_upstream_json",
            "Upstream response was not valid JSON.",
        )
    })
}

fn expires_in(value: Option<&Value>) -> Option<u64> {
    let expires_at = value.and_then(Value::as_u64)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some(expires_at.saturating_sub(now).saturating_div(1000).max(1))
}

fn status_code(status: reqwest::StatusCode) -> StatusCode {
    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY)
}

fn should_fallback_chat_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 402 | 408 | 409 | 425 | 429 | 500..=599)
}

fn upstream_request_error(error: reqwest::Error) -> ProxyError {
    ProxyError::new(
        StatusCode::BAD_GATEWAY,
        "upstream_request_failed",
        error.to_string(),
    )
}

fn oauth_error(error: &'static str, description: &'static str) -> Response<Body> {
    json_response(
        StatusCode::BAD_REQUEST,
        json!({
            "error": error,
            "error_description": description,
        }),
    )
}

fn respond(result: Result<Response<Body>, ProxyError>) -> Response<Body> {
    match result {
        Ok(response) => response,
        Err(error) => json_response(
            error.status,
            json!({
                "error": error.code,
                "error_description": error.message,
            }),
        ),
    }
}

fn json_response(status: StatusCode, body: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| internal_error("failed to build JSON response"))
}

fn internal_error(message: &'static str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(
            json!({
                "error": "server_error",
                "error_description": message,
            })
            .to_string(),
        ))
        .expect("static internal error response should be valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn chat_upstreams_prefer_vercel_then_llm_gateway() {
        let env = HashMap::from([
            ("AI_GATEWAY_API_KEY", "vercel-key"),
            ("CODEUS_CHAT_BASE", "https://llm.example/v1"),
            ("CODEUS_CHAT_API_KEY", "llm-key"),
        ]);

        let candidates = chat_upstream_candidates_from_lookup(false, |key| {
            env.get(key).map(|value| value.to_string())
        })
        .unwrap();

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "vercel_ai_gateway");
        assert_eq!(
            candidates[0].url,
            "https://ai-gateway.vercel.sh/v1/chat/completions"
        );
        assert_eq!(
            candidates[0].auth,
            ChatAuth::ServerToken("vercel-key".to_string())
        );
        assert_eq!(candidates[1].name, "llm_gateway");
        assert_eq!(candidates[1].url, "https://llm.example/v1/chat/completions");
        assert_eq!(
            candidates[1].auth,
            ChatAuth::ServerToken("llm-key".to_string())
        );
    }

    #[test]
    fn chat_upstreams_keep_existing_llm_gateway_without_vercel_key() {
        let env = HashMap::from([("CODEUS_CHAT_COMPLETIONS_URL", "https://llm.example/chat")]);

        let candidates = chat_upstream_candidates_from_lookup(true, |key| {
            env.get(key).map(|value| value.to_string())
        })
        .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "llm_gateway");
        assert_eq!(candidates[0].url, "https://llm.example/chat");
        assert_eq!(candidates[0].auth, ChatAuth::UserBearer);
    }

    #[test]
    fn fallback_statuses_cover_credit_and_rate_limit_failures() {
        assert!(should_fallback_chat_status(
            reqwest::StatusCode::PAYMENT_REQUIRED
        ));
        assert!(should_fallback_chat_status(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(should_fallback_chat_status(
            reqwest::StatusCode::BAD_GATEWAY
        ));
        assert!(!should_fallback_chat_status(
            reqwest::StatusCode::BAD_REQUEST
        ));
        assert!(!should_fallback_chat_status(
            reqwest::StatusCode::UNAUTHORIZED
        ));
    }
}
