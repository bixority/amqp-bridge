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

/// Run an HTTP health server exposing liveness, readiness and startup probes.
///
/// # Errors
/// Returns an error if the TCP listener cannot bind to the specified `port`
/// or if the HTTP server fails while serving requests.
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    fn shared(state: HealthState) -> SharedHealthState {
        Arc::new(RwLock::new(state))
    }

    #[tokio::test]
    async fn liveness_ok_for_starting_and_healthy() {
        let st = shared(HealthState {
            liveness: HealthStatus::Starting,
            readiness: HealthStatus::Starting,
            last_message_processed: None,
        });
        assert_eq!(
            super::liveness_probe(State(st.clone())).await,
            StatusCode::OK
        );

        {
            let mut w = st.write().await;
            w.liveness = HealthStatus::Healthy;
        }
        assert_eq!(super::liveness_probe(State(st)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn liveness_unhealthy_returns_503() {
        let st = shared(HealthState {
            liveness: HealthStatus::Unhealthy,
            readiness: HealthStatus::Unhealthy,
            last_message_processed: None,
        });
        assert_eq!(
            super::liveness_probe(State(st)).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn readiness_ok_only_when_healthy() {
        let st = shared(HealthState::default());
        // default is Starting -> 503
        assert_eq!(
            super::readiness_probe(State(st.clone())).await,
            StatusCode::SERVICE_UNAVAILABLE
        );

        {
            let mut w = st.write().await;
            w.readiness = HealthStatus::Healthy;
        }
        assert_eq!(super::readiness_probe(State(st)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn startup_ok_only_when_healthy() {
        let st = shared(HealthState::default());
        // default is Starting -> 503
        assert_eq!(
            super::startup_probe(State(st.clone())).await,
            StatusCode::SERVICE_UNAVAILABLE
        );

        {
            let mut w = st.write().await;
            w.liveness = HealthStatus::Healthy;
        }
        assert_eq!(super::startup_probe(State(st)).await, StatusCode::OK);
    }
}
