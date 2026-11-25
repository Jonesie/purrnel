//! Local text-to-speech: a persistent Piper worker plus a playback task that also
//! drives the cat's mouth from the audio envelope.
//!
//! Cat noises written in the text ("mrow", "mew", "purr"...) are not spoken: they
//! are replaced by recorded/synthesised sounds from `assets/sfx`.

use crate::hub::{Hub, Speak};
use crate::protocol::Out;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// How long to prepend silence to a sound when the audio device has gone to sleep.
const COLD_PREROLL_MS: usize = 500;
/// Silence before the first sound of a sentence, so the stream has linked up before the first sample.
const LEAD_MS: usize = 80;
/// Silence after the last sound of a sentence. When a stream closes, the card or a receiver on a
/// digital link can mute straight away and swallow the end of the sound.
const TAIL_MS: usize = 300;
/// Between the pieces of one sentence there is almost no gap.
const JOIN_TAIL_MS: usize = 30;

/// One playable piece of a sentence.
struct Item {
    /// The whole sentence, attached to its first piece so the caption appears once.
    caption: Option<String>,
    path: Option<PathBuf>,
    /// Whether the mouth should move with the sound (not for purring).
    mouth: bool,
    /// Cut the sound short (with a fade): a purr mentioned in passing, not a long cuddle.
    max_ms: Option<usize>,
    temp: bool,
    /// Last piece of the sentence: the cat stops "talking" after this one.
    done: bool,
    /// Background noise: counted in `Hub::ambient`, not in `pending_speech`.
    ambient: bool,
}

#[derive(Debug, PartialEq)]
pub enum Seg {
    Speech(String),
    Sound { kind: &'static str, word: String },
}

/// Starts the worker, playback and wake-up tasks and registers them on the hub.
/// Returns false if TTS is unavailable (the cat then only shows subtitles).
pub async fn start(hub: Arc<Hub>) -> bool {
    let cfg = &hub.cfg;
    if !cfg.voice.is_file() || !cfg.tts_script.is_file() || !cfg.piper_python.is_file() {
        eprintln!("purrnel: piper voice/worker not found; speaking off (subtitles only)");
        return false;
    }
    let spawned = Command::new(&cfg.piper_python)
        .arg(&cfg.tts_script)
        .arg(&cfg.voice)
        .arg(cfg.pitch.to_string())
        .arg(cfg.speed.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            eprintln!("purrnel: cannot start piper worker: {e}");
            return false;
        }
    };
    let mut stdin = child.stdin.take().expect("piped");
    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    match tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await {
        Ok(Ok(Some(l))) if l.trim() == "ready" => {}
        _ => {
            eprintln!("purrnel: piper worker did not become ready; speaking off");
            return false;
        }
    }
    // The first synthesis loads the model's runtime and ffmpeg; do it now, not on her first words.
    let dir = std::env::temp_dir().join(format!("purrnel-tts-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let warm = dir.join("warm.wav");
    let warmed = stdin
        .write_all(format!("{}\tHello there.\n", warm.display()).as_bytes())
        .await
        .is_ok()
        && matches!(lines.next_line().await, Ok(Some(r)) if r.trim() == "ok");
    let _ = std::fs::remove_file(&warm);
    if !warmed {
        eprintln!("purrnel: piper warm-up failed; speaking off");
        return false;
    }
    let sfx = load_sfx(&cfg.sfx_dir);
    eprintln!(
        "purrnel: voice ready (pitch {}, {} cat sounds)",
        cfg.pitch,
        sfx.values().map(Vec::len).sum::<usize>()
    );

    let (speak_tx, mut speak_rx) = mpsc::unbounded_channel::<Speak>();
    let (item_tx, mut item_rx) = mpsc::channel::<Item>(3);
    let (wake_tx, mut wake_rx) = mpsc::unbounded_channel::<()>();
    hub.set_speaker(speak_tx);
    hub.set_waker(wake_tx);
    if cfg.keepalive && keep_alive(&hub) {
        eprintln!("purrnel: holding the sound card open so sounds are not clipped (PURRNEL_KEEPALIVE=0 to turn off)");
    }

    // Wake the sound card as soon as the user stops talking (see Hub::wake_sound_device).
    let silence = dir.join("silence.wav");
    let _ = std::fs::write(&silence, wav_bytes(22050, &vec![0i16; 22050 * 3 / 10]));
    let wake_hub = hub.clone();
    tokio::spawn(async move {
        while wake_rx.recv().await.is_some() {
            if wake_hub.audio_cold() {
                wake_hub.audio_touch();
                let _ = Command::new("paplay")
                    .arg(&silence)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
        }
    });

    // Synthesiser: keeps a sentence or two ahead of playback.
    let synth_hub = hub.clone();
    let synth_dir = dir.clone();
    tokio::spawn(async move {
        let _child = child; // keep the worker alive for the life of this task
        let (mut n, mut picks) = (0u64, 0usize);
        while let Some(request) = speak_rx.recv().await {
            let text = match request {
                Speak::Text(t) => t,
                Speak::Ambient(kind) => {
                    match sfx.get(kind) {
                        Some(files) => {
                            picks += 1;
                            let item = Item {
                                caption: None,
                                path: Some(files[picks % files.len()].clone()),
                                mouth: false,
                                max_ms: None,
                                temp: false,
                                done: false,
                                ambient: true,
                            };
                            if item_tx.send(item).await.is_err() {
                                return;
                            }
                        }
                        None => {
                            synth_hub.ambient.fetch_sub(1, Ordering::Relaxed);
                        }
                    }
                    continue;
                }
                Speak::Sfx(kind) => {
                    // A bare noise (emote, petting): no caption, just the sound.
                    let (path, mouth) = match sfx.get(kind) {
                        Some(files) => {
                            picks += 1;
                            (Some(files[picks % files.len()].clone()), kind != "purr")
                        }
                        None => (None, false),
                    };
                    let item = Item { caption: None, path, mouth, max_ms: None, temp: false, done: true, ambient: false };
                    if item_tx.send(item).await.is_err() {
                        return;
                    }
                    continue;
                }
            };
            let segs: Vec<Seg> = split_cat_sounds(&text)
                .into_iter()
                .map(|s| match s {
                    Seg::Sound { kind, word } if !sfx.contains_key(kind) => Seg::Speech(word),
                    other => other,
                })
                .collect();
            if segs.is_empty() {
                synth_hub.emit(Out::Say { text });
                synth_hub.pending_speech.fetch_sub(1, Ordering::Relaxed);
                continue;
            }
            let last = segs.len() - 1;
            for (i, seg) in segs.into_iter().enumerate() {
                let mut max_ms = None;
                let (path, temp, mouth) = match seg {
                    Seg::Sound { kind, .. } => {
                        picks += 1;
                        let files = &sfx[kind];
                        if kind == "purr" {
                            max_ms = Some(1400);
                        }
                        (Some(files[picks % files.len()].clone()), false, kind != "purr")
                    }
                    Seg::Speech(words) => {
                        n += 1;
                        let path = synth_dir.join(format!("{n}.wav"));
                        let line = format!("{}\t{}\n", path.display(), words.replace(['\t', '\n'], " "));
                        let ok = stdin.write_all(line.as_bytes()).await.is_ok()
                            && matches!(lines.next_line().await, Ok(Some(r)) if r.trim() == "ok");
                        synth_hub.lap("synthesised");
                        (ok.then_some(path), true, true)
                    }
                };
                let item = Item { caption: (i == 0).then(|| text.clone()), path, mouth, max_ms, temp, done: i == last, ambient: false };
                if item_tx.send(item).await.is_err() {
                    return;
                }
            }
        }
    });

    // Player.
    tokio::spawn(async move {
        while let Some(item) = item_rx.recv().await {
            play(&hub, &item).await;
            if item.temp {
                if let Some(p) = &item.path {
                    let _ = std::fs::remove_file(p);
                }
            }
            hub.audio_touch();
            hub.mark_spoke();
            if item.ambient {
                hub.ambient.fetch_sub(1, Ordering::Relaxed);
            } else if item.done {
                hub.pending_speech.fetch_sub(1, Ordering::Relaxed);
            }
        }
    });
    true
}

async fn play(hub: &Arc<Hub>, item: &Item) {
    let caption = |delay: Duration| {
        if let Some(text) = item.caption.clone() {
            let hub = hub.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                hub.emit(Out::Say { text });
            });
        }
    };
    let Some(path) = &item.path else {
        caption(Duration::ZERO);
        return;
    };
    let Some((rate, mut samples)) = std::fs::read(path).ok().and_then(|b| parse_wav(&b)) else {
        caption(Duration::ZERO);
        return;
    };
    if let Some(ms) = item.max_ms {
        let keep = rate as usize * ms / 1000;
        if samples.len() > keep {
            samples.truncate(keep);
            let fade = (rate as usize * 200 / 1000).min(samples.len());
            let from = samples.len() - fade;
            for (i, s) in samples[from..].iter_mut().enumerate() {
                *s = (*s as f32 * (1.0 - i as f32 / fade as f32)) as i16;
            }
        }
    }
    // A sleeping sound card eats the start of a sound and a closing stream can swallow its end,
    // so every sound gets a little silence around it (more after a long idle, if the card can sleep).
    let first = item.caption.is_some() || item.ambient || item.done;
    let last = item.ambient || item.done;
    let cold = hub.audio_cold();
    let lead_ms = if cold { COLD_PREROLL_MS } else if first { LEAD_MS } else { 0 };
    let tail_ms = if last { TAIL_MS } else { JOIN_TAIL_MS };
    samples = pad(&samples, rate, lead_ms, tail_ms);
    let lead = Duration::from_millis(lead_ms as u64);
    // Sound effects are shared files: play a padded copy, never edit the original.
    let target = if item.temp { path.clone() } else { std::env::temp_dir().join("purrnel-padded.wav") };
    if std::fs::write(&target, wav_bytes(rate, &samples)).is_err() {
        return;
    }
    play_file(hub, &target, rate, &samples, item.mouth, caption, lead).await;
    if !item.temp {
        let _ = std::fs::remove_file(&target);
    }
}

/// `samples` with `lead_ms` of silence before and `tail_ms` after.
fn pad(samples: &[i16], rate: u32, lead_ms: usize, tail_ms: usize) -> Vec<i16> {
    let (lead, tail) = (rate as usize * lead_ms / 1000, rate as usize * tail_ms / 1000);
    let mut out = Vec::with_capacity(lead + samples.len() + tail);
    out.resize(lead, 0);
    out.extend_from_slice(samples);
    out.resize(lead + samples.len() + tail, 0);
    out
}

/// A silent stream that never ends keeps the sound card awake. A card that suspends when idle
/// clips the start of the next sound, and on a digital link (S/PDIF, HDMI) the amplifier or TV
/// loses the signal each time the stream stops and clips both ends. Restarts itself if it dies.
fn keep_alive(hub: &Arc<Hub>) -> bool {
    let spawn = || {
        Command::new("paplay")
            .args(["--raw", "--rate=22050", "--channels=1", "--format=s16le", "/dev/zero"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
    };
    let Ok(first) = spawn() else { return false };
    hub.set_keepalive(true);
    let hub = hub.clone();
    tokio::spawn(async move {
        let mut child = first;
        loop {
            let _ = child.wait().await;
            hub.set_keepalive(false);
            tokio::time::sleep(Duration::from_secs(2)).await;
            match spawn() {
                Ok(c) => {
                    child = c;
                    hub.set_keepalive(true);
                }
                Err(_) => return,
            }
        }
    });
    true
}

async fn play_file(
    hub: &Arc<Hub>,
    path: &Path,
    rate: u32,
    samples: &[i16],
    mouth: bool,
    caption: impl Fn(Duration),
    lead: Duration,
) {
    let envelope = if mouth { envelope(rate, samples) } else { Vec::new() };
    eprintln!("purrnel: playing {}", path.file_name().and_then(|n| n.to_str()).unwrap_or("?"));
    caption(lead);
    hub.lap("speaking");
    hub.audio_touch();
    let started = Instant::now();
    let player = Command::new("paplay")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    if let Ok(mut p) = player {
        let mut tick = tokio::time::interval(Duration::from_millis(33));
        loop {
            tokio::select! {
                _ = p.wait() => break,
                _ = tick.tick() => {
                    let i = (started.elapsed().as_millis() / 33) as usize;
                    if mouth {
                        hub.emit(Out::Mouth { level: envelope.get(i).copied().unwrap_or(0.0) });
                    }
                }
            }
        }
    }
    hub.emit(Out::Mouth { level: 0.0 });
}

/// `assets/sfx/meow1.wav`, `meow2.wav`, `purr1.wav`... grouped by the name before the digits.
fn load_sfx(dir: &Path) -> HashMap<&'static str, Vec<PathBuf>> {
    let mut out: HashMap<&'static str, Vec<PathBuf>> = HashMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    files.sort();
    for p in files {
        if p.extension().and_then(|e| e.to_str()) != Some("wav") {
            continue;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let name = stem.trim_end_matches(|c: char| c.is_ascii_digit());
        if let Some(kind) = ["meow", "mew", "mrrp", "purr", "chatter", "hiss", "yelp"].into_iter().find(|k| *k == name) {
            out.entry(kind).or_default().push(p);
        }
    }
    out
}

/// Which cat noise, if any, is this word? Handles stretched spellings ("mrrrow", "purrrr").
pub fn cat_sound(word: &str) -> Option<&'static str> {
    let mut d = String::new();
    for c in word.to_lowercase().chars() {
        if d.chars().last() != Some(c) {
            d.push(c);
        }
    }
    match d.as_str() {
        "mrow" | "meow" | "miaow" | "miau" | "meowr" | "mrowr" => Some("meow"),
        "mew" | "mewl" | "mow" => Some("mew"),
        "mrp" | "mr" | "mrrp" | "mrup" => Some("mrrp"),
        "pur" | "pr" | "prr" => Some("purr"),
        _ => None,
    }
}

/// Splits a sentence into spoken text and cat noises.
pub fn split_cat_sounds(text: &str) -> Vec<Seg> {
    fn flush(buf: &mut String, out: &mut Vec<Seg>) {
        // Leading punctuation belonged to the cat noise before it, e.g. "Mrow! Hello".
        let t = buf.trim().trim_start_matches(|c: char| !c.is_alphanumeric());
        if t.chars().any(|c| c.is_alphanumeric()) {
            out.push(Seg::Speech(t.to_string()));
        }
        buf.clear();
    }
    let (mut out, mut buf, mut word) = (Vec::new(), String::new(), String::new());
    let end_word = |word: &mut String, buf: &mut String, out: &mut Vec<Seg>| {
        if word.is_empty() {
            return;
        }
        match cat_sound(word) {
            Some(kind) => {
                flush(buf, out);
                out.push(Seg::Sound { kind, word: word.clone() });
            }
            None => buf.push_str(word),
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphabetic() {
            word.push(c);
        } else {
            end_word(&mut word, &mut buf, &mut out);
            buf.push(c);
        }
    }
    end_word(&mut word, &mut buf, &mut out);
    flush(&mut buf, &mut out);
    // "Purr purr purr" is one purr, not three.
    out.dedup_by(|b, a| matches!((&*a, &*b), (Seg::Sound { kind: k1, .. }, Seg::Sound { kind: k2, .. }) if k1 == k2));
    out
}

/// 44-byte PCM WAV header + 16-bit mono samples.
pub fn wav_bytes(rate: u32, pcm: &[i16]) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

/// Mouth openness (0..1) per 33 ms window of a 16-bit mono WAV.
#[cfg(test)]
pub fn mouth_envelope(wav: &[u8]) -> Vec<f32> {
    parse_wav(wav).map(|(rate, s)| envelope(rate, &s)).unwrap_or_default()
}

fn envelope(rate: u32, samples: &[i16]) -> Vec<f32> {
    let win = (rate as usize / 30).max(1);
    samples
        .chunks(win)
        .map(|c| {
            let ms = c.iter().map(|&s| (s as f32).powi(2)).sum::<f32>() / c.len() as f32;
            (ms.sqrt() / 5000.0).clamp(0.0, 1.0)
        })
        .collect()
}

fn parse_wav(b: &[u8]) -> Option<(u32, Vec<i16>)> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut rate = 22050;
    let mut pos = 12;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let len = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 8 <= b.len() {
            rate = u32::from_le_bytes(b[body + 4..body + 8].try_into().ok()?);
        } else if id == b"data" {
            let end = (body + len).min(b.len());
            let samples = b[body..end]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            return Some((rate, samples));
        }
        pos = body + len + (len & 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_adds_silence_around_a_sound() {
        let out = pad(&[5, 6, 7], 1000, 20, 30);
        assert_eq!(out.len(), 20 + 3 + 30);
        assert!(out[..20].iter().all(|s| *s == 0));
        assert_eq!(&out[20..23], &[5, 6, 7]);
        assert!(out[23..].iter().all(|s| *s == 0));
        assert_eq!(pad(&[1], 1000, 0, 0), vec![1]);
    }

    #[test]
    fn envelope_follows_loudness() {
        let mut pcm = vec![0i16; 1600];
        pcm.extend(std::iter::repeat_n(8000i16, 1600));
        let env = mouth_envelope(&wav_bytes(16000, &pcm));
        assert!(env.len() >= 6);
        assert!(env[0] < 0.01);
        assert!(*env.last().unwrap() > 0.9);
    }

    #[test]
    fn wav_round_trip() {
        let (rate, s) = parse_wav(&wav_bytes(22050, &[1, -2, 3])).unwrap();
        assert_eq!((rate, s), (22050, vec![1, -2, 3]));
    }

    #[test]
    fn garbage_is_not_a_wav() {
        assert!(mouth_envelope(b"nope").is_empty());
    }

    #[test]
    fn recognises_stretched_cat_noises_only() {
        for w in ["mrow", "Mrrrow", "MEOW", "meeeow", "mew", "mrrp", "purr", "purrrr"] {
            assert!(cat_sound(w).is_some(), "{w}");
        }
        for w in ["purrs", "meal", "more", "pure", "meet", "mop", "hello"] {
            assert!(cat_sound(w).is_none(), "{w}");
        }
    }

    #[test]
    fn repeated_noises_collapse_to_one() {
        assert_eq!(
            split_cat_sounds("Purr purr purr, you are nice."),
            [
                Seg::Sound { kind: "purr", word: "Purr".into() },
                Seg::Speech("you are nice.".into())
            ]
        );
        // Different noises stay separate.
        assert_eq!(split_cat_sounds("Mrow purr").len(), 2);
    }

    #[test]
    fn splits_speech_around_cat_noises() {
        assert_eq!(
            split_cat_sounds("Mrow! Hello there."),
            [
                Seg::Sound { kind: "meow", word: "Mrow".into() },
                Seg::Speech("Hello there.".into())
            ]
        );
        assert_eq!(
            split_cat_sounds("Okay, purr, I will."),
            [
                Seg::Speech("Okay,".into()),
                Seg::Sound { kind: "purr", word: "purr".into() },
                Seg::Speech("I will.".into())
            ]
        );
        assert_eq!(split_cat_sounds("Hello friend."), [Seg::Speech("Hello friend.".into())]);
        assert!(split_cat_sounds("...").is_empty());
    }
}
