use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

impl LogFormat {
    pub fn from_env() -> Self {
        match std::env::var("LOG_FORMAT")
            .unwrap_or_else(|_| "json".to_string())
            .to_lowercase()
            .as_str()
        {
            "json" => LogFormat::Json,
            _ => LogFormat::Pretty,
        }
    }
}

/// Initialize the tracing subscriber based on the desired format
pub fn init_logging(format: LogFormat) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    match format {
        LogFormat::Json => {
            // JSON format for production/Loki
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .json()
                .with_current_span(true)
                .with_span_list(true)
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_file(true)
                .with_line_number(true)
                .init();
        }
        LogFormat::Pretty => {
            // Pretty format for development
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .pretty()
                .with_target(true)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(false)
                .init();
        }
    }
}
