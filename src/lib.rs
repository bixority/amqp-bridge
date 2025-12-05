pub mod bridge;
pub mod conf;
pub mod health;
pub mod logging;

use anyhow::{Context, Result};
use std::time::Duration;
use tokio::time;
use tracing::{error, info, warn};

pub use crate::bridge::MessageBridge;
pub use crate::conf::Config;
pub use crate::health::{run_health_server, HealthState, HealthStatus, SharedHealthState};
pub use crate::logging::{init_logging, LogFormat};

pub async fn run_with_recovery(config: Config, health_state: SharedHealthState) -> Result<()> {
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

pub async fn run_bridge_until<S>(
    config: Config,
    health_state: SharedHealthState,
    shutdown: S,
) -> Result<()>
where
    S: std::future::Future<Output = ()>,
{
    info!(
        event = "application_starting",
        "Starting AMQP message bridge with auto-recovery and health checks"
    );

    info!(
        event = "config_loaded",
        source_queue = %config.source_queue,
        target_exchange = %config.target_exchange,
        target_routing_key = %config.target_routing_key,
        health_port = config.health_port,
        "Configuration loaded"
    );

    // Start health check server
    info!(
        event = "health_server_starting",
        port = config.health_port,
        "Starting health check server"
    );
    let health_server = run_health_server(config.health_port, health_state.clone());

    // Start message bridge with recovery
    let bridge = run_with_recovery(config, health_state);

    tokio::pin!(health_server);
    tokio::pin!(bridge);
    tokio::pin!(shutdown);

    tokio::select! {
        result = &mut health_server => {
            error!(
                event = "health_server_failed",
                error = ?result,
                "Health server failed"
            );
            result.context("Health server failed")?;
        }
        result = &mut bridge => {
            error!(
                event = "bridge_failed",
                error = ?result,
                "Bridge failed"
            );
            result.context("Bridge failed")?;
        }
        _ = &mut shutdown => {
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

pub async fn run_with_ctrl_c(config: Config, health_state: SharedHealthState) -> Result<()> {
    let shutdown = async {
        // Convert Result<(), std::io::Error> into () for our runner
        let _ = tokio::signal::ctrl_c().await;
    };
    run_bridge_until(config, health_state, shutdown).await
}
