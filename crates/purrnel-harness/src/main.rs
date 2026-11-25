mod audio;
mod brain;
mod camera;
mod config;
mod hub;
mod protocol;
mod server;
mod stt;
mod tts;
mod weather;

use config::Config;
use hub::Hub;
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let cfg = Config::from_env();
    eprintln!(
        "purrnel: model {} | brain key: {} | data: {}",
        cfg.model,
        if cfg.api_key.is_some() { "found" } else { "MISSING" },
        cfg.data_dir.display()
    );

    let (utter_tx, utter_rx) = mpsc::channel::<String>(16);
    let hub = Arc::new(Hub::new(cfg.clone(), utter_tx)?);

    tokio::spawn({
        let hub = hub.clone();
        async move { hub.run_cat_ticker().await }
    });
    tokio::spawn(brain::run(hub.clone(), utter_rx));
    tokio::spawn(weather::run(hub.clone()));

    // Voice comes up in the background so the UI is available immediately.
    tokio::spawn({
        let hub = hub.clone();
        async move {
            // Both load models, so do them side by side rather than one after the other.
            let (voice, stt) = tokio::join!(tts::start(hub.clone()), stt::Stt::start(&hub.cfg));
            let stt = stt.map(Arc::new);
            hub.set_ready(voice, stt.is_some());
            eprintln!("purrnel: ready (voice: {voice}, ears: {})", stt.is_some());
            audio::run(hub, stt).await;
        }
    });

    server::serve(hub).await
}
