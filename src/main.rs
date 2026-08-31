use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{env, net::SocketAddr};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::info;

#[derive(Clone)]
struct AppState {
    device_name: String,
    boot_state: std::sync::Arc<RwLock<BootState>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct BootState {
    active_slot: String,
    candidate_slot: String,
    boot_state: String,
    health_ok: bool,
    last_update_result: String,
    last_boot_time: String,
    persistent_data_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UpdateRequest {
    result: String,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate<'a> {
    device_name: &'a str,
    boot_state: &'a str,
    active_slot: &'a str,
    candidate_slot: &'a str,
    health_ok: bool,
    last_update_result: &'a str,
}

impl Default for BootState {
    fn default() -> Self {
        Self {
            active_slot: "A".to_string(),
            candidate_slot: "B".to_string(),
            boot_state: "confirmed".to_string(),
            health_ok: true,
            last_update_result: "none".to_string(),
            last_boot_time: "unknown".to_string(),
            persistent_data_path: "/data".to_string(),
        }
    }
}

impl BootState {
    fn from_environment() -> Self {
        let mut state = Self::default();
        if let Ok(active) = env::var("E4_ACTIVE_SLOT") {
            state.active_slot = active;
        }
        if let Ok(candidate) = env::var("E4_CANDIDATE_SLOT") {
            state.candidate_slot = candidate;
        }
        if let Ok(boot) = env::var("E4_BOOT_STATE") {
            state.boot_state = boot;
        }
        if let Ok(health) = env::var("E4_HEALTH_OK") {
            state.health_ok = matches!(health.as_str(), "1" | "true" | "yes" | "ok");
        }
        if let Ok(result) = env::var("E4_LAST_UPDATE_RESULT") {
            state.last_update_result = result;
        }
        if let Ok(last_boot) = env::var("E4_LAST_BOOT_TIME") {
            state.last_boot_time = last_boot;
        }
        if let Ok(data_path) = env::var("E4_PERSISTENT_DATA_PATH") {
            state.persistent_data_path = data_path;
        }
        state
    }

    fn record_update_result(&mut self, result: &str) {
        self.last_update_result = result.to_string();
        self.boot_state = if result.eq_ignore_ascii_case("success") {
            "confirmed".to_string()
        } else {
            "rollback-required".to_string()
        };
    }

    fn rollback(&mut self) {
        let previous = self.active_slot.clone();
        self.active_slot = if previous == "A" { "B".to_string() } else { "A".to_string() };
        self.candidate_slot = previous;
        self.boot_state = "rolled-back".to_string();
        self.health_ok = true;
    }
}

#[test]
fn rollback_switches_active_and_candidate_slots() {
    let mut state = BootState::default();
    state.active_slot = "A".to_string();
    state.candidate_slot = "B".to_string();

    state.rollback();

    assert_eq!(state.active_slot, "B");
    assert_eq!(state.candidate_slot, "A");
    assert_eq!(state.boot_state, "rolled-back");
}

#[test]
fn failed_update_marks_rollback_required() {
    let mut state = BootState::default();

    state.record_update_result("failed");

    assert_eq!(state.last_update_result, "failed");
    assert_eq!(state.boot_state, "rollback-required");
}

async fn index(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    let boot_state = state.boot_state.read().await;
    IndexTemplate {
        device_name: &state.device_name,
        boot_state: &boot_state.boot_state,
        active_slot: &boot_state.active_slot,
        candidate_slot: &boot_state.candidate_slot,
        health_ok: boot_state.health_ok,
        last_update_result: &boot_state.last_update_result,
    }
    .render()
    .map(Html)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn system_status(State(state): State<AppState>) -> Result<Json<BootState>, StatusCode> {
    let boot_state = state.boot_state.read().await;
    Ok(Json(boot_state.clone()))
}

async fn update_status(State(state): State<AppState>) -> Result<Json<BootState>, StatusCode> {
    let boot_state = state.boot_state.read().await;
    Ok(Json(boot_state.clone()))
}

async fn update_result(
    State(state): State<AppState>,
    Json(payload): Json<UpdateRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let mut boot_state = state.boot_state.write().await;
    boot_state.record_update_result(&payload.result);
    Ok((StatusCode::OK, Json(boot_state.clone())))
}

async fn rollback(State(state): State<AppState>) -> Result<impl IntoResponse, StatusCode> {
    let mut boot_state = state.boot_state.write().await;
    boot_state.rollback();
    Ok((StatusCode::OK, Json(boot_state.clone())))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let address = env::var("WEB_ADDRESS").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let device_name = env::var("DEVICE_NAME").unwrap_or_else(|_| "x86 device".to_owned());
    let address: SocketAddr = address.parse()?;

    let state = AppState {
        device_name,
        boot_state: std::sync::Arc::new(RwLock::new(BootState::from_environment())),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/system/status", get(system_status))
        .route("/api/boot/status", get(system_status))
        .route("/api/update/status", get(update_status))
        .route("/api/update/result", post(update_result))
        .route("/api/actions/rollback", post(rollback))
        .with_state(state);

    let listener = TcpListener::bind(address).await?;
    info!(address = %address, "e4-management server listening");
    axum::serve(listener, app).await?;

    Ok(())
}
