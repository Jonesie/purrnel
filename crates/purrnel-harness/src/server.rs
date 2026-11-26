//! Local web server: serves the kiosk UI and bridges the websocket to the hub.
//! Bound to localhost only: the UI is not a network service.

use crate::hub::Hub;
use crate::protocol::{In, Out};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

const EMBEDDED_UI: &str = include_str!("../../../ui/index.html");

/// The release this was built from (the git tag, e.g. "v0.2.0"), set as PURRNEL_VERSION when the
/// release image is built. Any other build says "dev".
pub const VERSION: &str = match option_env!("PURRNEL_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => "dev",
};

/// Fill the version into the page (the placeholder is shown dimly at the top).
fn with_version(html: &str) -> String {
    html.replace("__PURRNEL_VERSION__", VERSION)
}

pub async fn serve(hub: Arc<Hub>) -> std::io::Result<()> {
    let addr = format!("{}:{}", hub.cfg.bind, hub.cfg.port);
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .with_state(hub);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("purrnel: open http://{addr}");
    axum::serve(listener, app).await
}

/// Serve the UI from disk when present (so edits show on reload), else the embedded copy.
async fn index(State(hub): State<Arc<Hub>>) -> impl IntoResponse {
    let html = std::fs::read_to_string(hub.cfg.ui_dir.join("index.html"))
        .unwrap_or_else(|_| EMBEDDED_UI.to_string());
    ([("cache-control", "no-store")], Html(with_version(&html)))
}

/// Only the kiosk page itself may drive the cat. Browsers let any website open a websocket to
/// localhost, and this socket can submit prompts and save settings, so check where it came from.
/// (Not a browser at all, so no Origin header: fine.)
pub fn origin_ok(origin: Option<&str>, port: u16) -> bool {
    match origin {
        None => true,
        Some(o) => [format!("http://127.0.0.1:{port}"), format!("http://localhost:{port}"), format!("http://[::1]:{port}")]
            .iter()
            .any(|ok| ok == o),
    }
}

async fn ws_upgrade(ws: WebSocketUpgrade, headers: HeaderMap, State(hub): State<Arc<Hub>>) -> impl IntoResponse {
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    if !origin_ok(origin, hub.cfg.port) {
        eprintln!("purrnel: refused a websocket from {}", origin.unwrap_or("?"));
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(move |socket| client(socket, hub)).into_response()
}

async fn client(socket: WebSocket, hub: Arc<Hub>) {
    let (mut sink, mut stream) = socket.split();
    // Subscribe before the snapshot so nothing is missed in between.
    let mut rx = hub.tx.subscribe();

    let mut snapshot = vec![
        Out::Privacy { muted: !hub.privacy().mic_live() },
        Out::Online { online: hub.online.load(std::sync::atomic::Ordering::Relaxed) },
        Out::Cat { state: hub.cat_state().as_str().into() },
        Out::Artefacts { items: hub.artefact_infos() },
    ];
    if let Some((voice, ears)) = hub.ready() {
        snapshot.push(Out::Ready { voice, ears });
    }
    if let Some(w) = hub.weather() {
        snapshot.push(w.to_out());
    }
    snapshot.push(Out::Info {
        repo: hub.cfg.repo.clone(),
        can_post_issue: hub.cfg.github_token.is_some(),
        version: env!("CARGO_PKG_VERSION").into(),
    });
    if crate::settings::setup_needed(&hub) {
        snapshot.push(crate::settings::dialog(&hub, true));
    }
    if let Some(id) = hub.current_artefact.lock().unwrap().clone() {
        if let Ok((meta, html)) = hub.store.lock().unwrap().get(&id) {
            snapshot.push(Out::Artefact {
                id: meta.id,
                title: meta.title,
                version: meta.versions.len(),
                html,
            });
        }
    }
    for msg in snapshot {
        if send(&mut sink, &msg).await.is_err() {
            return;
        }
    }

    let writer = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if send(&mut sink, &msg).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    while let Some(Ok(frame)) = stream.next().await {
        let Message::Text(text) = frame else { continue };
        match serde_json::from_str::<In>(&text) {
            Ok(msg) => handle(&hub, msg).await,
            Err(e) => eprintln!("purrnel: bad ui message: {e}"),
        }
    }
    writer.abort();
}

async fn send(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    msg: &Out,
) -> Result<(), axum::Error> {
    let json = serde_json::to_string(msg).expect("Out always serialises");
    sink.send(Message::Text(json.into())).await
}

async fn handle(hub: &Arc<Hub>, msg: In) {
    // Only real user actions count as activity. An artefact saving its own state
    // (a ticking timer, say) must not keep the cat from getting bored.
    match &msg {
        In::Text { .. } | In::Show { .. } | In::Close | In::Pet => hub.wake(),
        In::ToggleMute | In::ConfirmAnswer { .. } => hub.touch(),
        In::DataGet { .. } | In::DataSet { .. } | In::Sfx { .. } | In::OpenSettings | In::SaveSettings { .. } | In::Feedback { .. } => {}
    }
    match msg {
        In::Text { text } => hub.submit(text, true).await,
        In::ToggleMute => hub.toggle_mute(),
        In::Sfx { kind } => hub.play_ambient(&kind),
        In::Pet => {
            hub.play_sfx("purr");
            hub.emit(Out::Emote { name: "purr".into() });
        }
        In::ConfirmAnswer { yes } => hub.answer_confirm(yes),
        In::Show { id } => {
            if let Err(e) = hub.show_artefact(&id) {
                hub.emit(Out::Notice { text: e });
            }
        }
        In::Close => hub.close_artefact(),
        In::OpenSettings => hub.emit(crate::settings::dialog(hub, false)),
        In::SaveSettings { api_key, location } => {
            let hub = hub.clone();
            tokio::spawn(async move { crate::settings::save(&hub, api_key, location).await });
        }
        In::Feedback { kind, title, details } => {
            let hub = hub.clone();
            tokio::spawn(async move { crate::feedback::submit(&hub, &kind, &title, &details).await });
        }
        In::DataGet { id, req, key } => {
            let value = hub.store.lock().unwrap().data_get(&id, &key).ok().flatten();
            hub.emit(Out::DataReply { id, req, value });
        }
        In::DataSet { id, key, value } => {
            if let Err(e) = hub.store.lock().unwrap().data_set(&id, &key, value) {
                eprintln!("purrnel: data_set {id}/{key}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{origin_ok, with_version, EMBEDDED_UI, VERSION};

    #[test]
    fn the_page_shows_the_version() {
        assert!(EMBEDDED_UI.contains("__PURRNEL_VERSION__"), "the page lost its version placeholder");
        let page = with_version(EMBEDDED_UI);
        assert!(!page.contains("__PURRNEL_VERSION__"));
        assert!(page.contains(&format!(r#"<div id="version">{VERSION}</div>"#)));
    }

    #[test]
    fn only_the_kiosk_page_may_connect() {
        assert!(origin_ok(None, 7979));
        assert!(origin_ok(Some("http://127.0.0.1:7979"), 7979));
        assert!(origin_ok(Some("http://localhost:7979"), 7979));
        assert!(!origin_ok(Some("https://evil.example"), 7979));
        assert!(!origin_ok(Some("http://127.0.0.1:8080"), 7979));
        assert!(!origin_ok(Some("http://127.0.0.1:7979.evil.example"), 7979));
        assert!(!origin_ok(Some("null"), 7979));
    }
}
