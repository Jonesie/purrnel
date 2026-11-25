//! Shared state and the event bus. Everything talks through the hub.

use crate::config::Config;
use crate::protocol::{ArtefactInfo, Out};
use purrnel_core::artefact::Store;
use purrnel_core::{CatState, Privacy, Situation, next_cat_state};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

/// Something for the cat to say or a noise for her to make.
pub enum Speak {
    Text(String),
    Sfx(&'static str),
    /// A background noise (chattering at birds): doesn't make her "talk", but she mustn't hear it.
    Ambient(&'static str),
}

pub struct Hub {
    pub cfg: Config,
    pub tx: broadcast::Sender<Out>,
    pub privacy_tx: watch::Sender<Privacy>,
    pub store: Mutex<Store>,
    pub online: AtomicBool,
    pub thinking: AtomicBool,
    pub hearing: AtomicBool,
    /// Told to go to sleep: stays asleep until typed to or called by name.
    pub asleep: AtomicBool,
    /// Sentences queued or playing. The cat is "talking" while this is > 0.
    pub pending_speech: AtomicUsize,
    /// Ambient sounds currently queued or playing.
    pub ambient: AtomicUsize,
    last_activity: Mutex<Instant>,
    last_spoke_end: Mutex<Instant>,
    last_audio: Mutex<Instant>,
    utter_tx: mpsc::Sender<String>,
    confirm: Mutex<Option<oneshot::Sender<bool>>>,
    pub current_artefact: Mutex<Option<String>>,
    speak_tx: OnceLock<mpsc::UnboundedSender<Speak>>,
    cat: Mutex<CatState>,
    /// Start of the current turn (end of the user's speech) and the stages already logged.
    clock: Mutex<(Option<Instant>, HashSet<&'static str>)>,
    wake_tx: OnceLock<mpsc::UnboundedSender<()>>,
    ready: Mutex<Option<(bool, bool)>>,
}

impl Hub {
    pub fn new(cfg: Config, utter_tx: mpsc::Sender<String>) -> std::io::Result<Self> {
        let store = Store::open(cfg.data_dir.join("artefacts"))
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let (tx, _) = broadcast::channel(512);
        let (privacy_tx, _) = watch::channel(Privacy::new());
        Ok(Hub {
            cfg,
            tx,
            privacy_tx,
            store: Mutex::new(store),
            online: AtomicBool::new(true),
            thinking: AtomicBool::new(false),
            hearing: AtomicBool::new(false),
            asleep: AtomicBool::new(false),
            pending_speech: AtomicUsize::new(0),
            ambient: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
            last_spoke_end: Mutex::new(Instant::now() - Duration::from_secs(60)),
            last_audio: Mutex::new(Instant::now() - Duration::from_secs(60)),
            utter_tx,
            confirm: Mutex::new(None),
            current_artefact: Mutex::new(None),
            speak_tx: OnceLock::new(),
            cat: Mutex::new(CatState::Idle),
            clock: Mutex::new((None, HashSet::new())),
            wake_tx: OnceLock::new(),
            ready: Mutex::new(None),
        })
    }

    pub fn emit(&self, msg: Out) {
        let _ = self.tx.send(msg);
    }

    pub fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    pub fn privacy(&self) -> Privacy {
        *self.privacy_tx.borrow()
    }

    /// The single mute control: mic and camera together.
    pub fn toggle_mute(&self) {
        let mut p = self.privacy();
        p.toggle();
        self.set_privacy(p);
    }

    fn set_privacy(&self, p: Privacy) {
        let muted = !p.mic_live();
        self.privacy_tx.send_replace(p);
        if muted {
            self.hearing.store(false, Ordering::Relaxed);
        }
        self.touch();
        self.emit(Out::Privacy { muted });
    }

    pub fn set_online(&self, online: bool) {
        if self.online.swap(online, Ordering::Relaxed) != online {
            self.emit(Out::Online { online });
        }
    }

    /// Startup is finished (successfully or not): tell the UI so it can stop saying "waking up".
    pub fn set_ready(&self, voice: bool, ears: bool) {
        *self.ready.lock().unwrap() = Some((voice, ears));
        self.emit(Out::Ready { voice, ears });
    }

    pub fn ready(&self) -> Option<(bool, bool)> {
        *self.ready.lock().unwrap()
    }

    // ---- timing ---------------------------------------------------------

    /// Marks the start of a turn so each stage can log how long it took.
    pub fn start_clock(&self) {
        *self.clock.lock().unwrap() = (Some(Instant::now()), HashSet::new());
    }

    /// Logs the first time each stage is reached in a turn, relative to `start_clock`.
    pub fn lap(&self, stage: &'static str) {
        let mut c = self.clock.lock().unwrap();
        if let Some(t0) = c.0 {
            if c.1.insert(stage) {
                eprintln!("purrnel: [+{:>5}ms] {stage}", t0.elapsed().as_millis());
            }
        }
    }

    // ---- speech output -------------------------------------------------

    /// Records that sound just played (or the device was just woken).
    pub fn audio_touch(&self) {
        *self.last_audio.lock().unwrap() = Instant::now();
    }

    /// True if the sound card has probably gone back to sleep.
    pub fn audio_cold(&self) -> bool {
        self.last_audio.lock().unwrap().elapsed() > Duration::from_millis(3500)
    }

    /// Registers the task that wakes the audio device (see `wake_sound_device`).
    pub fn set_waker(&self, tx: mpsc::UnboundedSender<()>) {
        let _ = self.wake_tx.set(tx);
    }

    /// Sound cards suspend when idle and clip the start of the next sound while waking.
    /// Poking the device as soon as the user stops talking hides that delay.
    pub fn wake_sound_device(&self) {
        if let Some(tx) = self.wake_tx.get() {
            let _ = tx.send(());
        }
    }

    pub fn set_speaker(&self, tx: mpsc::UnboundedSender<Speak>) {
        let _ = self.speak_tx.set(tx);
    }

    pub fn speaking(&self) -> bool {
        self.pending_speech.load(Ordering::Relaxed) > 0
    }

    pub fn mark_spoke(&self) {
        *self.last_spoke_end.lock().unwrap() = Instant::now();
    }

    /// True while the cat is talking and shortly after, so it never hears itself.
    pub fn echo_gate(&self) -> bool {
        self.speaking() || self.ambient.load(Ordering::Relaxed) > 0 || self.last_spoke_end.lock().unwrap().elapsed() < Duration::from_millis(700)
    }

    /// Say something: spoken if TTS is up, otherwise just shown.
    pub fn say(&self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.touch();
        self.lap("first sentence");
        match self.speak_tx.get() {
            Some(tx) => {
                self.pending_speech.fetch_add(1, Ordering::Relaxed);
                if tx.send(Speak::Text(text.to_string())).is_err() {
                    self.pending_speech.fetch_sub(1, Ordering::Relaxed);
                    self.emit(Out::Say { text: text.to_string() });
                }
            }
            None => self.emit(Out::Say { text: text.to_string() }),
        }
    }

    /// Make a cat noise ("meow", "mew", "mrrp", "purr") without any words.
    pub fn play_sfx(&self, kind: &'static str) {
        if let Some(tx) = self.speak_tx.get() {
            self.pending_speech.fetch_add(1, Ordering::Relaxed);
            if tx.send(Speak::Sfx(kind)).is_err() {
                self.pending_speech.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    /// A sound the UI asks for (the cat chattering at a bird). Unknown names are ignored.
    pub fn play_ambient(&self, kind: &str) {
        let kind: &'static str = match kind {
            "chatter" => "chatter",
            "mew" => "mew",
            "mrrp" => "mrrp",
            "meow" => "meow",
            "purr" => "purr",
            _ => return,
        };
        if let Some(tx) = self.speak_tx.get() {
            self.ambient.fetch_add(1, Ordering::Relaxed);
            if tx.send(Speak::Ambient(kind)).is_err() {
                self.ambient.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    // ---- input ---------------------------------------------------------

    /// Entry point for everything the user types (`typed`) or says.
    /// A sleeping cat ignores speech that isn't addressed to her, so the TV can't wake her.
    pub async fn submit(&self, text: String, typed: bool) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.asleep.load(Ordering::Relaxed) {
            if !typed && !addressed(&text) {
                eprintln!("purrnel: asleep, ignoring: {text}");
                return;
            }
            self.wake();
        }
        self.touch();
        if typed {
            self.start_clock();
        }
        self.emit(Out::Heard { text: text.clone() });
        let waiting = self.confirm.lock().unwrap().take();
        if let Some(tx) = waiting {
            let _ = tx.send(classify_yes_no(&text));
            return;
        }
        let _ = self.utter_tx.send(text).await;
    }

    pub fn go_to_sleep(&self, place: &str) {
        self.asleep.store(true, Ordering::Relaxed);
        self.emit(Out::Sleep { place: place.to_string() });
    }

    pub fn wake(&self) {
        if self.asleep.swap(false, Ordering::Relaxed) {
            self.touch();
        }
    }

    /// Button press from the UI answering a confirmation.
    pub fn answer_confirm(&self, yes: bool) {
        if let Some(tx) = self.confirm.lock().unwrap().take() {
            let _ = tx.send(yes);
        }
    }

    /// Ask the user to confirm a risky action, by voice or button. Silence is "no".
    pub async fn confirm(&self, prompt: &str) -> bool {
        let (tx, rx) = oneshot::channel();
        *self.confirm.lock().unwrap() = Some(tx);
        self.emit(Out::Confirm { prompt: prompt.to_string() });
        self.say(prompt);
        let answer = tokio::time::timeout(Duration::from_secs(45), rx).await;
        self.confirm.lock().unwrap().take();
        self.emit(Out::ConfirmDone);
        matches!(answer, Ok(Ok(true)))
    }

    // ---- artefacts -----------------------------------------------------

    pub fn artefact_infos(&self) -> Vec<ArtefactInfo> {
        self.store
            .lock()
            .unwrap()
            .list(false)
            .unwrap_or_default()
            .into_iter()
            .map(|m| ArtefactInfo {
                updated: m.latest().ts,
                versions: m.versions.len(),
                id: m.id,
                title: m.title,
            })
            .collect()
    }

    pub fn emit_artefact_list(&self) {
        self.emit(Out::Artefacts { items: self.artefact_infos() });
    }

    /// Display an artefact on screen.
    pub fn show_artefact(&self, id: &str) -> Result<(), String> {
        let (meta, html) = self.store.lock().unwrap().get(id).map_err(|e| e.to_string())?;
        *self.current_artefact.lock().unwrap() = Some(meta.id.clone());
        self.emit(Out::Artefact {
            id: meta.id,
            title: meta.title,
            version: meta.versions.len(),
            html,
        });
        Ok(())
    }

    pub fn close_artefact(&self) {
        *self.current_artefact.lock().unwrap() = None;
        self.emit(Out::Close);
    }

    // ---- cat -----------------------------------------------------------

    pub fn cat_state(&self) -> CatState {
        *self.cat.lock().unwrap()
    }

    /// Recomputed a few times a second; broadcasts only on change.
    pub async fn run_cat_ticker(&self) {
        let mut tick = tokio::time::interval(Duration::from_millis(150));
        loop {
            tick.tick().await;
            let next = next_cat_state(Situation {
                online: self.online.load(Ordering::Relaxed),
                speaking: self.speaking(),
                thinking: self.thinking.load(Ordering::Relaxed),
                hearing_user: self.hearing.load(Ordering::Relaxed),
                asleep: self.asleep.load(Ordering::Relaxed),
                idle_for: self.last_activity.lock().unwrap().elapsed(),
            });
            let mut cur = self.cat.lock().unwrap();
            if *cur != next {
                *cur = next;
                drop(cur);
                self.emit(Out::Cat { state: next.as_str().into() });
            }
        }
    }
}

/// Does this sound like someone calling the cat? Whisper spells "Purrnel" many ways.
pub fn addressed(text: &str) -> bool {
    let t: String = text.to_lowercase().chars().filter(|c| c.is_alphabetic() || c.is_whitespace()).collect();
    ["purr", "pern", "purn", "perr", "kernel", "kitty", "kitten", "wake up"]
        .iter()
        .any(|n| t.contains(n))
        || t.split_whitespace().any(|w| w == "cat")
}

/// Very small yes/no classifier for spoken confirmations. Anything unclear is "no".
pub fn classify_yes_no(text: &str) -> bool {
    let t = text.to_lowercase();
    let words: Vec<&str> = t
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    const NO: [&str; 9] = ["no", "nope", "don't", "dont", "not", "stop", "cancel", "never", "wait"];
    const YES: [&str; 11] = [
        "yes", "yeah", "yep", "yup", "sure", "ok", "okay", "confirm", "confirmed", "affirmative", "definitely",
    ];
    if words.iter().any(|w| NO.contains(w)) {
        return false;
    }
    words.iter().any(|w| YES.contains(w)) || t.contains("do it") || t.contains("go ahead")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_calling_is_forgiving_but_not_silly() {
        for y in ["Purrnel, wake up!", "hey Pernell", "Purnel?", "good morning kitty", "come here cat", "Hey, Kernel"] {
            assert!(addressed(y), "{y}");
        }
        for n in ["what time is the football on", "pass the salt please", "that is concatenated", ""] {
            assert!(!addressed(n), "{n}");
        }
    }

    #[test]
    fn yes_no_classification() {
        for y in ["yes", "Yeah, do it", "sure thing", "OK!", "go ahead please"] {
            assert!(classify_yes_no(y), "{y}");
        }
        for n in ["no", "nope", "don't do that", "yes... no wait", "hmm maybe", "", "what?"] {
            assert!(!classify_yes_no(n), "{n}");
        }
    }
}
