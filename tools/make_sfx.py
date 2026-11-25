#!/usr/bin/env python3
"""Synthesise cat noises (meow, mew, mrrp, purr) into assets/sfx/*.wav.

A buzzy glottal source with a gliding pitch is run through three resonators whose
centre frequencies glide too (the "ee-a-ow" vowel shape of a meow). These are a
first attempt; drop real recordings named meow1.wav, purr1.wav... into assets/sfx
to replace them.

usage: make_sfx.py [out_dir]
"""
import sys
import wave
from pathlib import Path

import numpy as np

RATE = 22050
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent / "assets" / "sfx"


def curve(points, n):
    """Piecewise-linear curve through (t 0..1, value) points, sampled n times."""
    xs, ys = zip(*points)
    return np.interp(np.linspace(0, 1, n), xs, ys)


def resonator(x, freq, bw):
    """Time-varying two-pole resonator (Klatt style). freq, bw are per-sample arrays."""
    y = np.zeros_like(x)
    y1 = y2 = 0.0
    for i in range(len(x)):
        r = np.exp(-np.pi * bw[i] / RATE)
        b = 2 * r * np.cos(2 * np.pi * freq[i] / RATE)
        c = -r * r
        a = 1 - b - c
        y0 = a * x[i] + b * y1 + c * y2
        y[i] = y0
        y2, y1 = y1, y0
    return y


def voice(dur, f0_pts, formants, amp_pts, trill_hz=0.0, breath=0.04, vibrato=5.0, rng=None):
    n = int(dur * RATE)
    f0 = curve(f0_pts, n)
    f0 = f0 * (1 + 0.012 * np.sin(2 * np.pi * vibrato * np.arange(n) / RATE))
    f0 = f0 * (1 + 0.006 * rng.standard_normal(n).cumsum() / np.sqrt(np.arange(1, n + 1)))
    phase = np.cumsum(f0) / RATE
    # Band-limited-ish sawtooth: sum of a few harmonics with 1/h roll-off.
    src = np.zeros(n)
    for h in range(1, 18):
        src += np.sin(2 * np.pi * h * phase) / h
    src += breath * rng.standard_normal(n) * 3
    out = np.zeros(n)
    for f_pts, bw, gain in formants:
        out += gain * resonator(src, curve(f_pts, n), np.full(n, bw))
    env = curve(amp_pts, n)
    if trill_hz:
        env = env * (0.55 + 0.45 * np.sin(2 * np.pi * trill_hz * np.arange(n) / RATE) ** 2)
    return out * env


def meow(seed, base, length):
    rng = np.random.default_rng(seed)
    d = 0.62 * length
    f0 = [(0, base * 0.62), (0.18, base * 0.95), (0.40, base * 1.25), (0.72, base * 0.9), (1, base * 0.6)]
    f1 = [(0, 320), (0.22, 400), (0.45, 880), (0.75, 520), (1, 420)]
    f2 = [(0, 2300), (0.22, 2200), (0.45, 1650), (0.75, 1000), (1, 850)]
    f3 = [(0, 3100), (1, 2900)]
    amp = [(0, 0), (0.06, 0.35), (0.25, 0.9), (0.55, 1.0), (0.85, 0.5), (1, 0)]
    return voice(d, f0, [(f1, 110, 1.0), (f2, 160, 0.9), (f3, 250, 0.35)], amp, rng=rng)


def mew(seed, base, length):
    rng = np.random.default_rng(seed)
    d = 0.32 * length
    f0 = [(0, base * 0.9), (0.4, base * 1.3), (1, base * 1.0)]
    f1 = [(0, 380), (0.5, 560), (1, 450)]
    f2 = [(0, 2500), (0.5, 2200), (1, 1900)]
    amp = [(0, 0), (0.1, 0.8), (0.6, 1.0), (1, 0)]
    return voice(d, f0, [(f1, 100, 1.0), (f2, 150, 0.9), ([(0, 3300), (1, 3300)], 250, 0.3)], amp, rng=rng)


def mrrp(seed, base, length):
    rng = np.random.default_rng(seed)
    d = 0.26 * length
    f0 = [(0, base * 0.8), (0.5, base * 1.1), (1, base * 1.35)]
    f1 = [(0, 450), (1, 520)]
    f2 = [(0, 1500), (1, 1800)]
    amp = [(0, 0), (0.08, 0.9), (0.8, 1.0), (1, 0)]
    return voice(d, f0, [(f1, 120, 1.0), (f2, 170, 0.8), ([(0, 2900), (1, 2900)], 250, 0.3)],
                 amp, trill_hz=26, vibrato=0, rng=rng)


def purr(seed, length):
    rng = np.random.default_rng(seed)
    d = 2.4 * length
    n = int(d * RATE)
    t = np.arange(n) / RATE
    rate = 24 + 3 * np.sin(2 * np.pi * 0.4 * t)  # purr pulses per second
    pulses = (np.sin(2 * np.pi * np.cumsum(rate) / RATE) > 0.1).astype(float)
    pulses = np.convolve(pulses, np.hanning(180) / np.hanning(180).sum(), mode="same")
    noise = rng.standard_normal(n)
    # Low rumble: smooth the noise heavily, add a little body at 60-120 Hz.
    k = np.hanning(60)
    rumble = np.convolve(noise, k / k.sum(), mode="same")
    body = np.sin(2 * np.pi * 90 * t) + 0.5 * np.sin(2 * np.pi * 180 * t)
    x = (rumble * 6 + body * 0.5) * (0.35 + 0.65 * pulses)
    return x * curve([(0, 0), (0.12, 1), (0.85, 1), (1, 0)], n)


def chatter(seed, rate, length):
    """The 'ek-ek-ek-ek' a cat makes at a bird: a short buzzy vowel gated on and off about 11 times a second."""
    rng = np.random.default_rng(seed)
    d = 1.15 * length
    n = int(d * RATE)
    f0 = [(0, 640), (0.5, 760), (1, 680)]
    f1 = [(0, 650), (1, 600)]
    f2 = [(0, 1900), (1, 1700)]
    amp = [(0, 0), (0.04, 1), (0.9, 1), (1, 0)]
    x = voice(d, f0, [(f1, 130, 1.0), (f2, 180, 0.8), ([(0, 3000), (1, 3000)], 260, 0.3)], amp, vibrato=0, rng=rng)
    t = np.arange(n) / RATE
    gate = (np.sin(2 * np.pi * rate * t) > 0.15).astype(float)
    k = np.hanning(int(0.006 * RATE) * 2 + 1)
    gate = np.convolve(gate, k / k.sum(), mode="same")
    clicks = rng.standard_normal(n) * 0.08 * np.diff(np.concatenate([[0], gate])).clip(0)  # a tick at each onset
    return x * gate + clicks * np.max(np.abs(x))


def save(name, x, peak=0.45):
    x = x / (np.max(np.abs(x)) + 1e-9) * peak
    with wave.open(str(OUT / name), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes((x * 32767).astype("<i2").tobytes())
    print("wrote", name, f"{len(x) / RATE:.2f}s")


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    for i, (base, length) in enumerate([(520, 1.0), (600, 0.85), (470, 1.15)], 1):
        save(f"meow{i}.wav", meow(10 + i, base, length))
    for i, (base, length) in enumerate([(780, 1.0), (880, 0.8)], 1):
        save(f"mew{i}.wav", mew(20 + i, base, length))
    for i, (base, length) in enumerate([(380, 1.0), (430, 0.9)], 1):
        save(f"mrrp{i}.wav", mrrp(30 + i, base, length))
    for i, length in enumerate([1.0, 1.3], 1):
        save(f"purr{i}.wav", purr(40 + i, length), peak=0.3)
    for i, (rate, length) in enumerate([(11, 1.0), (13, 1.2)], 1):
        save(f"chatter{i}.wav", chatter(50 + i, rate, length), peak=0.4)
