//! Tiny HTTP frontend for the transmit path. Serves a single embedded HTML
//! page and one POST endpoint per button press. Requests are handled
//! sequentially — the SDR can't be shared, and the page itself is one static
//! GET, so concurrency would only buy us reordered failures.
//!
//! The SDR is opened once at startup and reused for every press; it stays
//! open until the server exits.

use std::path::{Path, PathBuf};

use serde_json::json;
use tiny_http::{Header, Method, Response, Server};

use crate::transmit::{Transmitter, TxParams};
use crate::{Button, bump_counter, default_counter_path};

const INDEX_HTML: &str = include_str!("index.html");

pub struct ServeConfig {
    pub bind: String,
    pub tx: TxParams,
    pub repeats: u32,
    pub lead_ms: f32,
    pub trail_ms: f32,
    pub counter_state: Option<PathBuf>,
}

type Resp = Response<Box<dyn std::io::Read + Send + 'static>>;

pub fn run(cfg: ServeConfig) -> std::io::Result<()> {
    let counter_path = cfg
        .counter_state
        .clone()
        .unwrap_or_else(default_counter_path);

    eprintln!(
        "opening SDR ({}, {:.3} MHz, fs {} Hz, gain {} dB)…",
        if cfg.tx.args.is_empty() {
            "first available"
        } else {
            cfg.tx.args.as_str()
        },
        cfg.tx.frequency_hz / 1e6,
        cfg.tx.sample_rate_hz,
        cfg.tx.gain_db,
    );
    let mut tx = Transmitter::open(&cfg.tx)
        .map_err(|e| std::io::Error::other(format!("opening SDR: {e}")))?;

    let server = Server::http(&cfg.bind).map_err(|e| std::io::Error::other(e.to_string()))?;
    eprintln!("led_remote serving on http://{}", cfg.bind);
    eprintln!(
        "  repeats {}, counter state {}",
        cfg.repeats,
        counter_path.display(),
    );

    for request in server.incoming_requests() {
        let resp = route(request.method(), request.url(), &cfg, &counter_path, &mut tx);
        if let Err(e) = request.respond(resp) {
            eprintln!("response error: {e}");
        }
    }
    // tx drops here, deactivating the stream cleanly.
    Ok(())
}

fn route(
    method: &Method,
    url: &str,
    cfg: &ServeConfig,
    counter_path: &Path,
    tx: &mut Transmitter,
) -> Resp {
    match method {
        Method::Get if matches!(url, "/" | "/index.html") => html(INDEX_HTML),
        Method::Post => {
            if let Some(name) = url.strip_prefix("/press/")
                && !name.is_empty()
                && !name.contains('/')
            {
                return press(name, cfg, counter_path, tx);
            }
            if let Some(value) = url.strip_prefix("/raw/")
                && !value.is_empty()
                && !value.contains('/')
            {
                return press_raw(value, cfg, counter_path, tx);
            }
            not_found()
        }
        _ => not_found(),
    }
}

fn press(name: &str, cfg: &ServeConfig, counter_path: &Path, tx: &mut Transmitter) -> Resp {
    let btn: Button = match name.parse() {
        Ok(b) => b,
        Err(e) => return json_resp(400, &json!({"ok": false, "error": e}).to_string()),
    };
    let counter = match bump_counter(counter_path) {
        Ok(c) => c,
        Err(e) => {
            return json_resp(
                500,
                &json!({"ok": false, "error": format!("counter: {e}")}).to_string(),
            );
        }
    };
    let repeats = repeats_for(btn, cfg.repeats);
    eprintln!("press {} (X={}, reps={})", btn.name(), counter, repeats);
    match tx.transmit_press(
        btn.command(),
        counter,
        repeats,
        cfg.lead_ms,
        cfg.trail_ms,
    ) {
        Ok(samples) => json_resp(
            200,
            &json!({
                "ok": true,
                "button": btn.name(),
                "counter": counter,
                "samples": samples,
            })
            .to_string(),
        ),
        Err(e) => {
            eprintln!("  transmit failed: {e}");
            json_resp(500, &json!({"ok": false, "error": e.to_string()}).to_string())
        }
    }
}

fn press_raw(value: &str, cfg: &ServeConfig, counter_path: &Path, tx: &mut Transmitter) -> Resp {
    let cmd = match parse_byte(value) {
        Ok(b) => b,
        Err(e) => return json_resp(400, &json!({"ok": false, "error": e}).to_string()),
    };
    let counter = match bump_counter(counter_path) {
        Ok(c) => c,
        Err(e) => {
            return json_resp(
                500,
                &json!({"ok": false, "error": format!("counter: {e}")}).to_string(),
            );
        }
    };
    let label = format!("0x{cmd:02X}");
    eprintln!("raw {label} (X={counter})");
    match tx.transmit_press(cmd, counter, cfg.repeats, cfg.lead_ms, cfg.trail_ms) {
        Ok(samples) => json_resp(
            200,
            &json!({
                "ok": true,
                "button": label,
                "counter": counter,
                "samples": samples,
            })
            .to_string(),
        ),
        Err(e) => {
            eprintln!("  transmit failed: {e}");
            json_resp(500, &json!({"ok": false, "error": e.to_string()}).to_string())
        }
    }
}

/// Accepts `0x0C` / `0X0c` (hex) or `12` (decimal). Mirrors the CLI's `--cmd`.
fn parse_byte(s: &str) -> Result<u8, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(rest, 16).map_err(|e| format!("invalid hex byte '{s}': {e}"))
    } else {
        s.parse::<u8>()
            .map_err(|e| format!("invalid byte '{s}': {e}"))
    }
}

/// Repeats per press. Brightness and temperature are level-changing — each
/// press should bump the LED by one step, so we send a single packet and let
/// the receiver's counter dedupe handle the rest. Power and pair stay at the
/// configured default for reliability.
fn repeats_for(btn: Button, default: u32) -> u32 {
    match btn {
        Button::BrightnessDown
        | Button::BrightnessUp
        | Button::TemperatureDown
        | Button::TemperatureUp => 1,
        _ => default,
    }
}

fn html(body: &str) -> Resp {
    Response::from_string(body)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
        )
        .boxed()
}

fn json_resp(status: u32, body: &str) -> Resp {
    Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
        )
        .boxed()
}

fn not_found() -> Resp {
    Response::from_string("not found")
        .with_status_code(404)
        .boxed()
}
