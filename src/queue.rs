//! Failed print jobs, retried every 60 seconds.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{debug, error, info, warn};

use crate::arp::{resolve_effective_ip, resolve_ip_from_mac};
use crate::escpos::{get_printer_status, send_to_printer, PaperStatus};
use crate::state::AppRef;

const INTERVAL: Duration = Duration::from_secs(60);
const MAX_QUEUE_SIZE: usize = 1000;

#[derive(Clone)]
pub struct QueuedJob {
    pub id: u64,
    pub printer_id: String,
    pub ip: String,
    pub port: u16,
    pub mac: Option<String>,
    pub data: Arc<Vec<u8>>,
}

#[derive(Default)]
pub struct PrintQueue {
    jobs: Mutex<Vec<QueuedJob>>,
    next_id: AtomicU64,
}

impl PrintQueue {
    pub fn add(&self, printer_id: &str, ip: &str, port: u16, data: Arc<Vec<u8>>, mac: Option<&str>) {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.len() >= MAX_QUEUE_SIZE {
            error!(target: "printQueue", printer_id, max_queue_size = MAX_QUEUE_SIZE, "Queue full, dropping job");
            return;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        jobs.push(QueuedJob { id, printer_id: printer_id.into(), ip: ip.into(), port, mac: mac.map(str::to_string), data });
        info!(target: "printQueue", job_id = id, printer_id, ip, port, mac = ?mac, queue_size = jobs.len(), "Job added to queue");
    }

    pub fn len(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }

    /// Put jobs that still failed back in front of the ones added meanwhile.
    fn requeue(&self, mut remaining: Vec<QueuedJob>) {
        if remaining.is_empty() {
            return;
        }
        let mut jobs = self.jobs.lock().unwrap();
        remaining.append(&mut jobs);
        *jobs = remaining;
    }
}

pub async fn run(app: AppRef) {
    info!(target: "printQueue", interval_ms = INTERVAL.as_millis() as u64, "Starting queue manager");
    let mut tick = tokio::time::interval(INTERVAL);
    tick.tick().await;
    loop {
        tick.tick().await;
        process(&app).await;
    }
}

async fn process(app: &AppRef) {
    let jobs = std::mem::take(&mut *app.queue.jobs.lock().unwrap());
    if jobs.is_empty() {
        return;
    }
    info!(target: "printQueue", queue_size = jobs.len(), "Processing queue");

    // Group by printer, keeping the original order, to check each printer once.
    let mut order: Vec<String> = Vec::new();
    let mut by_printer: HashMap<String, Vec<QueuedJob>> = HashMap::new();
    for job in jobs {
        if !by_printer.contains_key(&job.printer_id) {
            order.push(job.printer_id.clone());
        }
        by_printer.entry(job.printer_id.clone()).or_default().push(job);
    }

    let mut remaining = Vec::new();
    for printer_id in order {
        let jobs = by_printer.remove(&printer_id).unwrap_or_default();
        let first = jobs[0].clone();
        let mut ip = resolve_effective_ip(&first.ip, first.mac.as_deref()).unwrap_or(first.ip.clone());

        let status = match get_printer_status(&ip, first.port).await {
            Ok(s) => Some(s),
            Err(e) => {
                warn!(target: "printQueue", printer_id = %printer_id, ip = %ip, port = first.port, error = %e, "Printer status check failed");
                let mac_ip = (!first.ip.is_empty())
                    .then_some(first.mac.as_deref())
                    .flatten()
                    .and_then(resolve_ip_from_mac)
                    .filter(|m| *m != ip);
                match mac_ip {
                    Some(mac_ip) => {
                        info!(target: "printQueue", printer_id = %printer_id, ip = %ip, mac_ip = %mac_ip, "IP unreachable, retrying with MAC-resolved IP");
                        match get_printer_status(&mac_ip, first.port).await {
                            Ok(s) => {
                                ip = mac_ip.clone();
                                app.set_printer_ip(&printer_id, &mac_ip);
                                let (app, pid) = (app.clone(), printer_id.clone());
                                tokio::spawn(async move { app.patch_printer_ip(&pid, &mac_ip).await });
                                Some(s)
                            }
                            Err(_) => {
                                warn!(target: "printQueue", printer_id = %printer_id, jobs = jobs.len(), "Printer unreachable via IP and MAC, keeping jobs in queue");
                                None
                            }
                        }
                    }
                    None => None,
                }
            }
        };

        match status {
            Some(PaperStatus::Ok | PaperStatus::Low) => {
                debug!(target: "printQueue", printer_id = %printer_id, status = ?status, "Printer status");
                for job in jobs {
                    match send_to_printer(&ip, job.port, &job.data).await {
                        Ok(()) => info!(target: "printQueue", job_id = job.id, printer_id = %printer_id, ip = %ip, port = job.port, "Queued job printed"),
                        Err(e) => {
                            error!(target: "printQueue", job_id = job.id, printer_id = %printer_id, ip = %ip, port = job.port, error = %e, "Failed to print queued job despite OK status");
                            remaining.push(job);
                        }
                    }
                }
            }
            Some(PaperStatus::Out) => {
                warn!(target: "printQueue", printer_id = %printer_id, jobs = jobs.len(), "Printer out of paper, keeping jobs in queue");
                remaining.extend(jobs);
            }
            None => remaining.extend(jobs),
        }
    }
    app.queue.requeue(remaining);
}
