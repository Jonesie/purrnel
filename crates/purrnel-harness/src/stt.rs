//! Local speech-to-text: supervises a `whisper-server` (whisper.cpp) child and posts audio to it.

use crate::config::Config;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};

pub struct Stt {
    client: reqwest::Client,
    url: String,
    _child: Child,
}

impl Stt {
    /// Starts whisper-server. Returns None (with a message) if it cannot be started,
    /// in which case the cat still works by keyboard.
    pub async fn start(cfg: &Config) -> Option<Stt> {
        let Some(bin) = &cfg.whisper_bin else {
            eprintln!("purrnel: whisper-server not found (set PURRNEL_WHISPER_BIN); voice input off");
            return None;
        };
        if !cfg.whisper_model.is_file() {
            eprintln!("purrnel: whisper model missing at {}; voice input off", cfg.whisper_model.display());
            return None;
        }
        let mut child = Command::new(bin)
            .arg("-m")
            .arg(&cfg.whisper_model)
            .args(["--host", "127.0.0.1", "--port", &cfg.whisper_port.to_string()])
            .args(["-t", "8", "-l", "en", "--no-timestamps"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| eprintln!("purrnel: cannot start whisper-server: {e}"))
            .ok()?;
        let url = format!("http://127.0.0.1:{}", cfg.whisper_port);
        let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().ok()?;
        for _ in 0..100 {
            if let Ok(Some(status)) = child.try_wait() {
                eprintln!("purrnel: whisper-server exited early ({status}); is port {} taken? voice input off", cfg.whisper_port);
                return None;
            }
            if client.get(&url).send().await.is_ok() {
                eprintln!("purrnel: speech recognition ready");
                return Some(Stt { client, url, _child: child });
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        eprintln!("purrnel: whisper-server did not come up; voice input off");
        None
    }

    /// `pcm` is 16 kHz mono s16. Returns cleaned text, or None if nothing usable was heard.
    pub async fn transcribe(&self, pcm: &[i16]) -> Option<String> {
        let part = reqwest::multipart::Part::bytes(wav16k(pcm))
            .file_name("speech.wav")
            .mime_str("audio/wav")
            .ok()?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "json")
            .text("temperature", "0.0");
        let resp = self
            .client
            .post(format!("{}/inference", self.url))
            .multipart(form)
            .send()
            .await
            .map_err(|e| eprintln!("purrnel: stt request failed: {e}"))
            .ok()?;
        let body: serde_json::Value = resp.json().await.ok()?;
        clean_transcript(body.get("text")?.as_str()?)
    }
}

/// Drops whisper's non-speech annotations like "[BLANK_AUDIO]" and "(music)".
pub fn clean_transcript(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut depth = 0u32;
    for c in raw.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            '*' => {}
            _ => out.push(c),
        }
    }
    let text = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let letters = text.chars().filter(|c| c.is_alphanumeric()).count();
    (letters >= 2).then_some(text)
}

/// 44-byte PCM WAV header + samples.
pub fn wav16k(pcm: &[i16]) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&16000u32.to_le_bytes());
    v.extend_from_slice(&32000u32.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_annotations_and_noise() {
        assert_eq!(clean_transcript(" Hello there.\n").as_deref(), Some("Hello there."));
        assert_eq!(clean_transcript("[BLANK_AUDIO]"), None);
        assert_eq!(clean_transcript(" (music) "), None);
        assert_eq!(clean_transcript("Hi [laughs] cat (nice)"), Some("Hi cat".into()));
        assert_eq!(clean_transcript("."), None);
    }

    #[test]
    fn wav_header_is_valid() {
        let w = wav16k(&[0, 1, -1]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
    }
}
