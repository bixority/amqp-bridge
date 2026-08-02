use crate::error::{BridgeError, Result};

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
    pub fn from_env() -> Result<Self> {
        Self::from_get_var(|k| std::env::var(k).ok())
    }

    fn from_get_var<F>(get_var: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        Ok(Self {
            source_dsn: get_var("SOURCE_DSN")
                .ok_or_else(|| BridgeError::Config("SOURCE_DSN environment variable not set".to_string()))?,
            source_queue: get_var("SOURCE_QUEUE").unwrap_or_else(|| "old".to_string()),
            target_dsn: get_var("TARGET_DSN")
                .ok_or_else(|| BridgeError::Config("TARGET_DSN environment variable not set".to_string()))?,
            target_exchange: get_var("TARGET_EXCHANGE").unwrap_or_else(|| "new_xchg".to_string()),
            target_routing_key: get_var("TARGET_ROUTING_KEY")
                .unwrap_or_else(|| "update".to_string()),
            health_port: get_var("HEALTH_PORT")
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn from_env_errors_when_required_missing() {
        let env = HashMap::<&str, &str>::new();
        let get_var = |k: &str| env.get(k).map(ToString::to_string);

        let res = Config::from_get_var(get_var);
        assert!(res.is_err());
        if let Err(err) = res {
            let s = format!("{err:#}");
            assert!(s.contains("SOURCE_DSN"));
        }

        // Only SOURCE_DSN set -> still missing TARGET_DSN
        let mut env = HashMap::new();
        env.insert("SOURCE_DSN", "amqp://host");
        let get_var = |k: &str| env.get(k).map(ToString::to_string);
        let res = Config::from_get_var(get_var);
        assert!(res.is_err());
        if let Err(err) = res {
            let s = format!("{err:#}");
            assert!(s.contains("TARGET_DSN"));
        }
    }

    #[test]
    fn from_env_uses_defaults_for_optionals() -> Result<()> {
        let mut env = HashMap::new();
        env.insert("SOURCE_DSN", "amqp://src");
        env.insert("TARGET_DSN", "amqp://dst");
        let get_var = |k: &str| env.get(k).map(ToString::to_string);

        let cfg = Config::from_get_var(get_var)?;
        assert_eq!(cfg.source_queue, "old");
        assert_eq!(cfg.target_exchange, "new_xchg");
        assert_eq!(cfg.target_routing_key, "update");
        assert_eq!(cfg.health_port, 8080);
        Ok(())
    }

    #[test]
    fn from_env_parses_overrides() -> Result<()> {
        let mut env = HashMap::new();
        env.insert("SOURCE_DSN", "amqp://src");
        env.insert("TARGET_DSN", "amqp://dst");
        env.insert("SOURCE_QUEUE", "q1");
        env.insert("TARGET_EXCHANGE", "ex1");
        env.insert("TARGET_ROUTING_KEY", "rk1");
        env.insert("HEALTH_PORT", "9000");
        let get_var = |k: &str| env.get(k).map(ToString::to_string);

        let cfg = Config::from_get_var(get_var)?;
        assert_eq!(cfg.source_queue, "q1");
        assert_eq!(cfg.target_exchange, "ex1");
        assert_eq!(cfg.target_routing_key, "rk1");
        assert_eq!(cfg.health_port, 9000);
        Ok(())
    }
}
