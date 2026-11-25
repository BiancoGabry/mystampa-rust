//! Order handling: kitchen receipts, cash receipt, cancellations, cash drawer,
//! general closure. Every print goes through `safe_print`, which queues the
//! job when the printer is unreachable or out of paper.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tracing::{debug, error, info, warn};

use crate::arp::{resolve_effective_ip, resolve_ip_from_mac};
use crate::escpos::{self, get_printer_status, send_to_printer, CashLine, KitchenLine, PaperStatus, StationTicket};
use crate::models::{value_to_string, IncomingOrder, OrderCancelled};
use crate::state::AppRef;

/// Check the paper, then print; on any failure the job goes to the queue.
pub async fn safe_print(app: &AppRef, printer_id: &str, ip: &str, port: u16, data: Arc<Vec<u8>>, mac: Option<&str>) {
    let Some(primary) = resolve_effective_ip(ip, mac) else {
        error!(target: "print", printer_id, mac = ?mac, "No address available for printer");
        return;
    };
    let queue_ip = if ip.trim().is_empty() { primary.clone() } else { ip.to_string() };
    let mut target = primary.clone();

    let status = match get_printer_status(&target, port).await {
        Ok(s) => s,
        Err(e) => {
            warn!(target: "print", printer_id, ip = %target, port, error = %e, "Printer status check failed");
            let mac_ip = (!ip.trim().is_empty()).then_some(mac).flatten().and_then(resolve_ip_from_mac).filter(|m| *m != target);
            let Some(mac_ip) = mac_ip else {
                error!(target: "print", printer_id, ip = %target, port, "Status check failed, adding job to queue");
                app.queue.add(printer_id, &queue_ip, port, data, mac);
                return;
            };
            info!(target: "print", printer_id, ip = %target, mac_ip = %mac_ip, port, "IP unreachable, retrying with MAC-resolved IP");
            match get_printer_status(&mac_ip, port).await {
                Ok(s) => {
                    target = mac_ip.clone();
                    app.set_printer_ip(printer_id, &mac_ip);
                    let (a, pid) = (app.clone(), printer_id.to_string());
                    tokio::spawn(async move { a.patch_printer_ip(&pid, &mac_ip).await });
                    s
                }
                Err(_) => {
                    error!(target: "print", printer_id, ip = %target, mac_ip = %mac_ip, port, "Both IP and MAC-resolved IP failed, adding job to queue");
                    app.queue.add(printer_id, &queue_ip, port, data, mac);
                    return;
                }
            }
        }
    };

    if status == PaperStatus::Out {
        warn!(target: "print", printer_id, ip = %target, port, "Printer out of paper, adding job to queue");
        app.queue.add(printer_id, &queue_ip, port, data, mac);
        return;
    }

    match send_to_printer(&target, port, &data).await {
        Ok(()) => {
            info!(target: "print", printer_id, ip = %target, port, "Printed successfully");
            if let Some(prev) = app.find_printer(printer_id).map(|p| p.status) {
                if prev != "ONLINE" {
                    info!(target: "print", printer_id, previous_status = %prev, "Printer reachable again, patching status to ONLINE");
                    app.set_printer_status(printer_id, "ONLINE");
                    let (a, pid) = (app.clone(), printer_id.to_string());
                    tokio::spawn(async move { a.patch_printer_status(&pid, "ONLINE").await });
                }
            }
        }
        Err(e) => {
            error!(target: "print", printer_id, ip = %target, port, error = %e, "Print failed, adding job to queue");
            app.queue.add(printer_id, &queue_ip, port, data, mac);
        }
    }
}

pub struct PrintResult {
    pub kitchen_printers: Vec<String>,
    pub cash_printer_id: String,
}

pub async fn handle_print_order(app: &AppRef, order: &IncomingOrder) -> Result<PrintResult, String> {
    let Some(items) = &order.order_items else {
        return Err("Invalid payload: missing orderItems[]".into());
    };
    let order_id = order.id.clone().unwrap_or_default();

    // Kitchen: reprintOrderItems when present (reprint-order), else all items.
    let kitchen_items = order.reprint_order_items.as_ref().unwrap_or(items);
    let mut kitchen_order: Vec<String> = Vec::new();
    let mut kitchen: HashMap<String, Vec<KitchenLine>> = HashMap::new();
    for it in kitchen_items {
        let pid = it.food.as_ref().and_then(|f| f.printer_id.clone()).unwrap_or_default();
        if pid.is_empty() {
            continue;
        }
        if !kitchen.contains_key(&pid) {
            kitchen_order.push(pid.clone());
        }
        kitchen.entry(pid).or_default().push(KitchenLine { food_name: it.food_name(), quantity: it.quantity, notes: it.notes.clone() });
    }

    let mut cash_lines = Vec::new();
    let mut single_tickets = Vec::new();
    for it in items {
        cash_lines.push(CashLine {
            food_name: it.food_name(),
            quantity: it.quantity,
            notes: it.notes.clone(),
            unit_price: it.unit_price.unwrap_or(0.0),
            surcharge: it.unit_surcharge.unwrap_or(0.0),
        });
        if it.food.as_ref().and_then(|f| f.separate_ticket) == Some(true) {
            single_tickets.push(KitchenLine { food_name: it.food_name(), quantity: it.quantity, notes: it.notes.clone() });
        }
    }

    // Pickup tickets per station (settings.stations.pickupTickets)
    let mut station_tickets: Vec<StationTicket> = Vec::new();
    if app.pickup_tickets() {
        let mut idx: HashMap<String, usize> = HashMap::new();
        for it in items {
            let Some(st) = it.food.as_ref().and_then(|f| f.category.as_ref()).and_then(|c| c.station.as_ref()) else {
                continue;
            };
            let sid = st.id.clone().unwrap_or_default();
            let i = *idx.entry(sid).or_insert_with(|| {
                station_tickets.push(StationTicket { station_name: st.name.clone().unwrap_or_default(), lines: Vec::new() });
                station_tickets.len() - 1
            });
            station_tickets[i].lines.push(KitchenLine { food_name: it.food_name(), quantity: it.quantity, notes: it.notes.clone() });
        }
    }

    let cash_printer = match order.cash_register_id.as_deref().filter(|s| !s.is_empty()) {
        Some(crid) => app.resolve_cash_register_printer(crid).await,
        None => None,
    };

    let mut jobs = Vec::new();
    for pid in &kitchen_order {
        let lines = &kitchen[pid];
        let progress = {
            let mut map = app.progress.lock().unwrap();
            let p = map.entry(pid.clone()).or_insert(1);
            let cur = *p;
            *p += 1;
            cur
        };
        let receipt = Arc::new(escpos::build_kitchen_receipt(app, order, lines, progress));
        match app.find_printer(pid) {
            Some(p) => {
                let app = app.clone();
                let pid = pid.clone();
                jobs.push(tokio::spawn(async move {
                    safe_print(&app, &pid, p.ip().trim(), p.port, receipt, p.mac()).await;
                }));
            }
            None => warn!(target: "print", printer_id = %pid, order_id = %order_id, "No cached printer for kitchen receipt"),
        }
    }

    if order.reprint_receipt == Some(false) {
        info!(target: "print", order_id = %order_id, "Skipping cash receipt: reprintReceipt is false");
    } else if let Some(cp) = &cash_printer {
        let receipt = Arc::new(escpos::build_cash_receipt(app, order, &cash_lines, &single_tickets, &station_tickets).await);
        let app = app.clone();
        let cp = cp.clone();
        jobs.push(tokio::spawn(async move {
            safe_print(&app, cp.id().trim(), cp.ip().trim(), cp.port, receipt, cp.mac()).await;
        }));
    } else {
        warn!(target: "print", order_id = %order_id, cash_register_id = ?order.cash_register_id, "No printer found for cash register, skipping cash receipt");
    }

    for j in jobs {
        if let Err(e) = j.await {
            error!(target: "print", order_id = %order_id, error = %e, "A print job failed unexpectedly");
        }
    }

    Ok(PrintResult {
        kitchen_printers: kitchen_order,
        cash_printer_id: cash_printer.map(|p| p.id().trim().to_string()).unwrap_or_default(),
    })
}

pub async fn handle_order_cancelled(app: &AppRef, payload: &OrderCancelled) {
    let order_id = payload.order_id.clone().unwrap_or_default();
    let printer_ids: Vec<String> = payload.printers.iter().filter_map(value_to_string).collect();
    info!(target: "print", order_id = %order_id, printers = ?printer_ids, "Processing order cancellation");
    if printer_ids.is_empty() {
        info!(target: "print", order_id = %order_id, "No printers in cancellation payload, nothing to print");
        return;
    }
    let receipt = Arc::new(escpos::build_cancellation_receipt(app, &payload.display_code, &payload.ticket_number, &payload.customer, &payload.table));
    let mut jobs = Vec::new();
    for pid in printer_ids {
        match app.find_printer(&pid) {
            Some(p) => {
                let (app, receipt) = (app.clone(), receipt.clone());
                jobs.push(tokio::spawn(async move {
                    safe_print(&app, p.id().trim(), p.ip().trim(), p.port, receipt, p.mac()).await;
                }));
            }
            None => warn!(target: "print", printer_id = %pid, order_id = %order_id, "No cached printer found for cancellation receipt"),
        }
    }
    for j in jobs {
        let _ = j.await;
    }
}

pub async fn handle_open_drawer(app: &AppRef, printer_id: &str) -> Result<(), String> {
    let p = app.find_printer(printer_id).ok_or_else(|| format!("Printer not found: {printer_id}"))?;
    let ip = resolve_effective_ip(p.ip(), p.mac()).ok_or_else(|| format!("No address for printer {}", p.id()))?;
    escpos::open_cash_drawer(&ip, p.port).await.map_err(|e| e.to_string())
}

pub async fn handle_open_drawer_by_cash_register(app: &AppRef, cash_register_id: &str) -> Result<(), String> {
    let crid = cash_register_id.trim();
    if crid.is_empty() {
        return Err("Missing cashRegisterId".into());
    }
    let p = app.resolve_cash_register_printer(crid).await.ok_or_else(|| format!("No printer for cash-register {crid}"))?;
    let ip = resolve_effective_ip(p.ip(), p.mac()).ok_or_else(|| format!("No address for printer {}", p.id()))?;
    escpos::open_cash_drawer(&ip, p.port).await.map_err(|e| e.to_string())
}

pub async fn handle_general_closure(app: &AppRef, payload: &Value) -> Result<(), String> {
    let crid = payload.get("cashRegister").and_then(value_to_string).unwrap_or_default();
    if crid.is_empty() {
        return Err("Missing cashRegister ID in payload".into());
    }
    info!(target: "closure", cash_register_id = %crid, "Processing general closure");
    let p = match app.resolve_cash_register_printer(&crid).await {
        Some(p) if !p.ip().is_empty() && p.port != 0 => p,
        _ => {
            error!(target: "closure", cash_register_id = %crid, "No printer found for cash register");
            return Err("No printer configured for this cash register".into());
        }
    };
    let receipts = escpos::build_general_closure_report(app, payload);
    info!(target: "closure", cash_register_id = %crid, printer_id = p.id(), receipts = receipts.len(), "Closure receipts generated");
    for (i, r) in receipts.into_iter().enumerate() {
        debug!(target: "closure", cash_register_id = %crid, receipt = i, "Printing closure receipt");
        safe_print(app, p.id().trim(), p.ip(), p.port, Arc::new(r), p.mac()).await;
    }
    info!(target: "closure", cash_register_id = %crid, printer_id = p.id(), "General closure report sent to printer");
    Ok(())
}
