use std::path::PathBuf;

use clap::{Parser, Subcommand};
use led_remote::{
    Button, DEFAULT_CENTER_FREQ_HZ, DEFAULT_SAMPLE_RATE, build_packet, build_packet_raw,
    encode_press_raw, write_cf32,
};

/// Parse a hex (`0x..`) or decimal byte. Used for the `--cmd` flag.
fn parse_hex_byte(s: &str) -> Result<u8, String> {
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(rest, 16).map_err(|e| format!("invalid hex byte '{s}': {e}"))
    } else {
        s.parse::<u8>().map_err(|e| format!("invalid byte '{s}': {e}"))
    }
}

#[derive(Parser)]
#[command(version, about = "Rayrun RM12 LED remote signal encoder")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Encode a button press to a baseband cf32_le file (or stdout).
    Encode {
        /// Button name (turn_on, turn_off, ...). Optional when --cmd is given.
        #[arg(required_unless_present = "cmd")]
        button: Option<String>,
        /// Override the command byte (hex like 0x04 or decimal). For probing unknown codes.
        #[arg(long, value_parser = parse_hex_byte)]
        cmd: Option<u8>,
        /// 4-bit press counter (0..15)
        #[arg(short = 'c', long, default_value_t = 0)]
        counter: u8,
        /// Number of packet repetitions per press
        #[arg(short = 'r', long, default_value_t = 5)]
        repeats: u32,
        /// Sample rate (Hz)
        #[arg(short = 's', long, default_value_t = DEFAULT_SAMPLE_RATE)]
        sample_rate: u32,
        /// Output path (cf32_le). Use - for stdout.
        #[arg(short = 'o', long, default_value = "-")]
        output: String,
    },
    /// Print protocol summary and per-button command codes.
    Info,
    /// Transmit a button press over an SDR (requires SoapySDR).
    Transmit {
        /// Button name (turn_on, turn_off, ...). Optional when --cmd is given.
        #[arg(required_unless_present = "cmd")]
        button: Option<String>,
        /// Override the command byte (hex like 0x04 or decimal). For probing unknown codes.
        #[arg(long, value_parser = parse_hex_byte)]
        cmd: Option<u8>,
        /// 4-bit press counter (0..15)
        #[arg(short = 'c', long, default_value_t = 0)]
        counter: u8,
        /// Number of packet repetitions per press
        #[arg(short = 'r', long, default_value_t = 5)]
        repeats: u32,
        /// Sample rate (Hz)
        #[arg(short = 's', long, default_value_t = DEFAULT_SAMPLE_RATE)]
        sample_rate: u32,
        /// Carrier frequency (Hz)
        #[arg(short = 'f', long, default_value_t = DEFAULT_CENTER_FREQ_HZ)]
        frequency: u64,
        /// TX gain (dB)
        #[arg(short = 'g', long, default_value_t = 50.0)]
        gain: f64,
        /// SoapySDR device args (e.g. "driver=uhd"); empty = first available
        #[arg(short = 'd', long, default_value = "")]
        device: String,
        /// Antenna name (B200: "TX/RX")
        #[arg(long)]
        antenna: Option<String>,
        /// Leading silence (ms)
        #[arg(long, default_value_t = 5.0)]
        lead_ms: f32,
        /// Trailing silence (ms) — let TX buffer drain before disabling
        #[arg(long, default_value_t = 20.0)]
        trail_ms: f32,
    },
    /// List SDR devices visible to SoapySDR.
    Devices,
}

fn main() -> std::io::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Encode {
            button,
            cmd,
            counter,
            repeats,
            sample_rate,
            output,
        } => {
            let btn: Option<Button> = button
                .as_deref()
                .map(|s| s.parse())
                .transpose()
                .map_err(|e: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            let cmd_byte = cmd.or_else(|| btn.map(|b| b.command())).expect(
                "clap requires button or --cmd",
            );
            let samples = encode_press_raw(cmd_byte, counter, repeats, sample_rate);
            let bits = build_packet_raw(cmd_byte, counter);
            let label = match (btn, cmd) {
                (Some(b), None) => b.name().to_string(),
                (Some(b), Some(_)) => format!("{} [cmd override]", b.name()),
                (None, Some(_)) => "raw".to_string(),
                (None, None) => unreachable!(),
            };
            eprintln!(
                "encoded {label} (cmd=0x{cmd_byte:02X}, X={}, packet=0x{bits:010X}) — {repeats} reps, {} samples @ {sample_rate} Hz",
                counter & 0x0F,
                samples.len(),
            );
            if output == "-" {
                use std::io::Write;
                let stdout = std::io::stdout();
                let mut w = std::io::BufWriter::new(stdout.lock());
                for c in &samples {
                    w.write_all(&c.re.to_le_bytes())?;
                    w.write_all(&c.im.to_le_bytes())?;
                }
                w.flush()?;
            } else {
                write_cf32(PathBuf::from(&output), &samples)?;
                eprintln!("wrote {output}");
            }
            Ok(())
        }
        Cmd::Info => {
            println!("device_id : 0x{:04X}", led_remote::DEVICE_ID);
            println!("data bits : {}", led_remote::DATA_BITS);
            println!("buttons   :");
            for &b in Button::ALL {
                println!(
                    "  {:18}  cmd=0x{:02X}  packet@X=0=0x{:010X}",
                    b.name(),
                    b.command(),
                    build_packet(b, 0)
                );
            }
            println!();
            println!(
                "timing (µs): preamble_on={} sync_off={} data_on={} short_off={} long_off={} stop_on={} gap_off={}",
                led_remote::PREAMBLE_ON_US as u32,
                led_remote::SYNC_OFF_US as u32,
                led_remote::DATA_ON_US as u32,
                led_remote::SHORT_OFF_US as u32,
                led_remote::LONG_OFF_US as u32,
                led_remote::STOP_ON_US as u32,
                led_remote::INTER_PACKET_GAP_US as u32,
            );
            Ok(())
        }
        Cmd::Transmit {
            button,
            cmd,
            counter,
            repeats,
            sample_rate,
            frequency,
            gain,
            device,
            antenna,
            lead_ms,
            trail_ms,
        } => transmit_cmd(
            button,
            cmd,
            counter,
            repeats,
            sample_rate,
            frequency,
            gain,
            device,
            antenna,
            lead_ms,
            trail_ms,
        ),
        Cmd::Devices => devices_cmd(),
    }
}

#[allow(clippy::too_many_arguments)]
fn transmit_cmd(
    button: Option<String>,
    cmd: Option<u8>,
    counter: u8,
    repeats: u32,
    sample_rate: u32,
    frequency: u64,
    gain: f64,
    device: String,
    antenna: Option<String>,
    lead_ms: f32,
    trail_ms: f32,
) -> std::io::Result<()> {
    use led_remote::transmit::{TxParams, transmit_press_raw};

    let btn: Option<Button> = button
        .as_deref()
        .map(|s| s.parse())
        .transpose()
        .map_err(|e: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let cmd_byte = cmd
        .or_else(|| btn.map(|b| b.command()))
        .expect("clap requires button or --cmd");
    let params = TxParams {
        args: device,
        frequency_hz: frequency as f64,
        sample_rate_hz: sample_rate as f64,
        gain_db: gain,
        channel: 0,
        antenna,
    };
    let bits = build_packet_raw(cmd_byte, counter);
    let label = match (btn, cmd) {
        (Some(b), None) => b.name().to_string(),
        (Some(b), Some(_)) => format!("{} [cmd override]", b.name()),
        (None, Some(_)) => "raw".to_string(),
        (None, None) => unreachable!(),
    };
    eprintln!(
        "transmitting {label} (cmd=0x{cmd_byte:02X}, X={}, packet=0x{bits:010X}) — {repeats} reps @ {:.3} MHz, gain {gain} dB, fs {sample_rate} Hz",
        counter & 0x0F,
        params.frequency_hz / 1e6,
    );
    let n = transmit_press_raw(cmd_byte, counter, repeats, lead_ms, trail_ms, &params)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    eprintln!("done — {n} samples sent");
    Ok(())
}

fn devices_cmd() -> std::io::Result<()> {
    let devs = led_remote::transmit::enumerate_devices()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    if devs.is_empty() {
        println!("no SDR devices found");
    } else {
        for (i, d) in devs.iter().enumerate() {
            println!("[{i}]  {d}");
        }
    }
    Ok(())
}
