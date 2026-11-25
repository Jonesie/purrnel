//! Wire protocol between the harness and the kiosk UI (JSON over websocket).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct ArtefactInfo {
    pub id: String,
    pub title: String,
    pub versions: usize,
    pub updated: u64,
}

/// Harness -> UI.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Out {
    Cat { state: String },
    Privacy { muted: bool },
    Online { online: bool },
    /// What the cat heard the user say (voice or typed).
    Heard { text: String },
    /// A sentence the cat is saying, sent when it starts being spoken.
    Say { text: String },
    /// Mouth opening 0..1 while speaking.
    Mouth { level: f32 },
    /// Microphone level 0..1 for the indicator.
    Level { level: f32 },
    Artefact { id: String, title: String, version: usize, html: String },
    Artefacts { items: Vec<ArtefactInfo> },
    Close,
    Confirm { prompt: String },
    ConfirmDone,
    Emote { name: String },
    /// A sequence of body actions ({do, target?}), see `act` in brain.rs.
    Act { actions: Vec<Value> },
    /// Walk to `place`, curl up and sleep until woken.
    Sleep { place: String },
    /// Startup finished: whether she can speak and whether she can hear.
    Ready { voice: bool, ears: bool },
    /// The real local weather outside: a WMO weather code plus cloud %, wind km/h, rain mm, temperature C.
    Weather { code: u16, cloud: f32, wind: f32, precip: f32, temp: f32 },
    Looking,
    Notice { text: String },
    DataReply { id: String, req: u64, value: Option<Value> },
}

/// UI -> harness.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum In {
    Text { text: String },
    ToggleMute,
    /// A background noise from the room (e.g. the cat chattering at a bird or hissing at a dog).
    Sfx { kind: String },
    /// The user clicked or petted the cat.
    Pet,
    ConfirmAnswer { yes: bool },
    Show { id: String },
    Close,
    DataGet { id: String, req: u64, key: String },
    DataSet { id: String, key: String, value: Value },
}
