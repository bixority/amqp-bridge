use crate::conf::Config;
use crate::health::{HealthStatus, SharedHealthState};
use anyhow::{Context, Error};
use futures::StreamExt;
use lapin::message::Delivery;
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicNackOptions, BasicPublishOptions, BasicQosOptions,
};
use lapin::publisher_confirm::PublisherConfirm;
use lapin::types::{DeliveryTag, FieldTable};
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
    /// Categorizes connection errors for better diagnostics
    fn categorize_error(error: &lapin::Error) -> &'static str {
        let error_string = format!("{error:?}");

        if error_string.contains("ConnectionRefused") || error_string.contains("Connection refused")
        {
            "connection_refused"
        } else if error_string.contains("ACCESSREFUSED") || error_string.contains("ACCESS_REFUSED")
        {
            "access_refused"
        } else if error_string.contains("timeout") || error_string.contains("Timeout") {
            "timeout"
        } else if error_string.contains("resolution") || error_string.contains("resolve") {
            "dns_resolution"
        } else {
            "unknown"
        }
    }

    /// Logs helpful hints based on the error category
    fn log_error_hint(error_category: &str, context_msg: &str) {
        match error_category {
            "connection_refused" => {
                warn!(
                    target = context_msg,
                    hint = "AMQP service may not be running or firewall blocking",
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
                    hint = "No response from AMQP server",
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
    }

    /// Logs connection failure with detailed error information
    fn log_connection_failure(
        e: &lapin::Error,
        attempt: u8,
        max_retries: u8,
        sanitized_uri: &str,
        context_msg: &str,
    ) {
        let error_message = format!("{e}");
        let error_category = Self::categorize_error(e);

        error!(
            target = context_msg,
            attempt = attempt,
            max_retries = max_retries,
            uri = %sanitized_uri,
            error_type = error_category,
            error_message = %error_message,
            is_io_error = e.is_io_error(),
            "Connection failed"
        );

        Self::log_error_hint(error_category, context_msg);
    }

    /// Attempts to connect to a `AMQP` instance,
    /// retrying up to 10 times with exponential backoff.
    async fn connect_with_retry(uri: &str, context_msg: &str) -> anyhow::Result<Connection> {
        const MAX_RETRIES: u8 = 10;
        const INITIAL_DELAY: Duration = Duration::from_secs(1);
        const MAX_DELAY: Duration = Duration::from_secs(30);

        let mut delay = INITIAL_DELAY;
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
                    Self::log_connection_failure(
                        &e,
                        attempt,
                        MAX_RETRIES,
                        &sanitized_uri,
                        context_msg,
                    );

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

        info!("Connecting to SOURCE AMQP");
        let source_conn = Self::connect_with_retry(&config.source_dsn, "source_amqp")
            .await
            .context("Source AMQP connection failed")?;

        let source_channel = source_conn
            .create_channel()
            .await
            .context("Failed to create source channel")?;

        info!("Connecting to TARGET AMQP");
        let target_conn = Self::connect_with_retry(&config.target_dsn, "target_amqp")
            .await
            .context("Target AMQP connection failed")?;

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
            "Successfully connected to both AMQP instances"
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

    /// Creates a preview of message content for logging
    fn create_message_preview(data: &[u8]) -> String {
        match std::str::from_utf8(data) {
            Ok(s) => {
                if s.len() > 200 {
                    format!("{}...", &s[..200])
                } else {
                    s.to_string()
                }
            }
            Err(_) => "<binary data>".to_string(),
        }
    }

    /// Handles acknowledgment after successful publish
    async fn handle_ack(
        &self,
        delivery: &Delivery,
        delivery_tag: DeliveryTag,
        message_count: &mut u64,
    ) -> Result<(), Error> {
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

        *message_count += 1;
        self.update_message_timestamp().await;
        Ok(())
    }

    /// Handles negative acknowledgment (requeue)
    async fn handle_nack(
        &self,
        delivery: &Delivery,
        delivery_tag: DeliveryTag,
    ) -> Result<(), Error> {
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
        Ok(())
    }

    /// Handles publisher confirmation
    async fn handle_publish_confirmation(
        &self,
        confirm: PublisherConfirm,
        delivery: &Delivery,
        delivery_tag: DeliveryTag,
        message_size: usize,
        message_count: &mut u64,
    ) -> Result<(), Error> {
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

                self.handle_ack(delivery, delivery_tag, message_count)
                    .await?;
            }
            Err(e) => {
                error!(
                    event = "publish_confirmation_failed",
                    delivery_tag = delivery_tag,
                    error = %e,
                    "Publisher confirmation failed"
                );
                self.handle_nack(delivery, delivery_tag).await?;
            }
        }
        Ok(())
    }

    /// Processes a single message delivery
    async fn process_message(
        &self,
        delivery: Delivery,
        message_count: &mut u64,
    ) -> Result<(), Error> {
        let data = delivery.data.clone();
        let properties = delivery.properties.clone();
        let message_size = data.len();
        let delivery_tag = delivery.delivery_tag;
        let message_preview = Self::create_message_preview(&data);

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
                self.handle_publish_confirmation(
                    confirm,
                    &delivery,
                    delivery_tag,
                    message_size,
                    message_count,
                )
                .await?;
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
                self.handle_nack(&delivery, delivery_tag).await?;
            }
        }

        Ok(())
    }

    async fn consume(&self, mut consumer: Consumer) -> Result<(), Error> {
        let mut message_count: u64 = 0;

        while let Some(delivery_result) = consumer.next().await {
            // Check connection health before processing
            if !self.is_connected() {
                error!(
                    event = "connection_lost",
                    messages_processed = message_count,
                    "Connection lost, stopping consumer loop"
                );
                self.mark_unhealthy().await;
                return Err(anyhow::anyhow!("Connection lost during message processing"));
            }

            match delivery_result {
                Ok(delivery) => {
                    self.process_message(delivery, &mut message_count).await?;
                }
                Err(e) => {
                    error!(
                        event = "consumer_error",
                        error = %e,
                        messages_processed = message_count,
                        "Error receiving message"
                    );
                    self.mark_unhealthy().await;
                    return Err(anyhow::anyhow!("Consumer error: {e}"));
                }
            }
        }

        warn!(
            event = "consumer_stream_ended",
            messages_processed = message_count,
            "Consumer stream ended"
        );

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
    let Ok(mut parsed_url) = url::Url::parse(uri) else {
        return uri.to_string();
    };

    if parsed_url.password().is_some() {
        let _ = parsed_url.set_password(Some("***"));
    }

    parsed_url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_uri_hides_password() {
        let uri = "amqp://user:secret@host:5672/vhost";
        let sanitized = sanitize_uri_for_logging(uri);
        assert!(!sanitized.contains("secret"));
        assert!(sanitized.contains("***"));
        assert!(sanitized.starts_with("amqp://user:"));
    }

    #[test]
    fn sanitize_uri_without_password_unchanged() {
        let uri = "amqp://user@host:5672/vhost";
        let sanitized = sanitize_uri_for_logging(uri);
        assert_eq!(sanitized, uri);
    }

    #[test]
    fn sanitize_uri_handles_invalid_url() {
        let uri = "not a url";
        let sanitized = sanitize_uri_for_logging(uri);
        assert_eq!(sanitized, uri);
    }

    #[test]
    fn create_message_preview_limits_length_and_handles_binary() {
        // UTF-8 long string
        let long = "a".repeat(300);
        let preview = MessageBridge::create_message_preview(long.as_bytes());
        assert!(preview.len() <= 203, "preview too long: {}", preview.len());
        assert!(preview.ends_with("..."));

        // Short UTF-8 string
        let short = "hello";
        let preview = MessageBridge::create_message_preview(short.as_bytes());
        assert_eq!(preview, short);

        // Non-UTF8 bytes
        let bin = &[0xFF, 0xFE, 0xFD];
        let preview = MessageBridge::create_message_preview(bin);
        assert_eq!(preview, "<binary data>");
    }
}
