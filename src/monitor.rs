//! Printer list refresh and TCP status probing (every 2 minutes).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use crate::arp::{resolve_effective_ip, resolve_ip_from_mac};
use crate::escpos::{paper_status, PaperStatus};
use crate::state::AppRef;

const INTERVAL: Duration = Duration::from_secs(120);

/// OFFLINE if unreachable, ERROR if out of paper, ONLINE otherwise
/// (including devices that don't answer DLE EOT 4).
async fn probe(ip: &str, port: u16) -> &'static str {
    if ip.is_empty() || port == 0 {
        return "OFFLINE";
    }
    let Ok(Ok(mut s)) = timeout(Duration::from_secs(3), TcpStream::connect((ip, port))).await else {
        return "OFFLINE";
    };
    if s.write_all(&[0x10, 0x04, 0x04]).await.is_err() {
        return "OFFLINE";
    }
    let mut b = [0u8; 1];
    match timeout(Duration::from_millis(500), s.read(&mut b)).await {
        Ok(Ok(n)) if n > 0 && paper_status(b[0]) == PaperStatus::Out => "ERROR",
        _ => "ONLINE",
    }
}

/// Probe every printer; PATCH the backend on change (ERROR always).
pub async fn run_status_check(app: &AppRef) {
    let printers = app.printers_snapshot();
    if printers.is_empty() {
        return;
    }
    debug!(target: "printers", printer_count = printers.len(), "Status check started");
    for p in printers {
        let id = p.id().to_string();
        let primary = resolve_effective_ip(p.ip(), p.mac()).unwrap_or_else(|| p.ip().to_string());
        let mut ip = p.ip().to_string();
        let mut status = probe(&primary, p.port).await;

        if status == "OFFLINE" && !p.ip().is_empty() {
            if let Some(mac_ip) = p.mac().and_then(resolve_ip_from_mac).filter(|m| *m != primary) {
                info!(target: "printers", printer_id = %id, printer_name = p.name(), ip = %primary, mac_ip = %mac_ip, "IP offline, retrying with MAC-resolved IP");
                status = probe(&mac_ip, p.port).await;
                if status != "OFFLINE" {
                    info!(target: "printers", printer_id = %id, printer_name = p.name(), previous_ip = p.ip(), ip = %mac_ip, "Printer IP updated");
                    app.set_printer_ip(&id, &mac_ip);
                    app.patch_printer_ip(&id, &mac_ip).await;
                    ip = mac_ip;
                }
            }
        }

        let current = app.find_printer(&id).map(|x| x.status).unwrap_or(p.status.clone());
        if status == "ERROR" {
            warn!(target: "printers", printer_id = %id, printer_name = p.name(), ip = %ip, port = p.port, "Printer out of paper");
            app.set_printer_status(&id, "ERROR");
            app.patch_printer_status(&id, "ERROR").await;
        } else if status != current {
            if status == "OFFLINE" {
                warn!(target: "printers", printer_id = %id, printer_name = p.name(), ip = %ip, port = p.port, previous_status = %current, status, "Printer status changed");
            } else {
                info!(target: "printers", printer_id = %id, printer_name = p.name(), ip = %ip, port = p.port, previous_status = %current, status, "Printer status changed");
            }
            app.set_printer_status(&id, status);
            app.patch_printer_status(&id, status).await;
        } else {
            debug!(target: "printers", printer_id = %id, status, "Printer status unchanged");
        }
    }
}

pub async fn refresh_printers(app: &AppRef) {
    match app.fetch_printers().await {
        Ok(list) => {
            debug!(target: "printers", printer_count = list.len(), "Printers updated");
            *app.printers.write().unwrap() = list;
        }
        // Keep the previous list: an empty one would stop every print.
        Err(e) => error!(target: "printers", error = %e, "Fetching printers failed, keeping the previous list"),
    }
}

pub async fn run(app: AppRef) {
    let mut tick = tokio::time::interval(INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tick.tick().await;
        refresh_printers(&app).await;
        run_status_check(&app).await;
    }
}

pub async fn run_cash_registers(app: AppRef) {
    let mut tick = tokio::time::interval(Duration::from_secs(300));
    tick.tick().await;
    loop {
        tick.tick().await;
        app.fetch_and_cache_cash_registers().await;
    }
}
