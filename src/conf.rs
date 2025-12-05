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
    /// Build a configuration from environment variables.
    ///
    /// Required variables: `SOURCE_DSN`, `TARGET_DSN`.
    /// Optional variables: `SOURCE_QUEUE`, `TARGET_EXCHANGE`, `TARGET_ROUTING_KEY`, `HEALTH_PORT`.
    ///
    /// # Errors
    /// Returns an error if required environment variables are missing or if
    /// `HEALTH_PORT` cannot be parsed to a valid `u16` when provided.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    // Global mutex to serialize environment-variable dependent tests
    static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_MUTEX.get_or_init(|| Mutex::new(()))
    }

    fn clear_env() {
        for (k, _) in std::env::vars() {
            // Only clear the vars we might read to avoid surprising the environment
            match k.as_str() {
                "SOURCE_DSN" | "SOURCE_QUEUE" | "TARGET_DSN" | "TARGET_EXCHANGE"
                | "TARGET_ROUTING_KEY" | "HEALTH_PORT" => {
                    unsafe { std::env::remove_var(k) };
                }
                _ => {}
            }
        }
    }

    #[test]
    fn from_env_errors_when_required_missing() {
        let _g = env_lock().lock().unwrap();
        clear_env();

        let err = Config::from_env().unwrap_err();
        let s = format!("{err:#}");
        assert!(s.contains("SOURCE_DSN"));

        // Only SOURCE_DSN set -> still missing TARGET_DSN
        unsafe { std::env::set_var("SOURCE_DSN", "amqp://host") };
        let err = Config::from_env().unwrap_err();
        let s = format!("{err:#}");
        assert!(s.contains("TARGET_DSN"));
    }

    #[test]
    fn from_env_uses_defaults_for_optionals() {
        let _g = env_lock().lock().unwrap();
        clear_env();

        unsafe { std::env::set_var("SOURCE_DSN", "amqp://src") };
        unsafe { std::env::set_var("TARGET_DSN", "amqp://dst") };

        let cfg = Config::from_env().expect("should parse");
        assert_eq!(cfg.source_queue, "old");
        assert_eq!(cfg.target_exchange, "new_xchg");
        assert_eq!(cfg.target_routing_key, "update");
        assert_eq!(cfg.health_port, 8080);
    }

    #[test]
    fn from_env_parses_overrides() {
        let _g = env_lock().lock().unwrap();
        clear_env();

        unsafe { std::env::set_var("SOURCE_DSN", "amqp://src") };
        unsafe { std::env::set_var("TARGET_DSN", "amqp://dst") };
        unsafe { std::env::set_var("SOURCE_QUEUE", "q1") };
        unsafe { std::env::set_var("TARGET_EXCHANGE", "ex1") };
        unsafe { std::env::set_var("TARGET_ROUTING_KEY", "rk1") };
        unsafe { std::env::set_var("HEALTH_PORT", "9000") };

        let cfg = Config::from_env().expect("should parse");
        assert_eq!(cfg.source_queue, "q1");
        assert_eq!(cfg.target_exchange, "ex1");
        assert_eq!(cfg.target_routing_key, "rk1");
        assert_eq!(cfg.health_port, 9000);
    }
}
