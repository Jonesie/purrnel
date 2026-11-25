//! First-run setup and the settings dialog: the API key and the place the weather is for.
//!
//! Both are saved under the config folder (`~/.config/purrnel`). The key is written readable by the
//! user only and is never sent back to the UI.

use crate::config::config_dir;
use crate::hub::Hub;
use crate::protocol::Out;
use crate::weather::{self, Check};
use std::io;
use std::path::Path;
use std::sync::Arc;

/// Writes `<dir>/<name>`, creating the folder, readable by the user only.
pub fn save_in(dir: &Path, name: &str, value: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    std::fs::create_dir_all(dir)?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    let path = dir.join(name);
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
    // a file that already existed keeps its old mode when opened, so tighten it
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(value.as_bytes())?;
    f.write_all(b"\n")
}

/// Is this plausibly an API key? One word, printable ASCII. (Real validation is the first request.)
pub fn valid_api_key(k: &str) -> bool {
    k.len() >= 8 && k.len() <= 300 && k.chars().all(|c| c.is_ascii_graphic())
}

/// A place name tidied up: one line, no control characters, not absurdly long.
pub fn clean_location(s: &str) -> Option<String> {
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s: String = s.chars().take(80).collect();
    (!s.is_empty()).then_some(s)
}

/// Does the cat still need something from the person? (No key, or no idea where she lives.)
pub fn setup_needed(hub: &Hub) -> bool {
    hub.api_key().is_none() || (hub.cfg.weather && hub.cfg.lat.is_none() && hub.location().is_none())
}

/// What the dialog shows. `first_run` makes it open by itself.
pub fn dialog(hub: &Hub, first_run: bool) -> Out {
    Out::Settings {
        first_run,
        has_key: hub.api_key().is_some(),
        location: hub.location(),
        suggested_location: weather::suggested_city(),
        weather: hub.cfg.weather,
    }
}

fn refuse(hub: &Hub, why: &str) {
    hub.emit(Out::SettingsResult { ok: false, error: Some(why.to_string()) });
}

/// Handles the dialog's Save: checks what was typed, stores it, and applies it straight away.
pub async fn save(hub: &Arc<Hub>, api_key: Option<String>, location: Option<String>) {
    let key = api_key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    if let Some(k) = &key {
        if !valid_api_key(k) {
            return refuse(hub, "That doesn't look like an API key. It is one long word, usually starting sk-ant-.");
        }
    }
    let place = location.as_deref().and_then(clean_location);
    if let Some(p) = &place {
        if weather::check_place(&hub.cfg, p).await == Check::NotFound {
            return refuse(hub, &format!("I can't find a place called \"{p}\". Try the nearest town, or \"Town, Country\"."));
        }
    }
    let dir = config_dir();
    if let Some(k) = &key {
        if let Err(e) = save_in(&dir, "api_key", k) {
            return refuse(hub, &format!("I couldn't save that: {e}"));
        }
        hub.set_api_key(k.clone());
    }
    if let Some(p) = &place {
        if let Err(e) = save_in(&dir, "location", p) {
            return refuse(hub, &format!("I couldn't save that: {e}"));
        }
        hub.set_location(p.clone());
    }
    eprintln!(
        "purrnel: settings saved ({})",
        [key.as_ref().map(|_| "API key"), place.as_ref().map(|_| "location")].into_iter().flatten().collect::<Vec<_>>().join(", ")
    );
    hub.emit(Out::SettingsResult { ok: true, error: None });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn api_keys_are_one_printable_word() {
        assert!(valid_api_key("sk-ant-api03-abcdefghijklmnop"));
        assert!(!valid_api_key("short"));
        assert!(!valid_api_key("has a space in it, not a key"));
        assert!(!valid_api_key("new\nline-key-1234"));
        assert!(!valid_api_key(&"x".repeat(400)));
    }

    #[test]
    fn locations_are_tidied() {
        assert_eq!(clean_location("  Auckland,   New Zealand \n").as_deref(), Some("Auckland, New Zealand"));
        assert_eq!(clean_location("   "), None);
        assert_eq!(clean_location("a\u{7}b").as_deref(), Some("ab"));
        assert_eq!(clean_location(&"x".repeat(200)).unwrap().len(), 80);
    }

    #[test]
    fn saved_secrets_are_private_to_the_user() {
        let dir = std::env::temp_dir().join(format!("purrnel-settings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // an existing, too-open file is tightened and overwritten rather than left readable
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("api_key"), "old").unwrap();
        std::fs::set_permissions(dir.join("api_key"), std::fs::Permissions::from_mode(0o644)).unwrap();
        save_in(&dir, "api_key", "sk-ant-new-key-123").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("api_key")).unwrap().trim(), "sk-ant-new-key-123");
        assert_eq!(std::fs::metadata(dir.join("api_key")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
