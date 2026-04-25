# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy"]
# ///
"""Dump the raw ON/OFF duration sequence around each preamble.

Lets us see actual packet structure rather than guessing it.

Usage: uv run analysis/dump_timings.py <name> [--first N]
"""
import json
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
CAPTURES = ROOT / "captures"

SMOOTH_US = 20
THRESHOLD_FRAC = 0.30


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
    if falling[0] < rising[0]:
        falling = falling[1:]
    n = min(len(rising), len(falling))
    return rising[:n], falling[:n]


def categorize_on(us: float) -> str:
    if us > 4000:
        return f"PREAMBLE({us:6.0f})"
    return f"on({us:5.0f})"


def categorize_off(us: float) -> str:
    if us > 4000:
        return f"GAP({us:>7.0f})"
    if us > 1000:
        return f"L({us:5.0f})"
    return f"s({us:5.0f})"


def main(argv):
    if len(argv) < 2:
        sys.exit("usage: dump_timings.py <name> [--first N]")
    name = argv[1]
    first_n = 80
    if "--first" in argv:
        first_n = int(argv[argv.index("--first") + 1])

    iq, fs = load_sigmf(name)
    binary = slice_binary(np.abs(iq), fs)
    rising, falling = edges(binary)
    on_us = (falling - rising) * 1e6 / fs
    off_us = np.diff(rising) * 1e6 / fs - on_us[:-1]  # OFF[k] = rising[k+1] - falling[k]

    print(f"name: {name}  ({len(rising)} ONs, {len(off_us)} OFFs)")
    print(f"first {first_n} ON/OFF pairs (ON, then trailing OFF):")
    print()

    line = []
    for i in range(min(first_n, len(off_us))):
        token = f"{categorize_on(on_us[i])}-{categorize_off(off_us[i])}"
        line.append(token)
        # Newline whenever we see a GAP to align packet boundaries visually
        if off_us[i] > 4000:
            print("  ".join(line))
            line = []
    if line:
        # last chunk
        if len(line) > 0:
            line[-1] = line[-1] + f"-on({on_us[i+1]:.0f})"
        print("  ".join(line))


if __name__ == "__main__":
    main(sys.argv)
