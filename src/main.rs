use std::sync::Arc;
use tokio::sync::RwLock;

use anyhow::{Context, Result};
use tracing::info;

use amqp_bridge::{Config, HealthState, LogFormat, init_logging, run_with_ctrl_c};

#[tokio::main]
/// CLI entry point that initializes logging, loads config, and runs the bridge
/// until Ctrl-C is received.
///
/// # Errors
/// Returns an error if configuration cannot be loaded from the environment or
/// if the underlying bridge fails to start or run.
async fn main() -> Result<()> {
    // Initialize logging based on LOG_FORMAT environment variable
    // Use LOG_FORMAT=json for production/Loki, LOG_FORMAT=pretty for development
    let log_format = LogFormat::from_env();
    init_logging(log_format);

    info!(event = "cli_start", log_format = ?log_format, "CLI starting");

    let config = Config::from_env().context("Failed to load configuration")?;

    let health_state = Arc::new(RwLock::new(HealthState::default()));

    // Delegate orchestration to the library convenience function without a custom transformer
    run_with_ctrl_c(config, health_state, None).await
}
