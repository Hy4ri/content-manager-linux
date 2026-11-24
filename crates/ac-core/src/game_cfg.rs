//! In-game options Assetto Corsa reads from `Documents/Assetto Corsa/cfg`:
//! video, audio, assists, gameplay and controls (keyboard bindings).
//!
//! AC prefers the Documents copy of each file and only falls back to the
//! defaults in `<ac_root>/cfg`. Users' Documents files are often partial, so
//! a file is loaded as the defaults with the Documents values on top, and
//! saved back to Documents in full (like Content Manager does).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::ini::IniFile;
use crate::paths;

pub const FILES: [&str; 5] = ["video.ini", "audio.ini", "assists.ini", "gameplay.ini", "controls.ini"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Video,
    Audio,
    Assists,
    Gameplay,
    Controls,
    Keys,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Video, Tab::Audio, Tab::Assists, Tab::Gameplay, Tab::Controls, Tab::Keys];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Video => "Video",
            Tab::Audio => "Audio",
            Tab::Assists => "Assists",
            Tab::Gameplay => "Gameplay",
            Tab::Controls => "Controls",
            Tab::Keys => "Keys",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Kind {
    /// `0` / `1`.
    Toggle,
    /// Fixed set of `(value, label)`.
    Choice(&'static [(&'static str, &'static str)]),
    /// Post-processing filter, choices are the `system/cfg/ppfilters/*.ini` names.
    Filter,
    /// Numeric value shown as `value * scale` with a suffix.
    Number { min: f64, max: f64, step: f64, scale: f64, suffix: &'static str },
}

#[derive(Clone, Debug)]
pub struct Setting {
    pub tab: Tab,
    pub group: &'static str,
    pub file: &'static str,
    pub section: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
}

const fn num(min: f64, max: f64, step: f64) -> Kind {
    Kind::Number { min, max, step, scale: 1., suffix: "" }
}
const fn pct(max: f64, step: f64) -> Kind {
    // Stored as 0..max, shown as 0..max*100 %.
    Kind::Number { min: 0., max, step, scale: 100., suffix: "%" }
}
const fn pct_int(step: f64) -> Kind {
    Kind::Number { min: 0., max: 100., step, scale: 1., suffix: "%" }
}

const OFF_2_4_8: &[(&str, &str)] = &[("1", "Off"), ("2", "2×"), ("4", "4×"), ("8", "8×")];
const ANISO: &[(&str, &str)] = &[("0", "Off"), ("2", "2×"), ("4", "4×"), ("8", "8×"), ("16", "16×")];
const SHADOWS: &[(&str, &str)] = &[("512", "512"), ("1024", "1024"), ("2048", "2048"), ("4096", "4096")];
const CUBEMAP: &[(&str, &str)] = &[("0", "Off"), ("256", "256"), ("512", "512"), ("1024", "1024"), ("2048", "2048")];
const MIRROR: &[(&str, &str)] = &[("256", "256"), ("512", "512"), ("1024", "1024")];
const ASSIST_LEVEL: &[(&str, &str)] = &[("0", "Off"), ("1", "Factory"), ("2", "On")];
const INPUT: &[(&str, &str)] = &[("KEYBOARD", "Keyboard"), ("X360", "Gamepad"), ("WHEEL", "Wheel")];

macro_rules! s {
    ($tab:ident, $group:literal, $file:literal, $section:literal, $key:literal, $label:literal, $kind:expr) => {
        Setting { tab: Tab::$tab, group: $group, file: $file, section: $section, key: $key, label: $label, kind: $kind }
    };
}

pub fn settings() -> Vec<Setting> {
    vec![
        s!(Video, "Display", "video.ini", "VIDEO", "FULLSCREEN", "Fullscreen", Kind::Toggle),
        s!(Video, "Display", "video.ini", "VIDEO", "VSYNC", "V-sync", Kind::Toggle),
        s!(Video, "Quality", "video.ini", "VIDEO", "AASAMPLES", "Anti-aliasing", Kind::Choice(OFF_2_4_8)),
        s!(Video, "Quality", "video.ini", "VIDEO", "ANISOTROPIC", "Anisotropic", Kind::Choice(ANISO)),
        s!(Video, "Quality", "video.ini", "VIDEO", "SHADOW_MAP_SIZE", "Shadows", Kind::Choice(SHADOWS)),
        s!(Video, "Quality", "video.ini", "ASSETTOCORSA", "WORLD_DETAIL", "World detail", num(0., 5., 1.)),
        s!(Video, "Quality", "video.ini", "EFFECTS", "SMOKE", "Smoke", num(0., 5., 1.)),
        s!(Video, "Quality", "video.ini", "EFFECTS", "MOTION_BLUR", "Motion blur", num(0., 12., 1.)),
        s!(Video, "Quality", "video.ini", "EFFECTS", "RENDER_SMOKE_IN_MIRROR", "Smoke in mirrors", Kind::Toggle),
        s!(Video, "Reflections", "video.ini", "CUBEMAP", "SIZE", "Resolution", Kind::Choice(CUBEMAP)),
        s!(Video, "Reflections", "video.ini", "CUBEMAP", "FACES_PER_FRAME", "Faces per frame", num(0., 6., 1.)),
        s!(Video, "Reflections", "video.ini", "CUBEMAP", "FARPLANE", "Render distance", Kind::Number { min: 100., max: 2000., step: 100., scale: 1., suffix: " m" }),
        s!(Video, "Reflections", "video.ini", "MIRROR", "SIZE", "Mirror resolution", Kind::Choice(MIRROR)),
        s!(Video, "Reflections", "video.ini", "MIRROR", "HQ", "HQ mirrors", Kind::Toggle),
        s!(Video, "Post-processing", "video.ini", "POST_PROCESS", "ENABLED", "Enabled", Kind::Toggle),
        s!(Video, "Post-processing", "video.ini", "POST_PROCESS", "FILTER", "Filter", Kind::Filter),
        s!(Video, "Post-processing", "video.ini", "POST_PROCESS", "QUALITY", "Quality", num(0., 5., 1.)),
        s!(Video, "Post-processing", "video.ini", "POST_PROCESS", "GLARE", "Glare", num(0., 5., 1.)),
        s!(Video, "Post-processing", "video.ini", "POST_PROCESS", "DOF", "Depth of field", num(0., 5., 1.)),
        s!(Video, "Post-processing", "video.ini", "POST_PROCESS", "FXAA", "FXAA", Kind::Toggle),
        s!(Video, "Post-processing", "video.ini", "POST_PROCESS", "HEAT_SHIMMER", "Heat shimmer", Kind::Toggle),
        s!(Video, "Post-processing", "video.ini", "POST_PROCESS", "RAYS_OF_GOD", "Sun rays", Kind::Toggle),
        s!(Video, "Post-processing", "video.ini", "SATURATION", "LEVEL", "Saturation", Kind::Number { min: 0., max: 200., step: 10., scale: 1., suffix: "%" }),
        s!(Video, "Cockpit", "video.ini", "ASSETTOCORSA", "HIDE_ARMS", "Hide arms", Kind::Toggle),
        s!(Video, "Cockpit", "video.ini", "ASSETTOCORSA", "HIDE_STEER", "Hide wheel", Kind::Toggle),
        s!(Video, "Cockpit", "video.ini", "ASSETTOCORSA", "LOCK_STEER", "Lock wheel", Kind::Toggle),
        s!(Audio, "Levels", "audio.ini", "LEVELS", "MASTER", "Master", pct(1., 0.05)),
        s!(Audio, "Levels", "audio.ini", "LEVELS", "ENGINE", "Engine", pct(1., 0.05)),
        s!(Audio, "Levels", "audio.ini", "LEVELS", "TYRES", "Tyres", pct(1., 0.05)),
        s!(Audio, "Levels", "audio.ini", "LEVELS", "SURFACES", "Surfaces", pct(1., 0.05)),
        s!(Audio, "Levels", "audio.ini", "LEVELS", "DIRT_BOTTOM", "Dirt", pct(1., 0.05)),
        s!(Audio, "Levels", "audio.ini", "LEVELS", "WIND", "Wind", pct(1., 0.05)),
        s!(Audio, "Levels", "audio.ini", "LEVELS", "OPPONENTS", "Opponents", pct(1., 0.05)),
        s!(Audio, "Levels", "audio.ini", "LEVELS", "UISOUNDS", "UI", pct(1., 0.05)),
        s!(Audio, "Levels", "audio.ini", "LEVELS", "MUSIC", "Music", pct(1., 0.05)),
        s!(Assists, "Driving aids", "assists.ini", "ASSISTS", "ABS", "ABS", Kind::Choice(ASSIST_LEVEL)),
        s!(Assists, "Driving aids", "assists.ini", "ASSISTS", "TRACTION_CONTROL", "Traction control", Kind::Choice(ASSIST_LEVEL)),
        s!(Assists, "Driving aids", "assists.ini", "ASSISTS", "STABILITY_CONTROL", "Stability control", pct_int(10.)),
        s!(Assists, "Driving aids", "assists.ini", "ASSISTS", "AUTO_SHIFTER", "Auto shifter", Kind::Toggle),
        s!(Assists, "Driving aids", "assists.ini", "ASSISTS", "AUTO_CLUTCH", "Auto clutch", Kind::Toggle),
        s!(Assists, "Driving aids", "assists.ini", "ASSISTS", "AUTO_BLIP", "Auto blip", Kind::Toggle),
        s!(Assists, "Driving aids", "assists.ini", "ASSISTS", "AUTO_BRAKE", "Auto brake", Kind::Toggle),
        s!(Assists, "Driving aids", "assists.ini", "ASSISTS", "IDEAL_LINE", "Ideal line", Kind::Toggle),
        s!(Assists, "Realism", "assists.ini", "ASSISTS", "DAMAGE", "Damage", pct_int(10.)),
        s!(Assists, "Realism", "assists.ini", "ASSISTS", "VISUALDAMAGE", "Visual damage", Kind::Toggle),
        s!(Assists, "Realism", "assists.ini", "ASSISTS", "FUEL_RATE", "Fuel rate", Kind::Number { min: 0., max: 5., step: 1., scale: 1., suffix: "×" }),
        s!(Assists, "Realism", "assists.ini", "ASSISTS", "TYRE_WEAR", "Tyre wear", Kind::Number { min: 0., max: 5., step: 1., scale: 1., suffix: "×" }),
        s!(Assists, "Realism", "assists.ini", "ASSISTS", "TYRE_BLANKETS", "Tyre blankets", Kind::Toggle),
        s!(Assists, "Realism", "assists.ini", "ASSISTS", "SLIPSTREAM", "Slipstream", Kind::Number { min: 0., max: 5., step: 1., scale: 1., suffix: "×" }),
        s!(Gameplay, "HUD", "gameplay.ini", "GUI", "SHOW_SECTORS", "Sector times", Kind::Toggle),
        s!(Gameplay, "HUD", "gameplay.ini", "TIME_DIFFERENCE", "IS_ACTIVE", "Time difference", Kind::Toggle),
        s!(Gameplay, "HUD", "gameplay.ini", "DRIVER_NAME_DISPLAYER", "IS_ACTIVE", "Driver names", Kind::Toggle),
        s!(Gameplay, "HUD", "gameplay.ini", "DAMAGE_DISPLAYER", "IS_ACTIVE", "Damage display", Kind::Toggle),
        s!(Gameplay, "HUD", "gameplay.ini", "VIRTUAL_MIRROR", "ACTIVE", "Virtual mirror", Kind::Toggle),
        s!(Gameplay, "HUD", "gameplay.ini", "PIT_INDICATOR", "ONLY_IN_PITLINE", "Pit marker only in pit lane", Kind::Toggle),
        s!(Gameplay, "Other", "gameplay.ini", "PYTHON", "ENABLE_PYTHON", "Python apps", Kind::Toggle),
        s!(Gameplay, "Other", "gameplay.ini", "PHYSICS", "ENABLE_CAR_COLLISIONS", "Car collisions", Kind::Toggle),
        s!(Controls, "Input", "controls.ini", "HEADER", "INPUT_METHOD", "Input method", Kind::Choice(INPUT)),
        s!(Controls, "Input", "controls.ini", "ADVANCED", "COMBINE_WITH_KEYBOARD_CONTROL", "Also use keyboard", Kind::Toggle),
        s!(Controls, "Keyboard", "controls.ini", "KEYBOARD", "STEERING_SPEED", "Steering speed", num(0.1, 5., 0.05)),
        s!(Controls, "Keyboard", "controls.ini", "KEYBOARD", "STEERING_OPPOSITE_DIRECTION_SPEED", "Counter-steer speed", num(0.1, 5., 0.05)),
        s!(Controls, "Keyboard", "controls.ini", "KEYBOARD", "STEER_RESET_SPEED", "Return speed", num(0.1, 5., 0.05)),
        s!(Controls, "Keyboard", "controls.ini", "KEYBOARD", "MOUSE_STEER", "Mouse steering", Kind::Toggle),
        s!(Controls, "Gamepad", "controls.ini", "X360", "STEER_SPEED", "Steering speed", num(0., 1., 0.05)),
        s!(Controls, "Gamepad", "controls.ini", "X360", "STEER_GAMMA", "Steering gamma", num(1., 5., 0.1)),
        s!(Controls, "Gamepad", "controls.ini", "X360", "STEER_DEADZONE", "Deadzone", pct(0.5, 0.01)),
        s!(Controls, "Gamepad", "controls.ini", "X360", "RUMBLE_INTENSITY", "Rumble", pct(1., 0.05)),
        s!(Controls, "Wheel", "controls.ini", "STEER", "LOCK", "Steering lock", Kind::Number { min: 180., max: 1080., step: 10., scale: 1., suffix: "°" }),
        s!(Controls, "Wheel", "controls.ini", "STEER", "FF_GAIN", "Force feedback", pct(2., 0.05)),
        s!(Controls, "Wheel", "controls.ini", "STEER", "FILTER_FF", "FF filter", pct(1., 0.05)),
        s!(Controls, "Wheel", "controls.ini", "FF_TWEAKS", "MIN_FF", "Minimum FF", pct(0.5, 0.01)),
        s!(Controls, "Wheel", "controls.ini", "FF_ENHANCEMENT", "CURBS", "Kerb effects", pct(1., 0.05)),
        s!(Controls, "Wheel", "controls.ini", "FF_ENHANCEMENT", "ROAD", "Road effects", pct(1., 0.05)),
        s!(Controls, "Wheel", "controls.ini", "FF_ENHANCEMENT", "SLIPS", "Slip effects", pct(1., 0.05)),
    ]
}

/// A keyboard binding: `section` in controls.ini, value in `key`.
#[derive(Clone, Copy, Debug)]
pub struct Bind {
    pub group: &'static str,
    pub section: &'static str,
    pub key: &'static str,
    pub label: &'static str,
}

const fn b(group: &'static str, section: &'static str, label: &'static str) -> Bind {
    Bind { group, section, key: "KEY", label }
}

pub const BINDS: &[Bind] = &[
    Bind { group: "Driving", section: "KEYBOARD", key: "GAS", label: "Throttle" },
    Bind { group: "Driving", section: "KEYBOARD", key: "BRAKE", label: "Brake" },
    Bind { group: "Driving", section: "KEYBOARD", key: "LEFT", label: "Steer left" },
    Bind { group: "Driving", section: "KEYBOARD", key: "RIGHT", label: "Steer right" },
    b("Driving", "GEARUP", "Shift up"),
    b("Driving", "GEARDN", "Shift down"),
    b("Driving", "HANDBRAKE", "Handbrake"),
    b("Driving", "CLUTCH", "Clutch"),
    b("Driving", "STARTER", "Starter"),
    b("Driving", "AUTO_SHIFTER", "Auto shifter"),
    b("Driving", "ABS", "ABS on/off"),
    b("Driving", "TRACTION_CONTROL", "TC on/off"),
    b("Car", "ACTION_HEADLIGHTS", "Headlights"),
    b("Car", "ACTION_HEADLIGHTS_FLASH", "Flash lights"),
    b("Car", "ACTION_HORN", "Horn"),
    b("Car", "DRS", "DRS"),
    b("Car", "KERS", "KERS"),
    b("Car", "ABSUP", "ABS +"),
    b("Car", "ABSDN", "ABS −"),
    b("Car", "TCUP", "TC +"),
    b("Car", "TCDN", "TC −"),
    b("Car", "BALANCEUP", "Brake bias +"),
    b("Car", "BALANCEDN", "Brake bias −"),
    b("Car", "TURBOUP", "Turbo +"),
    b("Car", "TURBODN", "Turbo −"),
    b("Car", "ENGINE_BRAKE_UP", "Engine brake +"),
    b("Car", "ENGINE_BRAKE_DN", "Engine brake −"),
    b("Car", "MGUK_DELIVERY_UP", "MGU-K delivery +"),
    b("Car", "MGUK_DELIVERY_DN", "MGU-K delivery −"),
    b("Car", "MGUK_RECOVERY_UP", "MGU-K recovery +"),
    b("Car", "MGUK_RECOVERY_DN", "MGU-K recovery −"),
    b("Car", "MGUH_MODE", "MGU-H mode"),
    b("View", "GLANCELEFT", "Look left"),
    b("View", "GLANCERIGHT", "Look right"),
    b("View", "GLANCEBACK", "Look back"),
    b("View", "ACTION_CHANGE_CAMERA", "Change camera"),
    b("View", "ACTION_CELEBRATE", "Celebrate"),
    b("View", "ACTION_CLAIM", "Complain"),
    b("View", "HIDE_APPS", "Hide apps"),
    b("View", "IDEAL_LINE", "Ideal line"),
    b("View", "SHOW_DAMAGE", "Show damage"),
    b("View", "HIDE_DAMAGE", "Hide damage"),
    b("View", "DRIVER_NAMES", "Driver names"),
    b("Session", "RESET_RACE", "Restart session"),
    b("Session", "ACTIVATE_AI", "AI drives for me"),
    b("Session", "MOUSE_STEERING", "Mouse steering"),
    b("Replay", "START_REPLAY", "Start replay"),
    b("Replay", "PAUSE_REPLAY", "Pause replay"),
    b("Replay", "FFWD", "Fast forward"),
    b("Replay", "REV", "Rewind"),
    b("Replay", "SLOWMO", "Slow motion"),
    b("Replay", "PREVIOUS_LAP", "Previous lap"),
    b("Replay", "NEXT_LAP", "Next lap"),
    b("Replay", "PREVIOUS_CAR", "Previous car"),
    b("Replay", "NEXT_CAR", "Next car"),
    b("Replay", "PLAYER_CAR", "Back to my car"),
];

/// Gamepad buttons as AC's XInput names, with their PlayStation labels.
pub const PAD_BUTTONS: &[(&str, &str)] = &[
    ("A", "Cross"), ("B", "Circle"), ("X", "Square"), ("Y", "Triangle"), ("LSHOULDER", "L1"), ("RSHOULDER", "R1"),
    ("BACK", "Share"), ("START", "Options"), ("LEFT_THUMB", "L3"), ("RIGHT_THUMB", "R3"), ("DPAD_UP", "D-pad up"),
    ("DPAD_DOWN", "D-pad down"), ("DPAD_LEFT", "D-pad left"), ("DPAD_RIGHT", "D-pad right"),
];

pub fn pad_label(name: &str) -> &str {
    PAD_BUTTONS.iter().find(|(n, _)| *n == name).map(|(_, l)| *l).unwrap_or(name)
}

/// Sections whose `XBOXBUTTON` AC's X360 controller reads: the exact list it loads in
/// `acs.exe` (and the list Content Manager offers for pads). Anything else is keyboard-only.
const PAD_SECTIONS: &[&str] = &[
    "GEARUP", "GEARDN", "HANDBRAKE", "GLANCELEFT", "GLANCERIGHT", "GLANCEBACK", "ACTION_CHANGE_CAMERA",
    "ACTION_HEADLIGHTS", "ACTION_HORN", "BALANCEDN", "BALANCEUP", "KERS", "DRS", "ACTION_HEADLIGHTS_FLASH",
    "ABSDN", "ABSUP", "TCDN", "TCUP", "TURBODN", "TURBOUP", "ENGINE_BRAKE_UP", "ENGINE_BRAKE_DN",
    "MGUK_DELIVERY_UP", "MGUK_DELIVERY_DN", "MGUK_RECOVERY_UP", "MGUK_RECOVERY_DN", "MGUH_MODE",
];

impl Bind {
    /// Why this action has no pad button to bind, or `None` when AC reads one for it.
    /// Throttle, brake and steering are analog (R2, L2, left stick); the pad controller
    /// forces auto clutch, so a clutch button is never read.
    pub fn pad_note(&self) -> Option<&'static str> {
        if self.section == "KEYBOARD" {
            return Some(match self.key {
                "GAS" => "R2 (automatic)",
                "BRAKE" => "L2 (automatic)",
                _ => "Left stick (automatic)",
            });
        }
        if self.section == "CLUTCH" {
            return Some("Auto clutch on pad");
        }
        (!PAD_SECTIONS.contains(&self.section)).then_some("Keyboard only")
    }
}

/// Parses a Windows virtual-key code as AC writes it (`0x26`, `32`, `-1`, empty).
pub fn parse_vk(value: &str) -> Option<u32> {
    let v = value.trim();
    let n = match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => v.parse::<i64>().ok().filter(|n| *n > 0)? as u32,
    };
    (n > 0).then_some(n)
}

pub fn format_vk(vk: Option<u32>) -> String {
    match vk {
        Some(vk) => format!("0x{vk:02X}"),
        None => "-1".into(),
    }
}

const PUNCT: &[(&str, u32)] = &[
    (";", 0xBA), (":", 0xBA), ("=", 0xBB), ("+", 0xBB), (",", 0xBC), ("<", 0xBC), ("-", 0xBD), ("_", 0xBD),
    (".", 0xBE), (">", 0xBE), ("/", 0xBF), ("?", 0xBF), ("`", 0xC0), ("~", 0xC0), ("[", 0xDB), ("{", 0xDB),
    ("\\", 0xDC), ("|", 0xDC), ("]", 0xDD), ("}", 0xDD), ("'", 0xDE), ("\"", 0xDE),
];
const NAMED: &[(&str, u32)] = &[
    ("backspace", 0x08), ("tab", 0x09), ("enter", 0x0D), ("escape", 0x1B), ("space", 0x20), ("pageup", 0x21),
    ("pagedown", 0x22), ("end", 0x23), ("home", 0x24), ("left", 0x25), ("up", 0x26), ("right", 0x27),
    ("down", 0x28), ("insert", 0x2D), ("delete", 0x2E),
];
const SHIFTED_DIGITS: &str = ")!@#$%^&*(";

/// Windows virtual-key code for a key name as reported by GPUI (`"a"`, `"f5"`, `"left"`, `"space"`).
pub fn vk_from_key(key: &str) -> Option<u32> {
    let k = key.to_ascii_lowercase();
    if let Some(&(_, vk)) = NAMED.iter().chain(PUNCT).find(|(n, _)| *n == k) {
        return Some(vk);
    }
    let mut chars = k.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        if c.is_ascii_lowercase() {
            return Some(c.to_ascii_uppercase() as u32);
        }
        if c.is_ascii_digit() {
            return Some(c as u32);
        }
        if let Some(i) = SHIFTED_DIGITS.find(c) {
            return Some(0x30 + i as u32);
        }
    }
    let n: u32 = k.strip_prefix('f')?.parse().ok()?;
    (1..=24).contains(&n).then(|| 0x6F + n)
}

/// Human name for a virtual-key code.
pub fn vk_name(vk: u32) -> String {
    let named = match vk {
        0x08 => "Backspace", 0x09 => "Tab", 0x0D => "Enter", 0x10 => "Shift", 0x11 => "Ctrl", 0x12 => "Alt",
        0x1B => "Esc", 0x20 => "Space", 0x21 => "Page Up", 0x22 => "Page Down", 0x23 => "End", 0x24 => "Home",
        0x25 => "Left", 0x26 => "Up", 0x27 => "Right", 0x28 => "Down", 0x2D => "Insert", 0x2E => "Delete",
        0xA0 => "Left Shift", 0xA1 => "Right Shift", 0xA2 => "Left Ctrl", 0xA3 => "Right Ctrl",
        0xA4 => "Left Alt", 0xA5 => "Right Alt", 0x6A => "Num *", 0x6B => "Num +", 0x6D => "Num −",
        0x6E => "Num .", 0x6F => "Num /",
        _ => "",
    };
    if !named.is_empty() {
        return named.into();
    }
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk).unwrap().to_string(),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => match PUNCT.iter().find(|(_, v)| *v == vk) {
            Some((n, _)) => (*n).into(),
            None => format!("0x{vk:02X}"),
        },
    }
}

pub fn documents_cfg_dir(ac_root: &Path) -> Option<PathBuf> {
    Some(paths::documents_ac_dir(ac_root)?.join("cfg"))
}

/// Applies every key of `over` on top of `base`.
fn overlay(base: &mut IniFile, over: &IniFile) {
    for s in over.sections() {
        let dst = base.section_mut(&s.name);
        for (k, v) in s.entries() {
            dst.set(k, v);
        }
    }
}

/// Formats a number without trailing zeros (`1`, `0.45`).
pub fn fmt_number(v: f64) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

pub struct GameConfig {
    pub dir: PathBuf,
    files: BTreeMap<&'static str, IniFile>,
    saved: BTreeMap<&'static str, String>,
    pub filters: Vec<String>,
}

impl GameConfig {
    pub fn load(ac_root: &Path) -> Result<Self> {
        let dir = documents_cfg_dir(ac_root).context("Documents/Assetto Corsa not found (run the game from Steam once)")?;
        Self::load_from(&ac_root.join("cfg"), &dir, &ac_root.join("system/cfg/ppfilters"))
    }

    pub fn load_from(defaults: &Path, dir: &Path, ppfilters: &Path) -> Result<Self> {
        let mut files = BTreeMap::new();
        let mut saved = BTreeMap::new();
        for name in FILES {
            let mut ini = IniFile::load_or_default(&defaults.join(name))?;
            overlay(&mut ini, &IniFile::load_or_default(&dir.join(name))?);
            saved.insert(name, ini.to_string());
            files.insert(name, ini);
        }
        let mut filters: Vec<String> = fs::read_dir(ppfilters)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension()? != "ini" {
                    return None;
                }
                Some(p.file_stem()?.to_string_lossy().into_owned())
            })
            .collect();
        filters.sort_by(|a, b| crate::content::natural_cmp(a, b));
        Ok(Self { dir: dir.to_path_buf(), files, saved, filters })
    }

    pub fn get(&self, file: &str, section: &str, key: &str) -> Option<&str> {
        self.files.get(file)?.get(section, key)
    }

    pub fn set(&mut self, file: &'static str, section: &str, key: &str, value: impl ToString) {
        self.files.entry(file).or_default().section_mut(section).set(key, value);
    }

    pub fn number(&self, s: &Setting) -> f64 {
        self.get(s.file, s.section, s.key).and_then(|v| v.parse().ok()).unwrap_or(0.)
    }

    /// Moves a numeric setting by `steps` increments, clamped to its range.
    pub fn step(&mut self, s: &Setting, steps: i32) {
        let Kind::Number { min, max, step, .. } = s.kind else { return };
        let v = self.number(s);
        // Snap to the step grid so odd stored values (0.600999) land on it.
        let snapped = ((v - min) / step).round() * step + min;
        let next = (snapped + steps as f64 * step).clamp(min, max);
        self.set(s.file, s.section, s.key, fmt_number(next));
    }

    pub fn bind(&self, b: &Bind) -> Option<u32> {
        self.get("controls.ini", b.section, b.key).and_then(parse_vk)
    }

    pub fn set_bind(&mut self, b: &Bind, vk: Option<u32>) {
        self.set("controls.ini", b.section, b.key, format_vk(vk));
    }

    /// Other bindings that use the same key.
    pub fn conflicts(&self, b: &Bind) -> Vec<&'static str> {
        let Some(vk) = self.bind(b) else { return Vec::new() };
        BINDS
            .iter()
            .filter(|o| !(o.section == b.section && o.key == b.key) && self.bind(o) == Some(vk))
            .map(|o| o.label)
            .collect()
    }

    pub fn pad(&self, b: &Bind) -> Option<&str> {
        if b.pad_note().is_some() {
            return None;
        }
        self.get("controls.ini", b.section, "XBOXBUTTON").map(str::trim).filter(|v| !v.is_empty() && *v != "-1")
    }

    pub fn set_pad(&mut self, b: &Bind, button: Option<&str>) {
        self.set("controls.ini", b.section, "XBOXBUTTON", button.unwrap_or("-1"));
    }

    pub fn pad_conflicts(&self, b: &Bind) -> Vec<&'static str> {
        let Some(v) = self.pad(b) else { return Vec::new() };
        BINDS.iter().filter(|o| o.section != b.section && self.pad(o) == Some(v)).map(|o| o.label).collect()
    }

    pub fn dirty_files(&self) -> Vec<&'static str> {
        self.files.iter().filter(|(n, f)| self.saved.get(*n) != Some(&f.to_string())).map(|(n, _)| *n).collect()
    }

    /// Writes changed files atomically into the Documents cfg folder.
    pub fn save(&mut self) -> Result<Vec<&'static str>> {
        use std::io::Write;
        let dirty = self.dirty_files();
        fs::create_dir_all(&self.dir).with_context(|| format!("creating {}", self.dir.display()))?;
        for name in &dirty {
            let text = self.files[name].to_string();
            let path = self.dir.join(name);
            let mut temp = tempfile::NamedTempFile::new_in(&self.dir)?;
            temp.write_all(text.replace('\n', "\r\n").as_bytes())?;
            temp.as_file().sync_all()?;
            temp.persist(&path).with_context(|| format!("writing {}", path.display()))?;
            self.saved.insert(name, text);
        }
        Ok(dirty)
    }
}
