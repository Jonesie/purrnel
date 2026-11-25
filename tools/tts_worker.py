#!/usr/bin/env python3
"""Persistent Piper TTS worker. Loads the voice once.

usage: tts_worker.py <voice.onnx> [pitch=1.0] [length_scale=1.0]

Protocol (stdin, one request per line):  <out.wav>\t<text>
Reply (stdout, one line per request):     ok  |  err <message>

A pitch above 1.0 shifts the voice up (formants too, so it also sounds smaller)
using ffmpeg's rubberband filter, which keeps the speed unchanged.
"""
import os
import subprocess
import sys
import wave

from piper import PiperVoice, SynthesisConfig

voice = PiperVoice.load(sys.argv[1])
pitch = float(sys.argv[2]) if len(sys.argv) > 2 else 1.0
length = float(sys.argv[3]) if len(sys.argv) > 3 else 1.0
cfg = SynthesisConfig(length_scale=length, noise_scale=0.8, noise_w_scale=0.9)
print("ready", flush=True)

for line in sys.stdin:
    line = line.rstrip("\n")
    if not line:
        continue
    try:
        out, text = line.split("\t", 1)
        raw = out + ".raw.wav" if abs(pitch - 1.0) > 0.01 else out
        with wave.open(raw, "wb") as wf:
            voice.synthesize_wav(text, wf, syn_config=cfg)
        if raw != out:
            subprocess.run(
                ["ffmpeg", "-loglevel", "error", "-y", "-i", raw, "-af", f"rubberband=pitch={pitch}", out],
                check=True,
            )
            os.remove(raw)
        print("ok", flush=True)
    except Exception as e:  # keep the worker alive
        print(f"err {e}", flush=True)
