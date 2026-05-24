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

fn is_enabled() -> bool {
    *LOG_ENABLED.get_or_init(|| match std::env::var("XI_DEBUG") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v.is_empty() || v == "0" || v == "false" || v == "off")
        }
        Err(_) => false,
    })
}

struct JsonLineLogger {
    writer: Mutex<BufWriter<File>>,
}

impl Log for JsonLineLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let entry = serde_json::json!({
            "timestamp": Utc::now().to_rfc3339(),
            "level": record.level().as_str(),
            "target": record.target(),
            "message": record.args().to_string(),
        });
        if let Ok(mut w) = self.writer.lock() {
            let _ = writeln!(w, "{entry}");
            let _ = w.flush();
        }
    }

    fn flush(&self) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.flush();
        }
    }
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

    let logger = Box::new(JsonLineLogger {
        writer: Mutex::new(BufWriter::new(file)),
    });

    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Debug);
        let _ = LOG_INITIALIZED.set(());
    }
}
