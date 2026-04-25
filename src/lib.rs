//! Rayrun RM12 LED remote protocol encoder.
//!
//! Wire format (40 data bits, MSB transmitted first):
//!     [39:24]  device_id  = 0xF3A2
//!     [23:16]  command    (per-button, see `Button::command`)
//!     [15: 8]  redundancy = 0x50 | (command ^ 0x01)
//!     [ 7: 0]  counter    = ((!X & 0xF) << 4) | (X & 0xF)   for 4-bit press counter X
//!
//! Frame:
//!     PREAMBLE_ON (~8.32 ms)
//!     SYNC_OFF    (~4.20 ms)
//!     for bit in data_bits (MSB first):
//!         ON  (~488 µs)
//!         OFF (~540 µs if 0, ~1580 µs if 1)
//!     STOP_ON     (~488 µs)
//!     INTER_PACKET_GAP_OFF (~10.7 ms)   // only between repeats, not after last
//!
//! Carrier: ~433.86 MHz, OOK/ASK. Receiver is wideband-on-a-cheap-crystal so
//! transmitting at 433.87 MHz works fine.

use num_complex::Complex32;

pub mod transmit;

pub const DEVICE_ID: u16 = 0xF3A2;
pub const DEFAULT_SAMPLE_RATE: u32 = 500_000;
pub const DEFAULT_CENTER_FREQ_HZ: u64 = 433_870_000;

// Timing constants in microseconds (medians from histogram analysis of all 5 captures).
pub const PREAMBLE_ON_US: f32 = 8_320.0;
pub const SYNC_OFF_US: f32 = 4_200.0;
pub const DATA_ON_US: f32 = 488.0;
pub const SHORT_OFF_US: f32 = 540.0;
pub const LONG_OFF_US: f32 = 1_580.0;
pub const STOP_ON_US: f32 = 488.0;
pub const INTER_PACKET_GAP_US: f32 = 10_700.0;

pub const DATA_BITS: u32 = 40;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Button {
    TurnOn,
    TurnOff,
    TemperatureDown,
    TemperatureUp,
    BrightnessDown,
    BrightnessUp,
}

impl Button {
    pub const ALL: &'static [Button] = &[
        Button::TurnOn,
        Button::TurnOff,
        Button::TemperatureDown,
        Button::TemperatureUp,
        Button::BrightnessDown,
        Button::BrightnessUp,
    ];

    pub fn command(self) -> u8 {
        match self {
            Button::TurnOn => 0x01,
            Button::TurnOff => 0x02,
            Button::TemperatureDown => 0x06,
            Button::TemperatureUp => 0x08,
            Button::BrightnessDown => 0x0A,
            Button::BrightnessUp => 0x04,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Button::TurnOn => "turn_on",
            Button::TurnOff => "turn_off",
            Button::TemperatureDown => "temperature_down",
            Button::TemperatureUp => "temperature_up",
            Button::BrightnessDown => "brightness_down",
            Button::BrightnessUp => "brightness_up",
        }
    }

}

impl std::str::FromStr for Button {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Button::ALL
            .iter()
            .copied()
            .find(|b| b.name() == s)
            .ok_or_else(|| {
                format!(
                    "unknown button '{s}' — expected one of: {}",
                    Button::ALL
                        .iter()
                        .map(|b| b.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

/// Build the 40-bit packet from a raw command byte (lets you probe unknown
/// command codes — see [`build_packet`] for the named-button variant).
pub fn build_packet_raw(cmd: u8, counter: u8) -> u64 {
    let device_id = DEVICE_ID as u64;
    let cmd = cmd as u64;
    let redundancy = 0x50u64 | (cmd ^ 0x01);
    let x = (counter as u64) & 0x0F;
    let counter_byte = ((!x) & 0x0F) << 4 | x;

    (device_id << 24) | (cmd << 16) | (redundancy << 8) | counter_byte
}

/// Build the 40-bit packet for a button press with the given 4-bit counter X.
pub fn build_packet(button: Button, counter: u8) -> u64 {
    build_packet_raw(button.command(), counter)
}

#[inline]
fn us_to_samples(us: f32, sample_rate: u32) -> usize {
    (us * 1e-6 * sample_rate as f32).round() as usize
}

#[inline]
fn append_const(buf: &mut Vec<Complex32>, value: Complex32, count: usize) {
    buf.resize(buf.len() + count, value);
}

/// Append baseband samples for one packet (preamble + sync + 40 data bits + stop)
/// to `buf`. Does NOT append the trailing inter-packet gap.
pub fn encode_packet(
    buf: &mut Vec<Complex32>,
    packet_bits: u64,
    sample_rate: u32,
    amplitude: f32,
) {
    let on = Complex32::new(amplitude, 0.0);
    let off = Complex32::new(0.0, 0.0);

    append_const(buf, on, us_to_samples(PREAMBLE_ON_US, sample_rate));
    append_const(buf, off, us_to_samples(SYNC_OFF_US, sample_rate));

    for i in (0..DATA_BITS).rev() {
        let bit = (packet_bits >> i) & 1;
        append_const(buf, on, us_to_samples(DATA_ON_US, sample_rate));
        let off_us = if bit == 1 { LONG_OFF_US } else { SHORT_OFF_US };
        append_const(buf, off, us_to_samples(off_us, sample_rate));
    }

    append_const(buf, on, us_to_samples(STOP_ON_US, sample_rate));
}

/// Encode a press for a raw command byte — see [`encode_button_press`] for
/// the named-button variant.
pub fn encode_press_raw(
    cmd: u8,
    counter: u8,
    repeats: u32,
    sample_rate: u32,
) -> Vec<Complex32> {
    let mut buf = Vec::new();
    let off = Complex32::new(0.0, 0.0);
    let gap = us_to_samples(INTER_PACKET_GAP_US, sample_rate);
    let bits = build_packet_raw(cmd, counter);

    for i in 0..repeats {
        encode_packet(&mut buf, bits, sample_rate, 1.0);
        if i + 1 < repeats {
            append_const(&mut buf, off, gap);
        }
    }
    buf
}

/// Encode a full button press: `repeats` packet repetitions separated by
/// inter-packet gaps. All repeats share the same counter value.
pub fn encode_button_press(
    button: Button,
    counter: u8,
    repeats: u32,
    sample_rate: u32,
) -> Vec<Complex32> {
    encode_press_raw(button.command(), counter, repeats, sample_rate)
}

// ---------- SigMF I/O ----------

#[derive(Debug)]
pub struct Capture {
    pub samples: Vec<Complex32>,
    pub sample_rate: u32,
    pub center_frequency: u64,
}

pub fn read_capture(
    name: &str,
    dir: impl AsRef<std::path::Path>,
) -> std::io::Result<Capture> {
    let dir = dir.as_ref();
    let meta = std::fs::read_to_string(dir.join(format!("{name}.sigmf-meta")))?;
    let meta: serde_json::Value = serde_json::from_str(&meta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let sample_rate = meta["global"]["core:sample_rate"]
        .as_u64()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing sample_rate"))?
        as u32;
    let center_frequency = meta["captures"][0]["core:frequency"]
        .as_u64()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing frequency"))?;

    let raw = std::fs::read(dir.join(format!("{name}.sigmf-data")))?;
    let n = raw.len() / 8;
    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        let re = f32::from_le_bytes(raw[i * 8..i * 8 + 4].try_into().unwrap());
        let im = f32::from_le_bytes(raw[i * 8 + 4..i * 8 + 8].try_into().unwrap());
        samples.push(Complex32::new(re, im));
    }
    Ok(Capture { samples, sample_rate, center_frequency })
}

pub fn write_cf32(
    path: impl AsRef<std::path::Path>,
    samples: &[Complex32],
) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for c in samples {
        f.write_all(&c.re.to_le_bytes())?;
        f.write_all(&c.im.to_le_bytes())?;
    }
    f.flush()
}

// ---------- Decoder (used only for round-trip validation) ----------

/// Slice IQ to a binary on/off signal using a centered moving-average smoother
/// followed by a fraction-of-peak threshold. Matches the Python pipeline.
pub fn slice_envelope(
    iq: &[Complex32],
    sample_rate: u32,
    smooth_us: f32,
    threshold_frac: f32,
) -> Vec<bool> {
    let n = iq.len();
    if n == 0 {
        return Vec::new();
    }
    let win = us_to_samples(smooth_us, sample_rate).max(1);
    let half = win / 2;

    // Magnitude
    let mag: Vec<f32> = iq.iter().map(|c| (c.re * c.re + c.im * c.im).sqrt()).collect();

    // Centered moving-average via prefix sum (O(n))
    let mut prefix = vec![0.0f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + mag[i] as f64;
    }
    let mut smooth = vec![0.0f32; n];
    let mut peak = 0.0f32;
    #[allow(clippy::needless_range_loop)]
    for i in 0..n {
        let lo = i.saturating_sub(half);
        let hi = (i + half + 1).min(n);
        let val = ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32;
        smooth[i] = val;
        if val > peak {
            peak = val;
        }
    }
    let threshold = peak * threshold_frac;
    smooth.into_iter().map(|x| x > threshold).collect()
}

/// Run-length encode the binary signal. Skips leading OFF samples; result
/// always starts with an ON run, then alternates OFF/ON/OFF/...
pub fn run_lengths(binary: &[bool]) -> Vec<usize> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < binary.len() && !binary[i] {
        i += 1;
    }
    while i < binary.len() {
        let start = i;
        let level = binary[i];
        while i < binary.len() && binary[i] == level {
            i += 1;
        }
        runs.push(i - start);
    }
    runs
}

/// Decode all 40-bit packets present in the run-length sequence.
pub fn decode_packets(runs: &[usize], sample_rate: u32) -> Vec<u64> {
    let preamble_min = us_to_samples(4_000.0, sample_rate);
    let bit_split = us_to_samples(1_000.0, sample_rate);
    let packet_runs = 2 + 2 * DATA_BITS as usize + 1; // preamble + sync + 40 (ON,OFF) + stop = 83

    let mut packets = Vec::new();
    let mut i = 0;
    while i < runs.len() {
        // Even indices are ON runs (run_lengths starts with ON).
        if i % 2 == 0 && runs[i] > preamble_min && i + packet_runs <= runs.len() {
            let mut bits = 0u64;
            for k in 0..DATA_BITS as usize {
                // Bit k is encoded by the OFF at index (i + 3 + 2k).
                let off = runs[i + 3 + 2 * k];
                let b = if off > bit_split { 1u64 } else { 0u64 };
                bits = (bits << 1) | b;
            }
            packets.push(bits);
            i += packet_runs + 1; // skip past stop ON and inter-packet gap (if present)
        } else {
            i += 1;
        }
    }
    packets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_packet_known_values() {
        // From captures (counter X = 0):
        assert_eq!(build_packet(Button::TurnOn, 0), 0xF3A2_0150_F0);
        assert_eq!(build_packet(Button::TurnOff, 0), 0xF3A2_0253_F0);
        assert_eq!(build_packet(Button::TemperatureDown, 0), 0xF3A2_0657_F0);
        assert_eq!(build_packet(Button::TemperatureUp, 0), 0xF3A2_0859_F0);
        assert_eq!(build_packet(Button::BrightnessDown, 0), 0xF3A2_0A5B_F0);
        assert_eq!(build_packet(Button::BrightnessUp, 0), 0xF3A2_0455_F0);

        // turn_on capture's first observed packet was X=1 → last byte 0xE1
        assert_eq!(build_packet(Button::TurnOn, 1), 0xF3A2_0150_E1);
        // Counter sequence we observed in non-turn_on captures:
        assert_eq!(build_packet(Button::TurnOff, 4) & 0xFF, 0xB4);
    }

    #[test]
    fn round_trip_each_button() {
        for &btn in Button::ALL {
            for counter in [0u8, 1, 5, 0x0F] {
                let expected = build_packet(btn, counter);
                let samples = encode_button_press(btn, counter, 3, DEFAULT_SAMPLE_RATE);
                let binary = slice_envelope(&samples, DEFAULT_SAMPLE_RATE, 20.0, 0.30);
                let runs = run_lengths(&binary);
                let packets = decode_packets(&runs, DEFAULT_SAMPLE_RATE);
                assert_eq!(packets.len(), 3, "wrong rep count for {}", btn.name());
                for (i, p) in packets.iter().enumerate() {
                    assert_eq!(*p, expected, "mismatch for {} rep {}", btn.name(), i);
                }
            }
        }
    }
}
