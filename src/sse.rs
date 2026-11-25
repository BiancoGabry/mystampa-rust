//! SSE client for `API_URL/events/printer`: reconnects forever with
//! exponential backoff; events are handled one at a time, in order.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use crate::models::{value_to_string, IncomingOrder, OrderCancelled};
use crate::print;
use crate::state::AppRef;

const BASE_DELAY_MS: u64 = 5_000;
const MAX_DELAY_MS: u64 = 120_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// No byte at all (not even a keep-alive) for this long → stale stream.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_BUFFER: usize = 1024 * 1024;

fn summarize(payload: &Value) -> String {
    let order = payload.get("createdOrder").unwrap_or(payload);
    let g = |v: &Value, k: &str| v.get(k).and_then(value_to_string);
    let mut parts = Vec::new();
    for (k, v) in [
        ("orderId", g(order, "id").or_else(|| g(payload, "orderId"))),
        ("displayCode", g(order, "displayCode")),
        ("ticketNumber", g(order, "ticketNumber")),
        ("cashRegisterId", g(order, "cashRegisterId").or_else(|| g(payload, "cashRegister"))),
        ("printerId", g(payload, "printerId")),
        ("items", order.get("orderItems").and_then(Value::as_array).map(|a| a.len().to_string())),
    ] {
        if let Some(v) = v {
            parts.push(format!("{k}={v}"));
        }
    }
    parts.join(" ")
}

async fn dispatch(app: &AppRef, event_type: &str, data: &str) {
    let payload: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(e) => {
            error!(target: "sse", event_type, bytes = data.len(), error = %e, "Invalid JSON payload");
            return;
        }
    };
    info!(target: "sse", event_type, summary = %summarize(&payload), "Event received");
    debug!(target: "sse", event_type, payload = %payload, "Event payload");

    match event_type {
        "confirmed-order" | "reprint-order" => {
            let raw = payload.get("createdOrder").cloned().unwrap_or(payload);
            let order: IncomingOrder = match serde_json::from_value(raw) {
                Ok(o) => o,
                Err(e) => {
                    error!(target: "sse", event_type, error = %e, "Invalid order payload");
                    return;
                }
            };
            let order_id = order.id.clone().unwrap_or_default();
            let is_cash = order.payment_method.as_deref().is_some_and(|m| m.eq_ignore_ascii_case("CASH"));
            if event_type == "confirmed-order" && is_cash {
                if let Some(crid) = order.cash_register_id.clone().filter(|s| !s.is_empty()) {
                    let app = app.clone();
                    tokio::spawn(async move {
                        match print::handle_open_drawer_by_cash_register(&app, &crid).await {
                            Ok(()) => info!(target: "sse", cash_register_id = %crid, "Cash drawer opened"),
                            Err(e) => error!(target: "sse", cash_register_id = %crid, error = %e, "Cash drawer open failed"),
                        }
                    });
                }
            }
            {
                let label = order
                    .display_code
                    .clone()
                    .filter(|c| !c.is_empty())
                    .or_else(|| order.ticket_number.as_ref().map(|n| format!("N° {n}")))
                    .unwrap_or_else(|| format!("#{order_id}"));
                let label = if event_type == "reprint-order" { format!("{label} (ristampa)") } else { label };
                *app.last_order.lock().unwrap() = Some((label, chrono::Local::now().format("%H:%M:%S").to_string()));
                app.orders_handled.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            match print::handle_print_order(app, &order).await {
                Ok(r) => info!(target: "sse", event_type, order_id = %order_id, kitchen_printers = ?r.kitchen_printers, cash_printer_id = %r.cash_printer_id, "Print order handled"),
                Err(e) => error!(target: "sse", event_type, order_id = %order_id, error = %e, "Print order failed"),
            }
        }
        "order-cancelled" => match serde_json::from_value::<OrderCancelled>(payload) {
            Ok(p) => print::handle_order_cancelled(app, &p).await,
            Err(e) => error!(target: "sse", event_type, error = %e, "Invalid cancellation payload"),
        },
        "general-closure" => match print::handle_general_closure(app, &payload).await {
            Ok(()) => info!(target: "sse", event_type, "General closure handled"),
            Err(e) => error!(target: "sse", event_type, error = %e, "General closure failed"),
        },
        "settings-update" => app.apply_settings(payload),
        "open-drawer" => {
            let pid = payload.get("printerId").and_then(value_to_string).unwrap_or_default();
            match print::handle_open_drawer(app, &pid).await {
                Ok(()) => info!(target: "sse", printer_id = %pid, "Cash drawer opened"),
                Err(e) => error!(target: "sse", printer_id = %pid, error = %e, "Cash drawer open failed"),
            }
        }
        _ => debug!(target: "sse", event_type, "Ignoring event"),
    }
}

/// Incremental SSE parser; state spans chunks (a proxy may split events anywhere).
#[derive(Default)]
pub struct Parser {
    buffer: String,
    event_type: String,
    data: String,
    pub last_id: String,
}

impl Parser {
    /// Feed a chunk; returns the completed (type, data) events.
    pub fn feed(&mut self, chunk: &str) -> Vec<(String, String)> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();
        loop {
            let Some(pos) = self.buffer.find(['\r', '\n']) else { break };
            // A trailing CR may be half of a CRLF split across chunks: wait.
            if self.buffer.as_bytes()[pos] == b'\r' && pos + 1 == self.buffer.len() {
                break;
            }
            let line: String = self.buffer[..pos].to_string();
            let skip = if self.buffer[pos..].starts_with("\r\n") { 2 } else { 1 };
            self.buffer.drain(..pos + skip);

            if line.starts_with(':') {
                continue;
            } else if let Some(v) = line.strip_prefix("event:") {
                self.event_type = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("id:") {
                let id = v.trim();
                if !id.is_empty() {
                    self.last_id = id.to_string();
                }
            } else if let Some(v) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(v.trim());
            } else if line.is_empty() {
                if !self.data.is_empty() {
                    events.push((std::mem::take(&mut self.event_type), std::mem::take(&mut self.data)));
                }
                self.event_type.clear();
                self.data.clear();
            }
        }
        events
    }

    pub fn reset_connection(&mut self) {
        self.buffer.clear();
        self.event_type.clear();
        self.data.clear();
    }
}

pub async fn run(app: AppRef) {
    // Events are handled on their own task so a slow print never stalls reading.
    let (tx, mut rx) = mpsc::unbounded_channel::<(String, String)>();
    {
        let app = app.clone();
        tokio::spawn(async move {
            while let Some((t, d)) = rx.recv().await {
                dispatch(&app, &t, &d).await;
            }
        });
    }

    let url = app.url("/events/printer");
    let mut attempt: u32 = 0;
    let mut parser = Parser::default();
    loop {
        if attempt > 0 {
            let delay = (BASE_DELAY_MS * 2u64.saturating_pow(attempt - 1)).min(MAX_DELAY_MS);
            info!(target: "sse", attempt, delay_ms = delay, "Waiting before reconnecting");
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        attempt += 1;
        info!(target: "sse", url = %url, attempt, "Connecting");

        let mut req = app
            .http
            .get(&url)
            .header("Accept", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Accept-Encoding", "identity")
            .header("X-API-KEY", &app.cfg.api_key);
        if !parser.last_id.is_empty() {
            req = req.header("Last-Event-ID", &parser.last_id);
        }
        let resp = match timeout(CONNECT_TIMEOUT, req.send()).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                error!(target: "sse", error = %crate::api::error_chain(&e), "Connection error");
                continue;
            }
            Err(_) => {
                error!(target: "sse", "Connection timed out");
                continue;
            }
        };
        if !resp.status().is_success() {
            error!(target: "sse", status = resp.status().as_u16(), "Connection failed");
            continue;
        }
        info!(target: "sse", "Connected");
        app.sse_connected.store(true, std::sync::atomic::Ordering::Relaxed);
        attempt = 0;
        // A settings-update may have been missed while disconnected.
        {
            let app = app.clone();
            tokio::spawn(async move { app.fetch_and_cache_settings().await });
        }

        parser.reset_connection();
        let mut stream = resp.bytes_stream();
        let mut pending: Vec<u8> = Vec::new(); // incomplete UTF-8 sequence
        loop {
            let chunk = match timeout(IDLE_TIMEOUT, stream.next()).await {
                Err(_) => {
                    error!(target: "sse", idle_timeout_ms = IDLE_TIMEOUT.as_millis() as u64, "No data received, dropping stale connection");
                    break;
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    error!(target: "sse", error = %crate::api::error_chain(&e), "Connection error");
                    break;
                }
                Ok(Some(Ok(c))) => c,
            };
            pending.extend_from_slice(&chunk);
            let valid = match std::str::from_utf8(&pending) {
                Ok(_) => pending.len(),
                Err(e) => e.valid_up_to(),
            };
            let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
            pending.drain(..valid);
            for ev in parser.feed(&text) {
                let _ = tx.send(ev);
            }
            if parser.buffer.len() > MAX_BUFFER {
                error!(target: "sse", max_buffer_size = MAX_BUFFER, "Buffer exceeded limit (malformed event?), resetting connection");
                break;
            }
        }
        app.sse_connected.store(false, std::sync::atomic::Ordering::Relaxed);
        warn!(target: "sse", "Connection closed, will reconnect");
        attempt = attempt.max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::Parser;

    #[test]
    fn split_chunks_and_crlf() {
        let mut p = Parser::default();
        assert!(p.feed("event: confirmed-order\r").is_empty());
        assert!(p.feed("\ndata: {\"a\"").is_empty());
        assert!(p.feed(":1}\r\nid: 7\r\n").is_empty());
        let ev = p.feed("\r\n: keepalive\n");
        assert_eq!(ev, vec![("confirmed-order".to_string(), "{\"a\":1}".to_string())]);
        assert_eq!(p.last_id, "7");
    }
}
