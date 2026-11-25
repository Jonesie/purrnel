use std::path::PathBuf;

/// Everything is configured by environment variables so the same binary runs on
/// a dev machine and (later) on the NixOS image.
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub data_dir: PathBuf,
    pub ui_dir: PathBuf,
    pub model: String,
    pub api_url: String,
    pub api_key: Option<String>,
    pub fallbacks: bool,
    /// Give her Anthropic's hosted web search and page-reading tools.
    pub web: bool,
    pub whisper_bin: Option<PathBuf>,
    pub whisper_model: PathBuf,
    pub whisper_port: u16,
    pub piper_python: PathBuf,
    pub tts_script: PathBuf,
    pub voice: PathBuf,
    /// Pitch multiplier for the cat voice (1.0 = as recorded).
    pub pitch: f32,
    /// Piper length scale: below 1.0 is faster.
    pub speed: f32,
    pub sfx_dir: PathBuf,
    pub mic_source: Option<String>,
    pub camera: String,
    /// Show the real local weather in the window.
    pub weather: bool,
    /// Place name for the weather (default: the city of the system timezone).
    pub location: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub weather_url: String,
    pub geocode_url: String,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn home() -> PathBuf {
    env("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn find_in_path(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| PathBuf::from(d).join(bin))
        .find(|p| p.is_file())
}

impl Config {
    pub fn from_env() -> Self {
        let root = env("PURRNEL_ROOT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let api_key = env("ANTHROPIC_API_KEY").or_else(|| {
            std::fs::read_to_string(home().join(".config/purrnel/api_key"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
        Config {
            port: env("PURRNEL_PORT").and_then(|v| v.parse().ok()).unwrap_or(7979),
            data_dir: env("PURRNEL_DATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".local/share/purrnel")),
            ui_dir: env("PURRNEL_UI").map(PathBuf::from).unwrap_or_else(|| root.join("ui")),
            model: env("PURRNEL_MODEL").unwrap_or_else(|| "claude-sonnet-5-5".into()),
            api_url: env("PURRNEL_API_URL").unwrap_or_else(|| "https://api.anthropic.com/v1/messages".into()),
            api_key,
            fallbacks: env("PURRNEL_FALLBACKS").as_deref() != Some("0"),
            web: env("PURRNEL_WEB").as_deref() != Some("0"),
            whisper_bin: env("PURRNEL_WHISPER_BIN")
                .map(PathBuf::from)
                .or_else(|| find_in_path("whisper-server")),
            whisper_model: env("PURRNEL_WHISPER_MODEL")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("models/ggml-base.en-q5_1.bin")),
            whisper_port: env("PURRNEL_WHISPER_PORT").and_then(|v| v.parse().ok()).unwrap_or(18178),
            piper_python: env("PURRNEL_PIPER_PYTHON")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".local/pipx/venvs/piper-tts/bin/python")),
            tts_script: env("PURRNEL_TTS_SCRIPT")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("tools/tts_worker.py")),
            voice: env("PURRNEL_VOICE")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("models/en_US-lessac-medium.onnx")),
            pitch: env("PURRNEL_PITCH").and_then(|v| v.parse().ok()).unwrap_or(1.3),
            speed: env("PURRNEL_SPEED").and_then(|v| v.parse().ok()).unwrap_or(0.92),
            sfx_dir: env("PURRNEL_SFX").map(PathBuf::from).unwrap_or_else(|| root.join("assets/sfx")),
            mic_source: env("PURRNEL_MIC"),
            camera: env("PURRNEL_CAMERA").unwrap_or_else(|| "/dev/video0".into()),
            weather: env("PURRNEL_WEATHER").as_deref() != Some("0"),
            location: env("PURRNEL_LOCATION"),
            lat: env("PURRNEL_LAT").and_then(|v| v.parse().ok()),
            lon: env("PURRNEL_LON").and_then(|v| v.parse().ok()),
            weather_url: env("PURRNEL_WEATHER_URL")
                .unwrap_or_else(|| "https://api.open-meteo.com/v1/forecast".into()),
            geocode_url: env("PURRNEL_GEOCODE_URL")
                .unwrap_or_else(|| "https://geocoding-api.open-meteo.com/v1/search".into()),
        }
    }
}
