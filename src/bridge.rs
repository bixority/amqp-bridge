use crate::conf::Config;
use crate::health::{HealthStatus, SharedHealthState};
use anyhow::{Context, Error};
use futures::StreamExt;
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicNackOptions, BasicPublishOptions, BasicQosOptions,
};
use lapin::types::FieldTable;
use lapin::{Channel, Connection, ConnectionProperties, Consumer};
use std::time::Duration;
use tokio::time;
use tracing::{error, info, warn};

pub struct MessageBridge {
    source_channel: Channel,
    target_channel: Channel,
    source_connection: Connection,
    target_connection: Connection,
    config: Config,
    health_state: SharedHealthState,
}

impl MessageBridge {
    /// Attempts to connect to a `RabbitMQ` instance,
    /// retrying up to 10 times with exponential backoff.
    async fn connect_with_retry(uri: &str, context_msg: &str) -> anyhow::Result<Connection> {
        const MAX_RETRIES: u8 = 10;
        const INITIAL_DELAY: Duration = Duration::from_secs(1);
        const MAX_DELAY: Duration = Duration::from_secs(30);

        let mut delay = INITIAL_DELAY;

        // Parse and display connection details (without password)
        let sanitized_uri = sanitize_uri_for_logging(uri);
        info!("Connecting to: {}", sanitized_uri);

        for attempt in 1..=MAX_RETRIES {
            info!(
                "{} - Attempt {}/{}: Connecting to {}",
                context_msg, attempt, MAX_RETRIES, sanitized_uri
            );

            match Connection::connect(uri, ConnectionProperties::default()).await {
                Ok(conn) => {
                    info!(
                        "✓ Successfully connected to {} on attempt {}",
                        sanitized_uri, attempt
                    );
                    return Ok(conn);
                }
                Err(e) => {
                    // Enhanced error logging with detailed information
                    error!(
                        "✗ {}: Connection failed on attempt {}/{}",
                        context_msg, attempt, MAX_RETRIES
                    );
                    error!("  URI: {}", sanitized_uri);
                    error!("  Error type: {:?}", e);
                    error!("  Error message: {}", e);

                    // Check for specific error types
                    let error_string = format!("{e:?}");
                    if error_string.contains("ConnectionRefused")
                        || error_string.contains("Connection refused")
                    {
                        error!("  → Connection refused: RabbitMQ is not accepting connections");
                        error!("    Possible causes:");
                        error!("      • RabbitMQ service is not running");
                        error!("      • Wrong host or port in DSN");
                        error!("      • Firewall blocking the connection");
                        error!("      • RabbitMQ not listening on the specified interface");
                    } else if error_string.contains("timeout") || error_string.contains("Timeout") {
                        error!("  → Connection timeout: No response from RabbitMQ server");
                    } else if error_string.contains("auth") || error_string.contains("Auth") {
                        error!("  → Authentication failed: Invalid credentials");
                    } else if error_string.contains("resolution")
                        || error_string.contains("resolve")
                    {
                        error!("  → DNS resolution failed: Cannot resolve hostname");
                    }

                    // Print additional error details
                    if e.is_io_error() {
                        error!("  I/O Error details: {e}");
                    }

                    if attempt < MAX_RETRIES {
                        warn!("  ⏳ Retrying in {:?}...", delay);
                        time::sleep(delay).await;
                        // Exponential backoff, capped at MAX_DELAY
                        delay = std::cmp::min(delay * 2, MAX_DELAY);
                    } else {
                        error!(
                            "✗ Failed to connect after {} attempts. Giving up.",
                            MAX_RETRIES
                        );
                        return Err(e).context(format!(
                            "{context_msg} after {MAX_RETRIES} attempts to {sanitized_uri}",
                        ));
                    }
                }
            }
        }
        Err(anyhow::anyhow!("Exhausted all connection retries."))
    }

    pub async fn new(config: Config, health_state: SharedHealthState) -> anyhow::Result<Self> {
        // Mark as starting
        {
            let mut state = health_state.write().await;
            state.liveness = HealthStatus::Starting;
            state.readiness = HealthStatus::Starting;
        }

        info!("=== Starting MessageBridge initialization ===");
        info!("Source queue: {}", config.source_queue);
        info!("Target exchange: {}", config.target_exchange);
        info!("Target routing key: {}", config.target_routing_key);

        info!("--- Connecting to SOURCE RabbitMQ ---");
        let source_conn =
            Self::connect_with_retry(&config.source_dsn, "Failed to connect to source RabbitMQ")
                .await
                .context("Source RabbitMQ connection failed")?;

        let source_channel = source_conn
            .create_channel()
            .await
            .context("Failed to create source channel")?;

        info!("--- Connecting to TARGET RabbitMQ ---");
        let target_conn =
            Self::connect_with_retry(&config.target_dsn, "Failed to connect to target RabbitMQ")
                .await
                .context("Target RabbitMQ connection failed")?;

        let target_channel = target_conn
            .create_channel()
            .await
            .context("Failed to create target channel")?;

        source_channel
            .basic_qos(1, BasicQosOptions::default())
            .await
            .context("Failed to set QoS")?;

        info!("✓ Successfully connected to both RabbitMQ instances");
        info!("=== MessageBridge initialization complete ===");

        // Mark as healthy after successful connection
        {
            let mut state = health_state.write().await;
            state.liveness = HealthStatus::Healthy;
            state.readiness = HealthStatus::Healthy;
        }

        Ok(Self {
            source_channel,
            target_channel,
            source_connection: source_conn,
            target_connection: target_conn,
            config,
            health_state,
        })
    }

    /// Check if connections are still alive
    fn is_connected(&self) -> bool {
        self.source_connection.status().connected() && self.target_connection.status().connected()
    }

    async fn mark_unhealthy(&self) {
        let mut state = self.health_state.write().await;
        state.liveness = HealthStatus::Unhealthy;
        state.readiness = HealthStatus::Unhealthy;
    }

    async fn update_message_timestamp(&self) {
        let mut state = self.health_state.write().await;
        state.last_message_processed = Some(std::time::Instant::now());
    }

    async fn consume(&self, mut consumer: Consumer) -> Result<(), Error> {
        while let Some(delivery_result) = consumer.next().await {
            // Check connection health before processing
            if !self.is_connected() {
                error!("Connection lost, stopping consumer loop");
                self.mark_unhealthy().await;
                return Err(anyhow::anyhow!("Connection lost during message processing"));
            }

            match delivery_result {
                Ok(delivery) => {
                    let data = delivery.data.clone();
                    let properties = delivery.properties.clone();

                    // Try to convert message to string for logging
                    let message_preview = match std::str::from_utf8(&data) {
                        Ok(s) => {
                            // Truncate if too long
                            if s.len() > 200 {
                                format!("{}...", &s[..200])
                            } else {
                                s.to_string()
                            }
                        }
                        Err(_) => format!("<binary data, {} bytes>", data.len()),
                    };

                    info!(
                        "Received message: {} bytes, delivery_tag: {}, content: {}",
                        data.len(),
                        delivery.delivery_tag,
                        message_preview
                    );

                    // Publish to target
                    match self
                        .target_channel
                        .basic_publish(
                            &self.config.target_exchange,
                            &self.config.target_routing_key,
                            BasicPublishOptions::default(),
                            &data,
                            properties,
                        )
                        .await
                    {
                        Ok(confirm) => {
                            // Wait for publisher confirmation
                            match confirm.await {
                                Ok(_) => {
                                    info!("Successfully published message to target");

                                    if let Err(e) = delivery.ack(BasicAckOptions::default()).await {
                                        error!("Failed to acknowledge message: {e}");
                                        self.mark_unhealthy().await;
                                        return Err(anyhow::anyhow!("Failed to ack: {e}"));
                                    }

                                    // Update health timestamp after successful processing
                                    self.update_message_timestamp().await;
                                }
                                Err(e) => {
                                    error!("Publisher confirmation failed: {}", e);
                                    if let Err(e) = delivery
                                        .nack(BasicNackOptions {
                                            requeue: true,
                                            multiple: false,
                                        })
                                        .await
                                    {
                                        error!("Failed to nack message: {e}");
                                        self.mark_unhealthy().await;
                                        return Err(anyhow::anyhow!("Failed to nack: {e}"));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("Failed to publish message: {e}",);

                            if let Err(e) = delivery
                                .nack(BasicNackOptions {
                                    requeue: true,
                                    multiple: false,
                                })
                                .await
                            {
                                error!("Failed to nack message: {e}");
                                self.mark_unhealthy().await;
                                return Err(anyhow::anyhow!("Failed to nack: {e}"));
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Error receiving message: {e}");
                    self.mark_unhealthy().await;
                    return Err(anyhow::anyhow!("Consumer error: {e}"));
                }
            }
        }

        Ok(())
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        info!(
            "Starting to consume from queue '{}'",
            self.config.source_queue
        );

        let consumer = self
            .source_channel
            .basic_consume(
                &self.config.source_queue,
                "bridge_consumer",
                BasicConsumeOptions::default(),
                FieldTable::default(),
            )
            .await
            .context("Failed to start consuming")?;

        info!("Consumer started, waiting for messages...");

        self.consume(consumer).await?;

        warn!("Consumer stream ended");
        self.mark_unhealthy().await;
        Err(anyhow::anyhow!("Consumer stream ended unexpectedly"))
    }
}

/// Helper function to sanitize URI for logging (removes password)
fn sanitize_uri_for_logging(uri: &str) -> String {
    if let Some(at_pos) = uri.find('@') {
        if let Some(scheme_end) = uri.find("://") {
            let scheme = &uri[..scheme_end + 3];
            let after_at = &uri[at_pos..];

            // Try to find username part
            if let Some(colon_pos) = uri[scheme_end + 3..at_pos].find(':') {
                let username = &uri[scheme_end + 3..scheme_end + 3 + colon_pos];
                return format!("{scheme}{username}:***{after_at}");
            }
        }
    }
    // If parsing fails, return as-is (or you could return a generic message)
    uri.to_string()
}
