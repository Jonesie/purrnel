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

pub fn pick_place(cfg: &Config) -> Option<Place> {
    if let (Some(lat), Some(lon)) = (cfg.lat, cfg.lon) {
        return Some(Place::Coords(lat, lon));
    }
    if let Some(name) = &cfg.location {
        return Some(Place::Name(name.clone(), None));
    }
    let tz = system_timezone()?;
    let city = city_from_timezone(&tz)?;
    Some(Place::Name(city, Some(tz)))
}

/// Choose a result from an Open-Meteo geocoding reply, preferring one in `tz`.
pub fn parse_geocode(v: &Value, tz: Option<&str>) -> Option<(f64, f64)> {
    let results = v.get("results")?.as_array()?;
    let pick = tz
        .and_then(|tz| results.iter().find(|r| r.get("timezone").and_then(Value::as_str) == Some(tz)))
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
            let v = get_json(
                client,
                &cfg.geocode_url,
                &[("name", name.clone()), ("count", "10".into()), ("language", "en".into())],
            )
            .await?;
            parse_geocode(&v, tz.as_deref()).ok_or_else(|| format!("no place called {name:?}"))
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

/// Keeps the window's weather up to date: once at start, then every 15 minutes.
pub async fn run(hub: Arc<Hub>) {
    let cfg = hub.cfg.clone();
    if !cfg.weather {
        return;
    }
    let Some(place) = pick_place(&cfg) else {
        eprintln!("purrnel: weather off: can't tell where you are (set PURRNEL_LOCATION=\"Your City\")");
        return;
    };
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut coords: Option<(f64, f64)> = None;
    let mut reported_error = false;
    loop {
        let result = async {
            let (lat, lon) = match coords {
                Some(c) => c,
                None => {
                    let c = locate(&client, &cfg, &place).await?;
                    coords = Some(c);
                    c
                }
            };
            fetch(&client, &cfg, lat, lon).await
        }
        .await;
        let wait = match result {
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
        };
        tokio::time::sleep(wait).await;
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
        assert_eq!(parse_geocode(&v, Some("Pacific/Auckland")), Some((-36.85, 174.76)));
        // without a timezone, or with no match, the first result wins
        assert_eq!(parse_geocode(&v, None), Some((38.0, -122.0)));
        assert_eq!(parse_geocode(&v, Some("Asia/Tokyo")), Some((38.0, -122.0)));
        assert_eq!(parse_geocode(&json!({}), None), None);
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
        assert_eq!(pick_place(&cfg), Some(Place::Coords(-41.29, 174.78)));
        cfg.lat = None;
        assert_eq!(pick_place(&cfg), Some(Place::Name("Elsewhere".into(), None)));
    }
}
