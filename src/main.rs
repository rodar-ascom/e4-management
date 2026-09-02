use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{env, fs, net::SocketAddr, path::PathBuf};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::info;

const DEFAULT_BOOT_STATE_PATH: &str = "/var/lib/e4/boot-state.conf";
static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
            persistent_data_path: "/var/lib/e4".to_string(),
        }
    }
}

impl BootState {
    fn state_path() -> String {
        env::var("E4_BOOT_STATE_FILE").unwrap_or_else(|_| DEFAULT_BOOT_STATE_PATH.to_string())
    }

    fn persist_to(path: &str, state: &Self) -> Result<(), std::io::Error> {
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = format!(
            "E4_ACTIVE_SLOT={}\nE4_CANDIDATE_SLOT={}\nE4_BOOT_STATE={}\nE4_HEALTH_OK={}\nE4_LAST_UPDATE_RESULT={}\nE4_LAST_BOOT_TIME={}\nE4_PERSISTENT_DATA_PATH={}\n",
            state.active_slot,
            state.candidate_slot,
            state.boot_state,
            if state.health_ok { "1" } else { "0" },
            state.last_update_result,
            state.last_boot_time,
            state.persistent_data_path,
        );

        fs::write(path, content)
    }

    fn from_persistent_file() -> Option<Self> {
        let path = PathBuf::from(Self::state_path());
        let contents = fs::read_to_string(path).ok()?;
        let mut state = Self::default();

        for line in contents.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "E4_ACTIVE_SLOT" => state.active_slot = value.to_string(),
                "E4_CANDIDATE_SLOT" => state.candidate_slot = value.to_string(),
                "E4_BOOT_STATE" => state.boot_state = value.to_string(),
                "E4_HEALTH_OK" => state.health_ok = matches!(value, "1" | "true" | "yes" | "ok"),
                "E4_LAST_UPDATE_RESULT" => state.last_update_result = value.to_string(),
                "E4_LAST_BOOT_TIME" => state.last_boot_time = value.to_string(),
                "E4_PERSISTENT_DATA_PATH" => state.persistent_data_path = value.to_string(),
                _ => {}
            }
        }

        Some(state)
    }

    fn persist(&self) -> Result<(), std::io::Error> {
        let preferred = Self::state_path();
        Self::persist_to(&preferred, self)
    }

    fn from_environment() -> Self {
        let mut state = Self::from_persistent_file().unwrap_or_default();
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

fn with_env_var<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
    let _guard = TEST_ENV_LOCK.lock().unwrap();
    let old = std::env::var("E4_BOOT_STATE_FILE").ok();
    match value {
        Some(value) => std::env::set_var("E4_BOOT_STATE_FILE", value),
        None => std::env::remove_var("E4_BOOT_STATE_FILE"),
    }
    let result = f();
    match old {
        Some(value) => std::env::set_var("E4_BOOT_STATE_FILE", value),
        None => std::env::remove_var("E4_BOOT_STATE_FILE"),
    }
    result
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

#[test]
fn default_state_file_uses_persistent_data_path() {
    with_env_var(None, || {
        assert_eq!(BootState::state_path(), "/var/lib/e4/boot-state.conf");
    });
}

#[test]
fn persist_returns_error_when_target_directory_is_a_file() {
    with_env_var(None, || {
        let path = std::env::temp_dir().join(format!("e4-state-parent-file-{}", std::process::id()));

        let _ = fs::remove_file(&path);
        fs::write(&path, "not a directory").unwrap();
        std::env::set_var("E4_BOOT_STATE_FILE", path.join("boot-state.conf"));

        let state = BootState::default();
        assert!(state.persist().is_err());

        let _ = fs::remove_file(&path);
    });
}

#[test]
fn persistent_state_round_trip_works() {
    with_env_var(None, || {
        let dir = std::env::temp_dir().join(format!("e4-state-test-{}", std::process::id()));
        let path = dir.join("boot-state.conf");
        std::env::set_var("E4_BOOT_STATE_FILE", &path);

        let mut state = BootState::default();
        state.boot_state = "rollback-required".to_string();
        state.active_slot = "B".to_string();
        state.candidate_slot = "A".to_string();
        state.persist().unwrap();

        let loaded = BootState::from_persistent_file().unwrap();
        assert_eq!(loaded.active_slot, "B");
        assert_eq!(loaded.candidate_slot, "A");
        assert_eq!(loaded.boot_state, "rollback-required");

        let _ = fs::remove_dir_all(&dir);
    });
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
    if !matches!(payload.result.as_str(), "success" | "failed") {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut boot_state = state.boot_state.write().await;
    boot_state.record_update_result(&payload.result);
    boot_state.persist().map_err(|err| {
        tracing::warn!(error = %err, "failed to persist update result");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok((StatusCode::OK, Json(boot_state.clone())))
}

async fn rollback(State(state): State<AppState>) -> Result<impl IntoResponse, StatusCode> {
    let mut boot_state = state.boot_state.write().await;
    boot_state.rollback();
    boot_state.persist().map_err(|err| {
        tracing::warn!(error = %err, "failed to persist rollback state");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
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
