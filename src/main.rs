mod bridge;
mod conf;
mod health;
mod logging;

use std::sync::Arc;
use tokio::sync::RwLock;

use crate::bridge::MessageBridge;
use crate::conf::Config;
use crate::health::{HealthState, HealthStatus, SharedHealthState, run_health_server};
use crate::logging::{LogFormat, init_logging};
use anyhow::{Context, Result};
use tokio::time::{self, Duration};
use tracing::{error, info, warn};

async fn run_with_recovery(config: Config, health_state: SharedHealthState) -> Result<()> {
    const RECONNECT_DELAY: Duration = Duration::from_secs(5);

    loop {
        info!(event = "bridge_creating", "Creating message bridge");

        match MessageBridge::new(config.clone(), health_state.clone()).await {
            Ok(bridge) => {
                info!(
                    event = "bridge_created",
                    status = "success",
                    "Bridge created successfully, starting message processing"
                );

                match bridge.run().await {
                    Ok(()) => {
                        warn!(
                            event = "bridge_stopped",
                            reason = "normal",
                            "Bridge stopped normally (unexpected)"
                        );
                    }
                    Err(e) => {
                        error!(
                            event = "bridge_error",
                            error = %e,
                            "Bridge encountered an error"
                        );
                    }
                }

                info!(
                    event = "bridge_reconnecting",
                    delay_secs = RECONNECT_DELAY.as_secs(),
                    "Connection lost or error occurred, will attempt to reconnect"
                );
            }
            Err(e) => {
                error!(
                    event = "bridge_creation_failed",
                    error = %e,
                    retry_delay_secs = RECONNECT_DELAY.as_secs(),
                    "Failed to create bridge"
                );

                // Mark as unhealthy if we can't create bridge
                let mut state = health_state.write().await;
                state.liveness = HealthStatus::Unhealthy;
                state.readiness = HealthStatus::Unhealthy;
            }
        }

        time::sleep(RECONNECT_DELAY).await;
        info!(event = "reconnect_attempt", "Attempting to reconnect");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging based on LOG_FORMAT environment variable
    // Use LOG_FORMAT=json for production/Loki, LOG_FORMAT=pretty for development
    let log_format = LogFormat::from_env();
    init_logging(log_format);

    info!(
        event = "application_starting",
        log_format = ?log_format,
        "Starting AMQP message bridge with auto-recovery and health checks"
    );

    let config = Config::from_env().context("Failed to load configuration")?;

    info!(
        event = "config_loaded",
        source_queue = %config.source_queue,
        target_exchange = %config.target_exchange,
        target_routing_key = %config.target_routing_key,
        health_port = config.health_port,
        "Configuration loaded"
    );

    let health_state = Arc::new(RwLock::new(HealthState::default()));

    // Start health check server
    info!(
        event = "health_server_starting",
        port = config.health_port,
        "Starting health check server"
    );
    let health_server = run_health_server(config.health_port, health_state.clone());

    // Start message bridge with recovery
    let bridge = run_with_recovery(config, health_state);

    // Handle graceful shutdown
    let shutdown = tokio::signal::ctrl_c();

    tokio::select! {
        result = health_server => {
            error!(
                event = "health_server_failed",
                error = ?result,
                "Health server failed"
            );
            result.context("Health server failed")?;
        }
        result = bridge => {
            error!(
                event = "bridge_failed",
                error = ?result,
                "Bridge failed"
            );
            result.context("Bridge failed")?;
        }
        _ = shutdown => {
            info!(
                event = "shutdown_signal",
                "Received shutdown signal, exiting gracefully"
            );
        }
    }

    info!(
        event = "application_stopped",
        "Application shutdown complete"
    );

    Ok(())
}
