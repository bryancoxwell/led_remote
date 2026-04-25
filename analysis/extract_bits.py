# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy"]
# ///
"""Extract bit sequences from a SigMF capture by slicing each packet.

Packet structure (derived from dump_timings.py):
    PREAMBLE_ON(~8.3 ms)
    SYNC_OFF(~4.2 ms)            # not a data bit
    [data_ON(~488 µs) data_OFF]  × 40
        data_OFF ≈  540 µs  → bit 0
        data_OFF ≈ 1580 µs  → bit 1
    STOP_ON(~488 µs)             # no data
    INTER_PACKET_GAP_OFF(~10.7 ms)

Usage: uv run analysis/extract_bits.py <name>
       uv run analysis/extract_bits.py --all
"""
import json
import sys
from collections import Counter
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
CAPTURES = ROOT / "captures"

# Tuned to the measurements; comfortably between modes.
PREAMBLE_ON_MIN_US = 4000   # > 4 ms ON ⇒ preamble
DATA_OFF_SPLIT_US = 1000    # OFF ≤ this = 0, > this = 1
SMOOTH_US = 20              # envelope smoothing
THRESHOLD_FRAC = 0.30       # fraction of peak for slicing
DATA_BITS = 40

BUTTONS = ["turn_on", "turn_off", "temperature_up", "temperature_down", "brightness_down"]


def load_sigmf(name: str):
    meta = json.loads((CAPTURES / f"{name}.sigmf-meta").read_text())
    fs = meta["global"]["core:sample_rate"]
    raw = np.fromfile(CAPTURES / f"{name}.sigmf-data", dtype=np.float32)
    return raw[0::2] + 1j * raw[1::2], fs


def slice_binary(mag: np.ndarray, fs: float):
    win = max(1, int(SMOOTH_US * 1e-6 * fs))
    smooth = np.convolve(mag, np.ones(win) / win, mode="same")
    return smooth > smooth.max() * THRESHOLD_FRAC


def edges(binary: np.ndarray):
    diff = np.diff(binary.astype(np.int8))
    rising = np.where(diff == 1)[0] + 1
    falling = np.where(diff == -1)[0] + 1
    if len(rising) == 0 or len(falling) == 0:
        return np.array([]), np.array([])
    if falling[0] < rising[0]:
        falling = falling[1:]
    n = min(len(rising), len(falling))
    return rising[:n], falling[:n]


def extract_packets(name: str):
    iq, fs = load_sigmf(name)
    binary = slice_binary(np.abs(iq), fs)
    rising, falling = edges(binary)
    on_us = (falling - rising) * 1e6 / fs

    preamble_idxs = np.where(on_us > PREAMBLE_ON_MIN_US)[0]
    packets: list[str] = []
    # Bit k is encoded by OFF between falling[p + 1 + k] and rising[p + 2 + k]:
    #   - p+0: preamble ON
    #   - p+0..p+1: sync gap OFF (skipped)
    #   - p+1+k: kth data ON (k = 0..DATA_BITS-1)
    #   - OFF between data ON k and data ON k+1 encodes bit k
    for p in preamble_idxs:
        if p + 1 + DATA_BITS >= len(rising):
            continue
        bits = []
        for k in range(DATA_BITS):
            off_samples = rising[p + 2 + k] - falling[p + 1 + k]
            off_us = off_samples * 1e6 / fs
            bits.append("1" if off_us > DATA_OFF_SPLIT_US else "0")
        packets.append("".join(bits))
    return packets, fs


def hexify(bits: str) -> str:
    n = len(bits)
    pad = (4 - n % 4) % 4
    padded = "0" * pad + bits
    return f"0x{int(padded, 2):0{(n + 3)//4}X}"


def summarize(name: str) -> str:
    packets, _ = extract_packets(name)
    counts = Counter(packets)
    most_common, n_most = counts.most_common(1)[0]
    return f"{name:18s}  packets={len(packets):3d}  unique={len(counts):2d}  consensus×{n_most}={most_common}  ({hexify(most_common)})"


def main(argv):
    if len(argv) == 2 and argv[1] == "--all":
        print("name                packets  unique  consensus")
        for n in BUTTONS:
            print(summarize(n))
        return
    if len(argv) != 2:
        sys.exit("usage: extract_bits.py <name> | --all")

    name = argv[1]
    packets, _ = extract_packets(name)
    print(f"name        : {name}")
    print(f"packets     : {len(packets)}")
    # Print packets in time order, collapsing consecutive duplicates
    print("time-ordered (consecutive duplicates collapsed):")
    prev = None
    run = 0
    for i, p in enumerate(packets):
        if p == prev:
            run += 1
            continue
        if prev is not None:
            print(f"  ×{run:2d}  {prev}  ({hexify(prev)})")
        prev = p
        run = 1
    if prev is not None:
        print(f"  ×{run:2d}  {prev}  ({hexify(prev)})")


if __name__ == "__main__":
    main(sys.argv)
