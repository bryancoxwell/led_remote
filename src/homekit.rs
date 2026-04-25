//! HomeKit Accessory Protocol bridge.
//!
//! Exposes a single Lightbulb accessory whose `On` / `Brightness` writes
//! dispatch through `PressContext` to the SDR. Runs on its own thread with a
//! dedicated tokio multi-thread runtime; the rest of the binary stays sync.
//!
//! v1 brightness behavior: snaps to the nearest of the three absolute
//! presets (10 / 50 / 100). The slider in the Home app stays where the user
//! put it; the LED lands at the closest preset. Step-based fine control is
//! a v2 problem (needs empirical step-size calibration).

use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use hap::{
    Config, MacAddress, Pin, Result as HapResult,
    accessory::{AccessoryCategory, AccessoryInformation, lightbulb::LightbulbAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    futures::future::FutureExt,
    server::{IpServer, Server},
    service::HapService,
    storage::{FileStorage, Storage},
};

use crate::Button;
use crate::serve::PressContext;

/// Generate a fresh locally-administered, unicast MAC. Used as the HAP
/// `device_id` on first run only — the persisted storage config holds it
/// after that, so the identity is stable across restarts but new every time
/// the state directory is wiped (which is what we want when retrying after
/// a failed pair: iOS's internal HMAccessory cache keys on this MAC, and
/// reusing a previously-seen MAC can leave `nodeID` null on the iOS side).
fn fresh_device_id() -> [u8; 6] {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let mut hasher = DefaultHasher::new();
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let h = hasher.finish().to_le_bytes();
    [
        (h[0] & 0xFC) | 0x02, // bit0=0 (unicast), bit1=1 (locally administered)
        h[1],
        h[2],
        h[3],
        h[4],
        h[5],
    ]
}

#[derive(Clone)]
pub struct HomekitConfig {
    pub name: String,
    /// `None` means: generate a random pin on first run (when no config has
    /// been persisted yet). Once paired, the pin lives in storage and this
    /// field is ignored.
    pub pin: Option<[u8; 8]>,
    pub state_dir: PathBuf,
}

/// Generate a random 8-digit pin that passes hap's trivial-pin filter.
/// Same hash-based approach as `fresh_device_id` — non-cryptographic, but
/// the threat model here is "local network ISM remote", so we don't need
/// OsRng. The retry loop guards against the (≈1 in 10^7) chance of
/// landing on one of the rejected pins.
fn generate_pin() -> [u8; 8] {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    for attempt in 0u64.. {
        let mut hasher = DefaultHasher::new();
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        attempt.hash(&mut hasher);
        let h = hasher.finish().to_le_bytes();
        let pin = [
            h[0] % 10,
            h[1] % 10,
            h[2] % 10,
            h[3] % 10,
            h[4] % 10,
            h[5] % 10,
            h[6] % 10,
            h[7] % 10,
        ];
        if Pin::new(pin).is_ok() {
            return pin;
        }
    }
    unreachable!()
}

pub fn default_state_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(p).join("led_remote").join("homekit");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/state/led_remote/homekit")
}

/// Accept "123-45-678" or "12345678"; rejects anything that doesn't yield
/// exactly 8 decimal digits.
pub fn parse_pin(s: &str) -> Result<[u8; 8], String> {
    let digits: Vec<u8> = s
        .chars()
        .filter(|c| c.is_ascii_digit())
        .map(|c| (c as u8) - b'0')
        .collect();
    if digits.len() != 8 {
        return Err(format!(
            "pin must be 8 digits (got {} from '{s}')",
            digits.len()
        ));
    }
    digits.try_into().map_err(|_| "pin parse failed".to_string())
}

pub fn format_pin(pin: [u8; 8]) -> String {
    format!(
        "{}{}{}-{}{}-{}{}{}",
        pin[0], pin[1], pin[2], pin[3], pin[4], pin[5], pin[6], pin[7]
    )
}

/// Spawn the HomeKit server on its own thread. The thread owns a dedicated
/// tokio runtime and runs until the HAP server exits (which is "forever"
/// under normal operation).
pub fn spawn(ctx: PressContext, cfg: HomekitConfig) -> JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("homekit: failed to build runtime: {e}");
                return;
            }
        };
        if let Err(e) = rt.block_on(run(ctx, cfg)) {
            eprintln!("homekit: server exited: {e}");
        }
    })
}

async fn run(ctx: PressContext, cfg: HomekitConfig) -> HapResult<()> {
    if let Err(e) = std::fs::create_dir_all(&cfg.state_dir) {
        return Err(hap::Error::from(std::io::Error::other(format!(
            "creating homekit state dir {}: {e}",
            cfg.state_dir.display()
        ))));
    }

    let state_dir_str = cfg.state_dir.to_string_lossy().to_string();
    let mut storage = FileStorage::new(&state_dir_str).await?;

    let config = match storage.load_config().await {
        Ok(mut c) => {
            c.redetermine_local_ip();
            storage.save_config(&c).await?;
            c
        }
        Err(_) => {
            let pin_bytes = match cfg.pin {
                Some(p) => p,
                None => {
                    let p = generate_pin();
                    eprintln!("homekit: generated random pin {}", format_pin(p));
                    p
                }
            };
            let mac = fresh_device_id();
            eprintln!(
                "homekit: no saved config, generated device_id {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            );
            let c = Config {
                pin: Pin::new(pin_bytes)?,
                name: cfg.name.clone(),
                device_id: MacAddress::from(mac),
                category: AccessoryCategory::Lightbulb,
                ..Default::default()
            };
            storage.save_config(&c).await?;
            c
        }
    };

    // Fill out every required AccessoryInformation field. The fork's defaults
    // leave manufacturer/model/serial as the literal string "undefined" and
    // firmware_revision as None — iOS will reject the accessory with
    // "Unable to add accessory. This accessory cannot be used with HomeKit"
    // when Firmware Revision is missing.
    let mut bulb = LightbulbAccessory::new(
        1,
        AccessoryInformation {
            name: cfg.name.clone(),
            manufacturer: "led_remote".to_string(),
            model: "RM12 Bridge".to_string(),
            serial_number: format!("LRRM12-{}", config.device_id),
            firmware_revision: Some(env!("CARGO_PKG_VERSION").to_string()),
            ..Default::default()
        },
    )?;
    fix_lightbulb_iids(&mut bulb);

    register_callbacks(&mut bulb, &ctx);

    let pin_display = config.pin.to_string();
    let server = IpServer::new(config, storage).await?;
    server.add_accessory(bulb).await?;

    eprintln!(
        "homekit: accessory '{}' ready, pin {}, state {}",
        cfg.name,
        pin_display,
        cfg.state_dir.display()
    );
    server.run_handle().await
}

/// Workaround for an iid bug in the ihciah hap-rs fork. `LightbulbAccessory::new`
/// computes the LightbulbService's base iid from
/// `accessory_information.get_characteristics().len()`, but `to_service`
/// places the optional `firmware_revision` characteristic at iid=10 (its
/// "natural" slot in `AccessoryInformationService::new`'s sparse layout) —
/// not at the dense iid=7 the count-based formula assumes. Result: brightness
/// (iid=10) collides with firmware_revision (iid=10) within the accessory.
/// HAP requires iids to be unique per accessory, so iOS accepts the PIN, then
/// fails database validation with the generic "this accessory cannot be used
/// with HomeKit" error.
///
/// Fix: shift the entire LightbulbService so its base iid is strictly greater
/// than any iid in the AccessoryInformationService.
pub fn fix_lightbulb_iids(bulb: &mut LightbulbAccessory) {
    let max_info_iid = bulb
        .accessory_information
        .get_characteristics()
        .iter()
        .map(|c| c.get_id())
        .max()
        .unwrap_or(1);
    let new_base = max_info_iid + 1;
    let cur_base = bulb.lightbulb.get_id();
    if new_base <= cur_base {
        return;
    }
    let shift = new_base - cur_base;
    bulb.lightbulb.set_id(new_base);
    for c in bulb.lightbulb.get_mut_characteristics() {
        let id = c.get_id();
        c.set_id(id + shift);
    }
}

fn register_callbacks(bulb: &mut LightbulbAccessory, ctx: &PressContext) {
    let on_ctx = ctx.clone();
    bulb.lightbulb
        .power_state
        .on_update_async(Some(move |current: bool, new: bool| {
            let ctx = on_ctx.clone();
            async move {
                if current == new {
                    return Ok(());
                }
                let btn = if new { Button::TurnOn } else { Button::TurnOff };
                dispatch(&ctx, btn).await
            }
            .boxed()
        }));

    if let Some(brightness) = bulb.lightbulb.brightness.as_mut() {
        let bright_ctx = ctx.clone();
        brightness.on_update_async(Some(move |_current: i32, new: i32| {
            let ctx = bright_ctx.clone();
            async move {
                let btn = snap_brightness(new);
                dispatch(&ctx, btn).await
            }
            .boxed()
        }));
    }
}

/// HomeKit's brightness range is 0..=100. We have three absolute presets;
/// pick the closest by midpoint of {10, 50, 100}.
fn snap_brightness(level: i32) -> Button {
    match level {
        n if n <= 29 => Button::Brightness10,
        n if n <= 74 => Button::Brightness50,
        _ => Button::Brightness100,
    }
}

/// Update-callback error type. The `hap` crate's `OnUpdateFuture` requires
/// this exact shape — not `hap::Error`.
type CbError = Box<dyn std::error::Error + Send + Sync>;

async fn dispatch(ctx: &PressContext, btn: Button) -> Result<(), CbError> {
    let ctx = ctx.clone();
    let btn_name = btn.name();
    let join = tokio::task::spawn_blocking(move || ctx.press(btn)).await;
    match join {
        Ok(Ok(r)) => {
            eprintln!(
                "homekit: {} (X={}, reps={})",
                btn_name, r.counter, r.repeats
            );
            Ok(())
        }
        Ok(Err(e)) => {
            eprintln!("homekit: {btn_name} failed: {e}");
            Err(Box::new(e))
        }
        Err(e) => {
            eprintln!("homekit: {btn_name} join error: {e}");
            Err(Box::new(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pin_accepts_dashed_and_plain() {
        assert_eq!(
            parse_pin("123-45-678").unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(parse_pin("12345678").unwrap(), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(parse_pin(" 1 2 3 4 5 6 7 8 ").unwrap(), [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn parse_pin_rejects_wrong_length() {
        assert!(parse_pin("123").is_err());
        assert!(parse_pin("1234567890").is_err());
        assert!(parse_pin("abcdefgh").is_err());
    }

    #[test]
    fn snap_brightness_thresholds() {
        assert_eq!(snap_brightness(0), Button::Brightness10);
        assert_eq!(snap_brightness(29), Button::Brightness10);
        assert_eq!(snap_brightness(30), Button::Brightness50);
        assert_eq!(snap_brightness(74), Button::Brightness50);
        assert_eq!(snap_brightness(75), Button::Brightness100);
        assert_eq!(snap_brightness(100), Button::Brightness100);
    }

    #[test]
    fn format_pin_groups_3_2_3() {
        assert_eq!(format_pin([1, 2, 3, 4, 5, 6, 7, 8]), "123-45-678");
        assert_eq!(format_pin([8, 3, 1, 9, 4, 6, 7, 2]), "831-94-672");
    }
}
