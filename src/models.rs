//! Domain models mirroring the JSON returned by the MySagra backend.
//!
//! The backend is not strict about types (ids may be numbers or strings,
//! amounts arrive as strings serialized from Decimal, ports may be "9.100"),
//! so every field is parsed loosely.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Any scalar as a trimmed string; null/missing → None.
pub fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Number from a number or a string (comma or dot decimal). Invalid → 0.
pub fn value_to_f64(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().replacen(',', ".", 1).parse::<f64>().ok().filter(|n| n.is_finite()).unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Port from a number or a string with dots as thousand separators ("9.100" → 9100).
pub fn value_to_port(v: &Value) -> u16 {
    match v {
        Value::Number(n) => n.as_u64().and_then(|p| u16::try_from(p).ok()).unwrap_or(0),
        Value::String(s) => s.trim().replace('.', "").parse::<u16>().unwrap_or(0),
        _ => 0,
    }
}

fn loose_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(value_to_string(&Value::deserialize(d)?))
}

fn loose_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(if v.is_null() { None } else { Some(value_to_f64(&v)) })
}

fn loose_port<'de, D: Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
    Ok(value_to_port(&Value::deserialize(d)?))
}

fn loose_qty<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(if v.is_null() { 1 } else { value_to_f64(&v) as i64 })
}

fn default_qty() -> i64 {
    1
}

fn default_status() -> String {
    "UNKNOWN".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Printer {
    #[serde(deserialize_with = "loose_string", default)]
    pub id: Option<String>,
    #[serde(deserialize_with = "loose_string", default)]
    pub name: Option<String>,
    #[serde(deserialize_with = "loose_string", default)]
    pub ip: Option<String>,
    #[serde(deserialize_with = "loose_port", default)]
    pub port: u16,
    #[serde(deserialize_with = "loose_string", default)]
    pub description: Option<String>,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(deserialize_with = "loose_string", default)]
    pub mac: Option<String>,
}

impl Printer {
    pub fn id(&self) -> &str {
        self.id.as_deref().unwrap_or("")
    }
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("")
    }
    pub fn ip(&self) -> &str {
        self.ip.as_deref().unwrap_or("")
    }
    pub fn mac(&self) -> Option<&str> {
        self.mac.as_deref().filter(|m| !m.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CashRegister {
    #[serde(deserialize_with = "loose_string", default)]
    pub id: Option<String>,
    #[serde(rename = "defaultPrinterId", deserialize_with = "loose_string", default)]
    pub default_printer_id: Option<String>,
    #[serde(rename = "defaultPrinter", default)]
    pub default_printer: Option<Printer>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Station {
    #[serde(deserialize_with = "loose_string", default)]
    pub id: Option<String>,
    #[serde(deserialize_with = "loose_string", default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Category {
    #[serde(default)]
    pub station: Option<Station>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Food {
    #[serde(deserialize_with = "loose_string", default)]
    pub name: Option<String>,
    #[serde(rename = "printerId", deserialize_with = "loose_string", default)]
    pub printer_id: Option<String>,
    #[serde(rename = "separateTicket", default)]
    pub separate_ticket: Option<bool>,
    #[serde(default)]
    pub category: Option<Category>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderItem {
    #[serde(deserialize_with = "loose_string", default)]
    pub id: Option<String>,
    #[serde(deserialize_with = "loose_qty", default = "default_qty")]
    pub quantity: i64,
    #[serde(deserialize_with = "loose_string", default)]
    pub notes: Option<String>,
    #[serde(rename = "unitPrice", deserialize_with = "loose_f64", default)]
    pub unit_price: Option<f64>,
    #[serde(rename = "unitSurcharge", deserialize_with = "loose_f64", default)]
    pub unit_surcharge: Option<f64>,
    #[serde(default)]
    pub food: Option<Food>,
}

impl OrderItem {
    pub fn food_name(&self) -> String {
        self.food
            .as_ref()
            .and_then(|f| f.name.clone())
            .unwrap_or_else(|| format!("FOOD({})", self.id.as_deref().unwrap_or("")))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct IncomingOrder {
    #[serde(deserialize_with = "loose_string", default)]
    pub id: Option<String>,
    #[serde(rename = "displayCode", deserialize_with = "loose_string", default)]
    pub display_code: Option<String>,
    /// Kept raw: "NO_TABLE_PRESET" is a sentinel compared before trimming.
    #[serde(deserialize_with = "loose_string", default)]
    pub table: Option<String>,
    #[serde(deserialize_with = "loose_string", default)]
    pub customer: Option<String>,
    #[serde(rename = "confirmedAt", deserialize_with = "loose_string", default)]
    pub confirmed_at: Option<String>,
    #[serde(rename = "ticketNumber", deserialize_with = "loose_string", default)]
    pub ticket_number: Option<String>,
    #[serde(rename = "paymentMethod", deserialize_with = "loose_string", default)]
    pub payment_method: Option<String>,
    #[serde(deserialize_with = "loose_f64", default)]
    pub discount: Option<f64>,
    #[serde(rename = "cashRegisterId", deserialize_with = "loose_string", default)]
    pub cash_register_id: Option<String>,
    #[serde(rename = "orderItems", default)]
    pub order_items: Option<Vec<OrderItem>>,
    /// Present on reprint-order events: the items to reprint in the kitchens.
    /// `Some(empty)` means no kitchen receipt; `None` means use `order_items`.
    #[serde(rename = "reprintOrderItems", default, deserialize_with = "reprint_items")]
    pub reprint_order_items: Option<Vec<OrderItem>>,
    #[serde(rename = "reprintReceipt", default)]
    pub reprint_receipt: Option<bool>,
}

/// `reprintOrderItems: null` counts as present-but-empty, like the original.
fn reprint_items<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<OrderItem>>, D::Error> {
    let v = Value::deserialize(d)?;
    if v.is_null() {
        return Ok(Some(Vec::new()));
    }
    serde_json::from_value(v).map(Some).map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderCancelled {
    #[serde(rename = "orderId", deserialize_with = "loose_string", default)]
    pub order_id: Option<String>,
    #[serde(rename = "ticketNumber", deserialize_with = "loose_string", default)]
    pub ticket_number: Option<String>,
    #[serde(rename = "displayCode", deserialize_with = "loose_string", default)]
    pub display_code: Option<String>,
    #[serde(deserialize_with = "loose_string", default)]
    pub customer: Option<String>,
    #[serde(deserialize_with = "loose_string", default)]
    pub table: Option<String>,
    #[serde(default)]
    pub printers: Vec<Value>,
}
