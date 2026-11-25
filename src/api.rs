//! Backend HTTP calls (always with X-API-KEY) with retry and exponential backoff.

use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tracing::{error, info, warn};

use crate::models::{CashRegister, Printer};
use crate::state::App;

const MAX_RETRIES: u32 = 6;
const RETRY_BASE_MS: u64 = 1000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Error with its causes (reqwest hides the real reason, e.g. "connection refused").
pub fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut src = err.source();
    while let Some(s) = src {
        let msg = s.to_string();
        if !out.contains(&msg) {
            out.push_str(": ");
            out.push_str(&msg);
        }
        src = s.source();
    }
    out
}

fn body_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("message").or_else(|| v.get("error")).and_then(|m| m.as_str()).map(str::to_string)
}

impl App {
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.api_url, path)
    }

    /// Request with up to 6 retries (2s, 4s, … 64s); 401 is never retried.
    pub async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<reqwest::Response, String> {
        let url = self.url(path);
        let mut attempt = 0u32;
        loop {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .timeout(REQUEST_TIMEOUT)
                .header("X-API-KEY", &self.cfg.api_key)
                .header("Accept", "application/json");
            if let Some(b) = body {
                req = req.json(b);
            }
            tracing::debug!(target: "http", method = %method, url = %url, "Request");
            let (status, msg) = match req.send().await {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    (Some(status), body_message(&text).unwrap_or_else(|| status.to_string()))
                }
                Err(e) => (None, error_chain(&e)),
            };
            let status_str = status.map(|s| s.as_u16().to_string()).unwrap_or_else(|| "network".into());

            if status == Some(StatusCode::UNAUTHORIZED) {
                error!(target: "http", method = %method, url = %url, status = %status_str, error = %msg, "Unauthorized: API key invalid or missing");
                return Err(msg);
            }
            if attempt >= MAX_RETRIES {
                error!(target: "http", method = %method, url = %url, status = %status_str, max_retries = MAX_RETRIES, error = %msg, "Request failed permanently");
                return Err(msg);
            }
            attempt += 1;
            let delay = RETRY_BASE_MS * 2u64.pow(attempt);
            warn!(target: "http", method = %method, url = %url, status = %status_str, attempt, max_retries = MAX_RETRIES, delay_ms = delay, error = %msg, "Request failed, retrying");
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let resp = self.request(Method::GET, path, None).await?;
        resp.json::<T>().await.map_err(|e| format!("invalid JSON: {e}"))
    }

    pub async fn fetch_printers(&self) -> Result<Vec<Printer>, String> {
        let raw: Vec<Value> = self.get_json("/v1/printers").await?;
        Ok(raw.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
    }

    /// Refresh the cash registers cache; on failure the previous cache is kept.
    pub async fn fetch_and_cache_cash_registers(&self) {
        match self.get_json::<Vec<Value>>("/v1/cash-registers?include=printer").await {
            Ok(raw) => {
                let map: std::collections::HashMap<String, CashRegister> = raw
                    .into_iter()
                    .filter_map(|v| serde_json::from_value::<CashRegister>(v).ok())
                    .filter_map(|cr| cr.id.clone().map(|id| (id, cr)))
                    .collect();
                tracing::debug!(target: "cashRegisters", count = map.len(), "Cash registers cached");
                *self.cash_registers.write().unwrap() = map;
            }
            Err(e) => error!(target: "cashRegisters", error = %e, "Failed to fetch cash registers"),
        }
    }

    /// Fetch all settings; on failure the previous ones are kept.
    pub async fn fetch_and_cache_settings(&self) {
        match self.get_json::<Value>("/v1/settings").await {
            Ok(v) => self.apply_settings(v),
            Err(e) => error!(target: "settings", error = %e, "Failed to fetch settings"),
        }
    }

    pub async fn patch_printer_status(&self, printer_id: &str, status: &str) {
        match self.request(Method::PATCH, &format!("/v1/printers/{printer_id}"), Some(&json!({ "status": status }))).await {
            Ok(_) => info!(target: "api", printer_id, status, "Printer status patched"),
            Err(e) => error!(target: "api", printer_id, status, error = %e, "Printer status patch failed"),
        }
    }

    pub async fn patch_printer_ip(&self, printer_id: &str, ip: &str) {
        match self.request(Method::PATCH, &format!("/v1/printers/{printer_id}"), Some(&json!({ "ip": ip }))).await {
            Ok(_) => info!(target: "api", printer_id, ip, "Printer IP patched"),
            Err(e) => error!(target: "api", printer_id, ip, error = %e, "Printer IP patch failed"),
        }
    }
}
