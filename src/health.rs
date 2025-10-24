use anyhow::Context;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HealthStatus {
    Starting,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone)]
pub struct HealthState {
    pub liveness: HealthStatus,
    pub readiness: HealthStatus,
    pub last_message_processed: Option<std::time::Instant>,
}

impl Default for HealthState {
    fn default() -> Self {
        Self {
            liveness: HealthStatus::Starting,
            readiness: HealthStatus::Starting,
            last_message_processed: None,
        }
    }
}

pub type SharedHealthState = Arc<RwLock<HealthState>>;

// Health check handlers
async fn liveness_probe(State(health_state): State<SharedHealthState>) -> StatusCode {
    let state = health_state.read().await;

    match state.liveness {
        HealthStatus::Healthy => StatusCode::OK,
        HealthStatus::Starting => {
            info!("Liveness probe: starting");
            StatusCode::OK // Allow pod to start
        }
        HealthStatus::Unhealthy => {
            error!("Liveness probe: unhealthy");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

async fn readiness_probe(State(health_state): State<SharedHealthState>) -> StatusCode {
    let state = health_state.read().await;

    match state.readiness {
        HealthStatus::Healthy => StatusCode::OK,
        HealthStatus::Starting | HealthStatus::Unhealthy => {
            error!("Readiness probe: not ready");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

async fn startup_probe(State(health_state): State<SharedHealthState>) -> StatusCode {
    let state = health_state.read().await;

    match state.liveness {
        HealthStatus::Healthy => StatusCode::OK,
        HealthStatus::Starting | HealthStatus::Unhealthy => {
            info!("Startup probe: not started yet");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

pub async fn run_health_server(port: u16, health_state: SharedHealthState) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/healthz", get(liveness_probe))
        .route("/ready", get(readiness_probe))
        .route("/startup", get(startup_probe))
        .with_state(health_state);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .context("Failed to bind health server")?;

    info!("Health check server listening on {}", addr);

    axum::serve(listener, app)
        .await
        .context("Health server error")?;

    Ok(())
}
