//! Shared application state: config, caches, settings and receipt logo.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::image::{self, ImageOptions};
use crate::models::{CashRegister, Printer};
use crate::queue::PrintQueue;

const LOGO_RETRY: Duration = Duration::from_secs(60);
const LOGO_TIMEOUT: Duration = Duration::from_secs(5);

/// Bundled MySagra logo: fallback receipt logo and footer.
pub const MYSAGRA_LOGO_PNG: &[u8] = include_bytes!("../resources/mysagralogo.png");
/// MySagra logo with "mysagra.com" next to it, pre-rendered (height 48).
pub const FOOTER_PNG: &[u8] = include_bytes!("../resources/footer.png");

#[derive(Default)]
struct LogoCache {
    key: Option<String>,
    buf: Option<Arc<Vec<u8>>>,
    failed_at: Option<Instant>,
}

pub struct App {
    pub cfg: Config,
    pub http: reqwest::Client,
    pub printers: RwLock<Vec<Printer>>,
    pub cash_registers: RwLock<HashMap<String, CashRegister>>,
    settings: RwLock<Option<Value>>,
    logo: tokio::sync::Mutex<LogoCache>,
    /// Kitchen receipt progressive number per printer, since the service started.
    pub progress: Mutex<HashMap<String, u64>>,
    pub queue: PrintQueue,
    /// ESC/POS raster of the bundled logo (receipt logo fallback).
    pub default_logo: Vec<u8>,
    /// ESC/POS raster of the footer.
    pub footer: Vec<u8>,
    /// SSE stream currently connected (shown in the GUI).
    pub sse_connected: AtomicBool,
    /// Last order handled: (label, local time).
    pub last_order: Mutex<Option<(String, String)>>,
    /// Orders handled since start.
    pub orders_handled: std::sync::atomic::AtomicU64,
}

pub type AppRef = Arc<App>;

impl App {
    pub fn new(cfg: Config) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("HTTP client");
        let default_logo = image::to_escpos(MYSAGRA_LOGO_PNG, &ImageOptions { exact_width: Some(210), ..Default::default() });
        let footer = image::to_escpos(FOOTER_PNG, &ImageOptions { autocrop: false, ..Default::default() });
        App {
            cfg,
            http,
            printers: RwLock::new(Vec::new()),
            cash_registers: RwLock::new(HashMap::new()),
            settings: RwLock::new(None),
            logo: tokio::sync::Mutex::new(LogoCache::default()),
            progress: Mutex::new(HashMap::new()),
            queue: PrintQueue::default(),
            default_logo,
            footer,
            sse_connected: AtomicBool::new(false),
            last_order: Mutex::new(None),
            orders_handled: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn printers_snapshot(&self) -> Vec<Printer> {
        self.printers.read().unwrap().clone()
    }

    pub fn find_printer(&self, id: &str) -> Option<Printer> {
        let id = id.trim();
        self.printers.read().unwrap().iter().find(|p| p.id().trim() == id).cloned()
    }

    /// Update the in-memory status; returns the previous one.
    pub fn set_printer_status(&self, id: &str, status: &str) -> Option<String> {
        let mut list = self.printers.write().unwrap();
        let p = list.iter_mut().find(|p| p.id() == id)?;
        Some(std::mem::replace(&mut p.status, status.to_string()))
    }

    pub fn set_printer_ip(&self, id: &str, ip: &str) {
        if let Some(p) = self.printers.write().unwrap().iter_mut().find(|p| p.id() == id) {
            p.ip = Some(ip.to_string());
        }
    }

    // ---------- settings ----------

    fn setting(&self, path: &[&str]) -> Option<Value> {
        let guard = self.settings.read().unwrap();
        let mut v = guard.as_ref()?;
        for key in path {
            v = v.get(*key)?;
        }
        Some(v.clone())
    }

    /// settings.tickets.showNumbers: true prints the ticket number, false the order code.
    pub fn show_numbers(&self) -> bool {
        self.setting(&["settings", "tickets", "showNumbers"]) == Some(Value::Bool(true))
    }

    pub fn pickup_tickets(&self) -> bool {
        self.setting(&["settings", "stations", "pickupTickets"]) == Some(Value::Bool(true))
    }

    /// settings.general.currency (ISO 4217), EUR until the settings are loaded.
    pub fn currency_code(&self) -> String {
        let c = self
            .setting(&["settings", "general", "currency"])
            .and_then(|v| v.as_str().map(|s| s.trim().to_uppercase()))
            .unwrap_or_default();
        if c.len() == 3 && c.chars().all(|ch| ch.is_ascii_uppercase()) {
            c
        } else {
            "EUR".into()
        }
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn sagra_name(&self) -> Option<String> {
        self.setting(&["sagra", "name"]).and_then(|v| v.as_str().map(str::to_string)).filter(|s| !s.is_empty())
    }

    fn receipt_logo_path(&self) -> Option<String> {
        self.setting(&["sagra", "receiptLogo"])
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|s| !s.is_empty())
    }

    /// Replace the cached settings (from GET /v1/settings or a settings-update event).
    pub fn apply_settings(self: &Self, next: Value) {
        let prev_logo = self.receipt_logo_path();
        *self.settings.write().unwrap() = Some(next);
        let next_logo = self.receipt_logo_path();
        info!(
            target: "settings",
            sagra = ?self.setting(&["sagra", "name"]),
            receipt_logo = ?next_logo,
            show_numbers = self.show_numbers(),
            currency = %self.currency_code(),
            pickup_tickets = self.pickup_tickets(),
            updated_at = ?self.setting(&["updatedAt"]),
            "Settings applied"
        );
        if next_logo != prev_logo {
            // Warm the cache so the next receipt doesn't wait for the download.
            if let Some(app) = crate::APP.get() {
                let app = app.clone();
                tokio::spawn(async move {
                    app.receipt_logo().await;
                });
            }
        }
    }

    fn logo_url(&self, receipt_logo: &str) -> String {
        let lower = receipt_logo.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            receipt_logo.to_string()
        } else {
            format!("{}/{}", self.cfg.api_url, receipt_logo.trim_start_matches('/'))
        }
    }

    /// Sagra logo (ESC/POS raster) or None when not configured / not downloadable.
    /// Downloaded and converted once per logo path.
    pub async fn receipt_logo(&self) -> Option<Arc<Vec<u8>>> {
        let key = self.receipt_logo_path()?;
        let mut cache = self.logo.lock().await;
        if cache.key.as_deref() == Some(key.as_str()) {
            if let Some(buf) = &cache.buf {
                return Some(buf.clone());
            }
            if cache.failed_at.is_some_and(|t| t.elapsed() < LOGO_RETRY) {
                return None;
            }
        }
        cache.key = Some(key.clone());
        cache.buf = None;
        cache.failed_at = None;

        let url = self.logo_url(&key);
        let result = async {
            let resp = self
                .http
                .get(&url)
                .timeout(LOGO_TIMEOUT)
                .header("Accept", "image/*")
                .header("X-API-KEY", &self.cfg.api_key)
                .send()
                .await
                .map_err(|e| crate::api::error_chain(&e))?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }
            resp.bytes().await.map_err(|e| e.to_string())
        }
        .await;

        match result {
            Ok(bytes) => {
                let buf = image::to_escpos(&bytes, &ImageOptions { exact_width: Some(210), ..Default::default() });
                if buf.is_empty() {
                    warn!(target: "settings", url = %url, "Receipt logo could not be decoded, skipping");
                    cache.failed_at = Some(Instant::now());
                    return None;
                }
                info!(target: "settings", url = %url, bytes = buf.len(), "Receipt logo loaded");
                let buf = Arc::new(buf);
                cache.buf = Some(buf.clone());
                Some(buf)
            }
            Err(e) => {
                error!(target: "settings", url = %url, error = %e, "Failed to download receipt logo");
                cache.failed_at = Some(Instant::now());
                None
            }
        }
    }

    /// Default printer of a cash register. On a cache miss the cache is refreshed once.
    /// Address details come from the printers list, the embedded printer is the fallback.
    pub async fn resolve_cash_register_printer(&self, cash_register_id: &str) -> Option<Printer> {
        let key = cash_register_id.trim().to_string();
        if !self.cash_registers.read().unwrap().contains_key(&key) {
            self.fetch_and_cache_cash_registers().await;
        }
        let cr = self.cash_registers.read().unwrap().get(&key).cloned()?;
        let printer_id = cr
            .default_printer_id
            .clone()
            .or_else(|| cr.default_printer.as_ref().and_then(|p| p.id.clone()))
            .unwrap_or_default();
        if printer_id.is_empty() {
            return None;
        }
        self.find_printer(&printer_id).or(cr.default_printer)
    }
}
