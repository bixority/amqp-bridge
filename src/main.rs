use futures::StreamExt;

use anyhow::{Context, Result};
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicNackOptions, BasicPublishOptions, BasicQosOptions,
};
use lapin::{Channel, Connection, ConnectionProperties, types::FieldTable};
use tokio::time::{self, Duration};
use tracing::{error, info, warn};

#[derive(Debug)]
struct Config {
    source_dsn: String,
    source_queue: String,
    target_dsn: String,
    target_exchange: String,
    target_routing_key: String,
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Self {
            source_dsn: std::env::var("SOURCE_DSN")
                .context("SOURCE_DSN environment variable not set")?,
            source_queue: std::env::var("SOURCE_QUEUE").unwrap_or_else(|_| "old".to_string()),
            target_dsn: std::env::var("TARGET_DSN")
                .context("TARGET_DSN environment variable not set")?,
            target_exchange: std::env::var("TARGET_EXCHANGE")
                .unwrap_or_else(|_| "new_xchg".to_string()),
            target_routing_key: std::env::var("TARGET_ROUTING_KEY")
                .unwrap_or_else(|_| "update".to_string()),
        })
    }
}

struct MessageBridge {
    source_channel: Channel,
    target_channel: Channel,
    source_connection: Connection,
    target_connection: Connection,
    config: Config,
}

impl MessageBridge {
    /// Attempts to connect to a RabbitMQ instance,
    /// retrying up to 10 times with exponential backoff.
    async fn connect_with_retry(dsn: &str, context_msg: &str) -> Result<Connection> {
        const MAX_RETRIES: u8 = 10;
        const INITIAL_DELAY: Duration = Duration::from_secs(1);
        const MAX_DELAY: Duration = Duration::from_secs(30);

        let mut delay = INITIAL_DELAY;

        for attempt in 1..=MAX_RETRIES {
            info!(
                "Attempting connection to RabbitMQ - attempt {}/{}",
                attempt, MAX_RETRIES
            );

            match Connection::connect(dsn, ConnectionProperties::default()).await {
                Ok(conn) => {
                    info!("Successfully connected on attempt {attempt}");
                    return Ok(conn);
                }
                Err(e) => {
                    warn!("{context_msg}: Failed on attempt {attempt}: {e}");

                    if attempt < MAX_RETRIES {
                        info!("Waiting {:?} before retry...", delay);

                        time::sleep(delay).await;
                        // Exponential backoff, capped at MAX_DELAY
                        delay = std::cmp::min(delay * 2, MAX_DELAY);
                    } else {
                        return Err(e)
                            .context(format!("{context_msg} after {MAX_RETRIES} attempts"));
                    }
                }
            }
        }
        Err(anyhow::anyhow!("Exhausted all connection retries."))
    }

    async fn new(config: Config) -> Result<Self> {
        let source_conn =
            Self::connect_with_retry(&config.source_dsn, "Failed to connect to source RabbitMQ")
                .await?;

        let source_channel = source_conn
            .create_channel()
            .await
            .context("Failed to create source channel")?;

        let target_conn =
            Self::connect_with_retry(&config.target_dsn, "Failed to connect to target RabbitMQ")
                .await?;

        let target_channel = target_conn
            .create_channel()
            .await
            .context("Failed to create target channel")?;

        source_channel
            .basic_qos(1, BasicQosOptions::default())
            .await
            .context("Failed to set QoS")?;

        info!("Successfully connected to both RabbitMQ instances");

        Ok(Self {
            source_channel,
            target_channel,
            source_connection: source_conn,
            target_connection: target_conn,
            config,
        })
    }

    /// Check if connections are still alive
    fn is_connected(&self) -> bool {
        self.source_connection.status().connected() && self.target_connection.status().connected()
    }

    async fn run(&self) -> Result<()> {
        info!(
            "Starting to consume from queue '{}'",
            self.config.source_queue
        );

        let mut consumer = self
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

        while let Some(delivery_result) = consumer.next().await {
            // Check connection health before processing
            if !self.is_connected() {
                error!("Connection lost, stopping consumer loop");
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
                                        // Connection might be lost
                                        return Err(anyhow::anyhow!("Failed to ack: {e}"));
                                    }
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
                                return Err(anyhow::anyhow!("Failed to nack: {e}"));
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Error receiving message: {e}");
                    // Consumer error might indicate connection loss
                    return Err(anyhow::anyhow!("Consumer error: {e}"));
                }
            }
        }

        warn!("Consumer stream ended");
        Err(anyhow::anyhow!("Consumer stream ended unexpectedly"))
    }
}

async fn run_with_recovery(config: Config) -> Result<()> {
    const RECONNECT_DELAY: Duration = Duration::from_secs(5);

    loop {
        info!("Creating message bridge...");

        match MessageBridge::new(config.clone()).await {
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
            }
        }

        time::sleep(RECONNECT_DELAY).await;
        info!("Attempting to reconnect...");
    }
}

// Make Config cloneable for recovery loop
impl Clone for Config {
    fn clone(&self) -> Self {
        Self {
            source_dsn: self.source_dsn.clone(),
            source_queue: self.source_queue.clone(),
            target_dsn: self.target_dsn.clone(),
            target_exchange: self.target_exchange.clone(),
            target_routing_key: self.target_routing_key.clone(),
        }
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

    // Load environment variables from .env file if present
    let _ = dotenvy::dotenv();

    info!("Starting RabbitMQ message bridge with auto-recovery");

    let config = Config::from_env().context("Failed to load configuration")?;

    info!("Configuration loaded:");
    info!("  Source queue: {}", config.source_queue);
    info!("  Target exchange: {}", config.target_exchange);
    info!("  Target routing key: {}", config.target_routing_key);

    // Handle graceful shutdown
    let shutdown = tokio::signal::ctrl_c();
    tokio::select! {
        result = run_with_recovery(config) => {
            result.context("Recovery loop failed")?;
        }
        _ = shutdown => {
            info!("Received shutdown signal, exiting gracefully");
        }
    }

    Ok(())
}
