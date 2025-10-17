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

        info!(
            target = context_msg,
            uri = %sanitized_uri,
            "Starting connection attempts"
        );

        for attempt in 1..=MAX_RETRIES {
            info!(
                target = context_msg,
                attempt = attempt,
                max_retries = MAX_RETRIES,
                uri = %sanitized_uri,
                "Attempting connection"
            );

            match Connection::connect(uri, ConnectionProperties::default()).await {
                Ok(conn) => {
                    info!(
                        target = context_msg,
                        uri = %sanitized_uri,
                        attempt = attempt,
                        status = "success",
                        "Successfully connected"
                    );
                    return Ok(conn);
                }
                Err(e) => {
                    let error_string = format!("{e:?}");
                    let error_message = format!("{e}");

                    // Determine error category
                    let error_category = if error_string.contains("ConnectionRefused")
                        || error_string.contains("Connection refused")
                    {
                        "connection_refused"
                    } else if error_string.contains("ACCESSREFUSED")
                        || error_string.contains("ACCESS_REFUSED")
                    {
                        "access_refused"
                    } else if error_string.contains("timeout") || error_string.contains("Timeout") {
                        "timeout"
                    } else if error_string.contains("resolution")
                        || error_string.contains("resolve")
                    {
                        "dns_resolution"
                    } else {
                        "unknown"
                    };

                    error!(
                        target = context_msg,
                        attempt = attempt,
                        max_retries = MAX_RETRIES,
                        uri = %sanitized_uri,
                        error_type = error_category,
                        error_message = %error_message,
                        is_io_error = e.is_io_error(),
                        "Connection failed"
                    );

                    // Log helpful hints based on error type
                    match error_category {
                        "connection_refused" => {
                            warn!(
                                target = context_msg,
                                hint = "RabbitMQ service may not be running or firewall blocking",
                                "Connection refused detected"
                            );
                        }
                        "access_refused" => {
                            warn!(
                                target = context_msg,
                                hint = "Check username, password, and vhost permissions",
                                "Authentication failed"
                            );
                        }
                        "timeout" => {
                            warn!(
                                target = context_msg,
                                hint = "No response from RabbitMQ server",
                                "Connection timeout"
                            );
                        }
                        "dns_resolution" => {
                            warn!(
                                target = context_msg,
                                hint = "Cannot resolve hostname",
                                "DNS resolution failed"
                            );
                        }
                        _ => {}
                    }

                    if attempt < MAX_RETRIES {
                        warn!(
                            target = context_msg,
                            retry_delay_secs = delay.as_secs(),
                            next_attempt = attempt + 1,
                            "Retrying connection"
                        );
                        time::sleep(delay).await;
                        delay = std::cmp::min(delay * 2, MAX_DELAY);
                    } else {
                        error!(
                            target = context_msg,
                            attempts = MAX_RETRIES,
                            status = "failed",
                            "Exhausted all connection retries"
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

        info!(
            source_queue = %config.source_queue,
            target_exchange = %config.target_exchange,
            target_routing_key = %config.target_routing_key,
            "Starting MessageBridge initialization"
        );

        info!("Connecting to SOURCE RabbitMQ");
        let source_conn = Self::connect_with_retry(&config.source_dsn, "source_rabbitmq")
            .await
            .context("Source RabbitMQ connection failed")?;

        let source_channel = source_conn
            .create_channel()
            .await
            .context("Failed to create source channel")?;

        info!("Connecting to TARGET RabbitMQ");
        let target_conn = Self::connect_with_retry(&config.target_dsn, "target_rabbitmq")
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

        info!(
            status = "initialized",
            "Successfully connected to both RabbitMQ instances"
        );

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
        let source_connected = self.source_connection.status().connected();
        let target_connected = self.target_connection.status().connected();

        if !source_connected || !target_connected {
            warn!(
                source_connected = source_connected,
                target_connected = target_connected,
                "Connection status check"
            );
        }

        source_connected && target_connected
    }

    async fn mark_unhealthy(&self) {
        let mut state = self.health_state.write().await;
        state.liveness = HealthStatus::Unhealthy;
        state.readiness = HealthStatus::Unhealthy;

        error!(
            liveness = "unhealthy",
            readiness = "unhealthy",
            "Marked bridge as unhealthy"
        );
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
                    let message_size = data.len();
                    let delivery_tag = delivery.delivery_tag;

                    // Try to convert message to string for logging
                    let message_preview = match std::str::from_utf8(&data) {
                        Ok(s) => {
                            if s.len() > 200 {
                                format!("{}...", &s[..200])
                            } else {
                                s.to_string()
                            }
                        }
                        Err(_) => format!("<binary data>"),
                    };

                    info!(
                        event = "message_received",
                        message_size = message_size,
                        delivery_tag = delivery_tag,
                        content_type = if std::str::from_utf8(&data).is_ok() { "text" } else { "binary" },
                        preview = %message_preview,
                        "Received message"
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
                                    info!(
                                        event = "message_published",
                                        delivery_tag = delivery_tag,
                                        message_size = message_size,
                                        exchange = %self.config.target_exchange,
                                        routing_key = %self.config.target_routing_key,
                                        "Successfully published message"
                                    );

                                    if let Err(e) = delivery.ack(BasicAckOptions::default()).await {
                                        error!(
                                            event = "ack_failed",
                                            delivery_tag = delivery_tag,
                                            error = %e,
                                            "Failed to acknowledge message"
                                        );
                                        self.mark_unhealthy().await;
                                        return Err(anyhow::anyhow!("Failed to ack: {e}"));
                                    }

                                    // Update health timestamp after successful processing
                                    self.update_message_timestamp().await;
                                }
                                Err(e) => {
                                    error!(
                                        event = "publish_confirmation_failed",
                                        delivery_tag = delivery_tag,
                                        error = %e,
                                        "Publisher confirmation failed"
                                    );

                                    if let Err(e) = delivery
                                        .nack(BasicNackOptions {
                                            requeue: true,
                                            multiple: false,
                                        })
                                        .await
                                    {
                                        error!(
                                            event = "nack_failed",
                                            delivery_tag = delivery_tag,
                                            error = %e,
                                            "Failed to nack message"
                                        );
                                        self.mark_unhealthy().await;
                                        return Err(anyhow::anyhow!("Failed to nack: {e}"));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                event = "publish_failed",
                                delivery_tag = delivery_tag,
                                exchange = %self.config.target_exchange,
                                routing_key = %self.config.target_routing_key,
                                error = %e,
                                "Failed to publish message"
                            );

                            if let Err(e) = delivery
                                .nack(BasicNackOptions {
                                    requeue: true,
                                    multiple: false,
                                })
                                .await
                            {
                                error!(
                                    event = "nack_failed",
                                    delivery_tag = delivery_tag,
                                    error = %e,
                                    "Failed to nack message"
                                );
                                self.mark_unhealthy().await;
                                return Err(anyhow::anyhow!("Failed to nack: {e}"));
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(
                        event = "consumer_error",
                        error = %e,
                        "Error receiving message"
                    );
                    self.mark_unhealthy().await;
                    return Err(anyhow::anyhow!("Consumer error: {e}"));
                }
            }
        }

        Ok(())
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        info!(
            event = "consumer_starting",
            queue = %self.config.source_queue,
            consumer_tag = "bridge_consumer",
            "Starting consumer"
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

        info!(
            event = "consumer_started",
            status = "waiting",
            "Consumer started, waiting for messages"
        );

        self.consume(consumer).await?;

        warn!(
            event = "consumer_ended",
            "Consumer stream ended unexpectedly"
        );
        self.mark_unhealthy().await;
        Err(anyhow::anyhow!("Consumer stream ended unexpectedly"))
    }
}

/// Helper function to sanitize URI for logging (removes password)
fn sanitize_uri_for_logging(uri: &str) -> String {
    // Attempt to parse the URI string into a structured Url object
    let Ok(mut parsed_url) = url::Url::parse(uri) else {
        return uri.to_string();
    };

    // The Url object safely handles setting and clearing credentials.
    // If a password exists, this method removes it while keeping the username.
    if parsed_url.password().is_some() {
        // This setter safely replaces the password component.
        // If there is a username, it is preserved. If not, it's a no-op.
        // We replace the password with a placeholder string.
        let _ = parsed_url.set_password(Some("***"));
    }

    // Convert the modified Url object back to a String
    parsed_url.to_string()
}
