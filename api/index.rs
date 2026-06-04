use axum::routing::{get, post};
use axum::Router;
use codeus::vercel_proxy;
use tower::ServiceBuilder;
use vercel_runtime::axum::VercelLayer;
use vercel_runtime::Error;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let router = Router::new()
        .route("/v1/auth/device/code", post(vercel_proxy::device_code))
        .route("/v1/auth/device/token", post(vercel_proxy::device_token))
        .route("/v1/auth/refresh", post(vercel_proxy::refresh))
        .route("/v1/device/link/init", post(vercel_proxy::device_code))
        .route("/v1/device/link/poll", post(vercel_proxy::device_token))
        .route("/v1/chat/completions", post(vercel_proxy::chat_completions))
        .route("/v1/account", get(vercel_proxy::account))
        .route("/v1/usage", get(vercel_proxy::usage))
        .route("/auth/device/code", post(vercel_proxy::device_code))
        .route("/auth/device/token", post(vercel_proxy::device_token))
        .route("/auth/refresh", post(vercel_proxy::refresh))
        .route("/device/link/init", post(vercel_proxy::device_code))
        .route("/device/link/poll", post(vercel_proxy::device_token))
        .route("/chat/completions", post(vercel_proxy::chat_completions))
        .route("/account", get(vercel_proxy::account))
        .route("/usage", get(vercel_proxy::usage))
        .fallback(vercel_proxy::fallback);

    let app = ServiceBuilder::new()
        .layer(VercelLayer::new())
        .service(router);

    vercel_runtime::run(app).await
}
