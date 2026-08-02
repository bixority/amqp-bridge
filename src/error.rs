use thiserror::Error;

#[derive(Error, Debug)]
pub enum BridgeError {
    #[error("AMQP error: {0}")]
    Amqp(#[from] lapin::Error),

    #[error("{context}: {source}")]
    AmqpWithContext {
        context: String,
        #[source]
        source: lapin::Error,
    },

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{context}: {source}")]
    IoWithContext {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Health server error: {0}")]
    HealthServer(String),

    #[error("Connection lost during message processing")]
    ConnectionLost,

    #[error("Consumer error: {0}")]
    ConsumerError(String),

    #[error("Consumer stream ended unexpectedly")]
    ConsumerStreamEnded,

    #[error("Failed to acknowledge message: {0}")]
    AckError(String),

    #[error("Failed to negatively acknowledge message: {0}")]
    NackError(String),

    #[error("Message transformation failed: {0}")]
    TransformError(String),

    #[error("Failed to receive publish confirmation: {0}")]
    PublishConfirmationError(String),

    #[error("Exhausted all connection retries: {0}")]
    ConnectionRetriesExhausted(String),
}

pub type Result<T> = std::result::Result<T, BridgeError>;
