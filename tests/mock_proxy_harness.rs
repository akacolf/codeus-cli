use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tokio::net::TcpListener;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mock_proxy_harness_covers_diagnostics_status_and_streaming() {
    let base_url = start_mock_proxy().await;
    let root = temp_root("codeus-mock-harness");
    let home = root.join("home");
    let workspace = root.join("workspace");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("AGENTS.md"), "Test instruction file.\n").unwrap();

    let doctor = codeus(&home, &workspace)
        .args([
            "--api-base",
            &base_url,
            "--token",
            "test-session",
            "--output-format",
            "json",
            "doctor",
        ])
        .output()
        .unwrap();
    assert!(
        doctor.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor_json: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor_json["ok"], true);
    assert!(doctor_json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| { check["name"] == "proxy.account" && check["status"] == "ok" }));

    let status = codeus(&home, &workspace)
        .args([
            "--api-base",
            &base_url,
            "--token",
            "test-session",
            "--permission-mode",
            "read-only",
            "--output-format",
            "json",
            "status",
        ])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status_json: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status_json["binary"], "codeus");
    assert_eq!(status_json["permission_mode"], "read-only");
    assert_eq!(status_json["auth"]["signed_in"], true);

    let run = codeus(&home, &workspace)
        .args([
            "--api-base",
            &base_url,
            "--token",
            "test-session",
            "run",
            "--cwd",
            workspace.to_str().unwrap(),
            "say hello",
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("mock reply"));

    let _ = fs::remove_dir_all(root);
}

async fn start_mock_proxy() -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/auth/whoami", get(account))
        .route("/v1/balance", get(usage))
        .route("/v1/device/link/init", post(device_link_init))
        .route("/v1/device/link/poll", post(device_link_poll));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/v1")
}

async fn chat() -> impl IntoResponse {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"mock reply\"}}]}\n\n",
        "data: [DONE]\n\n"
    );
    (
        StatusCode::OK,
        [(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))],
        body,
    )
}

async fn account() -> Json<Value> {
    Json(json!({
        "id": "acct_mock",
        "email": "mock@example.com",
        "plan": "test"
    }))
}

async fn usage() -> Json<Value> {
    Json(json!({
        "usage_remaining": 123
    }))
}

async fn device_link_init() -> Json<Value> {
    Json(json!({
        "deviceCode": "device_mock",
        "userCode": "MOCK-CODE",
        "linkUrl": "https://quantumclub.sh/account/link?code=MOCK-CODE",
        "expiresIn": 600,
        "interval": 1
    }))
}

async fn device_link_poll() -> Json<Value> {
    Json(json!({
        "status": "approved",
        "sessionToken": "test-session",
        "refreshToken": "refresh-session",
        "expiresIn": 3600
    }))
}

fn codeus(home: &PathBuf, workspace: &PathBuf) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_codeus"));
    command
        .current_dir(workspace)
        .env("HOME", home)
        .env("CODEUS_NO_UPDATE_CHECK", "1")
        .env_remove("CODEUS_SESSION_TOKEN")
        .env_remove("CODEUS_API_BASE")
        .env_remove("CODEUS_MODEL")
        .env_remove("CODEUS_PERMISSION_MODE")
        .env_remove("CODEUS_OUTPUT_FORMAT");
    command
}

fn temp_root(name: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{name}-{nonce}"))
}
