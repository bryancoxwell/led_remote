# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "scipy", "matplotlib"]
# ///
"""Slice a SigMF capture to binary, measure pulse widths, histogram them.

Usage: uv run analysis/decode.py <name>
"""
import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

ROOT = Path(__file__).resolve().parent.parent
CAPTURES = ROOT / "captures"
FIGURES = Path(__file__).resolve().parent / "figures"


def load_sigmf(name: str):
    meta = json.loads((CAPTURES / f"{name}.sigmf-meta").read_text())
    fs = meta["global"]["core:sample_rate"]
    raw = np.fromfile(CAPTURES / f"{name}.sigmf-data", dtype=np.float32)
    return raw[0::2] + 1j * raw[1::2], fs


def slice_binary(mag: np.ndarray, fs: float, smooth_us: float = 20.0) -> np.ndarray:
    """30%-of-peak threshold slice with a short moving-average pre-filter."""
    win = max(1, int(smooth_us * 1e-6 * fs))
    kernel = np.ones(win) / win
    smooth = np.convolve(mag, kernel, mode="same")
    threshold = smooth.max() * 0.30
    return smooth > threshold


def runs(binary: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Return (on_run_lengths, off_run_lengths) in samples."""
    diff = np.diff(binary.astype(np.int8))
    rising = np.where(diff == 1)[0] + 1
    falling = np.where(diff == -1)[0] + 1
    if len(rising) == 0 or len(falling) == 0:
        return np.array([]), np.array([])
    # Trim to ON-then-OFF pairs starting with a rising edge
    if falling[0] < rising[0]:
        falling = falling[1:]
    n = min(len(rising), len(falling))
    on_runs = falling[:n] - rising[:n]
    off_runs = rising[1:n] - falling[: n - 1] if n > 1 else np.array([])
    return on_runs, off_runs


def main(name: str) -> None:
    iq, fs = load_sigmf(name)
    mag = np.abs(iq)
    binary = slice_binary(mag, fs)
    on_samples, off_samples = runs(binary)
    on_us = on_samples * 1e6 / fs
    off_us = off_samples * 1e6 / fs

    print(f"name        : {name}")
    print(f"on  runs    : n={len(on_us)},  median {np.median(on_us):7.1f} µs,  range {on_us.min():7.1f}..{on_us.max():.1f}")
    print(f"off runs    : n={len(off_us)}, median {np.median(off_us):7.1f} µs, range {off_us.min():7.1f}..{off_us.max():.1f}")

    # Inter-packet gaps stand out as long-tail OFF runs.
    # Anything > 5 * median is probably an inter-packet gap (or inter-burst gap).
    gap_thresh = max(5 * np.median(off_us), 4000)
    intra_off = off_us[off_us <= gap_thresh]
    inter_off = off_us[off_us > gap_thresh]
    print(f"intra-packet OFFs: n={len(intra_off)} (≤ {gap_thresh:.0f} µs)")
    print(f"inter-packet gaps: n={len(inter_off)}, median {np.median(inter_off) if len(inter_off) else 0:.0f} µs, max {inter_off.max() if len(inter_off) else 0:.0f} µs")

    # Histogram plots (log y to see modes and tail simultaneously)
    fig, axes = plt.subplots(2, 2, figsize=(14, 8))

    axes[0, 0].hist(on_us, bins=120, range=(0, 3000), color="steelblue")
    axes[0, 0].set_title(f"{name}: ON pulse widths (zoom 0–3000 µs)")
    axes[0, 0].set_xlabel("µs"); axes[0, 0].set_yscale("log"); axes[0, 0].grid(True, alpha=0.3)

    axes[0, 1].hist(on_us, bins=120, color="steelblue")
    axes[0, 1].set_title(f"ON pulse widths (full range)")
    axes[0, 1].set_xlabel("µs"); axes[0, 1].set_yscale("log"); axes[0, 1].grid(True, alpha=0.3)

    axes[1, 0].hist(intra_off, bins=120, range=(0, 3000), color="indianred")
    axes[1, 0].set_title(f"intra-packet OFF widths (zoom 0–3000 µs)")
    axes[1, 0].set_xlabel("µs"); axes[1, 0].set_yscale("log"); axes[1, 0].grid(True, alpha=0.3)

    axes[1, 1].hist(off_us, bins=120, color="indianred")
    axes[1, 1].set_title("OFF widths (full range; inter-packet gaps in tail)")
    axes[1, 1].set_xlabel("µs"); axes[1, 1].set_yscale("log"); axes[1, 1].grid(True, alpha=0.3)

    fig.tight_layout()
    out = FIGURES / f"{name}_hist.png"
    fig.savefig(out, dpi=120)
    print(f"wrote {out.relative_to(ROOT)}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: decode.py <name>")
    main(sys.argv[1])
