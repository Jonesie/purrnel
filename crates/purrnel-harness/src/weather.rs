//! The real local weather, shown in the window.
//!
//! Uses Open-Meteo (free, no key). Only a coarse location is ever sent: a place name for
//! geocoding, then its latitude/longitude. By default the place is the city from the
//! system timezone (e.g. `Pacific/Auckland` -> "Auckland"); `PURRNEL_LOCATION` or
//! `PURRNEL_LAT` + `PURRNEL_LON` override it, and `PURRNEL_WEATHER=0` turns it all off.
//! If the lookup fails the window simply keeps its last weather (or fair weather).

use crate::config::Config;
use crate::hub::Hub;
use crate::protocol::Out;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// How the weather looks right now. `code` is a WMO weather code
/// (0 clear, 3 overcast, 45 fog, 61 rain, 71 snow, 95 thunderstorm ...); the UI decides how to draw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weather {
    pub code: u16,
    /// Cloud cover, 0..100 %.
    pub cloud: f32,
    /// Wind speed at 10 m, km/h.
    pub wind: f32,
    /// Precipitation over the last hour, mm.
    pub precip: f32,
    /// Air temperature, deg C.
    pub temp: f32,
}

impl Weather {
    pub fn to_out(self) -> Out {
        Out::Weather { code: self.code, cloud: self.cloud, wind: self.wind, precip: self.precip, temp: self.temp }
    }

    /// A few words for logs.
    pub fn describe(&self) -> &'static str {
        match self.code {
            0 => "clear",
            1 => "mostly clear",
            2 => "partly cloudy",
            3 => "overcast",
            45 | 48 => "fog",
            51..=57 => "drizzle",
            61..=67 | 80..=82 => "rain",
            71..=77 | 85 | 86 => "snow",
            95..=99 => "thunderstorm",
            _ => "unsettled",
        }
    }
}

/// Where to ask about the weather.
#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    Coords(f64, f64),
    /// A place name, with the timezone to prefer when several places share the name.
    Name(String, Option<String>),
}

/// The IANA timezone of this machine, e.g. `Pacific/Auckland`.
fn system_timezone() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim().trim_start_matches(':').to_string();
        if tz.contains('/') {
            return Some(tz);
        }
    }
    if let Ok(s) = std::fs::read_to_string("/etc/timezone") {
        let s = s.trim();
        if s.contains('/') {
            return Some(s.to_string());
        }
    }
    let link = std::fs::read_link("/etc/localtime").ok()?;
    let link = link.to_string_lossy().to_string();
    link.split("zoneinfo/").nth(1).map(|s| s.to_string()).filter(|s| s.contains('/'))
}

/// `Pacific/Auckland` -> `Auckland`, `America/Argentina/Buenos_Aires` -> `Buenos Aires`.
pub fn city_from_timezone(tz: &str) -> Option<String> {
    let city = tz.rsplit('/').next()?.replace('_', " ");
    if city.is_empty() || tz.starts_with("Etc/") || tz == "UTC" {
        None
    } else {
        Some(city)
    }
}

/// The city of this machine's timezone: the settings dialog suggests it as the place.
pub fn suggested_city() -> Option<String> {
    system_timezone().and_then(|tz| city_from_timezone(&tz))
}

/// `location` is the place currently set (typed in the settings dialog, or `PURRNEL_LOCATION`).
pub fn pick_place(cfg: &Config, location: Option<&str>) -> Option<Place> {
    if let (Some(lat), Some(lon)) = (cfg.lat, cfg.lon) {
        return Some(Place::Coords(lat, lon));
    }
    if let Some(name) = location {
        return Some(Place::Name(name.to_string(), None));
    }
    let tz = system_timezone()?;
    let city = city_from_timezone(&tz)?;
    Some(Place::Name(city, Some(tz)))
}

/// Choose a result from an Open-Meteo geocoding reply. `hint` is what followed the comma in
/// "Auckland, New Zealand" (a country, country code or region); otherwise prefer one in `tz`.
pub fn parse_geocode(v: &Value, tz: Option<&str>, hint: Option<&str>) -> Option<(f64, f64)> {
    let results = v.get("results")?.as_array()?;
    let by_hint = hint.map(str::trim).filter(|h| !h.is_empty()).and_then(|h| {
        results.iter().find(|r| {
            ["country", "country_code", "admin1", "admin2"]
                .iter()
                .any(|k| r.get(*k).and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case(h)))
        })
    });
    let pick = by_hint
        .or_else(|| tz.and_then(|tz| results.iter().find(|r| r.get("timezone").and_then(Value::as_str) == Some(tz))))
        .or_else(|| results.first())?;
    Some((pick.get("latitude")?.as_f64()?, pick.get("longitude")?.as_f64()?))
}

/// Parse the `current` block of an Open-Meteo forecast reply.
pub fn parse_current(v: &Value) -> Option<Weather> {
    let c = v.get("current")?;
    let num = |k: &str| c.get(k).and_then(Value::as_f64);
    Some(Weather {
        code: num("weather_code")? as u16,
        cloud: num("cloud_cover").unwrap_or(0.0) as f32,
        wind: num("wind_speed_10m").unwrap_or(0.0) as f32,
        precip: num("precipitation").unwrap_or(0.0) as f32,
        temp: num("temperature_2m").unwrap_or(15.0) as f32,
    })
}

async fn get_json(client: &reqwest::Client, url: &str, query: &[(&str, String)]) -> Result<Value, String> {
    let url = reqwest::Url::parse_with_params(url, query).map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<Value>().await.map_err(|e| e.to_string())
}

async fn locate(client: &reqwest::Client, cfg: &Config, place: &Place) -> Result<(f64, f64), String> {
    match place {
        Place::Coords(lat, lon) => Ok((*lat, *lon)),
        Place::Name(name, tz) => {
            // "Auckland, New Zealand": the geocoder only searches by name, the rest narrows the answer.
            let (query, hint) = match name.split_once(',') {
                Some((a, b)) => (a.trim(), Some(b.trim())),
                None => (name.trim(), None),
            };
            let v = get_json(
                client,
                &cfg.geocode_url,
                &[("name", query.to_string()), ("count", "10".into()), ("language", "en".into())],
            )
            .await?;
            parse_geocode(&v, tz.as_deref(), hint).ok_or_else(|| format!("no place called {name:?}"))
        }
    }
}

async fn fetch(client: &reqwest::Client, cfg: &Config, lat: f64, lon: f64) -> Result<Weather, String> {
    let v = get_json(
        client,
        &cfg.weather_url,
        &[
            ("latitude", format!("{lat:.3}")),
            ("longitude", format!("{lon:.3}")),
            ("current", "temperature_2m,weather_code,cloud_cover,wind_speed_10m,precipitation".into()),
            ("wind_speed_unit", "kmh".into()),
        ],
    )
    .await?;
    parse_current(&v).ok_or_else(|| "unexpected reply".to_string())
}

/// Can this place name be found? `Unknown` means we couldn't tell (offline), so it is accepted.
#[derive(Debug, PartialEq)]
pub enum Check {
    Found,
    NotFound,
    Unknown,
}

pub async fn check_place(cfg: &Config, name: &str) -> Check {
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(10)).build() else { return Check::Unknown };
    match locate(&client, cfg, &Place::Name(name.to_string(), None)).await {
        Ok(_) => Check::Found,
        Err(e) if e.starts_with("no place called") => Check::NotFound,
        Err(_) => Check::Unknown,
    }
}

/// Keeps the window's weather up to date: once at start, then every 15 minutes, and straight away
/// when the place changes in the settings dialog.
pub async fn run(hub: Arc<Hub>) {
    let cfg = hub.cfg.clone();
    if !cfg.weather {
        return;
    }
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut cached: Option<(Place, (f64, f64))> = None;
    let mut reported_error = false;
    let mut reported_nowhere = false;
    loop {
        let wait = match pick_place(&cfg, hub.location().as_deref()) {
            None => {
                if !reported_nowhere {
                    eprintln!("purrnel: weather off until you say where you are (settings, or PURRNEL_LOCATION=\"Your City\")");
                    reported_nowhere = true;
                }
                Duration::from_secs(3600)
            }
            Some(place) => {
                reported_nowhere = false;
                let result = async {
                    let (lat, lon) = match &cached {
                        Some((p, c)) if *p == place => *c,
                        _ => {
                            let c = locate(&client, &cfg, &place).await?;
                            cached = Some((place.clone(), c));
                            c
                        }
                    };
                    fetch(&client, &cfg, lat, lon).await
                }
                .await;
                match result {
                    Ok(w) => {
                        eprintln!("purrnel: weather: {} ({:.0} C, cloud {:.0}%, wind {:.0} km/h)", w.describe(), w.temp, w.cloud, w.wind);
                        reported_error = false;
                        hub.set_weather(w);
                        Duration::from_secs(15 * 60)
                    }
                    Err(e) => {
                        // Being offline is normal and the cat already handles it; say it once.
                        if !reported_error {
                            eprintln!("purrnel: weather unavailable ({e}); will keep trying");
                            reported_error = true;
                        }
                        Duration::from_secs(120)
                    }
                }
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = hub.weather_kick.notified() => { reported_error = false; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn city_comes_from_timezone() {
        assert_eq!(city_from_timezone("Pacific/Auckland").as_deref(), Some("Auckland"));
        assert_eq!(city_from_timezone("America/Argentina/Buenos_Aires").as_deref(), Some("Buenos Aires"));
        assert_eq!(city_from_timezone("Etc/UTC"), None);
        assert_eq!(city_from_timezone("UTC"), None);
    }

    #[test]
    fn geocode_prefers_the_local_timezone() {
        let v = json!({"results": [
            {"name": "Auckland", "latitude": 38.0, "longitude": -122.0, "timezone": "America/Los_Angeles"},
            {"name": "Auckland", "latitude": -36.85, "longitude": 174.76, "timezone": "Pacific/Auckland"},
        ]});
        assert_eq!(parse_geocode(&v, Some("Pacific/Auckland"), None), Some((-36.85, 174.76)));
        // without a timezone, or with no match, the first result wins
        assert_eq!(parse_geocode(&v, None, None), Some((38.0, -122.0)));
        assert_eq!(parse_geocode(&v, Some("Asia/Tokyo"), None), Some((38.0, -122.0)));
        assert_eq!(parse_geocode(&json!({}), None, None), None);
    }

    #[test]
    fn parses_current_conditions() {
        let v = json!({"current": {"temperature_2m": 11.4, "weather_code": 63, "cloud_cover": 92,
                                   "wind_speed_10m": 23.0, "precipitation": 1.2}});
        let w = parse_current(&v).unwrap();
        assert_eq!(w.code, 63);
        assert_eq!(w.describe(), "rain");
        assert_eq!(w.cloud, 92.0);
        assert_eq!(w.wind, 23.0);
        assert!(parse_current(&json!({"current": {}})).is_none());
        assert!(parse_current(&json!({"error": true})).is_none());
    }

    #[test]
    fn coords_override_everything() {
        let mut cfg = Config::from_env();
        cfg.lat = Some(-41.29);
        cfg.lon = Some(174.78);
        cfg.location = Some("Elsewhere".into());
        assert_eq!(pick_place(&cfg, Some("Elsewhere")), Some(Place::Coords(-41.29, 174.78)));
        cfg.lat = None;
        assert_eq!(pick_place(&cfg, Some("Elsewhere")), Some(Place::Name("Elsewhere".into(), None)));
    }

    #[test]
    fn a_country_after_the_comma_picks_the_right_town() {
        let v = json!({"results": [
            {"name": "Hamilton", "latitude": 43.2, "longitude": -79.9, "country": "Canada", "country_code": "CA", "admin1": "Ontario", "timezone": "America/Toronto"},
            {"name": "Hamilton", "latitude": -37.78, "longitude": 175.28, "country": "New Zealand", "country_code": "NZ", "admin1": "Waikato", "timezone": "Pacific/Auckland"},
        ]});
        assert_eq!(parse_geocode(&v, None, Some("New Zealand")), Some((-37.78, 175.28)));
        assert_eq!(parse_geocode(&v, None, Some("nz")), Some((-37.78, 175.28)));
        assert_eq!(parse_geocode(&v, None, Some("Ontario")), Some((43.2, -79.9)));
        // a hint that matches nothing falls back to the first result rather than failing
        assert_eq!(parse_geocode(&v, None, Some("Narnia")), Some((43.2, -79.9)));
    }
}
