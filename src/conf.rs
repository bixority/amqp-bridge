use anyhow::Context;

#[derive(Debug, Clone)]
pub struct Config {
    pub source_dsn: String,
    pub source_queue: String,
    pub target_dsn: String,
    pub target_exchange: String,
    pub target_routing_key: String,
    pub health_port: u16,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
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
            health_port: std::env::var("HEALTH_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080),
        })
    }
}
