//! Building `race.ini` for offline sessions, following AcTools `Game.cs`.

use serde::{Deserialize, Serialize};

use crate::ini::IniFile;

/// AC's `SESSION_n/TYPE` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SessionType {
    Practice = 1,
    Qualification = 2,
    Race = 3,
    Hotlap = 4,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Mode {
    Practice { minutes: u32 },
    Hotlap,
    Race { laps: u32, opponents: u32, ai_level: u32, starting_position: u32 },
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Practice { minutes: 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conditions {
    /// Seconds since midnight. AC accepts roughly 08:00-18:00.
    pub time_seconds: u32,
    pub ambient_c: f64,
    pub weather_id: String,
    pub time_multiplier: f64,
}

impl Default for Conditions {
    fn default() -> Self {
        Self { time_seconds: 13 * 3600, ambient_c: 26.0, weather_id: "3_clear".into(), time_multiplier: 1.0 }
    }
}

pub const TIME_MIN: u32 = 8 * 3600;
pub const TIME_MAX: u32 = 18 * 3600;

pub fn sun_angle(seconds: f64) -> f64 {
    16.0 * (seconds - 46800.0) / 3600.0
}

/// Content Manager's road temperature estimate (`GetRoadTemperature`).
pub fn road_temperature(seconds: f64, ambient: f64, weather_coefficient: f64) -> f64 {
    let (min, max) = (TIME_MIN as f64, TIME_MAX as f64);
    if seconds < min || seconds > max {
        let a = road_temperature(min, ambient, weather_coefficient);
        let b = road_temperature(max, ambient, weather_coefficient);
        let span = (max - 86400.0) - min;
        let s = if seconds > max { seconds - 86400.0 } else { seconds };
        return a + (b - a) * (s - min) / span;
    }
    let t = (seconds / 3600.0 - 7.0) * 0.04167;
    let w = if weather_coefficient == 0.0 { 1.0 } else { weather_coefficient };
    ambient
        * (1.0
            + 5.33332
                * w
                * (1.0 - t)
                * ((-6.0 * t).exp() * (6.0 * t).sin() + 0.25)
                * (0.9 * t).sin())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Opponent {
    pub car_id: String,
    pub skin_id: String,
    pub driver_name: String,
    pub nationality: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuickDrive {
    pub car_id: String,
    pub skin_id: String,
    pub track_id: String,
    pub layout_id: Option<String>,
    pub driver_name: String,
    pub nationality: String,
    pub mode: Mode,
    pub conditions: Conditions,
    pub penalties: bool,
    pub fixed_setup: bool,
    pub use_mph: bool,
    /// Filled in by the caller for race mode (see [`pick_opponents`]).
    pub opponents: Vec<Opponent>,
}

/// AI names used when the caller does not supply any.
const AI_NAMES: &[(&str, &str)] = &[
    ("Marco Rossi", "ITA"), ("James Walker", "GBR"), ("Lukas Becker", "GER"),
    ("Pierre Martin", "FRA"), ("Carlos Ruiz", "ESP"), ("Kenji Sato", "JPN"),
    ("Daan de Vries", "NED"), ("Ethan Miller", "USA"), ("Mateus Silva", "BRA"),
    ("Mikko Laine", "FIN"), ("Liam O'Brien", "IRL"), ("Anders Berg", "SWE"),
    ("Tomasz Nowak", "POL"), ("Lucas Moreau", "BEL"), ("Jack Taylor", "AUS"),
    ("Nico Weber", "AUT"), ("Ivan Petrov", "RUS"), ("Luca Bianchi", "SUI"),
];

/// Same car as the player, cycling through its other skins.
pub fn pick_opponents(car_id: &str, player_skin: &str, skins: &[String], count: u32) -> Vec<Opponent> {
    let pool: Vec<&String> = {
        let others: Vec<&String> = skins.iter().filter(|s| *s != player_skin).collect();
        if others.is_empty() { skins.iter().collect() } else { others }
    };
    (0..count as usize)
        .map(|i| {
            let (name, nat) = AI_NAMES[i % AI_NAMES.len()];
            Opponent {
                car_id: car_id.to_owned(),
                skin_id: pool.get(i % pool.len().max(1)).map(|s| s.to_string()).unwrap_or_default(),
                driver_name: name.to_owned(),
                nationality: nat.to_owned(),
            }
        })
        .collect()
}

fn session(file: &mut IniFile, name: &str, kind: SessionType, minutes: u32, spawn: &str) {
    let s = file.section_mut("SESSION_0");
    s.set("NAME", name);
    s.set("TYPE", kind as u8);
    s.set("DURATION_MINUTES", minutes);
    s.set("SPAWN_SET", spawn);
}

/// Mirrors `Game.ClearUpIniFile`: turns off replay/online/benchmark modes and
/// removes per-session sections left by previous launches.
pub fn clear_up(file: &mut IniFile) {
    file.section_mut("BENCHMARK").set_bool("ACTIVE", false);
    file.section_mut("REPLAY").set_bool("ACTIVE", false);
    let remote = file.section_mut("REMOTE");
    remote.set_bool("ACTIVE", false);
    // A server password from the last online session must not stay in the file.
    remote.set("PASSWORD", "");
    remote.remove("__FEATURES");
    remote.remove("__CM_EXTENDED");
    file.section_mut("RESTART").set_bool("ACTIVE", false);
    file.section_mut("__PREVIEW_GENERATION").set_bool("ACTIVE", false);
    let lighting = file.section_mut("LIGHTING");
    for k in ["__CM_WEATHER_TYPE", "__CM_WEATHER_CONTROLLER", "__CM_DATE", "__CM_DATE_USE_TIME", "__CM_WEATHER_HUMIDITY", "__CM_WEATHER_PRESSURE", "__CM_UNCLAMPED_SUN_ANGLE"] {
        lighting.remove(k);
    }
    file.section_mut("RACE").remove("__CM_CUSTOM_MODE");
    file.section_mut("OPTIONS").remove("__BACKGROUND_IMAGE");
    file.remove_numbered("CAR", 1);
    file.remove_numbered("SESSION", 0);
    file.remove_numbered("CONDITION", 0);
    file.remove_section("EVENT");
    file.remove_section("SPECIAL_EVENT");
}

/// Applies a quick-drive setup on top of an existing race.ini (so unknown
/// keys AC or CSP rely on are kept).
pub fn apply(file: &mut IniFile, q: &QuickDrive) {
    clear_up(file);

    let header = file.section_mut("HEADER");
    header.set("VERSION", 2);
    header.set("__CM_FEATURE_SET", 2);
    file.section_mut("LAP_INVALIDATOR").set("ALLOWED_TYRES_OUT", -1);

    let race = file.section_mut("RACE");
    race.set("MODEL", q.car_id.to_lowercase());
    race.set("MODEL_CONFIG", "");
    race.set("SKIN", &q.skin_id);
    race.set("TRACK", q.track_id.to_lowercase());
    race.set("CONFIG_TRACK", q.layout_id.as_deref().unwrap_or("").to_lowercase());
    race.set("CARS", 1);
    race.set("AI_LEVEL", 100);
    race.set_bool("DRIFT_MODE", false);
    race.set_bool("FIXED_SETUP", q.fixed_setup);
    race.set_bool("PENALTIES", q.penalties);
    race.set("JUMP_START_PENALTY", 0);
    race.remove("RACE_LAPS");

    let car0 = file.section_mut("CAR_0");
    *car0 = crate::ini::Section::new("CAR_0");
    car0.set("SETUP", "");
    car0.set("SKIN", q.skin_id.to_lowercase());
    car0.set("MODEL", "-");
    car0.set("MODEL_CONFIG", "");
    car0.set("BALLAST", 0);
    car0.set("RESTRICTOR", 0);
    car0.set("DRIVER_NAME", &q.driver_name);
    car0.set("NATIONALITY", &q.nationality);
    car0.set("NATION_CODE", &q.nationality);
    file.section_mut("OPTIONS").set_bool("USE_MPH", q.use_mph);

    let ghost = file.section_mut("GHOST_CAR");
    ghost.set_bool("RECORDING", false);
    ghost.set_bool("PLAYING", false);
    ghost.set("SECONDS_ADVANTAGE", 0);
    ghost.set_bool("LOAD", false);
    ghost.set("FILE", "");
    ghost.set_bool("ENABLED", false);

    let (virtual_laps, max_laps, starting_laps) = match q.mode {
        Mode::Practice { .. } => (10, 30, 0),
        _ => (10, 1, 1),
    };
    let groove = file.section_mut("GROOVE");
    groove.set("VIRTUAL_LAPS", virtual_laps);
    groove.set("MAX_LAPS", max_laps);
    groove.set("STARTING_LAPS", starting_laps);

    match &q.mode {
        Mode::Practice { minutes } => session(file, "Practice", SessionType::Practice, *minutes, "PIT"),
        Mode::Hotlap => session(file, "Hotlap", SessionType::Hotlap, 0, "HOTLAP_START"),
        Mode::Race { laps, ai_level, starting_position, .. } => {
            let race = file.section_mut("RACE");
            race.set("CARS", q.opponents.len() + 1);
            race.set("AI_LEVEL", ai_level);
            race.set("RACE_LAPS", laps);
            session(file, "Quick Race", SessionType::Race, 0, "START");
            let s = file.section_mut("SESSION_0");
            s.set("LAPS", laps);
            s.set("STARTING_POSITION", (*starting_position).clamp(1, q.opponents.len() as u32 + 1));
            for (i, o) in q.opponents.iter().enumerate() {
                let c = file.section_mut(&format!("CAR_{}", i + 1));
                c.set("MODEL", o.car_id.to_lowercase());
                c.set("SKIN", o.skin_id.to_lowercase());
                c.set("SETUP", "");
                c.set("MODEL_CONFIG", "");
                c.set("AI_LEVEL", ai_level);
                c.set("AI_AGGRESSION", 0);
                c.set("DRIVER_NAME", &o.driver_name);
                c.set("BALLAST", 0);
                c.set("RESTRICTOR", 0);
                c.set("NATIONALITY", &o.nationality);
                c.set("NATION_CODE", &o.nationality);
            }
        }
    }

    let c = &q.conditions;
    let secs = c.time_seconds as f64;
    let temp = file.section_mut("TEMPERATURE");
    temp.set("AMBIENT", format!("{:.0}", c.ambient_c));
    temp.set("ROAD", format!("{:.0}", road_temperature(secs, c.ambient_c, 1.0)));
    let lighting = file.section_mut("LIGHTING");
    lighting.set("SUN_ANGLE", format!("{:.2}", sun_angle(secs)));
    lighting.set("TIME_MULT", format!("{:.1}", c.time_multiplier));
    lighting.set("CLOUD_SPEED", "0.200");
    file.section_mut("WEATHER").set("NAME", &c.weather_id);
    let wind = file.section_mut("WIND");
    wind.set("SPEED_KMH_MIN", 0);
    wind.set("SPEED_KMH_MAX", 0);
    wind.set("DIRECTION_DEG", 0);

    // "Optimum" track grip, CM's default preset.
    let dynamic = file.section_mut("DYNAMIC_TRACK");
    dynamic.set("SESSION_START", 100);
    dynamic.set("RANDOMNESS", 0);
    dynamic.set("LAP_GAIN", 1);
    dynamic.set("SESSION_TRANSFER", 100);
}
