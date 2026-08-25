use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use std::{env, net::SocketAddr};
use tokio::net::TcpListener;
use tracing::info;

#[derive(Clone)]
struct AppState {
    device_name: String,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate<'a> {
    device_name: &'a str,
}

async fn index(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    IndexTemplate {
        device_name: &state.device_name,
    }
    .render()
    .map(Html)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let address = env::var("WEB_ADDRESS").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let device_name = env::var("DEVICE_NAME").unwrap_or_else(|_| "x86 device".to_owned());
    let address: SocketAddr = address.parse()?;

    let state = AppState { device_name };
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .with_state(state);

    let listener = TcpListener::bind(address).await?;
    info!(address = %address, "e4-management server listening");
    axum::serve(listener, app).await?;

    Ok(())
}
