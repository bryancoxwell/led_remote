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
    match (method, url) {
        (Method::Get, "/") | (Method::Get, "/index.html") => html(INDEX_HTML),
        (Method::Post, url) => match url.strip_prefix("/press/") {
            Some(name) if !name.is_empty() && !name.contains('/') => {
                press(name, cfg, counter_path, tx)
            }
            _ => not_found(),
        },
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
    eprintln!("press {} (X={})", btn.name(), counter);
    match tx.transmit_press(
        btn.command(),
        counter,
        cfg.repeats,
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
