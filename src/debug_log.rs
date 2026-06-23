use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    sync::{Mutex, OnceLock},
};

use crate::dirs::PROJECT_DIRS;
use chrono::{Local, Utc};
use log::{LevelFilter, Log, Metadata, Record};

static LOG_ENABLED: OnceLock<bool> = OnceLock::new();
static LOG_INITIALIZED: OnceLock<()> = OnceLock::new();

/// Shared writer used by both the `log` backend and `log_structured`.
static WRITER: OnceLock<Mutex<BufWriter<File>>> = OnceLock::new();

fn is_enabled() -> bool {
    *LOG_ENABLED.get_or_init(|| match std::env::var("XI_DEBUG") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v.is_empty() || v == "0" || v == "false" || v == "off")
        }
        Err(_) => false,
    })
}

struct JsonLineLogger;

impl Log for JsonLineLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let entry = serde_json::json!({
            "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            "level": record.level().as_str(),
            "target": record.target(),
            "message": record.args().to_string(),
        });
        write_entry(&entry);
    }

    fn flush(&self) {
        if let Some(w) = WRITER.get()
            && let Ok(mut w) = w.lock()
        {
            let _ = w.flush();
        }
    }
}

fn write_entry(value: &serde_json::Value) {
    if let Some(w) = WRITER.get()
        && let Ok(mut w) = w.lock()
    {
        let _ = writeln!(w, "{value}");
        let _ = w.flush();
    }
}

/// Write a fully structured JSONL record directly to the debug log,
/// bypassing the `log` crate's string-message path.
///
/// `fields` should be a JSON object.  `timestamp`, `level`, and `target`
/// are injected automatically when not already present.  This is used
/// for LLM request/response payloads so they appear as native JSON
/// rather than as strings embedded inside a `"message"` field.
pub fn log_structured(level: log::Level, target: &str, mut fields: serde_json::Value) {
    if !is_enabled() {
        return;
    }
    if let Some(obj) = fields.as_object_mut() {
        obj.entry("timestamp").or_insert_with(|| {
            serde_json::Value::String(
                Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            )
        });
        obj.entry("level")
            .or_insert_with(|| serde_json::Value::String(level.as_str().to_owned()));
        obj.entry("target")
            .or_insert_with(|| serde_json::Value::String(target.to_owned()));
    }
    write_entry(&fields);
}

pub fn init_logging() {
    if !is_enabled() {
        return;
    }

    if LOG_INITIALIZED.get().is_some() {
        return;
    }

    let Some(dirs) = PROJECT_DIRS.as_ref() else {
        return;
    };
    let timestamp = Local::now().format("%Y%m%d-%H%M%S");
    let log_path = dirs.cache_dir().join(format!("xi-debug-{timestamp}.jsonl"));

    if let Some(parent) = log_path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return;
    }

    let Ok(file) = OpenOptions::new().create(true).append(true).open(log_path) else {
        return;
    };

    let _ = WRITER.set(Mutex::new(BufWriter::new(file)));

    let logger = Box::new(JsonLineLogger);

    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Debug);
        let _ = LOG_INITIALIZED.set(());
    }
}
