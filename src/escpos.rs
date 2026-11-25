//! ESC/POS receipt builders, CP858 encoding and TCP transport.

use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};
use chrono_tz::Europe::Rome;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::models::{value_to_f64, value_to_string, IncomingOrder};
use crate::state::App;

pub const RECEIPT_W: usize = 48; // Font A on 80 mm paper

const GS: &str = "\x1D";
const TXT_NORMAL: &str = "\x1D!\x00";
const TXT_BIG: &str = "\x1D!\x11"; // 2x width + 2x height
const TXT_MEDIUM: &str = "\x1D!\x01"; // double height only
const BOLD_ON: &str = "\x1BE\x01";
const BOLD_OFF: &str = "\x1BE\x00";
const SELECT_CP858: &[u8] = b"\x1Bt\x13";
const FEED_AND_CUT: &[u8] = b"\x1Bd\x06\x1Bi";

// ---------------------------------------------------------------- CP858

/// Code page 858 (CP850 with € at 0xD5), bytes 0x80..=0xFF.
const CP858_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', 'ø', '£', 'Ø', '×', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '®', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', 'Á', 'Â', 'À', '©', '╣', '║', '╗', '╝', '¢', '¥', '┐', //
    '└', '┴', '┬', '├', '─', '┼', 'ã', 'Ã', '╚', '╔', '╩', '╦', '╠', '═', '╬', '¤', //
    'ð', 'Ð', 'Ê', 'Ë', 'È', '€', 'Í', 'Î', 'Ï', '┘', '┌', '█', '▄', '¦', 'Ì', '▀', //
    'Ó', 'ß', 'Ô', 'Ò', 'õ', 'Õ', 'µ', 'þ', 'Þ', 'Ú', 'Û', 'Ù', 'ý', 'Ý', '¯', '´', //
    '\u{AD}', '±', '‗', '¾', '¶', '§', '÷', '¸', '°', '¨', '·', '¹', '³', '²', '■', '\u{A0}',
];

pub fn encode_cp858(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        if (ch as u32) < 0x80 {
            out.push(ch as u8);
            continue;
        }
        if let Some(i) = CP858_HIGH.iter().position(|c| *c == ch) {
            out.push(0x80 + i as u8);
            continue;
        }
        // Common typographic characters the code page lacks
        let sub: &str = match ch {
            '‘' | '’' | '‚' | '′' => "'",
            '“' | '”' | '„' | '″' => "\"",
            '–' | '—' | '−' => "-",
            '…' => "...",
            _ => "?",
        };
        out.extend_from_slice(sub.as_bytes());
    }
    out
}

// ---------------------------------------------------------------- text helpers

fn clen(s: &str) -> usize {
    s.chars().count()
}

fn cut(s: &str, w: usize) -> String {
    s.chars().take(w).collect()
}

fn line(ch: char) -> String {
    ch.to_string().repeat(RECEIPT_W)
}

fn pad_right(s: &str, w: usize) -> String {
    let l = clen(s);
    if l >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - l))
    }
}

/// Text on the left, amount on the right, always aligned.
fn lr(left: &str, right: &str) -> String {
    let left = left.trim_end();
    let right = right.trim();
    let left_w = RECEIPT_W.saturating_sub(clen(right) + 1);
    format!("{} {}", pad_right(&cut(left, left_w), left_w), right)
}

fn trim_opt(v: &Option<String>) -> String {
    v.as_deref().map(str::trim).unwrap_or("").to_string()
}

/// Order code with zeros as slashed "Ø" so they can't be read as the letter O.
fn code_str(v: &Option<String>) -> String {
    trim_opt(v).replace('0', "Ø")
}

/// Order code in double width on top of `base` size (GS ! n), restored afterwards.
fn wide_code(v: &Option<String>, base: u8) -> String {
    let code = code_str(v);
    if code.is_empty() || base & 0x70 != 0 {
        return code;
    }
    format!("{GS}!{}{code}{GS}!{}", (base | 0x10) as char, base as char)
}

/// Word-wrap to `max` chars; continuation lines start with `indent`.
pub fn wrap_text(text: &str, max: usize, indent: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split(' ') {
        if clen(&current) + clen(word) > max {
            if !current.is_empty() {
                lines.push(current.trim_end().to_string());
            }
            current = if lines.is_empty() { String::new() } else { indent.to_string() };
            if clen(&current) + clen(word) > max {
                // Single word longer than the line: hard split
                let mut temp: Vec<char> = word.chars().collect();
                while clen(&current) + temp.len() > max {
                    let cut_len = max.saturating_sub(clen(&current)).max(1).min(temp.len());
                    let head: String = temp[..cut_len].iter().collect();
                    lines.push(format!("{current}{head}"));
                    temp.drain(..cut_len);
                    current = indent.to_string();
                }
                current.push_str(&temp.iter().collect::<String>());
                current.push(' ');
            } else {
                current.push_str(word);
                current.push(' ');
            }
        } else {
            current.push_str(word);
            current.push(' ');
        }
    }
    if !current.trim().is_empty() {
        lines.push(current.trim_end().to_string());
    }
    lines
}

/// UTC timestamp → Italian local time "DD/MM/YYYY HH:MM".
pub fn format_local_date(s: &str) -> String {
    let utc: Option<DateTime<Utc>> = DateTime::parse_from_rfc3339(s.trim())
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%dT%H:%M:%S%.f")
                .or_else(|_| NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S%.f"))
                .ok()
                .map(|n| n.and_utc())
        });
    match utc {
        Some(d) => d.with_timezone(&Rome).format("%d/%m/%Y %H:%M").to_string(),
        None => s.to_string(),
    }
}

fn currency_info(code: &str) -> (&str, usize) {
    match code {
        "EUR" => ("€", 2),
        "GBP" => ("£", 2),
        "CAD" => ("CA$", 2),
        "AUD" => ("A$", 2),
        "HUF" => ("HUF", 0),
        other => (other, 2),
    }
}

/// Amount in the configured currency with decimal comma.
/// suffix: 12,34€ / 12,34 CHF (receipt columns); prefix: €12,34 / CHF 12,34.
fn format_money(app: &App, n: f64, suffix: bool) -> String {
    let code = app.currency_code();
    let (symbol, decimals) = currency_info(&code);
    let v = if n.is_finite() { n } else { 0.0 };
    let amount = format!("{v:.decimals$}").replace('.', ",");
    let sep = if clen(symbol) > 1 { " " } else { "" };
    if suffix {
        format!("{amount}{sep}{symbol}")
    } else {
        format!("{symbol}{sep}{amount}")
    }
}

// ---------------------------------------------------------------- receipts

pub struct KitchenLine {
    pub food_name: String,
    pub quantity: i64,
    pub notes: Option<String>,
}

pub struct CashLine {
    pub food_name: String,
    pub quantity: i64,
    pub notes: Option<String>,
    pub unit_price: f64,
    pub surcharge: f64,
}

pub struct StationTicket {
    pub station_name: String,
    pub lines: Vec<KitchenLine>,
}

pub fn build_kitchen_receipt(app: &App, order: &IncomingOrder, lines: &[KitchenLine], progress: u64) -> Vec<u8> {
    let show_numbers = app.show_numbers();
    let mut out: Vec<String> = vec![String::new()];

    let mut top = format!("PROGR: {progress}");
    if show_numbers {
        if let Some(n) = &order.ticket_number {
            top = format!("N°: {n} - {top}");
        }
    } else if !code_str(&order.display_code).is_empty() {
        top = format!("COD: {} - {top}", code_str(&order.display_code));
    }
    let confirmed = trim_opt(&order.confirmed_at);
    if !confirmed.is_empty() {
        let time_text = format!("  ORA: {}", format_local_date(&confirmed));
        if clen(&top) + clen(&time_text) <= RECEIPT_W {
            let padding = RECEIPT_W - clen(&top) - clen(&time_text);
            top = format!("{top}{}{time_text}", " ".repeat(padding.max(1)));
        } else {
            top.push_str(&time_text);
        }
    }
    out.push(cut(&top, RECEIPT_W));
    out.push(line('='));

    let table = Some(trim_opt(&order.table)).filter(|s| !s.is_empty()).unwrap_or_else(|| "-".into());
    let customer = Some(trim_opt(&order.customer)).filter(|s| !s.is_empty()).unwrap_or_else(|| "-".into());
    let has_table = order.table.as_deref() != Some("NO_TABLE_PRESET");

    let tav = if has_table { format!("PROGR: {progress} - TAVOLO: {table}") } else { format!("N°: {progress}") };
    out.push(format!("{TXT_BIG}{}{TXT_NORMAL}", cut(&tav, RECEIPT_W / 2)));

    if customer != "-" {
        let cust = format!("CLIENTE: {customer}");
        if clen(&cust) <= RECEIPT_W / 2 {
            out.push(format!("{TXT_BIG}{cust}{TXT_NORMAL}"));
        } else {
            out.push(format!("{TXT_MEDIUM}{}{TXT_NORMAL}", cut(&cust, RECEIPT_W)));
        }
    }

    if show_numbers {
        if let Some(n) = &order.ticket_number {
            out.push(format!("{TXT_BIG}{BOLD_ON}N° ORDINE: {n}{BOLD_OFF}{TXT_NORMAL}"));
        }
        if !code_str(&order.display_code).is_empty() {
            out.push(cut(&format!("CODICE: {}", wide_code(&order.display_code, 0)), RECEIPT_W));
        }
    } else if !code_str(&order.display_code).is_empty() {
        out.push(format!("{TXT_MEDIUM}CODICE: {}{TXT_NORMAL}", wide_code(&order.display_code, 0x01)));
    }

    out.push(line('-'));

    for l in lines {
        let name = Some(l.food_name.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "FOOD".into());
        let prefix = format!("{}x ", l.quantity);
        let indent = " ".repeat(clen(&prefix));
        for w in wrap_text(&format!("{prefix}{name}"), RECEIPT_W, &indent) {
            out.push(format!("{TXT_MEDIUM}{w}{TXT_NORMAL}"));
        }
        let notes = trim_opt(&l.notes);
        if !notes.is_empty() {
            let note_indent = format!("{indent}  * ");
            for note in notes.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                out.extend(wrap_text(&format!("{note_indent}{note}"), RECEIPT_W, &format!("{indent}    ")));
            }
        }
        out.push(String::new());
    }
    out.push(line('-'));
    encode_cp858(&out.join("\n"))
}

#[allow(clippy::too_many_lines)]
pub async fn build_cash_receipt(
    app: &App,
    order: &IncomingOrder,
    lines: &[CashLine],
    single_tickets: &[KitchenLine],
    station_tickets: &[StationTicket],
) -> Vec<u8> {
    let show_numbers = app.show_numbers();
    let money = |n: f64| format_money(app, n, true);
    let mut out: Vec<String> = Vec::new();

    if show_numbers {
        if let Some(n) = &order.ticket_number {
            out.push(format!("{TXT_BIG}{BOLD_ON}N° ORDINE: {n}{BOLD_OFF}{TXT_NORMAL}"));
        }
        if !code_str(&order.display_code).is_empty() {
            out.push(cut(&format!("CODICE: {}", wide_code(&order.display_code, 0)), RECEIPT_W));
        }
    } else {
        if !code_str(&order.display_code).is_empty() {
            out.push(format!("{TXT_BIG}{BOLD_ON}Codice Ordine: {}{BOLD_OFF}{TXT_NORMAL}", code_str(&order.display_code)));
        }
        if !trim_opt(&order.ticket_number).is_empty() {
            out.push(cut(&format!("NUMERO: {}", trim_opt(&order.ticket_number)), RECEIPT_W));
        }
    }
    let table = trim_opt(&order.table);
    if table != "NO_TABLE_PRESET" {
        out.push(cut(&format!("TAVOLO: {}", if table.is_empty() { "-" } else { &table }), RECEIPT_W));
    }
    let customer = trim_opt(&order.customer);
    out.push(cut(&format!("CLIENTE: {}", if customer.is_empty() { "-" } else { &customer }), RECEIPT_W));

    let pm = trim_opt(&order.payment_method).to_uppercase();
    if !pm.is_empty() {
        let pm = match pm.as_str() {
            "CASH" => "CONTANTI".to_string(),
            "CARD" => "PAGAMENTO ELETTRONICO".to_string(),
            _ => pm,
        };
        out.push(cut(&format!("PAGAMENTO: {pm}"), RECEIPT_W));
    }
    let confirmed = trim_opt(&order.confirmed_at);
    if !confirmed.is_empty() {
        out.push(cut(&format!("ORA: {}", format_local_date(&confirmed)), RECEIPT_W));
    }
    out.push(line('-'));

    let mut subtotal = 0.0;
    let mut surcharge_sum = 0.0;
    for l in lines {
        let name = Some(l.food_name.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "FOOD NAME NOT FOUND".into());
        let qty = l.quantity as f64;
        let row_base = l.unit_price * qty;
        let row_extra = l.surcharge * qty;
        subtotal += row_base;
        surcharge_sum += row_extra;

        let price = money(row_base + row_extra);
        let avail = RECEIPT_W.saturating_sub(clen(&price) + 1);
        let prefix = format!("{}x ", l.quantity);
        let indent = " ".repeat(clen(&prefix));
        let wrapped = wrap_text(&format!("{prefix}{name}"), avail, &indent);
        out.push(lr(wrapped.first().map(String::as_str).unwrap_or(""), &price));
        for w in wrapped.iter().skip(1) {
            out.push(cut(w, RECEIPT_W));
        }
        let notes = trim_opt(&l.notes);
        if !notes.is_empty() {
            out.extend(wrap_text(&format!("{indent}NOTE: {notes}"), RECEIPT_W, &format!("{indent}      ")));
        }
        if row_extra > 0.0 {
            out.push(lr(&format!("{indent}EXTRA"), &money(row_extra)));
        }
        out.push(String::new());
    }
    out.push(line('-'));

    let discount = order.discount.unwrap_or(0.0);
    let total = (subtotal + surcharge_sum - discount).max(0.0);
    out.push(lr("SUBTOTALE", &money(subtotal)));
    if surcharge_sum > 0.0 {
        out.push(lr("EXTRA TOTALI", &money(surcharge_sum)));
    }
    if discount > 0.0 {
        out.push(lr("SCONTO", &format!("-{}", money(discount))));
    }
    out.push(line('='));
    out.push(lr("TOTALE", &money(total)));
    out.push(line('='));

    let mut buf: Vec<u8> = Vec::new();
    // Sagra logo from the backend settings, or the bundled MySagra logo.
    match app.receipt_logo().await {
        Some(logo) => buf.extend_from_slice(&logo),
        None => buf.extend_from_slice(&app.default_logo),
    }
    buf.extend(encode_cp858(&format!("\n\n\n{}\n\n", out.join("\n"))));
    buf.extend_from_slice(&app.footer);

    // Single tickets
    for t in single_tickets {
        buf.extend_from_slice(FEED_AND_CUT);
        let mut s = format!("\n\n{TXT_BIG}{}x {}{TXT_NORMAL}\n", t.quantity, t.food_name.trim().to_uppercase());
        let notes = trim_opt(&t.notes);
        if !notes.is_empty() {
            for w in wrap_text(&format!("NOTE: {notes}"), RECEIPT_W, "      ") {
                s.push_str(&format!("{TXT_MEDIUM}{w}{TXT_NORMAL}\n"));
            }
        }
        let mut sub = String::new();
        if show_numbers {
            if let Some(n) = &order.ticket_number {
                sub.push_str(&format!("N°: {n}"));
            }
            if !code_str(&order.display_code).is_empty() {
                if !sub.is_empty() {
                    sub.push_str(" - ");
                }
                sub.push_str(&format!("COD: {}", wide_code(&order.display_code, 0)));
            }
        } else if !code_str(&order.display_code).is_empty() {
            sub.push_str(&format!("CODICE: {}", wide_code(&order.display_code, 0)));
        }
        if !customer.is_empty() {
            if !sub.is_empty() {
                sub.push_str(" - ");
            }
            sub.push_str(&format!("CLIENTE: {customer}"));
        }
        if !sub.is_empty() {
            s.push_str(&format!("{}\n", cut(&sub, RECEIPT_W)));
        }
        s.push('\n');
        buf.extend(encode_cp858(&s));
        buf.extend_from_slice(&app.footer);
    }

    // Pickup tickets per station
    for st in station_tickets {
        buf.extend_from_slice(FEED_AND_CUT);
        let label = if show_numbers {
            match &order.ticket_number {
                Some(n) => format!("N° {n}"),
                None => code_str(&order.display_code),
            }
        } else {
            let c = code_str(&order.display_code);
            if !c.is_empty() {
                c
            } else {
                order.ticket_number.as_ref().map(|n| format!("N° {n}")).unwrap_or_default()
            }
        };
        let mut s = String::from("\n\n");
        if !label.is_empty() {
            s.push_str(&format!("{TXT_BIG}{BOLD_ON}{label}{BOLD_OFF}{TXT_NORMAL}\n"));
        }
        s.push_str(&format!("{TXT_BIG}{BOLD_ON}RITIRA A: {}{BOLD_OFF}{TXT_NORMAL}\n", st.station_name.trim().to_uppercase()));
        let mut sub = String::new();
        if show_numbers {
            if !code_str(&order.display_code).is_empty() {
                sub.push_str(&format!("COD: {}", wide_code(&order.display_code, 0)));
            }
        } else if let Some(n) = &order.ticket_number {
            sub.push_str(&format!("N°: {n}"));
        }
        if !customer.is_empty() {
            if !sub.is_empty() {
                sub.push_str(" - ");
            }
            sub.push_str(&format!("CLIENTE: {customer}"));
        }
        if !sub.is_empty() {
            s.push_str(&format!("{}\n", cut(&sub, RECEIPT_W)));
        }
        s.push('\n');
        for item in &st.lines {
            s.push_str(&format!("{TXT_MEDIUM}{}x {}{TXT_NORMAL}\n", item.quantity, item.food_name.trim().to_uppercase()));
            let notes = trim_opt(&item.notes);
            if !notes.is_empty() {
                s.push_str(&format!("  -> {notes}\n"));
            }
        }
        s.push('\n');
        buf.extend(encode_cp858(&s));
        buf.extend_from_slice(&app.footer);
    }
    buf
}

pub fn build_cancellation_receipt(
    app: &App,
    display_code: &Option<String>,
    ticket_number: &Option<String>,
    customer: &Option<String>,
    table: &Option<String>,
) -> Vec<u8> {
    let show_numbers = app.show_numbers();
    let identifier = if show_numbers {
        ticket_number.as_ref().map(|n| format!("N° {n}")).unwrap_or_else(|| code_str(display_code))
    } else {
        code_str(display_code)
    };
    let mut out = vec![String::new(), line('=')];
    out.push(format!("{TXT_BIG}{BOLD_ON}ORDINE ANNULLATO: {identifier}{BOLD_OFF}{TXT_NORMAL}"));
    if show_numbers && !code_str(display_code).is_empty() {
        out.push(format!("{TXT_MEDIUM}{}{TXT_NORMAL}", cut(&format!("CODICE: {}", wide_code(display_code, 0x01)), RECEIPT_W)));
    } else if !show_numbers {
        if let Some(n) = ticket_number {
            out.push(format!("{TXT_MEDIUM}N°: {n}{TXT_NORMAL}"));
        }
    }
    out.push(line('='));
    out.push(String::new());
    let c = trim_opt(customer);
    if !c.is_empty() {
        out.push(format!("{TXT_MEDIUM}{}{TXT_NORMAL}", cut(&format!("CLIENTE: {c}"), RECEIPT_W)));
    }
    let t = trim_opt(table);
    if !t.is_empty() && t != "NO_TABLE_PRESET" {
        out.push(format!("{TXT_MEDIUM}TAVOLO: {t}{TXT_NORMAL}"));
    }
    out.push(String::new());
    out.push(line('-'));
    encode_cp858(&out.join("\n"))
}

/// JS-style truthiness fallback: `value || default`.
fn or_default<'a>(v: Option<&'a Value>) -> Option<&'a Value> {
    v.filter(|v| match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    })
}

fn field_str(v: &Value, key: &str, default: &str) -> String {
    or_default(v.get(key)).and_then(value_to_string).unwrap_or_else(|| default.to_string())
}

fn field_num(v: &Value, key: &str) -> f64 {
    or_default(v.get(key)).map(value_to_f64).unwrap_or(0.0)
}

fn field_list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

/// General closure: main report first, then one ticket per category.
pub fn build_general_closure_report(app: &App, payload: &Value) -> Vec<Vec<u8>> {
    let money = |n: f64| format_money(app, n, false);
    let empty = Value::Null;
    let report = payload.get("report").unwrap_or(&empty);
    let mut receipts = Vec::new();

    let mut m: Vec<String> = vec![String::new()];
    m.push(format!("{TXT_BIG}{BOLD_ON}CHIUSURA GENERALE{BOLD_OFF}{TXT_NORMAL}"));
    m.push(line('='));
    if let Some(ts) = or_default(report.get("timestamp")).and_then(value_to_string) {
        m.push(cut(&format!("DATA: {}", format_local_date(&ts)), RECEIPT_W));
    }
    m.push(line('-'));
    m.push(format!("{TXT_MEDIUM}{BOLD_ON}RIEPILOGO GENERALE{BOLD_OFF}{TXT_NORMAL}"));
    m.push(line('-'));
    m.push(lr("Totale Ordini:", &field_str(report, "totalOrders", "0")));
    m.push(lr("Incasso Totale:", &money(field_num(report, "totalRevenue"))));
    m.push(lr("Incasso Contanti:", &money(field_num(report, "totalCashRevenue"))));
    m.push(lr("Incasso Elettronico:", &money(field_num(report, "totalCardRevenue"))));

    let cr_stats = field_list(report, "cashRegisterStats");
    if !cr_stats.is_empty() {
        m.extend([String::new(), line('='), format!("{TXT_MEDIUM}{BOLD_ON}STATISTICHE PER CASSA{BOLD_OFF}{TXT_NORMAL}"), line('=')]);
        for cr in cr_stats {
            m.push(String::new());
            m.push(format!("{BOLD_ON}{}{BOLD_OFF}", cut(&field_str(cr, "cashRegisterName", "Cassa"), RECEIPT_W)));
            m.push(line('-'));
            m.push(lr("Totale:", &money(field_num(cr, "totalRevenue"))));
            m.push(lr("Contanti:", &money(field_num(cr, "totalCashRevenue"))));
            m.push(lr("Elettronico:", &money(field_num(cr, "totalCardRevenue"))));
        }
    }

    let cat_stats = field_list(report, "categoryStats");
    if !cat_stats.is_empty() {
        m.extend([String::new(), line('='), format!("{TXT_MEDIUM}{BOLD_ON}STATISTICHE PER CATEGORIA{BOLD_OFF}{TXT_NORMAL}"), line('=')]);
        for c in cat_stats {
            let name = cut(&field_str(c, "categoryName", "Categoria").to_uppercase(), RECEIPT_W - 10);
            m.push(lr(&name, &money(field_num(c, "revenue"))));
        }
    }
    m.extend([String::new(), line('='), String::new()]);
    receipts.push(encode_cp858(&format!("\n\n{}\n\n", m.join("\n"))));

    for c in cat_stats {
        let mut o: Vec<String> = vec![String::new()];
        let qty = field_str(c, "quantity", "0");
        let name = field_str(c, "categoryName", "Categoria").to_uppercase();
        o.push(format!("{TXT_BIG}{BOLD_ON}{qty}x {name}{BOLD_OFF}{TXT_NORMAL}"));
        o.push(line('='));
        let foods = field_list(c, "foodStats");
        if !foods.is_empty() {
            o.extend([String::new(), line('-'), format!("{BOLD_ON}DETTAGLIO PRODOTTI{BOLD_OFF}"), line('-')]);
            for f in foods {
                o.push(String::new());
                o.push(cut(&format!("{}x {}", field_str(f, "quantity", "0"), field_str(f, "foodName", "Prodotto")), RECEIPT_W));
            }
        }
        o.extend([String::new(), line('='), String::new()]);
        receipts.push(encode_cp858(&format!("\n\n{}\n\n", o.join("\n"))));
    }
    receipts
}

// ---------------------------------------------------------------- TCP

async fn connect(ip: &str, port: u16) -> std::io::Result<TcpStream> {
    let ip = ip.trim();
    if ip.is_empty() || port == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("Invalid printer address ip=\"{ip}\" port=\"{port}\"")));
    }
    TcpStream::connect((ip, port)).await
}

fn timeout_err(ip: &str, port: u16) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, format!("Timeout: printer {ip}:{port} did not respond"))
}

/// Send a receipt: select CP858, data, feed and cut. 5 s overall timeout.
pub async fn send_to_printer(ip: &str, port: u16, data: &[u8]) -> std::io::Result<()> {
    let fut = async {
        let mut s = connect(ip, port).await?;
        let mut payload = Vec::with_capacity(data.len() + 8);
        payload.extend_from_slice(SELECT_CP858);
        payload.extend_from_slice(data);
        payload.extend_from_slice(FEED_AND_CUT);
        s.write_all(&payload).await?;
        s.flush().await?;
        let _ = s.shutdown().await;
        Ok(())
    };
    timeout(Duration::from_secs(5), fut).await.unwrap_or_else(|_| Err(timeout_err(ip, port)))
}

/// Open the cash drawer on pin 2 (ESC p 0 25 250).
pub async fn open_cash_drawer(ip: &str, port: u16) -> std::io::Result<()> {
    let fut = async {
        let mut s = connect(ip, port).await?;
        s.write_all(&[0x1b, 0x70, 0x00, 0x19, 0xfa]).await?;
        s.flush().await?;
        let _ = s.shutdown().await;
        Ok(())
    };
    timeout(Duration::from_secs(5), fut).await.unwrap_or_else(|_| Err(timeout_err(ip, port)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperStatus {
    Ok,
    Low,
    Out,
}

/// Paper status via DLE EOT 4. Errors when unreachable or no reply within 3 s.
pub async fn get_printer_status(ip: &str, port: u16) -> std::io::Result<PaperStatus> {
    let fut = async {
        let mut s = connect(ip, port).await?;
        s.write_all(&[0x10, 0x04, 0x04]).await?;
        let mut b = [0u8; 1];
        let n = s.read(&mut b).await?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "printer closed the connection"));
        }
        Ok(paper_status(b[0]))
    };
    timeout(Duration::from_secs(3), fut)
        .await
        .unwrap_or_else(|_| Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "Timeout: la stampante non risponde")))
}

pub fn paper_status(byte: u8) -> PaperStatus {
    if byte & 0x60 == 0x60 {
        PaperStatus::Out
    } else if byte & 0x0C == 0x0C {
        PaperStatus::Low
    } else {
        PaperStatus::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp858() {
        assert_eq!(encode_cp858("€°Ø"), vec![0xD5, 0xF8, 0x9D]);
        assert_eq!(encode_cp858("à è ì"), vec![0x85, b' ', 0x8A, b' ', 0x8D]);
    }

    #[test]
    fn wrap() {
        assert_eq!(wrap_text("2x PANINO CON SALSICCIA", 12, "   "), vec!["2x PANINO", "   CON", "   SALSICCIA"]);
        assert_eq!(wrap_text("1x ABCDEFGHIJKLMNOP", 8, "   "), vec!["1x", "   ABCDE", "   FGHIJ", "   KLMNO", "   P"]);
    }

    #[test]
    fn dates() {
        assert_eq!(format_local_date("2026-07-15T18:30:00.000Z"), "15/07/2026 20:30");
        assert_eq!(format_local_date("2026-01-15T18:30:00Z"), "15/01/2026 19:30");
    }

    #[test]
    fn lr_align() {
        let s = lr("TOTALE", "12,50€");
        assert_eq!(s.chars().count(), RECEIPT_W);
        assert!(s.ends_with(" 12,50€"));
    }
}
