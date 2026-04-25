# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "scipy", "matplotlib"]
# ///
"""Plot the magnitude envelope and spectrogram of a SigMF capture.

Usage: uv run analysis/plot_capture.py <name>      # e.g. turn_on
"""
import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import signal

ROOT = Path(__file__).resolve().parent.parent
CAPTURES = ROOT / "captures"
FIGURES = Path(__file__).resolve().parent / "figures"


def load_sigmf(name: str):
    meta = json.loads((CAPTURES / f"{name}.sigmf-meta").read_text())
    fs = meta["global"]["core:sample_rate"]
    fc = meta["captures"][0]["core:frequency"]
    raw = np.fromfile(CAPTURES / f"{name}.sigmf-data", dtype=np.float32)
    iq = raw[0::2] + 1j * raw[1::2]
    return iq, fs, fc


def find_first_burst(mag: np.ndarray, fs: float, ratio: float = 0.2, gap_s: float = 0.05):
    """Return (start, end) sample indices of the first burst, with a small margin."""
    threshold = mag.max() * ratio
    above = mag > threshold
    if not above.any():
        return 0, len(mag)
    start = int(np.argmax(above))
    gap = int(gap_s * fs)
    # Walk forward until we find `gap` consecutive samples below threshold.
    i = start
    while i < len(mag) - gap:
        if not above[i:i + gap].any():
            end = i
            break
        i += 1
    else:
        end = min(start + int(0.5 * fs), len(mag))
    margin = int(0.005 * fs)
    return max(0, start - margin), min(len(mag), end + margin)


def main(name: str) -> None:
    iq, fs, fc = load_sigmf(name)
    mag = np.abs(iq)
    duration = len(iq) / fs
    t = np.arange(len(iq)) / fs

    print(f"name        : {name}")
    print(f"samples     : {len(iq):,}")
    print(f"duration    : {duration:.3f} s")
    print(f"fs          : {fs/1e3:.0f} kHz")
    print(f"fc          : {fc/1e6:.3f} MHz")
    print(f"mag mean/max: {mag.mean():.4f} / {mag.max():.4f}")
    print(f"noise floor : {np.median(mag):.4f}  (median |IQ|)")

    b_start, b_end = find_first_burst(mag, fs)
    print(f"first burst : samples {b_start:,}..{b_end:,}  ({(b_end-b_start)/fs*1000:.2f} ms)")

    # Symbol-level zoom: 8 ms from a stable region inside the first burst.
    deep_dur_s = 0.008
    deep_start = b_start + int(0.05 * fs)        # skip first 50 ms (preamble territory)
    deep_end = min(deep_start + int(deep_dur_s * fs), b_end)

    # Estimate carrier offset from spectrum of the first burst (FFT peak).
    burst_iq = iq[b_start:b_end]
    spec = np.fft.fftshift(np.abs(np.fft.fft(burst_iq * np.hanning(len(burst_iq)))))
    fft_freqs = np.fft.fftshift(np.fft.fftfreq(len(burst_iq), d=1/fs))
    peak_offset_hz = fft_freqs[np.argmax(spec)]
    print(f"carrier off : {peak_offset_hz:+.0f} Hz from fc  (true carrier ≈ {(fc+peak_offset_hz)/1e6:.6f} MHz)")

    fig, axes = plt.subplots(4, 1, figsize=(14, 12))

    axes[0].plot(t, mag, linewidth=0.4)
    axes[0].axvspan(b_start / fs, b_end / fs, color="orange", alpha=0.2, label="first burst")
    axes[0].set_title(f"{name}: magnitude envelope  ({duration:.2f} s @ {fs/1e3:.0f} kHz, fc={fc/1e6:.3f} MHz)")
    axes[0].set_xlabel("time (s)")
    axes[0].set_ylabel("|IQ|")
    axes[0].legend(loc="upper right")
    axes[0].grid(True, alpha=0.3)

    axes[1].plot(t[b_start:b_end] * 1000, mag[b_start:b_end], linewidth=0.5)
    axes[1].axvspan(deep_start / fs * 1000, deep_end / fs * 1000, color="red", alpha=0.25, label="deep zoom")
    axes[1].set_title(f"first burst zoom  ({(b_end-b_start)/fs*1000:.2f} ms, {b_end-b_start:,} samples)")
    axes[1].set_xlabel("time (ms)")
    axes[1].set_ylabel("|IQ|")
    axes[1].legend(loc="upper right")
    axes[1].grid(True, alpha=0.3)

    axes[2].plot((t[deep_start:deep_end] - t[deep_start]) * 1000, mag[deep_start:deep_end], linewidth=0.8)
    axes[2].set_title(f"symbol-level zoom  ({deep_dur_s*1000:.0f} ms, {deep_end-deep_start:,} samples — 1 sample = {1e6/fs:.1f} µs)")
    axes[2].set_xlabel("time (ms, relative)")
    axes[2].set_ylabel("|IQ|")
    axes[2].grid(True, alpha=0.3)

    f, tt, Sxx = signal.spectrogram(
        iq, fs=fs, nperseg=1024, noverlap=512, return_onesided=False
    )
    f = np.fft.fftshift(f)
    Sxx = np.fft.fftshift(Sxx, axes=0)
    axes[3].pcolormesh(tt, f / 1e3, 10 * np.log10(Sxx + 1e-12), shading="auto", cmap="magma")
    axes[3].set_title(f"spectrogram  (y = offset from {fc/1e6:.3f} MHz)")
    axes[3].set_xlabel("time (s)")
    axes[3].set_ylabel("offset (kHz)")

    fig.tight_layout()
    out = FIGURES / f"{name}.png"
    fig.savefig(out, dpi=120)
    print(f"wrote {out.relative_to(ROOT)}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: plot_capture.py <name>   e.g. turn_on")
    main(sys.argv[1])
