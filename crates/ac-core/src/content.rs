//! Scanning installed cars, tracks and weather.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::ai_line::{self, RacingLine};
use crate::ini::IniFile;
use crate::json;

#[derive(Debug, Clone)]
pub struct Skin {
    pub id: String,
    pub dir: PathBuf,
    pub name: String,
    pub number: Option<String>,
    pub preview: Option<PathBuf>,
    pub livery: Option<PathBuf>,
    pub priority: i64,
}

#[derive(Debug, Clone)]
pub struct Car {
    pub id: String,
    pub dir: PathBuf,
    pub name: String,
    pub brand: Option<String>,
    pub class: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub year: Option<String>,
    pub country: Option<String>,
    pub author: Option<String>,
    /// `specs` from ui_car.json in file order (bhp, torque, weight, ...).
    pub specs: Vec<(String, String)>,
    pub skins: Vec<Skin>,
    pub badge: Option<PathBuf>,
    pub has_data_acd: bool,
    pub has_data_dir: bool,
    /// Set when ui_car.json was missing or unreadable.
    pub ui_error: Option<String>,
}

impl Car {
    /// Display group used by both the brand picker and its filtering predicate.
    pub fn brand_name(&self) -> &str {
        self.brand
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .unwrap_or("Unknown")
    }

    pub fn preview(&self) -> Option<&Path> {
        self.skins.iter().find_map(|s| s.preview.as_deref())
    }

    pub fn search_text(&self) -> String {
        let mut s = format!("{} {} {}", self.id, self.name, self.brand.as_deref().unwrap_or(""));
        if let Some(c) = &self.class {
            s.push(' ');
            s.push_str(c);
        }
        for t in &self.tags {
            s.push(' ');
            s.push_str(t);
        }
        s.to_lowercase()
    }
}

#[derive(Debug, Clone)]
pub struct TrackLayout {
    /// `None` for single-layout tracks (CONFIG_TRACK left empty).
    pub layout_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub length: Option<String>,
    pub pitboxes: Option<u32>,
    pub tags: Vec<String>,
    pub preview: Option<PathBuf>,
    pub outline: Option<PathBuf>,
    /// AI racing line from `ai/fast_lane.ai`, drawn instead of `outline` when present.
    pub racing_line: Option<Arc<RacingLine>>,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub id: String,
    pub dir: PathBuf,
    pub layouts: Vec<TrackLayout>,
}

/// One selectable track + layout combination.
#[derive(Debug, Clone)]
pub struct TrackEntry {
    pub track_id: String,
    pub layout: TrackLayout,
}

impl TrackEntry {
    pub fn key(&self) -> String {
        match &self.layout.layout_id {
            Some(l) => format!("{}/{}", self.track_id, l),
            None => self.track_id.clone(),
        }
    }

    pub fn search_text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.key(),
            self.layout.name,
            self.layout.country.as_deref().unwrap_or(""),
            self.layout.city.as_deref().unwrap_or("")
        )
        .to_lowercase()
    }
}

#[derive(Debug, Clone)]
pub struct Weather {
    pub id: String,
    pub name: String,
}

/// Case-insensitive order that compares digit runs as numbers, so
/// "Drag 200m" sorts before "Drag 1000m".
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (na, nb) = (take(&mut a), take(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

fn existing(p: PathBuf) -> Option<PathBuf> {
    p.is_file().then_some(p)
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort_by_key(|p| p.file_name().map(|n| n.to_ascii_lowercase()));
    v
}

fn dir_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn tags(v: &Value) -> Vec<String> {
    v.get("tags")
        .and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_owned()).collect())
        .unwrap_or_default()
}

/// AC descriptions use a few HTML tags; turn them into plain text.
pub fn plain_text(s: &str) -> String {
    let s = s.replace("<br/>", "\n").replace("<br>", "\n").replace("<br />", "\n");
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.lines().map(str::trim).collect::<Vec<_>>().join("\n").trim().to_owned()
}

fn load_skin(dir: PathBuf) -> Skin {
    let id = dir_name(&dir);
    let ui = json::parse_file(&dir.join("ui_skin.json")).ok();
    let name = ui
        .as_ref()
        .and_then(|v| json::text(v, "skinname"))
        .unwrap_or_else(|| id.replace('_', " "));
    Skin {
        number: ui.as_ref().and_then(|v| json::text(v, "number")).filter(|n| n != "0"),
        priority: ui.as_ref().and_then(|v| v.get("priority")).and_then(|p| p.as_i64()).unwrap_or(0),
        preview: existing(dir.join("preview.jpg")).or_else(|| existing(dir.join("preview.png"))),
        livery: existing(dir.join("livery.png")),
        name,
        id,
        dir,
    }
}

pub fn load_car(dir: &Path) -> Car {
    let id = dir_name(dir);
    let ui_path = dir.join("ui").join("ui_car.json");
    let (ui, ui_error) = match json::parse_file(&ui_path) {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let get = |k: &str| ui.as_ref().and_then(|v| json::text(v, k));

    let specs = ui
        .as_ref()
        .and_then(|v| v.get("specs"))
        .and_then(|s| s.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| match v {
                    Value::String(s) if !s.trim().is_empty() && s.trim() != "--" => Some((k.clone(), s.trim().to_owned())),
                    Value::Number(n) => Some((k.clone(), n.to_string())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let mut skins: Vec<Skin> = subdirs(&dir.join("skins")).into_iter().map(load_skin).collect();
    skins.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.to_lowercase().cmp(&b.id.to_lowercase())));

    Car {
        name: get("name").unwrap_or_else(|| id.clone()),
        brand: get("brand"),
        class: get("class"),
        description: get("description").map(|d| plain_text(&d)),
        tags: ui.as_ref().map(tags).unwrap_or_default(),
        year: get("year"),
        country: get("country"),
        author: get("author"),
        specs,
        skins,
        badge: existing(dir.join("ui").join("badge.png")),
        has_data_acd: dir.join("data.acd").is_file(),
        has_data_dir: dir.join("data").is_dir(),
        ui_error,
        id,
        dir: dir.to_path_buf(),
    }
}

/// Drivable cars. Folders with neither `data.acd` nor `data/` (placeholders
/// for DLC that isn't owned) are skipped.
pub fn scan_cars(ac_root: &Path) -> Vec<Car> {
    let mut cars: Vec<Car> = subdirs(&ac_root.join("content").join("cars"))
        .iter()
        .map(|d| load_car(d))
        .filter(|c| c.has_data_acd || c.has_data_dir)
        .collect();
    cars.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    cars
}

fn load_layout(track_id: &str, ui_dir: &Path, ai_dir: &Path, layout_id: Option<String>) -> Option<TrackLayout> {
    let ui_json = ui_dir.join("ui_track.json");
    if !ui_json.is_file() {
        return None;
    }
    let v = json::parse_file(&ui_json).ok();
    let get = |k: &str| v.as_ref().and_then(|v| json::text(v, k));
    Some(TrackLayout {
        name: get("name").unwrap_or_else(|| match &layout_id {
            Some(l) => format!("{track_id} ({l})"),
            None => track_id.to_owned(),
        }),
        description: get("description").map(|d| plain_text(&d)),
        country: get("country"),
        city: get("city"),
        length: get("length"),
        pitboxes: get("pitboxes").and_then(|p| p.trim().parse().ok()),
        tags: v.as_ref().map(tags).unwrap_or_default(),
        preview: existing(ui_dir.join("preview.png")),
        outline: existing(ui_dir.join("outline.png")),
        racing_line: ai_line::read(&ai_dir.join("fast_lane.ai")).map(Arc::new),
        layout_id,
    })
}

pub fn load_track(dir: &Path) -> Track {
    let id = dir_name(dir);
    let ui = dir.join("ui");
    let mut layouts = Vec::new();
    if let Some(l) = load_layout(&id, &ui, &dir.join("ai"), None) {
        layouts.push(l);
    }
    // Multi-layout tracks keep one ui folder per layout; the layout must also
    // exist as a folder next to `ui` (models_<layout>.ini sits at the root).
    for sub in subdirs(&ui) {
        let layout_id = dir_name(&sub);
        if !dir.join(&layout_id).is_dir() {
            continue;
        }
        if let Some(l) = load_layout(&id, &sub, &dir.join(&layout_id).join("ai"), Some(layout_id)) {
            layouts.push(l);
        }
    }
    Track { id, dir: dir.to_path_buf(), layouts }
}

pub fn scan_tracks(ac_root: &Path) -> Vec<Track> {
    subdirs(&ac_root.join("content").join("tracks"))
        .iter()
        .map(|d| load_track(d))
        .filter(|t| !t.layouts.is_empty())
        .collect()
}

pub fn track_entries(tracks: &[Track]) -> Vec<TrackEntry> {
    let mut v: Vec<TrackEntry> = tracks
        .iter()
        .flat_map(|t| {
            t.layouts.iter().map(|l| TrackEntry { track_id: t.id.clone(), layout: l.clone() })
        })
        .collect();
    v.sort_by(|a, b| natural_cmp(&a.layout.name, &b.layout.name));
    v
}

pub fn scan_weather(ac_root: &Path) -> Vec<Weather> {
    subdirs(&ac_root.join("content").join("weather"))
        .into_iter()
        .filter(|d| d.join("weather.ini").is_file())
        .map(|d| {
            let id = dir_name(&d);
            let ini = IniFile::load(&d.join("weather.ini")).unwrap_or_default();
            let name = ini
                .get("LAUNCHER", "NAME")
                .map(str::to_owned)
                .unwrap_or_else(|| pretty_weather_name(&id));
            Weather { id, name }
        })
        .collect()
}

/// `4_mid_clear` -> `Mid Clear`.
fn pretty_weather_name(id: &str) -> String {
    let trimmed = id.trim_start_matches(|c: char| c.is_ascii_digit() || c == '_');
    trimmed
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Human labels for common ui_car.json spec keys.
pub fn spec_label(key: &str) -> &str {
    match key {
        "bhp" => "Power",
        "torque" => "Torque",
        "weight" => "Weight",
        "topspeed" => "Top speed",
        "acceleration" => "Acceleration",
        "pwratio" => "Power/weight",
        "range" => "Range",
        other => other,
    }
}

/// Groups cars by brand for quick filtering.
pub fn brands(cars: &[Car]) -> BTreeMap<String, usize> {
    let mut groups = BTreeMap::<String, (String, usize)>::new();
    for car in cars {
        let name = car.brand_name();
        let group = groups
            .entry(name.to_lowercase())
            .or_insert_with(|| (name.to_owned(), 0));
        group.1 += 1;
    }
    groups.into_values().collect()
}
