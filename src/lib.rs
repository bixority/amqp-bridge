pub mod bridge;
pub mod conf;
pub mod health;
pub mod logging;
pub mod transform;

use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{error, info, warn};

pub use crate::bridge::MessageBridge;
pub use crate::conf::Config;
pub use crate::health::{HealthState, HealthStatus, SharedHealthState, run_health_server};
pub use crate::logging::{LogFormat, init_logging};
pub use crate::transform::{Message, MessageTransformer};

/// Run the bridge with automatic reconnection and health status updates.
///
/// # Errors
/// Returns an error if establishing AMQP connections, creating channels,
/// consuming, publishing, or other underlying I/O/AMQP operations fail while
/// creating or running the bridge.
pub async fn run_with_recovery(
    config: Config,
    health_state: SharedHealthState,
    transformer: Option<Arc<dyn MessageTransformer>>,
) -> Result<()> {
    const RECONNECT_DELAY: Duration = Duration::from_secs(5);

    loop {
        info!(event = "bridge_creating", "Creating message bridge");

        let bridge_result =
            MessageBridge::new(config.clone(), health_state.clone(), transformer.clone()).await;

        match bridge_result {
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

/// Run the bridge and a health server until the provided `shutdown` future completes.
///
/// # Errors
/// Returns an error if the health server fails to bind or serve requests, or if the
/// message bridge fails to initialize or run due to AMQP/IO errors.
pub async fn run_bridge_until<S>(
    config: Config,
    health_state: SharedHealthState,
    shutdown: S,
    transformer: Option<Arc<dyn MessageTransformer>>,
) -> Result<()>
where
    S: Future<Output = ()>,
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
    let bridge = run_with_recovery(config, health_state, transformer);

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
        () = &mut shutdown => {
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

/// Convenience runner that waits for Ctrl-C and then shuts down gracefully.
///
/// # Errors
/// Propagates any errors from `run_bridge_until`, including failures starting
/// the health server or AMQP bridge.
pub async fn run_with_ctrl_c(
    config: Config,
    health_state: SharedHealthState,
    transformer: Option<Arc<dyn MessageTransformer>>,
) -> Result<()> {
    let shutdown = async {
        // Convert Result<(), std::io::Error> into () for our runner
        let _ = tokio::signal::ctrl_c().await;
    };
    run_bridge_until(config, health_state, shutdown, transformer).await
}
