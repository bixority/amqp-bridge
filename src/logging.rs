use serde_json::json;
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    PythonJson,
    Pretty,
}

impl LogFormat {
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_get_var(|k| std::env::var(k).ok())
    }

    fn from_get_var<F>(get_var: F) -> Self
    where
        F: FnOnce(&str) -> Option<String>,
    {
        match get_var("LOG_FORMAT")
            .unwrap_or_else(|| "json".to_string())
            .to_lowercase()
            .as_str()
        {
            "json" => LogFormat::PythonJson,
            _ => LogFormat::Pretty,
        }
    }
}

struct PythonJsonFormatter;

impl<S, N> FormatEvent<S, N> for PythonJsonFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let metadata = event.metadata();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();

        let levelname = match *metadata.level() {
            tracing::Level::ERROR => "ERROR",
            tracing::Level::WARN => "WARNING",
            tracing::Level::INFO => "INFO",
            tracing::Level::DEBUG | tracing::Level::TRACE => "DEBUG",
        };

        let target = metadata.target();

        // Collect all event fields, including any error carrier.
        let mut fields = serde_json::Map::new();
        let mut message = String::new();
        let mut visitor = FieldVisitor {
            fields: &mut fields,
            message: &mut message,
            error: None,
        };
        event.record(&mut visitor);
        let error = visitor.error;

        let timestamp = {
            let secs = now.as_secs();
            let micros = now.subsec_micros();
            let (ss, mm, hh) = (secs % 60, (secs / 60) % 60, (secs / 3600) % 24);
            let z = secs / 86400 + 719_468;
            let era = z / 146_097;
            let doe = z % 146_097;
            let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let (d, m) = (
                doy - (153 * mp + 2) / 5 + 1,
                if mp < 10 { mp + 3 } else { mp - 9 },
            );
            let y = yoe + era * 400 + u64::from(m <= 2);
            format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{micros:06}Z")
        };

        let mut log_json = json!({
            "message": message,
            "level": levelname,
            "logger": target,
            "timestamp": timestamp,
        });

        // Append error fields matching Python's exc_type / exc_value / exc_traceback.
        // Present only when the event carries an error field.
        if let Some(err) = error
            && let Some(obj) = log_json.as_object_mut()
        {
            obj.insert("exc_type".to_string(), json!(err.exc_type));
            obj.insert("exc_value".to_string(), json!(err.value));
            obj.insert("exc_traceback".to_string(), json!(err.traceback));
        }

        // Remaining custom fields (all non-error, non-message fields).
        if let Some(obj) = log_json.as_object_mut() {
            for (key, value) in fields {
                obj.insert(key, value);
            }
        }

        let line = serde_json::to_string(&log_json)
            .unwrap_or_else(|_| "{\"msg\":\"log serialization failed\"}".to_string());

        writeln!(writer, "{line}")?;

        Ok(())
    }
}

struct ErrorInfo {
    /// Approximated type name. When recorded via `record_error` this comes from the
    /// Debug output prefix (e.g. `"Os"` for `std::io::Error`). When recorded via
    /// `record_debug` (%err / ?err) it falls back to the field name (`"err"`).
    exc_type: String,
    /// `Display` string of the top-level error.
    value: String,
    /// Full cause chain, one line per `.source()` level — mirrors Python's
    /// `traceback.format_exception` chained output:
    ///
    /// ```text
    /// connection refused
    /// Caused by: Os { code: 111, kind: ConnectionRefused, ... }
    /// ```
    traceback: String,
}

struct FieldVisitor<'a> {
    fields: &'a mut serde_json::Map<String, serde_json::Value>,
    message: &'a mut String,
    error: Option<ErrorInfo>,
}

impl tracing::field::Visit for FieldVisitor<'_> {
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        let name = field.name();
        if name == "message" {
            *self.message = value.to_string();
        } else {
            self.fields.insert(name.to_string(), json!(value));
        }
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        let exc_value = value.to_string();

        // Walk the full source chain — equivalent to Python's chained exception output.
        let mut chain_lines = vec![exc_value.clone()];
        let mut cause = value.source();
        while let Some(src) = cause {
            chain_lines.push(format!("Caused by: {src}"));
            cause = src.source();
        }
        let traceback = chain_lines.join("\n");

        // Extract a type-name prefix from the Debug output.
        // For `std::io::Error` this gives `"Os"`, for custom types their struct name.
        let debug_repr = format!("{value:?}");
        let exc_type = debug_repr
            .split([' ', '(', '{'])
            .next()
            .unwrap_or(field.name())
            .to_string();

        self.error = Some(ErrorInfo {
            exc_type,
            value: exc_value,
            traceback,
        });
    }

    /// Handles `?value` (Debug) and `%value` (Display routed through Debug by tracing).
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        let value_str = format!("{value:?}");

        if name == "message" {
            *self.message = value_str.trim_matches('"').to_string();
            return;
        }

        // Fields named "err" or "error" recorded with %err / ?err land here.
        // tracing wraps Display strings in quotes when routing through record_debug,
        // so trim them to get the original Display output.
        if name == "err" || name == "error" {
            let display_value = value_str.trim_matches('"').to_string();

            // Check for an explicit sibling cause field the caller may have attached:
            //   tracing::error!(err = %e, err.source = ?e.source(), "...");
            let source_key = format!("{name}.source");
            let extra_cause = self
                .fields
                .get(&source_key)
                .and_then(|v| v.as_str())
                .map(str::to_string);

            let mut chain_lines = vec![display_value.clone()];
            if let Some(src) = extra_cause {
                chain_lines.push(format!("Caused by: {src}"));
            }

            self.error = Some(ErrorInfo {
                // Field name is the best type approximation available here,
                // since the concrete type is erased. Use record_error for richer output.
                exc_type: name.to_string(),
                value: display_value,
                traceback: chain_lines.join("\n"),
            });
            return;
        }

        if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&value_str) {
            self.fields.insert(name.to_string(), json_val);
        } else {
            let clean = value_str.trim_matches('"').to_string();
            self.fields.insert(name.to_string(), json!(clean));
        }
    }
}

/// Initialize the tracing subscriber based on the desired format.
pub fn init_logging(format: LogFormat) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    match format {
        LogFormat::PythonJson => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .event_format(PythonJsonFormatter)
                        .with_writer(std::io::stdout),
                )
                .init();
        }
        LogFormat::Pretty => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn default_is_python_json() {
        assert_eq!(
            LogFormat::from_get_var(|_| None),
            LogFormat::PythonJson
        );
    }

    #[test]
    fn json_is_python_json_case_insensitive() {
        assert_eq!(
            LogFormat::from_get_var(|_| Some("JSON".to_string())),
            LogFormat::PythonJson
        );
        assert_eq!(
            LogFormat::from_get_var(|_| Some("json".to_string())),
            LogFormat::PythonJson
        );
    }

    #[test]
    fn unknown_values_are_pretty() {
        assert_eq!(
            LogFormat::from_get_var(|_| Some("pretty".to_string())),
            LogFormat::Pretty
        );
        assert_eq!(
            LogFormat::from_get_var(|_| Some("anything".to_string())),
            LogFormat::Pretty
        );
    }

    /// Minimal error types for testing the cause chain logic.
    #[derive(Debug)]
    struct LeafError;
    impl std::fmt::Display for LeafError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "leaf error occurred")
        }
    }
    impl std::error::Error for LeafError {}

    #[derive(Debug)]
    struct WrappedError(LeafError);
    impl std::fmt::Display for WrappedError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "wrapped: {}", self.0)
        }
    }
    impl std::error::Error for WrappedError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    /// Exercises the cause-chain walk in isolation — the same logic used in `record_error`.
    #[test]
    fn cause_chain_single_error() {
        let err = LeafError;
        let mut chain = vec![err.to_string()];
        let mut cause: Option<&(dyn Error + 'static)> = err.source();
        while let Some(src) = cause {
            chain.push(format!("Caused by: {src}"));
            cause = src.source();
        }
        let tb = chain.join("\n");
        assert_eq!(tb, "leaf error occurred");
    }

    #[test]
    fn cause_chain_wrapped_error_includes_all_sources() {
        let err = WrappedError(LeafError);
        let mut chain = vec![err.to_string()];
        let mut cause: Option<&(dyn Error + 'static)> = err.source();
        while let Some(src) = cause {
            chain.push(format!("Caused by: {src}"));
            cause = src.source();
        }
        let tb = chain.join("\n");
        assert_eq!(
            tb,
            "wrapped: leaf error occurred\nCaused by: leaf error occurred"
        );
    }

    /// Verifies that the Debug-prefix type extraction works for a known type.
    #[test]
    fn exc_type_extracted_from_debug_prefix() {
        let err = LeafError;
        let debug_repr = format!("{err:?}");
        let exc_type = debug_repr
            .split([' ', '(', '{'])
            .next()
            .unwrap_or("unknown")
            .to_string();
        assert_eq!(exc_type, "LeafError");
    }
}
