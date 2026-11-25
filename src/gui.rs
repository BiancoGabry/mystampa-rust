//! Desktop window in the MySagra style (dark zinc theme, yellow primary, Geist).
//!
//! - Setup screen: API link and token, verified against the backend, saved
//!   to mystampa.json.
//! - Status screen: server connection, printers, print queue, last order and
//!   the activity log.
//!
//! The print service runs on a tokio runtime in the background; the window
//! only reads its state.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, ColorImage, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame, Layout,
    Margin, RichText, Sense, Stroke, TextureHandle, TextureOptions, Vec2,
};
use tracing::Level;

use crate::config::{self, Config};
use crate::logging::{LogLine, GUI_LOG};
use crate::service;
use crate::state::FOOTER_PNG;
use crate::APP;

// ------------------------------------------------------------------ theme
// Dark tokens of the MySagra frontends (shadcn zinc + yellow primary).

const BG: Color32 = Color32::from_rgb(0x09, 0x09, 0x0b);
const HEADER: Color32 = Color32::from_rgb(0x0f, 0x0f, 0x12);
const CARD: Color32 = Color32::from_rgb(0x18, 0x18, 0x1b);
const MUTED: Color32 = Color32::from_rgb(0x27, 0x27, 0x2a);
const BORDER: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26); // white 10%
const INPUT_BORDER: Color32 = Color32::from_rgba_premultiplied(38, 38, 38, 38); // white 15%
const TEXT: Color32 = Color32::from_rgb(0xfa, 0xfa, 0xfa);
const TEXT_MUTED: Color32 = Color32::from_rgb(0xa1, 0xa1, 0xaa);
const PRIMARY: Color32 = Color32::from_rgb(0xea, 0xb3, 0x08);
const PRIMARY_FG: Color32 = Color32::from_rgb(0x42, 0x20, 0x06);
const SUCCESS: Color32 = Color32::from_rgb(0x22, 0xc5, 0x5e);
const WARNING: Color32 = Color32::from_rgb(0xf5, 0x9e, 0x0b);
const DESTRUCTIVE: Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);
const RADIUS: u8 = 10;

const GEIST: &[u8] = include_bytes!("../resources/fonts/Geist-Regular.ttf");
const ICON_PNG: &[u8] = include_bytes!("../resources/icon.png");
/// MyStampa logo (tent with printer), rendered from resources/logo-mystampa.svg.
const LOGO_PNG: &[u8] = include_bytes!("../resources/logo-128.png");

fn setup_style(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("geist".into(), Arc::new(FontData::from_static(GEIST)));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "geist".into());
    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    use egui::TextStyle::*;
    style.text_styles = [
        (Heading, FontId::proportional(22.0)),
        (Body, FontId::proportional(14.0)),
        (Button, FontId::proportional(14.0)),
        (Small, FontId::proportional(12.0)),
        (Monospace, FontId::monospace(12.5)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(14.0, 8.0);
    style.spacing.interact_size.y = 34.0;

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.window_corner_radius = CornerRadius::same(RADIUS + 4);
    v.extreme_bg_color = BG; // text inputs
    v.faint_bg_color = MUTED;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = PRIMARY;
    v.selection.bg_fill = PRIMARY.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0_f32, PRIMARY);
    v.text_cursor.stroke = Stroke::new(2.0_f32, PRIMARY);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(RADIUS - 2);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.inactive.bg_fill = MUTED;
    v.widgets.inactive.weak_bg_fill = MUTED;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, INPUT_BORDER);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x3f, 0x3f, 0x46);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x3f, 0x3f, 0x46);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, INPUT_BORDER);
    v.widgets.active.bg_fill = Color32::from_rgb(0x52, 0x52, 0x5b);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x52, 0x52, 0x5b);
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, PRIMARY);
    // Same MySagra dark look whatever the system theme (light on many desktops).
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_style_of(egui::Theme::Light, style.clone());
    ctx.set_style_of(egui::Theme::Dark, style);
}

fn card() -> Frame {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(RADIUS + 4))
        .inner_margin(Margin::same(20))
}

fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(PRIMARY_FG).size(14.5))
        .fill(PRIMARY)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(RADIUS - 2))
}

fn secondary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(TEXT))
        .fill(MUTED)
        .stroke(Stroke::new(1.0_f32, INPUT_BORDER))
        .corner_radius(CornerRadius::same(RADIUS - 2))
}

/// Rounded pill with a colored dot, e.g. "● Online" (fixed size, any layout).
fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(12.5), color);
    let pad = Vec2::new(10.0, 5.0);
    let dot = 8.0;
    let gap = 6.0;
    let size = Vec2::new(pad.x * 2.0 + dot + gap + galley.size().x, pad.y * 2.0 + galley.size().y);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(255), color.gamma_multiply(0.15), Stroke::new(1.0_f32, color.gamma_multiply(0.35)), egui::StrokeKind::Inside);
    p.circle_filled(egui::pos2(rect.left() + pad.x + dot / 2.0, rect.center().y), dot / 2.0, color);
    p.galley(egui::pos2(rect.left() + pad.x + dot + gap, rect.center().y - galley.size().y / 2.0), galley, color);
}

fn texture(ctx: &egui::Context, name: &str, png: &[u8], white: bool) -> Option<TextureHandle> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut px = img.into_raw();
    if white {
        // Black logo on transparent → white logo, alpha kept.
        for p in px.chunks_exact_mut(4) {
            p[0] = 255;
            p[1] = 255;
            p[2] = 255;
        }
    }
    let ci = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px);
    Some(ctx.load_texture(name, ci, TextureOptions::LINEAR))
}

pub fn window_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(ICON_PNG).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}

fn open_path(target: &str) {
    #[cfg(windows)]
    let cmd = std::process::Command::new("explorer").arg(target).spawn();
    #[cfg(not(windows))]
    let cmd = std::process::Command::new("xdg-open").arg(target).spawn();
    if let Err(e) = cmd {
        tracing::warn!(target: "gui", error = %e, "Cannot open {target}");
    }
}

/// Start a new instance (after changing the connection) and let this one close.
fn restart_self() {
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok());
    if let Some(exe) = exe {
        if let Err(e) = std::process::Command::new(exe).spawn() {
            tracing::error!(target: "gui", error = %e, "Restart failed");
        }
    }
}

// ------------------------------------------------------------------ log text

/// Italian, user-facing wording of the most common service messages.
fn friendly(msg: &str) -> &str {
    match msg {
        "MyStampa starting" => "Avvio di MyStampa",
        "Initialization complete" => "Avvio completato",
        "Connecting" => "Connessione al server MySagra…",
        "Connected" => "Connesso al server MySagra",
        "Connection closed, will reconnect" => "Connessione chiusa, nuovo tentativo a breve",
        "Connection error" => "Errore di connessione al server",
        "Connection failed" => "Il server ha rifiutato la connessione",
        "Connection timed out" => "Il server non risponde",
        "Waiting before reconnecting" => "Attesa prima di riconnettersi",
        "No data received, dropping stale connection" => "Nessun dato dal server, riconnessione",
        "Event received" => "Evento ricevuto",
        "Print order handled" => "Ordine stampato",
        "Print order failed" => "Stampa dell'ordine non riuscita",
        "Printed successfully" => "Stampa completata",
        "Job added to queue" => "Stampa messa in coda",
        "Queued job printed" => "Stampa in coda completata",
        "Processing queue" => "Nuovo tentativo per le stampe in coda",
        "Printer status changed" => "Stato della stampante cambiato",
        "Printer out of paper" => "Stampante senza carta",
        "Printer out of paper, adding job to queue" => "Carta finita: stampa messa in coda",
        "Printer status check failed" => "Stampante non raggiungibile",
        "Status check failed, adding job to queue" => "Stampante non raggiungibile: stampa messa in coda",
        "Print failed, adding job to queue" => "Stampa non riuscita: messa in coda",
        "Printer reachable again, patching status to ONLINE" => "Stampante di nuovo raggiungibile",
        "Printer status patched" => "Stato della stampante inviato al server",
        "Settings applied" => "Impostazioni della sagra aggiornate",
        "Receipt logo loaded" => "Logo dello scontrino caricato",
        "Failed to download receipt logo" => "Logo dello scontrino non scaricabile",
        "Cash drawer opened" => "Cassetto aperto",
        "Cash drawer open failed" => "Apertura del cassetto non riuscita",
        "Processing order cancellation" => "Ordine annullato",
        "General closure handled" => "Chiusura generale stampata",
        "General closure failed" => "Chiusura generale non riuscita",
        "Request failed, retrying" => "Richiesta al server non riuscita, nuovo tentativo",
        "Request failed permanently" => "Richiesta al server non riuscita",
        "Unauthorized: API key invalid or missing" => "Token API non valido",
        "No printer found for cash register, skipping cash receipt" => "Nessuna stampante per questa cassa: scontrino non stampato",
        "No cached printer for kitchen receipt" => "Stampante della cucina non trovata",
        "Fetching printers failed, keeping the previous list" => "Elenco stampanti non aggiornato (server non raggiungibile)",
        "Queue full, dropping job" => "Coda piena: stampa scartata",
        m => m,
    }
}

/// The few fields worth showing under a log line, in Italian,
/// with printer ids replaced by their names.
fn friendly_fields(line: &LogLine) -> String {
    let app = APP.get();
    let printer_name = |id: &str| match app.and_then(|a| a.find_printer(id)) {
        Some(p) if !p.name().is_empty() => p.name().to_string(),
        _ => id.to_string(),
    };
    line.fields
        .split("  ")
        .filter_map(|f| {
            let (k, v) = f.split_once('=')?;
            let v = v.trim_matches('"');
            Some(match k {
                "printer_id" | "cash_printer_id" if !v.is_empty() => format!("stampante {}", printer_name(v)),
                "kitchen_printers" => {
                    let names: Vec<String> = v.trim_matches(['[', ']']).split(',').map(|x| printer_name(x.trim().trim_matches('"'))).filter(|x| !x.is_empty()).collect();
                    if names.is_empty() { return None; }
                    format!("cucina {}", names.join(", "))
                }
                "order_id" => format!("ordine {v}"),
                "status" => format!("stato {v}"),
                "printer_count" => format!("{v} stampanti"),
                "queue_size" => format!("{v} in coda"),
                "pending_jobs" => format!("{v} stampe in coda"),
                "attempt" => format!("tentativo {v}"),
                "error" => v.to_string(),
                _ => return None,
            })
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

// ------------------------------------------------------------------ app

#[derive(PartialEq)]
enum Screen {
    Setup,
    Status,
}

type Pending<T> = Arc<Mutex<Option<T>>>;

struct GuiApp {
    rt: tokio::runtime::Runtime,
    screen: Screen,
    running: bool,
    url: String,
    key: String,
    show_key: bool,
    verifying: Option<Pending<Result<usize, String>>>,
    setup_error: Option<String>,
    can_force: bool,
    confirm_close: bool,
    allow_close: bool,
    log_dir: Option<PathBuf>,
    logo: Option<TextureHandle>,
    footer: Option<TextureHandle>,
    screenshot: Option<(PathBuf, Instant, Duration, bool)>,
}

impl GuiApp {
    fn new(cc: &eframe::CreationContext<'_>, log_dir: Option<PathBuf>) -> Self {
        setup_style(&cc.egui_ctx);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
        let saved = config::load_saved();
        let screenshot = std::env::var_os("MYSTAMPA_SCREENSHOT").map(|p| {
            let delay = std::env::var("MYSTAMPA_SCREENSHOT_DELAY").ok().and_then(|d| d.parse().ok()).unwrap_or(3.0);
            (PathBuf::from(p), Instant::now(), Duration::from_secs_f64(delay), false)
        });
        let mut app = GuiApp {
            rt,
            screen: Screen::Setup,
            running: false,
            url: saved.as_ref().map(|c| c.api_url.clone()).unwrap_or_default(),
            key: saved.as_ref().map(|c| c.api_key.clone()).unwrap_or_default(),
            show_key: false,
            verifying: None,
            setup_error: None,
            can_force: false,
            confirm_close: false,
            allow_close: false,
            log_dir,
            logo: texture(&cc.egui_ctx, "logo", LOGO_PNG, false),
            footer: texture(&cc.egui_ctx, "footer", FOOTER_PNG, false),
            screenshot,
        };
        // A saved connection starts printing right away (e.g. after a power cut).
        if let Some(cfg) = saved {
            app.start_service(cfg);
        }
        app
    }

    fn start_service(&mut self, cfg: Config) {
        self.rt.spawn(service::start(cfg));
        self.running = true;
        self.screen = Screen::Status;
    }

    fn current_config(&self) -> Config {
        Config { api_url: config::normalize_url(&self.url), api_key: self.key.trim().to_string() }
    }

    fn submit(&mut self, ctx: &egui::Context) {
        let cfg = self.current_config();
        if cfg.api_url.is_empty() {
            self.setup_error = Some("Inserisci l'indirizzo delle API.".into());
            return;
        }
        if cfg.api_key.is_empty() {
            self.setup_error = Some("Inserisci il token API.".into());
            return;
        }
        self.setup_error = None;
        self.can_force = false;
        let slot: Pending<Result<usize, String>> = Arc::new(Mutex::new(None));
        let out = slot.clone();
        let ctx = ctx.clone();
        self.rt.spawn(async move {
            let r = config::verify(&cfg).await;
            *out.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
        self.verifying = Some(slot);
    }

    /// Save and start; when already running, restart with the new connection.
    fn accept(&mut self, ctx: &egui::Context) {
        let cfg = self.current_config();
        if let Err(e) = config::save_config(&cfg) {
            self.setup_error = Some(format!("Impossibile salvare la configurazione: {e}"));
            return;
        }
        if self.running {
            restart_self();
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            self.start_service(cfg);
        }
    }

    fn poll_verify(&mut self, ctx: &egui::Context) {
        let Some(slot) = &self.verifying else { return };
        let result = slot.lock().unwrap().take();
        match result {
            None => ctx.request_repaint_after(Duration::from_millis(100)),
            Some(Ok(_)) => {
                self.verifying = None;
                self.accept(ctx);
            }
            Some(Err(e)) => {
                self.verifying = None;
                self.setup_error = Some(format!("Verifica non riuscita: {e}."));
                self.can_force = true;
            }
        }
    }

    // ---------------------------------------------------------- header

    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .exact_height(64.0)
            .frame(Frame::new().fill(HEADER).stroke(Stroke::new(1.0_f32, BORDER)).inner_margin(Margin::symmetric(20, 0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(46.0), Sense::hover());
                    if let Some(logo) = &self.logo {
                        ui.painter().image(logo.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.add_space(12.0);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new("MyStampa").size(18.0).strong());
                        let sub = APP.get().and_then(|a| a.sagra_name()).unwrap_or_else(|| "Servizio di stampa MySagra".into());
                        ui.label(RichText::new(sub).size(12.5).color(TEXT_MUTED));
                    });
                    if self.screen == Screen::Status {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match APP.get() {
                            Some(app) if app.sse_connected.load(Ordering::Relaxed) => badge(ui, "Connesso al server", SUCCESS),
                            Some(_) => badge(ui, "Connessione in corso…", WARNING),
                            None => badge(ui, "Avvio…", TEXT_MUTED),
                        });
                    }
                });
            });
    }

    // ---------------------------------------------------------- setup

    fn setup_screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(Frame::new().fill(BG).inner_margin(Margin::same(24))).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.08).clamp(8.0, 60.0));
                    ui.set_max_width(460.0);
                    card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            ui.label(RichText::new("Collega MyStampa a MySagra").size(20.0).strong());
                            ui.label(
                                RichText::new("Inserisci l'indirizzo del server MySagra e il token API del servizio di stampa. Li trovi nel pannello di amministrazione.")
                                    .color(TEXT_MUTED),
                            );
                            ui.add_space(14.0);

                            let busy = self.verifying.is_some();
                            ui.label(RichText::new("Indirizzo API").size(13.5));
                            let full_w = ui.available_width();
                            let url_resp = ui
                                .add_enabled_ui(!busy, |ui| {
                                    ui.add_sized(
                                        [full_w, 36.0],
                                        egui::TextEdit::singleline(&mut self.url)
                                            .hint_text(RichText::new("http://192.168.1.10:4300").color(TEXT_MUTED.gamma_multiply(0.7)))
                                            .margin(Margin::symmetric(10, 8)),
                                    )
                                })
                                .inner;
                            ui.add_space(8.0);
                            ui.label(RichText::new("Token API").size(13.5));
                            let mut key_enter = false;
                            ui.horizontal(|ui| {
                                let toggle_w = 70.0;
                                let key_w = ui.available_width() - toggle_w - 8.0;
                                let r = ui
                                    .add_enabled_ui(!busy, |ui| {
                                        ui.add_sized(
                                            [key_w, 36.0],
                                            egui::TextEdit::singleline(&mut self.key)
                                                .hint_text(RichText::new("ms_pt_…").color(TEXT_MUTED.gamma_multiply(0.7)))
                                                .password(!self.show_key)
                                                .margin(Margin::symmetric(10, 8)),
                                        )
                                    })
                                    .inner;
                                key_enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                let label = if self.show_key { "Nascondi" } else { "Mostra" };
                                if ui.add_sized([toggle_w, 36.0], secondary_button(label)).clicked() {
                                    self.show_key = !self.show_key;
                                }
                            });
                            let url_enter = url_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                            if let Some(err) = &self.setup_error {
                                ui.add_space(6.0);
                                Frame::new()
                                    .fill(DESTRUCTIVE.gamma_multiply(0.12))
                                    .stroke(Stroke::new(1.0_f32, DESTRUCTIVE.gamma_multiply(0.4)))
                                    .corner_radius(CornerRadius::same(RADIUS - 2))
                                    .inner_margin(Margin::symmetric(12, 8))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(RichText::new(err).color(DESTRUCTIVE));
                                    });
                            }

                            ui.add_space(12.0);
                            let w = ui.available_width();
                            if busy {
                                ui.add_enabled_ui(false, |ui| {
                                    ui.add_sized([w, 38.0], primary_button("Verifica in corso…"));
                                });
                            } else if ui.add_sized([w, 38.0], primary_button("Collega e avvia")).clicked() || key_enter || url_enter {
                                self.submit(ctx);
                            }
                            if self.can_force && ui.add_sized([w, 34.0], secondary_button("Salva e avvia comunque")).clicked() {
                                self.accept(ctx);
                            }
                            if self.running && !busy && ui.add_sized([w, 34.0], secondary_button("Annulla")).clicked() {
                                self.setup_error = None;
                                self.can_force = false;
                                self.screen = Screen::Status;
                            }
                        });
                    });
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new(format!(
                            "La configurazione viene salvata in {}",
                            config::data_dir().join(config::CONFIG_FILE).display()
                        ))
                        .size(12.0)
                        .color(TEXT_MUTED),
                    );
                });
            });
        });
    }

    // ---------------------------------------------------------- status

    fn footer_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("footer")
            .frame(Frame::new().fill(HEADER).stroke(Stroke::new(1.0_f32, BORDER)).inner_margin(Margin::symmetric(20, 10)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(tex) = &self.footer {
                        // Footer image is black on white: shown small and inverted-free on a light chip.
                        Frame::new().fill(Color32::WHITE).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(6, 2)).show(ui, |ui| {
                            ui.add(egui::Image::new((tex.id(), tex.size_vec2() * 0.42)));
                        });
                    }
                    let api = APP.get().map(|a| a.cfg.api_url.clone()).unwrap_or_default();
                    ui.label(RichText::new(format!("v{}  ·  {api}", env!("CARGO_PKG_VERSION"))).size(12.0).color(TEXT_MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(secondary_button("Modifica connessione")).clicked() {
                            self.screen = Screen::Setup;
                        }
                        if let Some(dir) = self.log_dir.clone() {
                            if ui.add(secondary_button("Cartella log")).clicked() {
                                open_path(&dir.display().to_string());
                            }
                        }
                    });
                });
            });
    }

    fn stat(ui: &mut egui::Ui, width: f32, title: &str, value: &str, value_color: Color32, sub: &str) {
        // Outer width = content + 2 * margin + 2 * stroke
        card().inner_margin(Margin::symmetric(16, 14)).show(ui, |ui| {
            ui.set_width(width - 34.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(RichText::new(title).size(12.5).color(TEXT_MUTED));
                ui.add(egui::Label::new(RichText::new(value).size(22.0).color(value_color)).truncate());
                ui.add(egui::Label::new(RichText::new(sub).size(12.0).color(TEXT_MUTED)).truncate());
            });
        });
    }

    fn status_screen(&mut self, ctx: &egui::Context) {
        self.footer_bar(ctx);
        egui::CentralPanel::default().frame(Frame::new().fill(BG).inner_margin(Margin::same(20))).show(ctx, |ui| {
            let Some(app) = APP.get().cloned() else {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
                return;
            };
            let printers = app.printers_snapshot();
            let online = printers.iter().filter(|p| p.status.eq_ignore_ascii_case("ONLINE")).count();
            let queued = app.queue.len();
            let connected = app.sse_connected.load(Ordering::Relaxed);

            // Stat cards
            let gap = 12.0;
            let w = ((ui.available_width() - gap * 3.0) / 4.0).floor().max(120.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                let (v, c, s) = if connected { ("Connesso", SUCCESS, "In attesa di ordini") } else { ("Non connesso", WARNING, "Nuovo tentativo automatico") };
                Self::stat(ui, w, "Server MySagra", v, c, s);
                let c = if printers.is_empty() || online == printers.len() { TEXT } else { WARNING };
                Self::stat(ui, w, "Stampanti online", &format!("{online} / {}", printers.len()), c, "Controllo ogni 2 minuti");
                let (c, s) = if queued > 0 { (WARNING, "Nuovo tentativo ogni minuto") } else { (TEXT, "Nessuna stampa in attesa") };
                Self::stat(ui, w, "Stampe in coda", &queued.to_string(), c, s);
                let last = app.last_order.lock().unwrap().clone();
                let handled = app.orders_handled.load(Ordering::Relaxed);
                match last {
                    Some((label, time)) => Self::stat(ui, w, "Ultimo ordine", &label, TEXT, &format!("alle {time} · {handled} da avvio")),
                    None => Self::stat(ui, w, "Ultimo ordine", "—", TEXT_MUTED, "Nessun ordine ancora"),
                }
            });
            ui.add_space(4.0);

            // Printers
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Stampanti").size(16.0).strong());
                ui.label(RichText::new("Stato in tempo reale delle stampanti configurate in MySagra.").size(12.5).color(TEXT_MUTED));
                ui.add_space(6.0);
                if printers.is_empty() {
                    ui.label(RichText::new("Nessuna stampante configurata.").color(TEXT_MUTED));
                }
                let total_w = ui.available_width();
                for (i, p) in printers.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(Vec2::new(total_w * 0.45, 36.0), Layout::top_down(Align::Min), |ui| {
                            ui.set_min_width(total_w * 0.45);
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(RichText::new(if p.name().is_empty() { p.id() } else { p.name() }).size(14.0));
                            if let Some(d) = p.description.as_deref().filter(|d| !d.is_empty()) {
                                ui.label(RichText::new(d).size(12.0).color(TEXT_MUTED));
                            }
                        });
                        ui.allocate_ui_with_layout(Vec2::new(total_w * 0.30, 36.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.label(RichText::new(format!("{}:{}", p.ip(), p.port)).monospace().color(TEXT_MUTED));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match p.status.to_uppercase().as_str() {
                            "ONLINE" => badge(ui, "Online", SUCCESS),
                            "OFFLINE" => badge(ui, "Offline", DESTRUCTIVE),
                            "ERROR" => badge(ui, "Carta finita", WARNING),
                            _ => badge(ui, "Sconosciuto", TEXT_MUTED),
                        });
                    });
                }
            });
            ui.add_space(4.0);

            // Activity
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(ui.available_height());
                ui.label(RichText::new("Attività").size(16.0).strong());
                ui.add_space(4.0);
                let lines: Vec<LogLine> = GUI_LOG.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default();
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().interact_size.y = 18.0;
                    ui.spacing_mut().item_spacing.y = 5.0;
                    for l in lines.iter().filter(|l| l.target != "http" || l.level <= Level::WARN) {
                        let color = match l.level {
                            Level::ERROR => DESTRUCTIVE,
                            Level::WARN => WARNING,
                            _ => TEXT,
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            ui.label(RichText::new(&l.time).monospace().color(TEXT_MUTED));
                            ui.label(RichText::new(friendly(&l.message)).color(color));
                            let f = friendly_fields(l);
                            if !f.is_empty() {
                                ui.label(RichText::new(f).size(12.0).color(TEXT_MUTED));
                            }
                        });
                    }
                });
            });
        });
    }

    fn close_dialog(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.running {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
        if !self.confirm_close {
            return;
        }
        egui::Modal::new(egui::Id::new("confirm_close"))
            .frame(card().inner_margin(Margin::same(24)))
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(RichText::new("Chiudere MyStampa?").size(18.0).strong());
                ui.label(RichText::new("Finché MyStampa è chiuso gli ordini non vengono stampati.").color(TEXT_MUTED));
                let queued = APP.get().map(|a| a.queue.len()).unwrap_or(0);
                if queued > 0 {
                    ui.label(RichText::new(format!("Ci sono {queued} stampe in coda che andranno perse.")).color(WARNING));
                }
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(egui::Button::new(RichText::new("Chiudi").color(TEXT)).fill(Color32::from_rgb(0xb9, 0x1c, 0x1c)).corner_radius(CornerRadius::same(RADIUS - 2))).clicked() {
                        if let Some(app) = APP.get() {
                            service::log_shutdown(app);
                        }
                        self.allow_close = true;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.add(secondary_button("Annulla")).clicked() {
                        self.confirm_close = false;
                    }
                });
            });
    }

    /// Test hook: MYSTAMPA_SCREENSHOT=file.png saves the window and exits.
    fn screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, start, delay, requested)) = &mut self.screenshot else { return };
        ctx.request_repaint_after(Duration::from_millis(100));
        if !*requested && start.elapsed() >= *delay {
            *requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                let _ = buf.save(&*path);
            }
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_verify(ctx);
        self.header(ctx);
        match self.screen {
            Screen::Setup => self.setup_screen(ctx),
            Screen::Status => self.status_screen(ctx),
        }
        self.close_dialog(ctx);
        self.screenshot(ctx);
        // Service state changes in the background
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::from(BG).to_array()
    }
}

pub fn run(log_dir: Option<PathBuf>) -> Result<(), String> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("MyStampa")
        .with_app_id("mystampa")
        .with_inner_size([980.0, 700.0])
        .with_min_inner_size([760.0, 520.0]);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(Arc::new(icon));
    }
    let options = eframe::NativeOptions { viewport, centered: true, ..Default::default() };
    eframe::run_native("MyStampa", options, Box::new(move |cc| Ok(Box::new(GuiApp::new(cc, log_dir)))))
        .map_err(|e| e.to_string())
}
