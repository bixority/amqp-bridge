mod bridge;
mod conf;
mod health;

use std::sync::Arc;
use tokio::sync::RwLock;

use crate::bridge::MessageBridge;
use crate::conf::Config;
use crate::health::{HealthState, HealthStatus, SharedHealthState, run_health_server};
use anyhow::{Context, Result};
use tokio::time::{self, Duration};
use tracing::{error, info, warn};

async fn run_with_recovery(config: Config, health_state: SharedHealthState) -> Result<()> {
    const RECONNECT_DELAY: Duration = Duration::from_secs(5);

    loop {
        info!("Creating message bridge...");

        match MessageBridge::new(config.clone(), health_state.clone()).await {
            Ok(bridge) => {
                info!("Bridge created successfully, starting message processing");

                match bridge.run().await {
                    Ok(()) => {
                        warn!("Bridge stopped normally (unexpected)");
                    }
                    Err(e) => {
                        error!("Bridge encountered an error: {e}");
                    }
                }

                info!(
                    "Connection lost or error occurred, will attempt to reconnect in {:?}",
                    RECONNECT_DELAY
                );
            }
            Err(e) => {
                error!("Failed to create bridge: {e}");
                info!("Will retry in {:?}", RECONNECT_DELAY);

                // Mark as unhealthy if we can't create bridge
                let mut state = health_state.write().await;
                state.liveness = HealthStatus::Unhealthy;
                state.readiness = HealthStatus::Unhealthy;
            }
        }

        time::sleep(RECONNECT_DELAY).await;
        info!("Attempting to reconnect...");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    info!("Starting RabbitMQ message bridge with auto-recovery and health checks");

    let config = Config::from_env().context("Failed to load configuration")?;

    info!("Configuration loaded:");
    info!("  Source queue: {}", config.source_queue);
    info!("  Target exchange: {}", config.target_exchange);
    info!("  Target routing key: {}", config.target_routing_key);
    info!("  Health port: {}", config.health_port);

    let health_state = Arc::new(RwLock::new(HealthState::default()));

    // Start health check server
    let health_server = run_health_server(config.health_port, health_state.clone());

    // Start message bridge with recovery
    let bridge = run_with_recovery(config, health_state);

    // Handle graceful shutdown
    let shutdown = tokio::signal::ctrl_c();

    tokio::select! {
        result = health_server => {
            result.context("Health server failed")?;
        }
        result = bridge => {
            result.context("Bridge failed")?;
        }
        _ = shutdown => {
            info!("Received shutdown signal, exiting gracefully");
        }
    }

    Ok(())
}
