use futures::StreamExt;

use anyhow::{Context, Result};
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicNackOptions, BasicPublishOptions, BasicQosOptions,
};
use lapin::{Channel, Connection, ConnectionProperties, types::FieldTable};
use tokio::time::{self, Duration};
use tracing::{error, info, warn};
// Required for async sleep operations

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
    config: Config,
}

impl MessageBridge {
    /// Attempts to connect to a RabbitMQ instance, retrying up to 10 times with a 5-second delay.
    async fn connect_with_retry(dsn: &str, context_msg: &str) -> Result<Connection> {
        const MAX_RETRIES: u8 = 10;
        const RETRY_DELAY: Duration = Duration::from_secs(5);

        for attempt in 1..=MAX_RETRIES {
            info!(
                "Attempting connection to RabbitMQ ({}) - attempt {}/{}",
                dsn, attempt, MAX_RETRIES
            );

            // Attempt to connect
            match Connection::connect(dsn, ConnectionProperties::default()).await {
                Ok(conn) => {
                    info!("Successfully connected on attempt {}", attempt);
                    return Ok(conn);
                }
                Err(e) => {
                    warn!("{}: Failed on attempt {}: {}", context_msg, attempt, e);
                    if attempt < MAX_RETRIES {
                        // Wait for 5 seconds before the next retry
                        time::sleep(RETRY_DELAY).await;
                    } else {
                        // All attempts failed, return the final error
                        return Err(e)
                            .context(format!("{context_msg} after {MAX_RETRIES} attempts"));
                    }
                }
            }
        }
        // Should be unreachable if logic is sound, but provides a fallback error.
        Err(anyhow::anyhow!("Exhausted all connection retries."))
    }

    async fn new(config: Config) -> Result<Self> {
        // Use the retry logic for the source connection
        let source_conn =
            Self::connect_with_retry(&config.source_dsn, "Failed to connect to source RabbitMQ")
                .await?;

        let source_channel = source_conn
            .create_channel()
            .await
            .context("Failed to create source channel")?;

        // Use the retry logic for the target connection
        let target_conn =
            Self::connect_with_retry(&config.target_dsn, "Failed to connect to target RabbitMQ")
                .await?;

        let target_channel = target_conn
            .create_channel()
            .await
            .context("Failed to create target channel")?;

        // Set QoS to process one message at a time
        source_channel
            .basic_qos(1, BasicQosOptions::default())
            .await
            .context("Failed to set QoS")?;

        info!("Successfully connected to both RabbitMQ instances");

        Ok(Self {
            source_channel,
            target_channel,
            config,
        })
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
            match delivery_result {
                Ok(delivery) => {
                    let data = delivery.data.clone();
                    let properties = delivery.properties.clone();

                    info!(
                        "Received message: {} bytes, delivery_tag: {}",
                        data.len(),
                        delivery.delivery_tag
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

                                    // Acknowledge the source message only after successful publish
                                    if let Err(e) = delivery.ack(BasicAckOptions::default()).await {
                                        error!("Failed to acknowledge message: {}", e);
                                    }
                                }
                                Err(e) => {
                                    error!("Publisher confirmation failed: {}", e);
                                    // Reject and requeue the message
                                    if let Err(e) = delivery
                                        .nack(BasicNackOptions {
                                            requeue: true,
                                            multiple: false,
                                        })
                                        .await
                                    {
                                        error!("Failed to nack message: {}", e);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("Failed to publish message: {}", e);
                            // Reject and requeue the message
                            if let Err(e) = delivery
                                .nack(BasicNackOptions {
                                    requeue: true,
                                    multiple: false,
                                })
                                .await
                            {
                                error!("Failed to nack message: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Error receiving message: {}", e);
                    // This is the consumer stream error, adding a small backoff before trying to consume the next message.
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }

        warn!("Consumer stream ended");
        Ok(())
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

    info!("Starting RabbitMQ message bridge");

    let config = Config::from_env().context("Failed to load configuration")?;

    info!("Configuration loaded:");
    info!("  Source queue: {}", config.source_queue);
    info!("  Target exchange: {}", config.target_exchange);
    info!("  Target routing key: {}", config.target_routing_key);

    let bridge = MessageBridge::new(config)
        .await
        .context("Failed to initialize message bridge")?;

    // Handle graceful shutdown
    let shutdown = tokio::signal::ctrl_c();
    tokio::select! {
        result = bridge.run() => {
            result.context("Bridge encountered an error")?;
        }
        _ = shutdown => {
            info!("Received shutdown signal, exiting gracefully");
        }
    }

    Ok(())
}
