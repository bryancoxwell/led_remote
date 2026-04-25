# led_remote

Reverse-engineered transmitter for the Rayrun RM12 LED remote control. Replaces the physical remote with a CLI that emits the same 433.92 MHz OOK packets via a SoapySDR-supported SDR.

All seven buttons are decoded and reproduced over the air: `turn_on`, `turn_off`, `brightness_up`, `brightness_down`, `temperature_up`, `temperature_down`, `pair`.

## Requirements

- A SoapySDR-supported SDR. Tested against an Ettus USRP B200mini (macOS) and a LimeSDR Mini (Linux).
- Rust (edition 2024).
- For TX: SoapySDR + the bridge module for your SDR (SoapyUHD for Ettus, SoapyLMS7 for Lime, etc.).

### Linux install (Ubuntu/Debian)

```sh
sudo apt-get install -y \
  pkg-config cmake \
  libsoapysdr-dev soapysdr-tools \
  soapysdr-module-lms7 limesuite      # for LimeSDR; swap module for your SDR
```

For other SDRs use the matching `soapysdr0.8-module-*` package (e.g. `soapysdr0.8-module-uhd` for Ettus, `soapysdr0.8-module-hackrf`, `soapysdr0.8-module-bladerf`, …).

Verify with `SoapySDRUtil --find` — it should print your SDR.

### macOS install

```sh
brew install pkgconf cmake soapysdr uhd

# SoapyUHD has no brew formula; build it from source into ~/.local
git clone --depth 1 https://github.com/pothosware/SoapyUHD.git /tmp/SoapyUHD
cmake -S /tmp/SoapyUHD -B /tmp/SoapyUHD/build \
  -DCMAKE_INSTALL_PREFIX="$HOME/.local" \
  -DCMAKE_PREFIX_PATH=/opt/homebrew
cmake --build /tmp/SoapyUHD/build --parallel
cmake --install /tmp/SoapyUHD/build

# SoapySDR doesn't auto-scan ~/.local; tell it where to look:
export SOAPY_SDR_PLUGIN_PATH="$HOME/.local/lib/SoapySDR/modules0.8"
```

Add the `export` to your shell rc to persist. Verify with `SoapySDRUtil --find` — it should print your SDR.

## Build

```sh
cargo build       # encoder, decoder, and SoapySDR transmitter
cargo test        # unit tests + capture round-trip
```

## Usage

```sh
# Protocol summary and per-button packet bits
cargo run -- info

# Encode a button press to a baseband cf32_le file
cargo run -- encode turn_on -o turn_on.cf32

# Transmit
cargo run -- transmit turn_on -g 50                    # B200mini
cargo run -- transmit turn_on -g 40 -d driver=lime     # LimeSDR

# Probe an unknown command byte (no button arg needed)
cargo run -- transmit --cmd 0x0C -g 50

# List visible SDR devices
cargo run -- devices
```

Useful flags (apply to both `encode` and `transmit`):

| flag | meaning | default |
|---|---|---|
| `-c X` | 4-bit press counter override; bypasses (and does not advance) the persistent state | auto from state |
| `--counter-state <path>` | counter state file | `$XDG_STATE_HOME/led_remote/counter` or `~/.local/state/led_remote/counter` |
| `-r N` | packet repetitions per press | 5 |
| `-s Hz` | sample rate | 500000 |
| `--cmd <hex\|dec>` | override the command byte; for probing unknown codes | — |

The receiver enforces the counter as a replay defense — once it's seen a given X from this device_id, it ignores future packets unless the counter advances. The CLI keeps a single 4-bit counter in `~/.local/state/led_remote/counter` and bumps it (mod 16) on every `encode` / `transmit`. After re-pairing the receiver, sync the local counter explicitly:

```sh
cargo run -- reset-counter 0   # next press will use X=0
```

`transmit` adds:

| flag | meaning | default |
|---|---|---|
| `-g dB` | TX gain — range is per-SDR (B200mini 0–89.75; LimeSDR Mini −12–64) | 50 |
| `-f Hz` | carrier frequency | 433920000 |
| `-d <args>` | SoapySDR device args (e.g. `driver=uhd`, `driver=lime`) | first available |
| `--lead-ms` / `--trail-ms` | silence padding around the burst | 5 / 20 |

## Protocol

OOK/ASK at 433.92 MHz (per the RM12 FCC filing; captures were taken at 433.87 MHz). 40 data bits per packet, MSB first.

```
Packet (40 bits):
  [39:24]  device_id  = 0xF3A2                          # this remote's ID
  [23:16]  command    (per-button)
  [15: 8]  redundancy = 0x50 | (command ^ 0x01)
  [ 7: 0]  counter    = ((!X & 0xF) << 4) | (X & 0xF)   # 4-bit press counter X

Frame (sent 3–5× per button press):
  PREAMBLE_ON   ~8.32 ms
  SYNC_OFF      ~4.20 ms
  for bit in data_bits:
    ON          ~488 µs
    OFF         ~540 µs (bit=0)  or  ~1580 µs (bit=1)
  STOP_ON       ~488 µs
  GAP_OFF       ~10.7 ms
```

| button | command | packet @ X=0 |
|---|---|---|
| `turn_on` | 0x01 | 0xF3A20150F0 |
| `turn_off` | 0x02 | 0xF3A20253F0 |
| `brightness_up` | 0x04 | 0xF3A20455F0 |
| `temperature_down` | 0x06 | 0xF3A20657F0 |
| `temperature_up` | 0x08 | 0xF3A20859F0 |
| `brightness_down` | 0x0A | 0xF3A20A5BF0 |
| `pair` | 0x20 | 0xF3A22071F0 |

Other observed command bytes:
- `0x0C` — sets brightness to minimum (discovered while probing for `brightness_up`)

## Project layout

```
src/
  lib.rs        # protocol constants, encoder, decoder, SigMF I/O
  main.rs       # CLI
  transmit.rs   # SoapySDR TX path
tests/
  round_trip.rs # loads each capture, decodes via Rust, asserts bits match build_packet
captures/       # 5 SigMF recordings (cf32_le, 500 kHz, 433.87 MHz)
analysis/       # Python scripts used during reverse engineering
```

## Analysis tooling

Python scripts in `analysis/` use PEP-723 inline dependency metadata; run with `uv run`:

- `plot_capture.py <name>` — envelope, burst zoom, symbol zoom, spectrogram
- `decode.py <name>` — pulse-width histograms (reveals OOK/PWM timing constants)
- `dump_timings.py <name>` — raw ON/OFF run-length sequence around each preamble
- `extract_bits.py [<name>|--all]` — extract decoded bits from each packet

Figures are written to `analysis/figures/`.
