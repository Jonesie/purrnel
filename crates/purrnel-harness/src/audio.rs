//! Always-on listening: capture -> voice activity detection -> speech-to-text -> hub.
//!
//! The microphone process only exists while unmuted. Muting kills it, so
//! nothing is captured at all (not merely ignored).

use crate::hub::Hub;
use crate::protocol::Out;
use crate::stt::Stt;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;

const RATE: usize = 16_000;
const FRAME: usize = RATE / 50; // 20 ms
const PREROLL_FRAMES: usize = 15; // 300 ms kept before speech starts
const START_FRAMES: u32 = 4; // 80 ms of speech to trigger
const END_SILENCE_FRAMES: u32 = 25; // 500 ms of quiet ends an utterance
const MIN_VOICED_FRAMES: u32 = 8; // ignore blips with under 160 ms of actual speech
const MAX_UTTERANCE_FRAMES: usize = 50 * 30;

pub async fn run(hub: Arc<Hub>, stt: Option<Arc<Stt>>) {
    let mut privacy = hub.privacy_tx.subscribe();
    loop {
        while !privacy.borrow().mic_live() {
            if privacy.changed().await.is_err() {
                return;
            }
        }
        if let Err(e) = capture(&hub, stt.clone(), &mut privacy).await {
            eprintln!("purrnel: microphone problem: {e}");
            hub.emit(Out::Notice { text: format!("I can't hear: {e}") });
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
}

async fn capture(
    hub: &Arc<Hub>,
    stt: Option<Arc<Stt>>,
    privacy: &mut tokio::sync::watch::Receiver<purrnel_core::Privacy>,
) -> std::io::Result<()> {
    let mut cmd = Command::new("parec");
    cmd.args(["--raw", "--format=s16le", "--rate=16000", "--channels=1", "--latency-msec=40"]);
    if let Some(src) = &hub.cfg.mic_source {
        cmd.arg(format!("--device={src}"));
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped");

    // Reader task so the select below only awaits cancel-safe things.
    let (frame_tx, mut frame_rx) = mpsc::channel::<Vec<i16>>(64);
    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; FRAME * 2];
        while stdout.read_exact(&mut buf).await.is_ok() {
            let frame = buf.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            if frame_tx.send(frame).await.is_err() {
                break;
            }
        }
    });

    let mut vad = Vad::new();
    let mut n = 0u32;
    let result = loop {
        tokio::select! {
            frame = frame_rx.recv() => {
                let Some(frame) = frame else {
                    break Err(std::io::Error::other("audio capture stopped (is a microphone available?)"));
                };
                let gated = hub.echo_gate();
                let (utterance, rms) = vad.push(&frame, gated);
                hub.hearing.store(vad.in_speech, Ordering::Relaxed);
                n += 1;
                if n % 3 == 0 {
                    hub.emit(Out::Level { level: (rms / 4000.0).clamp(0.0, 1.0) });
                }
                if let Some(pcm) = utterance {
                    if let Some(stt) = stt.clone() {
                        let hub = hub.clone();
                        hub.start_clock();
                        // The reply is seconds away; wake the speakers now so the first words aren't clipped.
                        hub.wake_sound_device();
                        tokio::spawn(async move {
                            let text = stt.transcribe(&pcm).await;
                            hub.lap("transcribed");
                            if let Some(text) = text {
                                eprintln!("purrnel: heard: {text}");
                                hub.submit(text, false).await;
                            }
                        });
                    }
                }
            }
            changed = privacy.changed() => {
                if changed.is_err() || !privacy.borrow().mic_live() {
                    break Ok(());
                }
            }
        }
    };
    hub.hearing.store(false, Ordering::Relaxed);
    hub.emit(Out::Level { level: 0.0 });
    reader.abort();
    let _ = child.kill().await;
    result
}

/// Energy-based voice activity detector with an adaptive noise floor.
pub struct Vad {
    floor: f32,
    calibrated: bool,
    run: u32,
    silence: u32,
    voiced: u32,
    pub in_speech: bool,
    preroll: VecDeque<Vec<i16>>,
    utter: Vec<i16>,
}

impl Vad {
    pub fn new() -> Self {
        Vad {
            floor: 80.0,
            calibrated: false,
            run: 0,
            silence: 0,
            voiced: 0,
            in_speech: false,
            preroll: VecDeque::new(),
            utter: Vec::new(),
        }
    }

    /// Noise floor: follows quiet moments down quickly, and loud ones up very slowly
    /// (about a minute), so a speech burst barely moves it but a noisy room does.
    fn track_floor(&mut self, rms: f32) {
        if !self.calibrated {
            self.floor = rms;
            self.calibrated = true;
        } else if rms < self.floor {
            self.floor = self.floor * 0.9 + rms * 0.1;
        } else {
            self.floor = (self.floor + (rms - self.floor) * 0.0008).min(3000.0);
        }
    }

    fn reset(&mut self) {
        self.run = 0;
        self.silence = 0;
        self.voiced = 0;
        self.in_speech = false;
        self.preroll.clear();
        self.utter.clear();
    }

    /// Feeds one 20 ms frame. `gated` means the cat is talking, so the frame is thrown away.
    /// Returns a finished utterance (if any) and the frame's loudness.
    pub fn push(&mut self, frame: &[i16], gated: bool) -> (Option<Vec<i16>>, f32) {
        let rms = rms(frame);
        if gated {
            self.reset();
            return (None, rms);
        }
        self.track_floor(rms);
        // Hysteresis: it takes more to start speech than to keep it going.
        let start_threshold = self.floor * 3.0 + 150.0;
        let keep_threshold = self.floor * 2.0 + 100.0;

        if !self.in_speech {
            let loud = rms > start_threshold;
            self.preroll.push_back(frame.to_vec());
            while self.preroll.len() > PREROLL_FRAMES {
                self.preroll.pop_front();
            }
            self.run = if loud { self.run + 1 } else { 0 };
            if self.run >= START_FRAMES {
                self.in_speech = true;
                self.silence = 0;
                self.voiced = self.run;
                self.utter = self.preroll.drain(..).flatten().collect();
            }
            return (None, rms);
        }

        let loud = rms > keep_threshold;
        self.utter.extend_from_slice(frame);
        self.silence = if loud { 0 } else { self.silence + 1 };
        self.voiced += loud as u32;
        let frames = self.utter.len() / FRAME;
        if self.silence >= END_SILENCE_FRAMES || frames >= MAX_UTTERANCE_FRAMES {
            let keep = self.utter.len().saturating_sub((self.silence.saturating_sub(10)) as usize * FRAME);
            let mut pcm = std::mem::take(&mut self.utter);
            pcm.truncate(keep);
            let long_enough = self.voiced >= MIN_VOICED_FRAMES;
            self.reset();
            return (long_enough.then_some(pcm), rms);
        }
        (None, rms)
    }
}

impl Default for Vad {
    fn default() -> Self {
        Self::new()
    }
}

fn rms(frame: &[i16]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    (frame.iter().map(|&s| (s as f32).powi(2)).sum::<f32>() / frame.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amp: i16) -> Vec<i16> {
        (0..FRAME).map(|i| if i % 2 == 0 { amp } else { -amp }).collect()
    }

    fn feed(vad: &mut Vad, amp: i16, frames: usize) -> Vec<Vec<i16>> {
        (0..frames).filter_map(|_| vad.push(&tone(amp), false).0).collect()
    }

    #[test]
    fn detects_one_utterance_between_silences() {
        let mut v = Vad::new();
        assert!(feed(&mut v, 20, 100).is_empty());
        assert!(feed(&mut v, 3000, 60).is_empty());
        assert!(v.in_speech);
        let done = feed(&mut v, 20, 60);
        assert_eq!(done.len(), 1);
        assert!(done[0].len() >= 60 * FRAME);
        assert!(!v.in_speech);
    }

    /// Pseudo-random noise of a given loudness (deterministic).
    fn noise(rms: f32, seed: &mut u32) -> Vec<i16> {
        (0..FRAME)
            .map(|_| {
                *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let u = ((*seed >> 16) as f32 / 65535.0) * 2.0 - 1.0;
                (u * rms * 1.732) as i16
            })
            .collect()
    }

    #[test]
    fn noisy_room_does_not_count_as_speech() {
        // A webcam mic with ~250 rms of hiss from the very first frame.
        let mut v = Vad::new();
        let mut seed = 7;
        for _ in 0..500 {
            let (out, _) = v.push(&noise(250.0, &mut seed), false);
            assert!(out.is_none());
            assert!(!v.in_speech);
        }
        // Real speech on top of that hiss is still detected and then ends.
        let mut got = 0;
        for _ in 0..60 {
            if v.push(&tone(3000), false).0.is_some() { got += 1; }
        }
        assert!(v.in_speech);
        for _ in 0..80 {
            if v.push(&noise(250.0, &mut seed), false).0.is_some() { got += 1; }
        }
        assert_eq!(got, 1);
        assert!(!v.in_speech);
    }

    #[test]
    fn ignores_short_blips() {
        let mut v = Vad::new();
        feed(&mut v, 20, 50);
        feed(&mut v, 3000, 5);
        assert!(feed(&mut v, 20, 80).is_empty());
    }

    #[test]
    fn gate_discards_speech_while_cat_talks() {
        let mut v = Vad::new();
        for _ in 0..100 {
            assert!(v.push(&tone(3000), true).0.is_none());
        }
        assert!(!v.in_speech);
    }
}
