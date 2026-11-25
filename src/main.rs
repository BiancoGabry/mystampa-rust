//! MyStampa: receives orders from the MySagra backend over SSE and prints
//! them on ESC/POS network printers.
//!
//! - Desktop (Windows/Linux executable): graphical window to enter the API
//!   link and token and follow printers and activity.
//! - Docker / server: headless, configured by API_URL and API_KEY.

// No console window behind the GUI on Windows.
#![cfg_attr(all(windows, feature = "gui"), windows_subsystem = "windows")]

mod api;
mod arp;
mod config;
mod escpos;
#[cfg(feature = "gui")]
mod gui;
mod image;
mod logging;
mod models;
mod monitor;
mod print;
mod queue;
mod service;
mod sse;
mod state;

use std::sync::OnceLock;

use state::AppRef;

pub static APP: OnceLock<AppRef> = OnceLock::new();

fn has_arg(name: &str) -> bool {
    std::env::args().skip(1).any(|a| a == name)
}

/// With `windows_subsystem = "windows"` there is no console: reuse the one of
/// the terminal that started us, if any, so console mode still prints.
#[cfg(all(windows, feature = "gui"))]
fn attach_parent_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    unsafe {
        AttachConsole(u32::MAX); // ATTACH_PARENT_PROCESS
    }
}

#[cfg(not(all(windows, feature = "gui")))]
fn attach_parent_console() {}

fn pause_before_exit() {
    // Double-clicked on Windows the console would close before the error can be read.
    if cfg!(windows) && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!("\nPremi INVIO per chiudere...");
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    }
}

/// Ctrl+C, or SIGTERM (`docker stop`: as PID 1 the default action would not apply).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

/// GUI unless asked otherwise, configured by env (Docker) or without a display.
fn use_gui() -> bool {
    if !cfg!(feature = "gui") || has_arg("--headless") || has_arg("--console") {
        return false;
    }
    let env_configured = ["API_URL", "API_KEY"].iter().all(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()));
    if env_configured {
        return false;
    }
    if cfg!(windows) {
        return true;
    }
    ["DISPLAY", "WAYLAND_DISPLAY"].iter().any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
}

fn main() {
    if has_arg("--version") || has_arg("-V") {
        attach_parent_console();
        println!("mystampa {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if has_arg("--help") || has_arg("-h") {
        attach_parent_console();
        println!(
            "mystampa {} - servizio di stampa MySagra\n\n\
             Senza opzioni apre la finestra (se c'è uno schermo).\n\
             --console   modalità testuale: chiede indirizzo API e token nel terminale\n\
             --headless  come --console (per servizi e server)\n\n\
             Variabili d'ambiente (Docker/servizio, nessuna domanda):\n  \
             API_URL   indirizzo del backend MySagra\n  \
             API_KEY   token API (X-API-KEY)\n  \
             LOG_LEVEL error|warn|info|debug|trace (default info)\n  \
             LOG_DIR   cartella dei log (default ./logs accanto alla configurazione)\n\n\
             La configurazione è salvata in {}.",
            env!("CARGO_PKG_VERSION"),
            config::CONFIG_FILE
        );
        return;
    }

    #[cfg(feature = "gui")]
    if use_gui() {
        let (_guard, log_dir) = logging::init(&config::data_dir());
        if let Err(e) = gui::run(log_dir) {
            eprintln!("Impossibile aprire la finestra: {e}");
            std::process::exit(1);
        }
        return;
    }
    let _ = use_gui;

    attach_parent_console();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    rt.block_on(console_main());
}

async fn console_main() {
    let cfg = match config::resolve().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ERRORE: {e}");
            pause_before_exit();
            std::process::exit(1);
        }
    };
    let (_log_guard, _log_dir) = logging::init(&config::data_dir());
    let app = service::start(cfg).await;
    shutdown_signal().await;
    service::log_shutdown(&app);
}
