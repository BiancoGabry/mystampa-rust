//! Startup configuration: backend URL and API key.
//!
//! Resolution order:
//! 1. `API_URL` + `API_KEY` environment variables (Docker/service use): no prompt.
//! 2. Interactive console: asks for the URL and the API token, showing the
//!    saved values (Enter keeps them), verifies them against the backend and
//!    saves them to `mystampa.json`.
//! 3. No console (service, redirected stdin): the saved file is used as is.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "mystampa.json";
/// With a saved config, start automatically after this long without input
/// (a PC restarting after a power cut must print without someone at the keyboard).
const AUTO_CONTINUE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api_url: String,
    pub api_key: String,
}

/// Directory holding the config file and the logs: next to the executable
/// when writable (portable install), otherwise the user's config directory.
pub fn data_dir() -> PathBuf {
    // Inside an AppImage the executable lives on a read-only mount: use the
    // folder of the .AppImage file itself.
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok());
    if let Some(dir) = exe.and_then(|p| p.parent().map(Path::to_path_buf)) {
        let probe = dir.join(".mystampa-write-test");
        if std::fs::write(&probe, b"").is_ok() {
            let _ = std::fs::remove_file(&probe);
            return dir;
        }
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    let dir = base.unwrap_or_else(|| PathBuf::from(".")).join("mystampa");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn normalize_url(raw: &str) -> String {
    let mut url = raw.trim().trim_end_matches('/').to_string();
    if !url.is_empty() && !url.contains("://") {
        url = format!("http://{url}");
    }
    url
}

fn load(path: &Path) -> Option<Config> {
    let text = std::fs::read_to_string(path).ok()?;
    let cfg: Config = serde_json::from_str(&text).ok()?;
    (!cfg.api_url.is_empty() && !cfg.api_key.is_empty()).then_some(cfg)
}

fn save(path: &Path, cfg: &Config) -> io::Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(cfg)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn read_line() -> String {
    let mut s = String::new();
    let _ = io::stdin().lock().read_line(&mut s);
    s.trim().to_string()
}

/// Read a line, or None after `timeout` without input.
fn read_line_timeout(timeout: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_line());
    });
    rx.recv_timeout(timeout).ok()
}

fn prompt(label: &str) -> String {
    print!("{label}");
    let _ = io::stdout().flush();
    read_line()
}

fn mask(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "****".into();
    }
    format!("{}…{}", chars[..6].iter().collect::<String>(), chars[chars.len() - 2..].iter().collect::<String>())
}

/// Check URL and key against the backend. Ok(printer count) or a readable error.
pub async fn verify(cfg: &Config) -> Result<usize, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(format!("{}/v1/printers", cfg.api_url))
        .header("X-API-KEY", &cfg.api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("backend non raggiungibile ({})", crate::api::error_chain(&e)))?;
    match resp.status().as_u16() {
        200..=299 => {
            let list: Vec<serde_json::Value> = resp.json().await.map_err(|_| "risposta non valida: l'indirizzo è quello del backend MySagra?".to_string())?;
            Ok(list.len())
        }
        401 | 403 => Err("token API non valido o senza permessi".into()),
        404 => Err("endpoint /v1/printers non trovato: l'indirizzo è quello del backend MySagra?".into()),
        s => Err(format!("il backend ha risposto con HTTP {s}")),
    }
}

fn ask_yes_no(label: &str) -> bool {
    matches!(prompt(label).to_lowercase().as_str(), "s" | "si" | "sì" | "y" | "yes")
}

pub async fn resolve() -> Result<Config, String> {
    let dir = data_dir();
    let path = dir.join(CONFIG_FILE);

    if let (Ok(url), Ok(key)) = (std::env::var("API_URL"), std::env::var("API_KEY")) {
        if !url.trim().is_empty() && !key.trim().is_empty() {
            return Ok(Config { api_url: normalize_url(&url), api_key: key.trim().to_string() });
        }
    }

    let saved = load(&path);
    if !io::stdin().is_terminal() {
        return saved.ok_or_else(|| {
            format!(
                "nessuna configurazione: avvia MyStampa da un terminale per inserire indirizzo e token, \
                 oppure imposta API_URL e API_KEY, oppure crea {}",
                path.display()
            )
        });
    }

    println!();
    println!("  ==============================");
    println!("   MyStampa - servizio di stampa");
    println!("  ==============================");
    println!();

    if let Some(cfg) = &saved {
        println!("  Configurazione salvata:");
        println!("    Indirizzo API: {}", cfg.api_url);
        println!("    Token API:     {}", mask(&cfg.api_key));
        println!();
        print!(
            "  Premi INVIO per usarla, oppure scrivi M e INVIO per modificarla\n  (avvio automatico tra {} secondi): ",
            AUTO_CONTINUE.as_secs()
        );
        let _ = io::stdout().flush();
        match read_line_timeout(AUTO_CONTINUE) {
            Some(answer) if answer.eq_ignore_ascii_case("m") => {}
            Some(_) => return Ok(cfg.clone()),
            None => {
                println!();
                return Ok(cfg.clone());
            }
        }
        println!();
    }

    let mut current = saved;
    loop {
        let default_url = current.as_ref().map(|c| c.api_url.clone()).unwrap_or_else(|| "http://localhost:4300".into());
        let url_in = prompt(&format!("  Indirizzo (link) delle API MySagra [{default_url}]: "));
        let api_url = normalize_url(if url_in.is_empty() { &default_url } else { &url_in });

        let key_label = match &current {
            Some(c) => format!("  Token API (X-API-KEY) [INVIO = {}]: ", mask(&c.api_key)),
            None => "  Token API (X-API-KEY): ".to_string(),
        };
        // Hidden input; falls back to a visible prompt if the console can't hide it.
        let key_in = rpassword::prompt_password(&key_label).unwrap_or_else(|_| read_line()).trim().to_string();
        let api_key = if key_in.is_empty() {
            match &current {
                Some(c) => c.api_key.clone(),
                None => {
                    println!("  Il token è obbligatorio.\n");
                    continue;
                }
            }
        } else {
            key_in
        };

        let cfg = Config { api_url, api_key };
        print!("  Verifica della connessione... ");
        let _ = io::stdout().flush();
        let ok = match verify(&cfg).await {
            Ok(n) => {
                println!("OK ({n} stampanti configurate)");
                true
            }
            Err(e) => {
                println!("ERRORE: {e}");
                ask_yes_no("  Salvare e avviare comunque? [s/N]: ")
            }
        };
        if ok {
            match save(&path, &cfg) {
                Ok(()) => println!("  Configurazione salvata in {}", path.display()),
                Err(e) => println!("  Attenzione: impossibile salvare {} ({e})", path.display()),
            }
            println!();
            return Ok(cfg);
        }
        println!();
        current = Some(cfg);
    }
}

/// Saved configuration, if any (used by the GUI).
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn load_saved() -> Option<Config> {
    load(&data_dir().join(CONFIG_FILE))
}

/// Save the configuration; returns the file path.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn save_config(cfg: &Config) -> io::Result<PathBuf> {
    let path = data_dir().join(CONFIG_FILE);
    save(&path, cfg)?;
    Ok(path)
}
