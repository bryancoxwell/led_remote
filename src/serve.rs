//! Tiny HTTP frontend for the transmit path. Serves a single embedded HTML
//! page and one POST endpoint per button press. Requests are handled
//! sequentially — the SDR can't be shared, and the page itself is one static
//! GET, so concurrency would only buy us reordered failures.
//!
//! The SDR is opened once at startup and reused for every press; it stays
//! open until the server exits. The `PressContext` wrapper lets other
//! interfaces (HomeKit, etc.) dispatch through the same code path.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tiny_http::{Header, Method, Response, Server};

use crate::homekit::{self, HomekitConfig};
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
    pub homekit: Option<HomekitConfig>,
}

/// Shared dispatch handle. Cloning it bumps the `Arc` refcount; the inner
/// `Transmitter` is locked per press so concurrent callers (HTTP loop +
/// HomeKit task) serialize cleanly.
#[derive(Clone)]
pub struct PressContext {
    tx: Arc<Mutex<Transmitter>>,
    counter_path: PathBuf,
    default_repeats: u32,
    lead_ms: f32,
    trail_ms: f32,
}

pub struct PressResult {
    pub counter: u8,
    pub samples: usize,
    pub repeats: u32,
}

impl PressContext {
    pub fn press(&self, btn: Button) -> std::io::Result<PressResult> {
        let repeats = repeats_for(btn, self.default_repeats);
        self.send(btn.command(), repeats)
    }

    pub fn press_raw(&self, cmd: u8) -> std::io::Result<PressResult> {
        self.send(cmd, self.default_repeats)
    }

    fn send(&self, cmd: u8, repeats: u32) -> std::io::Result<PressResult> {
        let counter = bump_counter(&self.counter_path)?;
        let mut tx = self.tx.lock().expect("tx mutex poisoned");
        let samples = tx
            .transmit_press(cmd, counter, repeats, self.lead_ms, self.trail_ms)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(PressResult { counter, samples, repeats })
    }
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
    let tx = Transmitter::open(&cfg.tx)
        .map_err(|e| std::io::Error::other(format!("opening SDR: {e}")))?;

    let ctx = PressContext {
        tx: Arc::new(Mutex::new(tx)),
        counter_path: counter_path.clone(),
        default_repeats: cfg.repeats,
        lead_ms: cfg.lead_ms,
        trail_ms: cfg.trail_ms,
    };

    if let Some(hk) = cfg.homekit.clone() {
        // Detached: the thread owns its tokio runtime and runs until the
        // process exits. We don't join it; main thread exit reaps it.
        let _ = homekit::spawn(ctx.clone(), hk);
    }

    let server = Server::http(&cfg.bind).map_err(|e| std::io::Error::other(e.to_string()))?;
    eprintln!("led_remote serving on http://{}", cfg.bind);
    eprintln!(
        "  repeats {}, counter state {}",
        cfg.repeats,
        counter_path.display(),
    );

    for request in server.incoming_requests() {
        let resp = route(request.method(), request.url(), &ctx);
        if let Err(e) = request.respond(resp) {
            eprintln!("response error: {e}");
        }
    }
    Ok(())
}

fn route(method: &Method, url: &str, ctx: &PressContext) -> Resp {
    match method {
        Method::Get if matches!(url, "/" | "/index.html") => html(INDEX_HTML),
        Method::Post => {
            if let Some(name) = url.strip_prefix("/press/")
                && !name.is_empty()
                && !name.contains('/')
            {
                return press(name, ctx);
            }
            if let Some(value) = url.strip_prefix("/raw/")
                && !value.is_empty()
                && !value.contains('/')
            {
                return press_raw(value, ctx);
            }
            not_found()
        }
        _ => not_found(),
    }
}

fn press(name: &str, ctx: &PressContext) -> Resp {
    let btn: Button = match name.parse() {
        Ok(b) => b,
        Err(e) => return json_resp(400, &json!({"ok": false, "error": e}).to_string()),
    };
    match ctx.press(btn) {
        Ok(r) => {
            eprintln!("press {} (X={}, reps={})", btn.name(), r.counter, r.repeats);
            json_resp(
                200,
                &json!({
                    "ok": true,
                    "button": btn.name(),
                    "counter": r.counter,
                    "samples": r.samples,
                })
                .to_string(),
            )
        }
        Err(e) => {
            eprintln!("press {} failed: {e}", btn.name());
            json_resp(500, &json!({"ok": false, "error": e.to_string()}).to_string())
        }
    }
}

fn press_raw(value: &str, ctx: &PressContext) -> Resp {
    let cmd = match parse_byte(value) {
        Ok(b) => b,
        Err(e) => return json_resp(400, &json!({"ok": false, "error": e}).to_string()),
    };
    let label = format!("0x{cmd:02X}");
    match ctx.press_raw(cmd) {
        Ok(r) => {
            eprintln!("raw {label} (X={}, reps={})", r.counter, r.repeats);
            json_resp(
                200,
                &json!({
                    "ok": true,
                    "button": label,
                    "counter": r.counter,
                    "samples": r.samples,
                })
                .to_string(),
            )
        }
        Err(e) => {
            eprintln!("raw {label} failed: {e}");
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

/// Repeats per press. Brightness +/- and temperature +/- are level-changing —
/// each press should bump the LED by one step, so we send a single packet and
/// let the receiver's counter dedupe handle the rest. Power, presets, and
/// pair stay at the configured default for reliability.
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
