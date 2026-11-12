//! Structured logging, title privacy masking, and runtime diagnostics/metrics.
//!
//! Per Section 6 & 7.3 of spec.md:
//! - Window titles are masked by default in logs (because trading terminals like MT5
//!   include account numbers in window titles). Full titles are logged only when
//!   verbose logging is explicitly enabled.
//! - High-frequency events (e.g. continuous mouse movement) must never be logged.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static VERBOSE_TITLE_LOGGING: AtomicBool = AtomicBool::new(false);
static LOG_FILE_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

/// Initialize the logger in `%LOCALAPPDATA%\SceneDock\scenedock.log` (or custom directory).
pub fn init_logger(app_data_dir: Option<PathBuf>, verbose_titles: bool) {
    VERBOSE_TITLE_LOGGING.store(verbose_titles, Ordering::Relaxed);
    let dir = app_data_dir.unwrap_or_else(default_app_data_dir);
    let _ = fs::create_dir_all(&dir);
    let log_path = dir.join("scenedock.log");
    let mutex = LOG_FILE_PATH.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = mutex.lock() {
        *guard = Some(log_path);
    }
}

pub fn set_verbose_title_logging(enabled: bool) {
    VERBOSE_TITLE_LOGGING.store(enabled, Ordering::Relaxed);
}

pub fn is_verbose_title_logging() -> bool {
    VERBOSE_TITLE_LOGGING.load(Ordering::Relaxed)
}

/// Mask a window title unless verbose logging is explicitly enabled.
/// Preserves only length and a safe prefix/redaction indicator so account numbers
/// in MT5/cTrader titles are never leaked to disk by default.
pub fn mask_title(title: &str) -> String {
    if is_verbose_title_logging() {
        return title.to_string();
    }
    let char_count = title.chars().count();
    if char_count == 0 {
        "<empty>".to_string()
    } else {
        format!("<masked:{}chars>", char_count)
    }
}

/// Write a structured log entry. Safe to call from anywhere; never panics.
pub fn log_event(level: LogLevel, category: &str, message: &str) {
    let ts = current_unix_ms();
    let line = format!("[{}] [{}] [{}] {}\n", ts, level.as_str(), category, message);

    #[cfg(debug_assertions)]
    eprint!("{}", line);

    if let Some(mutex) = LOG_FILE_PATH.get() {
        if let Ok(guard) = mutex.lock() {
            if let Some(ref path) = *guard {
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = file.write_all(line.as_bytes());
                }
            }
        }
    }
}

pub fn log_info(category: &str, message: &str) {
    log_event(LogLevel::Info, category, message);
}

pub fn log_warn(category: &str, message: &str) {
    log_event(LogLevel::Warn, category, message);
}

pub fn log_error(category: &str, message: &str) {
    log_event(LogLevel::Error, category, message);
}

pub fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn default_app_data_dir() -> PathBuf {
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(local_app_data).join("SceneDock")
    } else {
        PathBuf::from(".").join("SceneDock")
    }
}

/// Snapshot of current process resource consumption (Section 9).
#[derive(Debug, Clone, Default)]
pub struct ResourceMetrics {
    pub gdi_objects: u32,
    pub user_objects: u32,
}

#[cfg(windows)]
pub fn sample_resource_metrics() -> ResourceMetrics {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetGuiResources, GR_GDIOBJECTS, GR_USEROBJECTS,
    };
    unsafe {
        let proc = GetCurrentProcess();
        let gdi = GetGuiResources(proc, GR_GDIOBJECTS);
        let user = GetGuiResources(proc, GR_USEROBJECTS);
        ResourceMetrics {
            gdi_objects: gdi,
            user_objects: user,
        }
    }
}

#[cfg(not(windows))]
pub fn sample_resource_metrics() -> ResourceMetrics {
    ResourceMetrics::default()
}
