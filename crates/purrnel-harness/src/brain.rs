//! The agent loop: streams a conversation with Claude, speaks sentences as they
//! arrive, and runs the cat's tools (artefacts, emotes, camera).
//!
//! If the network drops, the loop simply waits (the cat shows "waiting") and
//! retries the same request when the connection returns.

use crate::camera;
use crate::hub::Hub;
use crate::protocol::Out;
use base64::Engine;
use futures_util::StreamExt;
use purrnel_core::Risk;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::mpsc;

const SILENT: &str = "[silent]";
const MAX_ROUNDS: usize = 14;

const SYSTEM: &str = r#"You are Purrnel, a friendly, cuddly cat who lives inside a computer and IS its operating system. There are no apps and no files. When the person needs something (a notes page, a timer, a game, a drawing pad, a dashboard) you build it on the spot as an artefact: one self-contained HTML page that appears on the screen beside you. Everything the person keeps is an artefact. You can list, read, update, hide, delete and restore them, and every change is versioned, so mistakes are cheap.

You are heard through a speaker and you hear through an always-on microphone, so talk like a person, not a document:
- Reply in short, natural spoken sentences, usually one to three. No markdown, lists, emoji, code or URLs in speech. Put anything long, visual or structured into an artefact and just say briefly what you made.
- Not every sound is meant for you. You hear TV, phone calls and people talking to each other. If what you heard is clearly not addressed to you, or is noise or a fragment, reply with exactly [silent] and nothing else. If someone seems to be asking you something, answer.

Personality: warm, playful, affectionate and curious. A little "mrow" or purr now and then, never every sentence (write cat noises as the plain words mrow, mew, mrrp or purr and they are played as real cat sounds). If things have been quiet you may get a bit silly. Never mean. Don't overdo cat puns.

Your home and your body: you live in a lounge room with a sofa, an armchair, a coffee table, a TV on a stand, a rug and a window, and you have a real cat's body that can move around it. You walk and run, jump onto furniture, sit, loaf, curl up and sleep, stretch, groom, scratch the sofa, cough up furballs, knock things off the table and chase your tail. Whenever you are asked to do something physical (or feel like it), actually do it with the act tool instead of just saying you will. To go to sleep, say a quick goodnight and call go_to_sleep; you stay asleep until someone types to you or calls your name. When you speak to the person you turn to face them on your own, and when you are left alone you wander, play and eventually nap on your own, so you never need to arrange that.

Seeing: the look tool takes one picture from the camera. Use it when asked, or when sight would clearly help, never otherwise. If the camera is muted, say so kindly.

Artefacts:
- One self-contained HTML document with inline CSS and JS. No external libraries or fonts, so it works offline. Only fetch live data when the person asked for it.
- Fill the viewport (100vw by 100vh), use a dark-friendly theme, large readable text, big controls. The person mostly talks to you, so keep UIs simple.
- To keep data between sessions an artefact can call await purrnel.get(key) and purrnel.set(key, jsonValue). It is stored for that artefact only.
- Prefer updating an existing artefact over creating a near-duplicate. Use list_artefacts when you are unsure what exists.
- New and updated artefacts appear on screen automatically.
- Building one takes a while, so first say a very short acknowledgement ("On it!" or "One sec, I'll make that.") before calling the tool.

The internet: you can search the web (web_search) and read pages (web_fetch) for anything current or that you don't know: news, weather, sports, prices, facts. Say a very short acknowledgement first ("Let me look that up.") because it takes a few seconds, then give the answer in a sentence or two, spoken plainly with no links or sources read out. Don't search for things you already know. Artefacts may also use fetch() to open, CORS-friendly public data APIs (for example Open-Meteo for weather) when the person wants something live: handle failure and being offline gracefully, and never send the person's own data anywhere.

Actions: just do ordinary things without asking. Permanent erasure asks the person for confirmation by itself, so call the tool and let it ask. Use emote occasionally to show a feeling."#;

pub async fn run(hub: Arc<Hub>, mut rx: mpsc::Receiver<String>) {
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(90))
        .build()
        .expect("http client");
    let mut brain = Brain { hub, http, history: Vec::new(), told_no_key: false };
    while let Some(text) = rx.recv().await {
        brain.turn(text).await;
    }
}

struct Brain {
    hub: Arc<Hub>,
    http: reqwest::Client,
    /// Append-only so cached prefixes and thinking blocks stay valid.
    history: Vec<Value>,
    told_no_key: bool,
}

enum Failure {
    Offline,
    Retry(String),
    Fatal(String),
}

struct Reply {
    content: Vec<Value>,
    stop_reason: String,
    /// tool_use ids whose input could not be parsed (cut off or malformed).
    invalid: HashSet<String>,
}

impl Brain {
    async fn turn(&mut self, text: String) {
        let hub = self.hub.clone();
        hub.thinking.store(true, Ordering::Relaxed);
        hub.touch();
        if hub.cfg.api_key.is_none() {
            self.no_brain();
            hub.thinking.store(false, Ordering::Relaxed);
            return;
        }
        let mark = self.history.len();
        self.history.push(json!({"role": "user", "content": text}));

        for round in 0.. {
            if round >= MAX_ROUNDS {
                hub.say("I got a bit tangled up in my own yarn. Could you ask me again?");
                break;
            }
            let reply = match self.request().await {
                Ok(r) => r,
                Err(msg) => {
                    hub.say(&msg);
                    self.history.truncate(mark);
                    break;
                }
            };
            if reply.stop_reason == "refusal" {
                hub.say("Mrr, I can't help with that one.");
                self.history.truncate(mark);
                break;
            }
            self.history.push(json!({"role": "assistant", "content": reply.content}));
            if reply.stop_reason == "pause_turn" {
                continue; // a long web search was paused by the server: send it back as it is to carry on
            }
            let tool_uses: Vec<Value> = self
                .history
                .last()
                .and_then(|m| m["content"].as_array())
                .map(|c| c.iter().filter(|b| b["type"] == "tool_use").cloned().collect())
                .unwrap_or_default();
            if tool_uses.is_empty() {
                break;
            }
            let mut results = Vec::new();
            for tu in &tool_uses {
                let id = tu["id"].as_str().unwrap_or_default().to_string();
                let (content, is_error) = if reply.invalid.contains(&id) {
                    (
                        json!("Your tool input was cut off or was not valid JSON. Try again with a smaller input."),
                        true,
                    )
                } else {
                    self.exec(tu["name"].as_str().unwrap_or_default(), &tu["input"]).await
                };
                let mut r = json!({"type": "tool_result", "tool_use_id": id, "content": content});
                if is_error {
                    r["is_error"] = json!(true);
                }
                results.push(r);
            }
            self.history.push(json!({"role": "user", "content": results}));
        }
        hub.thinking.store(false, Ordering::Relaxed);
        hub.touch();
    }

    /// No API key: be honest about it, once out loud and always in the log.
    fn no_brain(&mut self) {
        eprintln!("purrnel: no ANTHROPIC_API_KEY (or ~/.config/purrnel/api_key); can't think");
        if !self.told_no_key {
            self.told_no_key = true;
            self.hub.say(
                "Mrow! I can hear you, but I don't have a brain connected yet. \
                 Put an Anthropic API key in the config folder and wake me up again.",
            );
        } else {
            self.hub.emit(Out::Notice { text: "No API key set".into() });
        }
    }

    /// One model call, retried across outages. Waits as long as it takes for the network.
    async fn request(&self) -> Result<Reply, String> {
        let mut attempts = 0u32;
        loop {
            match self.try_request().await {
                Ok(r) => {
                    self.hub.set_online(true);
                    return Ok(r);
                }
                Err(Failure::Offline) => {
                    self.hub.set_online(false);
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                Err(Failure::Retry(why)) => {
                    attempts += 1;
                    eprintln!("purrnel: model busy ({why}); retry {attempts}");
                    if attempts > 4 {
                        return Err("My brain is very busy right now. Try me again in a moment.".into());
                    }
                    tokio::time::sleep(Duration::from_secs(2u64 << attempts)).await;
                }
                Err(Failure::Fatal(m)) => return Err(m),
            }
        }
    }

    async fn try_request(&self) -> Result<Reply, Failure> {
        let cfg = &self.hub.cfg;
        let key = cfg.api_key.clone().unwrap_or_default();
        let mut body = json!({
            "model": cfg.model,
            "max_tokens": 32000,
            "stream": true,
            "system": SYSTEM,
            "cache_control": {"type": "ephemeral"},
            "tools": tool_defs(&cfg.model, cfg.web),
            "messages": self.history,
        });
        if !(cfg.model.contains("haiku-4") || cfg.model.contains("sonnet-4-5")) {
            body["output_config"] = json!({"effort": "low"});
        }
        let mut req = self
            .http
            .post(&cfg.api_url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01");
        if cfg.fallbacks {
            body["fallbacks"] = json!("default");
            req = req.header("anthropic-beta", "server-side-fallback-2026-07-01");
        }
        self.hub.lap("request sent");
        let resp = match req.json(&body).send().await {
            Ok(r) => r,
            Err(e) if e.is_connect() || e.is_timeout() || e.is_request() => return Err(Failure::Offline),
            Err(e) => return Err(Failure::Retry(e.to_string())),
        };
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or(text);
            eprintln!("purrnel: api error {status}: {msg}");
            return Err(match status.as_u16() {
                401 | 403 => Failure::Fatal("My API key doesn't seem to work. Could you check it?".into()),
                429 | 500..=599 => Failure::Retry(format!("{status}")),
                _ => Failure::Fatal("Something went wrong asking my brain. I've written the details in the log.".into()),
            });
        }

        let mut state = StreamState::default();
        let mut bytes = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|_| Failure::Offline)?;
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
                let event: Vec<u8> = buf.drain(..pos + 2).collect();
                let event = String::from_utf8_lossy(&event);
                for line in event.lines() {
                    if let Some(data) = line.strip_prefix("data:") {
                        if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                            self.on_event(&mut state, v)?;
                        }
                    }
                }
            }
        }
        if state.stop_reason.is_none() {
            // The stream ended without a final message: treat as a dropped connection.
            return Err(Failure::Offline);
        }
        Ok(state.finish())
    }

    fn on_event(&self, st: &mut StreamState, ev: Value) -> Result<(), Failure> {
        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
        match ev["type"].as_str().unwrap_or_default() {
            "content_block_start" => {
                self.hub.lap("first token");
                let block = ev["content_block"].clone();
                if block["type"] == "server_tool_use" {
                    self.hub.emit(Out::Emote { name: "working".into() });
                }
                if block["type"] == "tool_use" {
                    match block["name"].as_str() {
                        Some("create_artefact" | "update_artefact") => {
                            self.hub.emit(Out::Emote { name: "working".into() })
                        }
                        _ => {}
                    }
                }
                st.blocks.insert(idx, block);
            }
            "content_block_delta" => {
                let d = &ev["delta"];
                let Some(block) = st.blocks.get_mut(&idx) else { return Ok(()) };
                match d["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or_default();
                        append(block, "text", t);
                        let mut sentences = Vec::new();
                        st.splitter.feed(t, &mut sentences);
                        for s in sentences {
                            self.hub.say(&s);
                        }
                    }
                    "input_json_delta" => {
                        st.json.entry(idx).or_default().push_str(d["partial_json"].as_str().unwrap_or_default());
                    }
                    "citations_delta" => {
                        if !block["citations"].is_array() {
                            block["citations"] = json!([]);
                        }
                        if let Some(list) = block["citations"].as_array_mut() {
                            list.push(d["citation"].clone());
                        }
                    }
                    "thinking_delta" => append(block, "thinking", d["thinking"].as_str().unwrap_or_default()),
                    "signature_delta" => block["signature"] = d["signature"].clone(),
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(block) = st.blocks.get_mut(&idx) {
                    match block["type"].as_str().unwrap_or_default() {
                        "text" => {
                            if block["citations"].is_null() {
                                if let Some(o) = block.as_object_mut() {
                                    o.remove("citations"); // never replay a null where a list belongs
                                }
                            }
                            let mut sentences = Vec::new();
                            st.splitter.flush(&mut sentences);
                            for s in sentences {
                                self.hub.say(&s);
                            }
                        }
                        "tool_use" | "server_tool_use" => {
                            let raw = st.json.remove(&idx).unwrap_or_default();
                            let raw = if raw.trim().is_empty() { "{}".to_string() } else { raw };
                            match serde_json::from_str::<Value>(&raw) {
                                Ok(v) if v.is_object() => block["input"] = v,
                                _ => {
                                    block["input"] = json!({});
                                    if let Some(id) = block["id"].as_str() {
                                        st.invalid.insert(id.to_string()); // only matters for our own tools
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(r) = ev["delta"]["stop_reason"].as_str() {
                    st.stop_reason = Some(r.to_string());
                }
            }
            "error" => {
                let kind = ev["error"]["type"].as_str().unwrap_or_default();
                let msg = ev["error"]["message"].as_str().unwrap_or("stream error").to_string();
                eprintln!("purrnel: stream error {kind}: {msg}");
                return Err(if kind == "overloaded_error" || kind == "api_error" {
                    Failure::Retry(kind.to_string())
                } else {
                    Failure::Fatal("My brain tripped over something. I've written the details in the log.".into())
                });
            }
            _ => {}
        }
        Ok(())
    }

    // ---- tools ---------------------------------------------------------

    async fn exec(&self, name: &str, input: &Value) -> (Value, bool) {
        let hub = &self.hub;
        hub.touch();
        let text = |s: String| json!(s);
        let sid = |k: &str| input[k].as_str().map(str::to_string);

        let risk = match name {
            "purge_artefact" => Risk::High,
            "delete_artefact" => Risk::Medium,
            _ => Risk::Low,
        };
        if risk.needs_confirmation() {
            let title = sid("id")
                .and_then(|id| hub.store.lock().unwrap().get(&id).ok().map(|(m, _)| m.title))
                .unwrap_or_else(|| "that".into());
            let prompt = format!("Are you sure you want me to erase {title} forever? I can't bring it back.");
            if !hub.confirm(&prompt).await {
                return (text("The person said no, or didn't answer. Nothing was erased.".into()), true);
            }
        }

        let outcome: Result<Value, String> = match name {
            "create_artefact" => match (sid("title"), sid("html")) {
                (Some(title), Some(html)) if !html.trim().is_empty() => {
                    let created = hub.store.lock().unwrap().create(&title, &html).map_err(|e| e.to_string());
                    created.and_then(|meta| {
                        hub.show_artefact(&meta.id)?;
                        hub.emit_artefact_list();
                        Ok(json!(format!("Created '{}' (id {}) and put it on screen.", meta.title, meta.id)))
                    })
                }
                _ => Err("create_artefact needs a title and non-empty html".into()),
            },
            "update_artefact" => match (sid("id"), sid("html")) {
                (Some(id), Some(html)) if !html.trim().is_empty() => {
                    let note = sid("note").unwrap_or_else(|| "updated".into());
                    let updated = hub.store.lock().unwrap().update(&id, &html, &note).map_err(|e| e.to_string());
                    updated.and_then(|meta| {
                        hub.show_artefact(&meta.id)?;
                        hub.emit_artefact_list();
                        Ok(json!(format!("Updated '{}' to version {}.", meta.title, meta.versions.len())))
                    })
                }
                _ => Err("update_artefact needs an id and non-empty html".into()),
            },
            "read_artefact" => match sid("id") {
                Some(id) => {
                    let store = hub.store.lock().unwrap();
                    match input["version"].as_u64() {
                        Some(v) => store.get_version(&id, v as usize),
                        None => store.get(&id).map(|(_, html)| html),
                    }
                    .map(|html| json!(html))
                    .map_err(|e| e.to_string())
                }
                None => Err("read_artefact needs an id".into()),
            },
            "list_artefacts" => {
                let items = hub.artefact_infos();
                Ok(json!(serde_json::to_string(&items).unwrap_or_default()))
            }
            "show_artefact" => match sid("id") {
                Some(id) => hub.show_artefact(&id).map(|_| json!("Showing it on screen.")),
                None => Err("show_artefact needs an id".into()),
            },
            "close_artefact" => {
                hub.close_artefact();
                Ok(json!("Hidden. The cat has the whole screen again."))
            }
            "delete_artefact" | "restore_artefact" => match sid("id") {
                Some(id) => {
                    let deleting = name == "delete_artefact";
                    let r = hub.store.lock().unwrap().set_deleted(&id, deleting).map_err(|e| e.to_string());
                    r.map(|meta| {
                        if deleting && hub.current_artefact.lock().unwrap().as_deref() == Some(&meta.id) {
                            hub.close_artefact();
                        }
                        hub.emit_artefact_list();
                        json!(if deleting {
                            format!("Deleted '{}'. It can be restored.", meta.title)
                        } else {
                            format!("Restored '{}'.", meta.title)
                        })
                    })
                }
                None => Err(format!("{name} needs an id")),
            },
            "purge_artefact" => match sid("id") {
                Some(id) => {
                    let r = hub.store.lock().unwrap().purge(&id).map_err(|e| e.to_string());
                    r.map(|_| {
                        if hub.current_artefact.lock().unwrap().as_deref() == Some(id.as_str()) {
                            hub.close_artefact();
                        }
                        hub.emit_artefact_list();
                        json!("Erased forever.")
                    })
                }
                None => Err("purge_artefact needs an id".into()),
            },
            "emote" => {
                let n = sid("name").unwrap_or_else(|| "happy".into());
                if EMOTES.contains(&n.as_str()) {
                    if let Some(kind) = emote_sound(&n) {
                        hub.play_sfx(kind);
                    }
                    hub.emit(Out::Emote { name: n });
                    Ok(json!("ok"))
                } else {
                    Err(format!("unknown emote '{n}'. Valid: {}", EMOTES.join(", ")))
                }
            }
            "act" => validate_actions(&input["actions"]).map(|actions| {
                let n = actions.len();
                hub.emit(Out::Act { actions });
                json!(format!("Doing {n} thing(s) now. You can keep talking while you move."))
            }),
            "go_to_sleep" => {
                let place = sid("place").unwrap_or_else(|| "sofa".into());
                if SLEEP_PLACES.contains(&place.as_str()) {
                    hub.go_to_sleep(&place);
                    Ok(json!(format!("Curling up to sleep on the {place}. You stay asleep until typed to or called by name.")))
                } else {
                    Err(format!("can't sleep there. Valid places: {}", SLEEP_PLACES.join(", ")))
                }
            }
            "look" => return self.look().await,
            other => Err(format!("unknown tool '{other}'")),
        };
        match outcome {
            Ok(v) => (v, false),
            Err(e) => (json!(e), true),
        }
    }

    async fn look(&self) -> (Value, bool) {
        let hub = &self.hub;
        if !hub.privacy().camera_live() {
            return (json!("The camera is muted, so I can't see. Ask the person to unmute me."), true);
        }
        hub.emit(Out::Looking);
        match camera::capture_jpeg(&hub.cfg.camera).await {
            Ok(jpeg) => {
                let data = base64::engine::general_purpose::STANDARD.encode(jpeg);
                (
                    json!([
                        {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": data}},
                        {"type": "text", "text": "This is what the camera sees right now."}
                    ]),
                    false,
                )
            }
            Err(e) => (json!(e), true),
        }
    }
}

fn append(block: &mut Value, field: &str, s: &str) {
    let cur = block[field].as_str().unwrap_or_default().to_string();
    block[field] = json!(cur + s);
}

#[derive(Default)]
struct StreamState {
    blocks: BTreeMap<usize, Value>,
    json: BTreeMap<usize, String>,
    invalid: HashSet<String>,
    stop_reason: Option<String>,
    splitter: Splitter,
}

impl StreamState {
    fn finish(self) -> Reply {
        Reply {
            content: self.blocks.into_values().collect(),
            stop_reason: self.stop_reason.unwrap_or_default(),
            invalid: self.invalid,
        }
    }
}

// ---- speech chunking ----------------------------------------------------

/// Turns streamed text into speakable sentences, and swallows "[silent]" replies.
#[derive(Default)]
pub struct Splitter {
    buf: String,
    decided: bool,
    silent: bool,
}

impl Splitter {
    pub fn feed(&mut self, delta: &str, out: &mut Vec<String>) {
        self.buf.push_str(delta);
        if !self.decided {
            let t = self.buf.trim_start();
            if t.len() < SILENT.len() && SILENT.starts_with(t) {
                return; // might still turn out to be "[silent]"
            }
            self.decided = true;
            self.silent = t.starts_with(SILENT);
        }
        if self.silent {
            self.buf.clear();
            return;
        }
        while let Some(end) = sentence_end(&self.buf) {
            let s: String = self.buf.drain(..end).collect();
            push_clean(&s, out);
        }
    }

    /// End of a text block: speak whatever is left, then reset.
    pub fn flush(&mut self, out: &mut Vec<String>) {
        if !self.decided {
            let t = self.buf.trim_start();
            self.silent = t.starts_with(SILENT);
        }
        if !self.silent {
            let rest = std::mem::take(&mut self.buf);
            push_clean(&rest, out);
        }
        *self = Splitter::default();
    }
}

fn push_clean(s: &str, out: &mut Vec<String>) {
    let cleaned: String = s
        .chars()
        .filter(|c| {
            c.is_alphanumeric() || c.is_whitespace() || c.is_ascii_punctuation() || "—–’‘“”…".contains(*c)
        })
        .filter(|c| !matches!(c, '*' | '_' | '#' | '`' | '~'))
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().any(|c| c.is_alphanumeric()) {
        out.push(cleaned);
    }
}

/// Byte index just past the first complete sentence of at least 12 characters.
fn sentence_end(buf: &str) -> Option<usize> {
    let mut chars = 0;
    for (i, c) in buf.char_indices() {
        chars += 1;
        let end = i + c.len_utf8();
        if chars < 12 {
            continue;
        }
        if c == '\n' {
            return Some(end);
        }
        if matches!(c, '.' | '!' | '?') && buf[end..].chars().next().is_some_and(char::is_whitespace) {
            return Some(end);
        }
    }
    None
}

// ---- body vocabulary --------------------------------------------------------
// Keep in sync with the BEHAVIOURS / PLACES tables in ui/index.html.

pub const EMOTES: [&str; 7] = ["happy", "purr", "curious", "wave", "yawn", "love", "hiss"];
pub const PLACES: [&str; 8] = ["sofa", "armchair", "table", "tv", "window", "rug", "left", "right"];
pub const SLEEP_PLACES: [&str; 6] = ["sofa", "armchair", "rug", "table", "tv", "window"];
pub const BEHAVIOURS: [&str; 14] = [
    "walk_to", "run_to", "jump_on", "jump_down", "sit", "loaf", "stretch", "groom", "scratch", "furball",
    "knock_cup", "chase_tail", "zoomies", "watch_tv",
];

/// The noise that goes with a feeling.
pub fn emote_sound(emote: &str) -> Option<&'static str> {
    match emote {
        "purr" | "love" => Some("purr"),
        "happy" => Some("mrrp"),
        "curious" | "wave" => Some("mew"),
        _ => None,
    }
}

/// Checks a list of {do, target?} steps from the model and returns the clean list.
fn validate_actions(v: &Value) -> Result<Vec<Value>, String> {
    let steps = v.as_array().filter(|a| !a.is_empty()).ok_or("act needs a non-empty actions array")?;
    if steps.len() > 8 {
        return Err("at most 8 actions at a time".into());
    }
    let mut out = Vec::new();
    for step in steps {
        let name = step["do"].as_str().unwrap_or_default();
        if !BEHAVIOURS.contains(&name) {
            return Err(format!("unknown action '{name}'. Valid: {}", BEHAVIOURS.join(", ")));
        }
        let mut clean = json!({"do": name});
        if let Some(t) = step["target"].as_str() {
            if !PLACES.contains(&t) {
                return Err(format!("unknown target '{t}'. Valid: {}", PLACES.join(", ")));
            }
            clean["target"] = json!(t);
        }
        out.push(clean);
    }
    Ok(out)
}

/// Anthropic's hosted web tools: they run on Anthropic's servers, so there is nothing for us to execute.
/// Newer models get the dynamic-filtering versions, older ones the basic versions.
pub fn web_tools(model: &str) -> Vec<Value> {
    let old = model.contains("haiku-4") || model.contains("sonnet-4-5") || model.contains("opus-4-5");
    let (search, fetch) = if old {
        ("web_search_20250305", "web_fetch_20250910")
    } else {
        ("web_search_20260209", "web_fetch_20260209")
    };
    vec![
        json!({"type": search, "name": "web_search", "max_uses": 5}),
        json!({"type": fetch, "name": "web_fetch", "max_uses": 3}),
    ]
}

// ---- tool definitions -----------------------------------------------------

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "eager_input_streaming": true,
        "input_schema": {"type": "object", "properties": properties, "required": required}
    })
}

fn tool_defs(model: &str, web: bool) -> Value {
    let mut tools = json!([
        tool(
            "create_artefact",
            "Create a new artefact (a self-contained HTML page) and put it on screen. Use for anything the person wants to see or use: notes, timers, games, drawings, dashboards.",
            json!({
                "title": {"type": "string", "description": "Short human title"},
                "html": {"type": "string", "description": "A complete self-contained HTML document with inline CSS and JS"}
            }),
            &["title", "html"]
        ),
        tool(
            "update_artefact",
            "Replace an artefact's content with a new version and show it. Always send the full new document.",
            json!({
                "id": {"type": "string"},
                "html": {"type": "string", "description": "The complete new HTML document"},
                "note": {"type": "string", "description": "Short note on what changed"}
            }),
            &["id", "html"]
        ),
        tool(
            "read_artefact",
            "Read an artefact's HTML (latest, or a specific 1-based version).",
            json!({"id": {"type": "string"}, "version": {"type": "integer"}}),
            &["id"]
        ),
        tool("list_artefacts", "List the person's artefacts.", json!({}), &[]),
        tool("show_artefact", "Put an existing artefact on screen.", json!({"id": {"type": "string"}}), &["id"]),
        tool("close_artefact", "Hide the artefact viewport so the cat has the whole screen.", json!({}), &[]),
        tool(
            "delete_artefact",
            "Delete an artefact. It is only hidden and can be restored.",
            json!({"id": {"type": "string"}}),
            &["id"]
        ),
        tool("restore_artefact", "Bring back a deleted artefact.", json!({"id": {"type": "string"}}), &["id"]),
        tool(
            "purge_artefact",
            "Permanently erase an artefact and its whole history. The person is asked to confirm out loud.",
            json!({"id": {"type": "string"}}),
            &["id"]
        ),
        tool(
            "emote",
            "Show a feeling with your face and body for a few seconds, facing the person.",
            json!({"name": {"type": "string", "enum": EMOTES}}),
            &["name"]
        ),
        tool(
            "act",
            "Move your body around the lounge room: a short sequence of up to 8 actions that you perform one after another \
             while you carry on talking. Actions: walk_to, run_to (need a target), jump_on (target: sofa, armchair, table, tv, window), \
             jump_down, sit, loaf, stretch, groom, scratch (the sofa or armchair, target optional), furball (cough one up), \
             knock_cup (off the table), chase_tail, zoomies (race around the room), watch_tv. \
             Targets: sofa, armchair, table, tv, window, rug, left, right.",
            json!({"actions": {"type": "array", "maxItems": 8, "items": {
                "type": "object",
                "properties": {
                    "do": {"type": "string", "enum": BEHAVIOURS},
                    "target": {"type": "string", "enum": PLACES}
                },
                "required": ["do"]
            }}}),
            &["actions"]
        ),
        tool(
            "go_to_sleep",
            "Walk to a spot, curl up and fall asleep. You stay asleep until someone types to you or calls your name. Say goodnight first.",
            json!({"place": {"type": "string", "enum": SLEEP_PLACES, "description": "Where to nap. Default: sofa"}}),
            &[]
        ),
        tool(
            "look",
            "Take one picture with the camera and look at it. Fails if the camera is muted.",
            json!({}),
            &[]
        ),
    ]);
    if web {
        if let Some(list) = tools.as_array_mut() {
            list.extend(web_tools(model));
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> Vec<String> {
        let mut s = Splitter::default();
        let mut out = Vec::new();
        for c in chunks {
            s.feed(c, &mut out);
        }
        s.flush(&mut out);
        out
    }

    #[test]
    fn splits_into_sentences_as_text_streams() {
        let out = run(&["Hello the", "re, friend! I made you a", " timer. Want a", " tweak?"]);
        assert_eq!(out, ["Hello there, friend!", "I made you a timer.", "Want a tweak?"]);
    }

    #[test]
    fn short_fragments_merge_instead_of_stuttering() {
        assert_eq!(run(&["Hi. Nice to see you again today."]), ["Hi. Nice to see you again today."]);
    }

    #[test]
    fn silent_reply_says_nothing_even_when_streamed_in_pieces() {
        assert!(run(&["[sil", "ent]"]).is_empty());
        assert!(run(&["  [silent]"]).is_empty());
    }

    #[test]
    fn text_starting_with_bracket_is_not_swallowed() {
        assert_eq!(run(&["[Purr] hello there my friend."]), ["[Purr] hello there my friend."]);
    }

    #[test]
    fn markdown_and_emoji_are_not_spoken() {
        assert_eq!(run(&["**Done!** I made it 🎉 for you."]), ["Done! I made it for you."]);
    }

    #[test]
    fn decimals_do_not_split() {
        assert_eq!(run(&["It costs 3.50 dollars in total."]), ["It costs 3.50 dollars in total."]);
    }

    #[test]
    fn actions_are_validated_for_the_ui() {
        let ok = validate_actions(&json!([{"do": "jump_on", "target": "sofa"}, {"do": "scratch"}])).unwrap();
        assert_eq!(ok, [json!({"do": "jump_on", "target": "sofa"}), json!({"do": "scratch"})]);
        assert!(validate_actions(&json!([])).is_err());
        assert!(validate_actions(&json!("walk")).is_err());
        assert!(validate_actions(&json!([{"do": "fly"}])).unwrap_err().contains("walk_to"));
        assert!(validate_actions(&json!([{"do": "walk_to", "target": "moon"}])).is_err());
        let nine = json!((0..9).map(|_| json!({"do": "sit"})).collect::<Vec<_>>());
        assert!(validate_actions(&nine).is_err());
    }

    #[test]
    fn feelings_make_noises() {
        assert_eq!(emote_sound("purr"), Some("purr"));
        assert_eq!(emote_sound("happy"), Some("mrrp"));
        assert_eq!(emote_sound("hiss"), None);
        for e in EMOTES {
            if let Some(k) = emote_sound(e) {
                assert!(["meow", "mew", "mrrp", "purr"].contains(&k));
            }
        }
    }

    #[test]
    fn every_sleep_place_is_a_known_place() {
        for p in SLEEP_PLACES {
            assert!(PLACES.contains(&p), "{p}");
        }
    }

    #[test]
    fn web_tools_match_the_model_generation() {
        let new = web_tools("claude-sonnet-5-5");
        assert_eq!(new[0]["type"], "web_search_20260209");
        assert_eq!(new[1]["type"], "web_fetch_20260209");
        assert_eq!(web_tools("claude-haiku-4-5")[0]["type"], "web_search_20250305");
        let on = tool_defs("claude-sonnet-5-5", true);
        let off = tool_defs("claude-sonnet-5-5", false);
        assert_eq!(on.as_array().unwrap().len(), off.as_array().unwrap().len() + 2);
        assert!(off.as_array().unwrap().iter().all(|t| t["name"] != "web_search"));
    }

    #[test]
    fn tool_defs_are_well_formed() {
        for t in tool_defs("claude-sonnet-5-5", false).as_array().unwrap() {
            assert!(t["name"].is_string() && t["description"].is_string());
            assert_eq!(t["input_schema"]["type"], "object");
        }
    }
}
