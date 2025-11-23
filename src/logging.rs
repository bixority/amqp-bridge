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
    pub fn from_env() -> Self {
        match std::env::var("LOG_FORMAT")
            .unwrap_or_else(|_| "json".to_string())
            .to_lowercase()
            .as_str()
        {
            "json" => LogFormat::PythonJson,
            _ => LogFormat::Pretty,
        }
    }
}

/// Custom formatter that outputs JSON in Python logging format
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

        // Get timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let created = now.as_secs_f64();
        let msecs = now.as_millis() % 1000;

        // Map tracing level to Python logging level
        let (levelname, levelno) = match *metadata.level() {
            tracing::Level::ERROR => ("ERROR", 40),
            tracing::Level::WARN => ("WARNING", 30),
            tracing::Level::INFO => ("INFO", 20),
            tracing::Level::DEBUG | tracing::Level::TRACE => ("DEBUG", 10),
        };

        // Get file, line, and module info
        let file = metadata.file().unwrap_or("unknown");
        let line = metadata.line().unwrap_or(0);
        let module = metadata.module_path().unwrap_or("unknown");
        let target = metadata.target();

        // Extract filename from path
        let filename = file.split('/').next_back().unwrap_or(file);

        // Get thread info
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("unnamed");
        let thread_id = format!("{:?}", thread.id());

        // Get process info
        let process_id = std::process::id();

        // Collect field data
        let mut fields = serde_json::Map::new();
        let mut message = String::new();

        // Visit event fields
        event.record(&mut FieldVisitor {
            fields: &mut fields,
            message: &mut message,
        });

        // Build the JSON object matching Python's structure
        let mut log_json = json!({
            "name": target,
            "msg": message,
            "args": [],
            "levelname": levelname,
            "levelno": levelno,
            "pathname": file,
            "filename": filename,
            "module": module,
            "exc_info": null,
            "exc_text": null,
            "stack_info": null,
            "lineno": line,
            "funcName": metadata.name(),
            "created": created,
            "msecs": msecs as u64,
            "thread": thread_id,
            "threadName": thread_name,
            "processName": "MainProcess",
            "process": process_id,
        });

        // Add custom fields
        if let Some(obj) = log_json.as_object_mut() {
            for (key, value) in fields {
                obj.insert(key, value);
            }
        }

        writeln!(writer, "{}", serde_json::to_string(&log_json).unwrap())?;
        Ok(())
    }
}

/// Visitor to extract fields from tracing events
struct FieldVisitor<'a> {
    fields: &'a mut serde_json::Map<String, serde_json::Value>,
    message: &'a mut String,
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

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        let value_str = format!("{value:?}");

        // Special handling for 'message' field
        if name == "message" {
            *self.message = value_str.trim_matches('"').to_string();
        } else {
            // Try to parse as JSON value
            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&value_str) {
                self.fields.insert(name.to_string(), json_val);
            } else {
                // Store as string, removing surrounding quotes if present
                let clean_value = value_str.trim_matches('"').to_string();
                self.fields.insert(name.to_string(), json!(clean_value));
            }
        }
    }
}

/// Initialize the tracing subscriber based on the desired format
pub fn init_logging(format: LogFormat) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    match format {
        LogFormat::PythonJson => {
            // Python-compatible JSON format
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

    fn lock_env() -> &'static Mutex<()> {
        ENV_MUTEX.get_or_init(|| Mutex::new(()))
    }

    fn unset_log_format() {
        unsafe { std::env::remove_var("LOG_FORMAT") };
    }

    #[test]
    fn default_is_python_json() {
        let _g = lock_env().lock().unwrap();
        unset_log_format();
        assert_eq!(LogFormat::from_env(), LogFormat::PythonJson);
    }

    #[test]
    fn json_is_python_json_case_insensitive() {
        let _g = lock_env().lock().unwrap();
        unsafe { std::env::set_var("LOG_FORMAT", "JSON") };
        assert_eq!(LogFormat::from_env(), LogFormat::PythonJson);

        unsafe { std::env::set_var("LOG_FORMAT", "json") };
        assert_eq!(LogFormat::from_env(), LogFormat::PythonJson);
    }

    #[test]
    fn unknown_values_are_pretty() {
        let _g = lock_env().lock().unwrap();
        unsafe { std::env::set_var("LOG_FORMAT", "pretty") };
        assert_eq!(LogFormat::from_env(), LogFormat::Pretty);

        unsafe { std::env::set_var("LOG_FORMAT", "anything") };
        assert_eq!(LogFormat::from_env(), LogFormat::Pretty);
    }
}
