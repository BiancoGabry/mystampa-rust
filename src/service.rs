//! Service startup shared by the console/Docker mode and the GUI.

use std::sync::Arc;

use tracing::{error, info};

use crate::config::Config;
use crate::state::{App, AppRef};
use crate::{monitor, queue, sse, APP};

/// Create the app state, publish it in `APP` and start every background task.
/// Returns once the first printers / cash registers / settings fetch is done.
pub async fn start(cfg: Config) -> AppRef {
    info!(target: "app", version = env!("CARGO_PKG_VERSION"), api_url = %cfg.api_url, "MyStampa starting");
    let app: AppRef = Arc::new(App::new(cfg));
    let _ = APP.set(app.clone());

    monitor::refresh_printers(&app).await;
    info!(target: "app", printer_count = app.printers_snapshot().len(), "Initialization complete");
    {
        let app = app.clone();
        tokio::spawn(async move { monitor::run_status_check(&app).await });
    }
    app.fetch_and_cache_cash_registers().await;
    app.fetch_and_cache_settings().await;

    tokio::spawn(queue::run(app.clone()));
    tokio::spawn(monitor::run(app.clone()));
    tokio::spawn(monitor::run_cash_registers(app.clone()));
    tokio::spawn(sse::run(app.clone()));
    app
}

pub fn log_shutdown(app: &App) {
    let pending = app.queue.len();
    if pending > 0 {
        error!(target: "app", pending_jobs = pending, "Shutting down with unprinted jobs in the queue");
    }
    info!(target: "app", "Shutting down");
}
