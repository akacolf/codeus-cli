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
    let chat_url = configured_url(
        "CODEUS_CHAT_COMPLETIONS_URL",
        "CODEUS_CHAT_BASE",
        "chat/completions",
    )?;
    let server_token = optional_env("CODEUS_CHAT_API_KEY");
    let mut builder = reqwest::Client::new()
        .post(chat_url)
        .header(header::CONTENT_TYPE.as_str(), "application/json");

    if let Some(server_token) = server_token {
        builder = builder.bearer_auth(server_token);
        if let Some(user_token) = user_token {
            builder = builder.header("x-codeus-session-token", user_token);
        }
    } else if let Some(user_token) = user_token {
        builder = builder.bearer_auth(user_token);
    }

    let response = builder
        .json(&body)
        .send()
        .await
        .map_err(upstream_request_error)?;

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

fn configured_url(
    override_name: &str,
    base_name: &'static str,
    path: &str,
) -> Result<String, ProxyError> {
    if let Some(url) = optional_env(override_name) {
        return Ok(url);
    }

    let base = require_env(base_name)?;
    Ok(upstream_url(&base, path))
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
