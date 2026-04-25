//! SoapySDR-based transmission. Requires a working libSoapySDR install plus
//! the bridge module for your SDR (e.g. SoapyUHD for Ettus, SoapyLMS7 for
//! LimeSDR).

use num_complex::Complex32;
use soapysdr::{Device, Direction};

use crate::{Button, DEFAULT_CENTER_FREQ_HZ, DEFAULT_SAMPLE_RATE, encode_press_raw};

#[derive(Debug, Clone)]
pub struct TxParams {
    /// Device args (e.g. `"driver=uhd"`, `"driver=uhd,serial=321F509"`).
    /// Empty string lets SoapySDR pick the first available device.
    pub args: String,
    pub frequency_hz: f64,
    pub sample_rate_hz: f64,
    pub gain_db: f64,
    pub channel: usize,
    /// Antenna name (e.g. `"TX/RX"` for B200). `None` lets the driver pick.
    pub antenna: Option<String>,
}

impl Default for TxParams {
    fn default() -> Self {
        Self {
            args: String::new(),
            frequency_hz: DEFAULT_CENTER_FREQ_HZ as f64,
            sample_rate_hz: DEFAULT_SAMPLE_RATE as f64,
            gain_db: 50.0,
            channel: 0,
            antenna: None,
        }
    }
}

pub type TxError = Box<dyn std::error::Error + Send + Sync>;

pub fn enumerate_devices() -> Result<Vec<String>, TxError> {
    let devs = soapysdr::enumerate(())?;
    Ok(devs.iter().map(|a| a.to_string()).collect())
}

pub fn transmit_samples(samples: &[Complex32], params: &TxParams) -> Result<(), TxError> {
    let dev = Device::new(&*params.args)?;

    dev.set_sample_rate(Direction::Tx, params.channel, params.sample_rate_hz)?;
    dev.set_frequency(Direction::Tx, params.channel, params.frequency_hz, ())?;
    dev.set_gain(Direction::Tx, params.channel, params.gain_db)?;
    if let Some(ant) = &params.antenna {
        dev.set_antenna(Direction::Tx, params.channel, ant.as_str())?;
    }

    let mut tx = dev.tx_stream::<Complex32>(&[params.channel])?;
    tx.activate(None)?;

    let mtu = tx.mtu()?;
    let mut written = 0;
    while written < samples.len() {
        let chunk_end = (written + mtu).min(samples.len());
        let is_last = chunk_end == samples.len();
        let chunk = &samples[written..chunk_end];
        let n = tx.write(&[chunk], None, is_last, 1_000_000)?;
        written += n;
    }

    tx.deactivate(None)?;
    Ok(())
}

/// Encode and transmit a press for a raw command byte (lead/trail silence
/// flanking `repeats` packet repetitions). See [`transmit_press`] for the
/// named-button variant.
pub fn transmit_press_raw(
    cmd: u8,
    counter: u8,
    repeats: u32,
    lead_silence_ms: f32,
    trail_silence_ms: f32,
    params: &TxParams,
) -> Result<usize, TxError> {
    let fs = params.sample_rate_hz as u32;
    let lead = (lead_silence_ms * 1e-3 * fs as f32).round() as usize;
    let trail = (trail_silence_ms * 1e-3 * fs as f32).round() as usize;
    let zero = Complex32::new(0.0, 0.0);

    let press = encode_press_raw(cmd, counter, repeats, fs);
    let mut samples = Vec::with_capacity(lead + press.len() + trail);
    samples.resize(lead, zero);
    samples.extend(press);
    samples.resize(samples.len() + trail, zero);

    let n = samples.len();
    transmit_samples(&samples, params)?;
    Ok(n)
}

pub fn transmit_press(
    button: Button,
    counter: u8,
    repeats: u32,
    lead_silence_ms: f32,
    trail_silence_ms: f32,
    params: &TxParams,
) -> Result<usize, TxError> {
    transmit_press_raw(
        button.command(),
        counter,
        repeats,
        lead_silence_ms,
        trail_silence_ms,
        params,
    )
}
