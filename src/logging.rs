//! Logging: readable lines on stdout + daily JSON files (7 days kept).
//! `LOG_LEVEL` (error|warn|info|debug|trace, default info), `LOG_DIR`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

struct LocalTime;

impl FormatTime for LocalTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z"))
    }
}

fn level() -> String {
    let raw = std::env::var("LOG_LEVEL").unwrap_or_default().to_lowercase();
    // Accept the winston names used by the Node version too.
    match raw.as_str() {
        "error" | "warn" | "info" | "debug" | "trace" => raw,
        "http" | "verbose" => "debug".into(),
        "silly" => "trace".into(),
        _ => "info".into(),
    }
}

/// One activity line for the GUI.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
#[derive(Clone)]
pub struct LogLine {
    pub time: String,
    pub level: Level,
    pub target: String,
    pub message: String,
    pub fields: String,
}

const GUI_LOG_MAX: usize = 500;
/// Last log lines, newest at the back (read by the GUI).
pub static GUI_LOG: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());

#[derive(Default)]
struct LineVisitor {
    message: String,
    fields: Vec<String>,
}

impl Visit for LineVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={}", field.name(), value));
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={:?}", field.name(), value));
        }
    }
}

struct GuiLayer;

impl<S: Subscriber> Layer<S> for GuiLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut v = LineVisitor::default();
        event.record(&mut v);
        let line = LogLine {
            time: chrono::Local::now().format("%H:%M:%S").to_string(),
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            message: v.message,
            fields: v.fields.join("  "),
        };
        if let Ok(mut log) = GUI_LOG.lock() {
            if log.len() >= GUI_LOG_MAX {
                log.pop_front();
            }
            log.push_back(line);
        }
    }
}

/// Returns the guard that flushes the file writer; keep it alive.
pub fn init(default_dir: &Path) -> (Option<WorkerGuard>, Option<PathBuf>) {
    let lvl = level();
    let filter = || EnvFilter::new(format!("{lvl},hyper=warn,hyper_util=warn,reqwest=warn,rustls=warn,h2=warn"));

    let console = fmt::layer().with_timer(LocalTime).with_ansi(cfg!(not(windows))).with_filter(filter());
    // Activity panel of the GUI: info and above, never the debug payloads.
    let gui = GuiLayer.with_filter(EnvFilter::new("info,hyper=warn,hyper_util=warn,reqwest=warn,rustls=warn,h2=warn"));

    let dir = std::env::var_os("LOG_DIR").map(PathBuf::from).unwrap_or_else(|| default_dir.join("logs"));
    let appender = std::fs::create_dir_all(&dir).ok().and_then(|_| {
        Builder::new()
            .rotation(Rotation::DAILY)
            .filename_prefix("mystampa")
            .filename_suffix("log")
            .max_log_files(7)
            .build(&dir)
            .ok()
    });

    match appender {
        Some(a) => {
            let (writer, guard) = tracing_appender::non_blocking(a);
            let file = fmt::layer().json().with_timer(LocalTime).with_writer(writer).with_filter(filter());
            tracing_subscriber::registry().with(console).with(gui).with(file).init();
            (Some(guard), Some(dir))
        }
        None => {
            tracing_subscriber::registry().with(console).with(gui).init();
            tracing::warn!(target: "logger", dir = %dir.display(), "Log directory not writable, logging to stdout only");
            (None, None)
        }
    }
}
