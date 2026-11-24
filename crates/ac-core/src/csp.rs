//! Custom Shaders Patch settings.
//!
//! CSP ships one annotated INI per module in `<ac_root>/extension/config`
//! (defaults) and reads user overrides from `Documents/Assetto Corsa/cfg/extension`
//! with the same file names. Every option carries its own UI description in the
//! trailing comment, which is what Content Manager builds its CSP pages from:
//!
//! ```ini
//! [ADVANCED_GAMEPADS]
//! ; PS4 DualShock, PS5 DualSense and Nintendo controllers integration
//! ENABLED=1 ; Active; 1 or 0
//! GAIN=1 ; Volume gain; from 0 to 2, perc.
//! MODE=1 ; Mode; Disabled is 0, Minimal is 1, Full volume is 2
//! ```
//!
//! The format follows `PythonAppConfigValue` in AcTools (MS-PL). Overrides are
//! written back with only the values that differ from the defaults, so CSP
//! updates still reach every option the user never touched.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::ini::IniFile;
use crate::paths;

/// One configurable CSP module (one `extension/config/*.ini`).
#[derive(Clone, Debug)]
pub struct Module {
    /// File name, e.g. `gamepad_fx.ini`.
    pub file: String,
    pub name: String,
    pub short: String,
    pub description: String,
    order: f64,
    pub sections: Vec<Section>,
}

#[derive(Clone, Debug)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub hint_top: Option<String>,
    pub hint_bottom: Option<String>,
    enabled_if: Option<Cond>,
    visible_if: Option<Cond>,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub section: String,
    pub key: String,
    pub label: String,
    pub tooltip: Option<String>,
    pub default: String,
    pub kind: Kind,
    pub new: bool,
    enabled_if: Option<Cond>,
    visible_if: Option<Cond>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    Value(f64),
    /// Another key of the same section (`from -1 to ANGLE_THRESHOLD_TO`).
    Key(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// Two values; the first is "on" (`1 or 0`).
    Toggle { on: String, off: String },
    /// Number between two bounds. `percent` shows `value * 100 %`.
    Range {
        min: Bound,
        max: Bound,
        step: f64,
        percent: bool,
        unit: String,
    },
    /// Fixed `(value, label)` options.
    Choice(Vec<(String, String)>),
    /// A Lua script: folders of `dir` that contain `file`.
    Script { dir: String, file: String },
    /// Free-form value (number lists, names, colors, paths).
    Text,
}

/// A boolean expression over other options: `ENABLED and BASIC/MODE:CMAA`.
#[derive(Clone, Debug, PartialEq)]
enum Cond {
    Or(Vec<Cond>),
    And(Vec<Cond>),
    Not(Box<Cond>),
    /// `[SECTION/]KEY` with an optional comparison.
    Test {
        section: Option<String>,
        key: String,
        op: Option<(char, String)>,
    },
}

const DISABLED: [&str; 7] = ["0", "off", "disabled", "false", "no", "none", ""];

fn truthy(v: &str) -> bool {
    let v = v.trim().trim_matches(|c| matches!(c, '"' | '\'' | '`'));
    !DISABLED.iter().any(|d| d.eq_ignore_ascii_case(v))
}

fn parse_f64(v: &str) -> Option<f64> {
    v.trim().parse::<f64>().ok()
}

/// `AXIS_REMAP` → `Axis remap`.
fn key_to_name(key: &str) -> String {
    let lower = key.trim_matches('_').replace('_', " ").to_lowercase();
    capitalize(&lower)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn is_quote(c: char) -> bool {
    matches!(c, '"' | '“' | '”' | '`')
}

/// Splits on `sep` outside quotes and parentheses.
fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut depth) = (false, 0i32);
    for c in s.chars() {
        if is_quote(c) {
            quoted = !quoted;
        } else if !quoted && c == '(' {
            depth += 1;
        } else if !quoted && c == ')' {
            depth -= 1;
        }
        if c == sep && !quoted && depth <= 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix(is_quote).unwrap_or(t);
    let t = t.strip_suffix(is_quote).unwrap_or(t);
    t.trim().to_owned()
}

/// Finds ` word ` outside quotes, returning its byte index (first or last match).
fn find_word(s: &str, word: &str, last: bool) -> Option<usize> {
    let pat = format!(" {word} ");
    let mut quoted = false;
    let mut found = None;
    for (i, c) in s.char_indices() {
        if is_quote(c) {
            quoted = !quoted;
        } else if !quoted && s[i..].starts_with(&pat) {
            found = Some(i);
            if !last {
                break;
            }
        }
    }
    found
}

fn parse_cond(query: &str) -> Option<Cond> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    let ors: Vec<Cond> = q
        .split(" or ")
        .filter_map(|part| {
            let ands: Vec<Cond> = part.split(" and ").filter_map(parse_test).collect();
            match ands.len() {
                0 => None,
                1 => ands.into_iter().next(),
                _ => Some(Cond::And(ands)),
            }
        })
        .collect();
    match ors.len() {
        0 => None,
        1 => ors.into_iter().next(),
        _ => Some(Cond::Or(ors)),
    }
}

fn parse_test(s: &str) -> Option<Cond> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("not ") {
        return Some(Cond::Not(Box::new(parse_test(rest)?)));
    }
    // KEY, KEY:VALUE, KEY=VALUE, KEY<N, KEY>N (keys are `[A-Z0-9_/]`).
    let split = s.find([':', '=', '<', '>', '≥', '≤']);
    let (path, op) = match split {
        Some(i) => {
            let c = s[i..].chars().next()?;
            (&s[..i], Some((c, unquote(&s[i + c.len_utf8()..]))))
        }
        None => (s, None),
    };
    let path = path
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '”'));
    if path.is_empty() {
        return None;
    }
    let (section, key) = match path.split_once('/') {
        Some((sec, key)) => (Some(sec.trim().to_owned()), key.trim().to_owned()),
        None => (None, path.to_owned()),
    };
    Some(Cond::Test { section, key, op })
}

/// Parses `from 0 px to 400, perc., round to 20`.
fn parse_range(spec: &str) -> Option<Kind> {
    let s = spec.trim();
    if !s.get(..5)?.eq_ignore_ascii_case("from ") {
        return None;
    }
    let rest = &s[5..];
    let to = rest.find(" to ")?;
    let from = rest[..to].trim();
    let mut parts = rest[to + 4..].split(',').map(str::trim);
    let max_s = parts.next()?;
    let (mut percent, mut step, mut round) = (false, None, false);
    for t in parts {
        let t = t.to_lowercase();
        if t.starts_with("per") {
            percent = true;
        } else if let Some(n) = t.strip_prefix("round to ") {
            step = parse_f64(n);
        } else if t == "round" {
            round = true;
        }
    }
    // The min may carry a unit: `0 px`, `0.5%`, `40°`, `0 km/h`, `1x`.
    let num_end = from
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || *c == '.' || (*c == '-' && *i == 0)))
        .map(|(i, _)| i)
        .unwrap_or(from.len());
    let (min, unit) = if num_end == 0 {
        (
            Bound::Key(from.split_whitespace().next()?.to_owned()),
            String::new(),
        )
    } else {
        (
            Bound::Value(parse_f64(&from[..num_end])?),
            from[num_end..].trim().to_owned(),
        )
    };
    let max = match parse_f64(max_s) {
        Some(v) => Bound::Value(v),
        None if is_key(max_s) => Bound::Key(max_s.to_owned()),
        None => return None,
    };
    let step = step.filter(|s| *s > 0.).unwrap_or_else(|| {
        if round {
            1.
        } else if percent {
            0.01
        } else {
            match (&min, &max) {
                (Bound::Value(a), Bound::Value(b)) => nice_step((b - a).abs() / 50.),
                _ => 0.01,
            }
        }
    });
    Some(Kind::Range {
        min,
        max,
        step,
        percent,
        unit,
    })
}

/// Rounds a raw step up to 1, 2 or 5 × 10^k.
fn nice_step(raw: f64) -> f64 {
    if !(raw.is_finite() && raw > 0.) {
        return 1.;
    }
    let mag = 10f64.powf(raw.log10().floor());
    let n = raw / mag;
    let m = if n <= 1. {
        1.
    } else if n <= 2. {
        2.
    } else if n <= 5. {
        5.
    } else {
        10.
    };
    m * mag
}

/// Parses `Smooth is SMOOTH, Disabled is 0` or `0 for "L2", 1 for "R2"`.
fn parse_choice(spec: &str) -> Option<Kind> {
    let items = split_top(spec, ',');
    let mut opts = Vec::new();
    for raw in items {
        // A trailing `... or X` item: `A, B or C`.
        for piece in split_or(&raw) {
            let item = piece.trim();
            if item.is_empty() {
                continue;
            }
            if let Some(i) = find_word(item, "is", true) {
                opts.push((unquote(&item[i + 4..]), unquote(&item[..i])));
            } else if let Some(i) = find_word(item, "for", false) {
                opts.push((unquote(&item[..i]), unquote(&item[i + 5..])));
            } else {
                let v = unquote(item);
                let label = if is_key(&v) && v.chars().any(|c| c.is_ascii_alphabetic()) {
                    key_to_name(&v)
                } else {
                    v.clone()
                };
                opts.push((v, label));
            }
        }
    }
    // Plain lists (`MEDIUM, HIGH or ULTRA`) must be simple tokens, not prose.
    let structured = spec.contains(" is ") || spec.contains(" for ");
    let plain = !structured
        && opts.iter().all(|(v, _)| {
            !v.is_empty()
                && v.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        });
    (opts.len() >= 2 && (structured || plain)).then_some(Kind::Choice(opts))
}

fn split_or(item: &str) -> Vec<String> {
    match find_word(item, "or", false) {
        Some(i) => vec![item[..i].to_owned(), item[i + 4..].to_owned()],
        None => vec![item.to_owned()],
    }
}

fn parse_kind(spec: &str) -> Option<Kind> {
    let s = spec.trim();
    let lower = s.to_lowercase();
    if lower.starts_with("from ") {
        return parse_range(s);
    }
    if let Some(rest) = lower.strip_prefix("look for ") {
        let (file, dir) = rest.split_once(" in ")?;
        let start = s.len() - rest.len();
        let file = s[start..start + file.len()].trim().to_owned();
        let dir = s[s.len() - dir.len()..].trim().to_owned();
        return Some(Kind::Script { dir, file });
    }
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() == 3 && words[1] == "or" && !s.contains(['"', ',']) {
        let (a, b) = (words[0].to_owned(), words[2].to_owned());
        let (on, off) = if truthy(&a) || !truthy(&b) {
            (a, b)
        } else {
            (b, a)
        };
        return Some(Kind::Toggle { on, off });
    }
    if s.contains(" is ") || s.contains(" for ") || (s.contains(',') && s.contains(" or ")) {
        return parse_choice(s);
    }
    // `ALWAYS, AUTO, NEVER`: a bare list of identifiers.
    let ids: Vec<&str> = s.split(',').map(str::trim).collect();
    if ids.len() >= 2
        && ids
            .iter()
            .all(|t| is_key(t) && t.chars().any(|c| c.is_ascii_alphabetic()))
    {
        return Some(Kind::Choice(
            ids.iter()
                .map(|t| (t.to_string(), key_to_name(t)))
                .collect(),
        ));
    }
    None
}

#[derive(Default)]
struct Flags {
    hidden: bool,
    new: bool,
    enabled_if: Option<Cond>,
    visible_if: Option<Cond>,
    kind: Option<Kind>,
}

fn apply_flag(part: &str, f: &mut Flags) {
    let p = part.trim();
    let lower = p.to_lowercase();
    if lower == "hidden" {
        f.hidden = true;
    } else if lower == "new" || lower == "n ew" {
        f.new = true;
    } else if let Some(q) = lower.strip_prefix("only with ") {
        f.enabled_if = parse_cond(&p[p.len() - q.len()..]);
    } else if let Some(q) = lower.strip_prefix("not available with ") {
        f.enabled_if = parse_cond(&p[p.len() - q.len()..]).map(|c| Cond::Not(Box::new(c)));
    } else if let Some(q) = lower.strip_prefix("hidden with ") {
        f.visible_if = parse_cond(&p[p.len() - q.len()..]).map(|c| Cond::Not(Box::new(c)));
    } else if let Some(q) = lower.strip_prefix("visible with ") {
        f.visible_if = parse_cond(&p[p.len() - q.len()..]);
    } else if f.kind.is_none() {
        f.kind = parse_kind(p);
    }
}

/// Splits `Label (tooltip)` keeping nested parentheses in the tooltip.
fn split_label(s: &str) -> (String, Option<String>) {
    let s = s.trim();
    let Some(open) = s.find('(') else {
        return (capitalize(s), None);
    };
    let mut depth = 0;
    for (i, c) in s[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let label = format!("{}{}", s[..open].trim_end(), s[open + i + 1..].trim_end());
                    let tip = s[open + 1..open + i].trim();
                    return (
                        capitalize(label.trim()),
                        (!tip.is_empty()).then(|| capitalize(tip)),
                    );
                }
            }
            _ => {}
        }
    }
    (capitalize(s), None)
}

/// Splits `KEY=VALUE ; comment` at the first `;` outside quotes.
fn split_value(rest: &str) -> (&str, Option<&str>) {
    let mut quote: Option<char> = None;
    for (i, c) in rest.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == ';' => return (rest[..i].trim(), Some(&rest[i + 1..])),
            None => {}
        }
    }
    (rest.trim(), None)
}

fn is_key(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

impl Module {
    /// Parses one annotated config file. Returns `None` for files with nothing to show.
    pub fn parse(file: &str, text: &str) -> Option<Module> {
        let mut m = Module {
            file: file.to_owned(),
            name: key_to_name(file.trim_end_matches(".ini")),
            short: String::new(),
            description: String::new(),
            order: 0.,
            sections: Vec::new(),
        };
        let mut info = false;
        let mut cur: Option<Section> = None;
        let mut cur_hidden = false;
        let mut header_comments = 0;
        let mut seen_key = false;
        let flush = |cur: &mut Option<Section>, hidden: bool, m: &mut Module| {
            if let Some(s) = cur.take()
                && !hidden
                && !s.items.is_empty()
            {
                m.sections.push(s);
            }
        };
        for raw in text.trim_start_matches('\u{feff}').lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[')
                && let Some(end) = line.find(']')
            {
                flush(&mut cur, cur_hidden, &mut m);
                let id = line[1..end].trim().to_owned();
                info = id == "ℹ";
                cur_hidden = false;
                header_comments = 0;
                seen_key = false;
                if !info {
                    cur = Some(Section {
                        title: key_to_name(&id),
                        id,
                        hint_top: None,
                        hint_bottom: None,
                        enabled_if: None,
                        visible_if: None,
                        items: Vec::new(),
                    });
                }
                continue;
            }
            if let Some(comment) = line.strip_prefix(';') {
                let comment = comment.trim();
                // A commented-out option (`; KEY=1 ; ...`) is disabled; skip it.
                if comment
                    .split_once('=')
                    .is_some_and(|(k, _)| is_key(k.trim()))
                {
                    continue;
                }
                if let Some(sec) = cur.as_mut().filter(|_| !seen_key) {
                    if header_comments == 0 {
                        if comment.eq_ignore_ascii_case("hidden") {
                            cur_hidden = true;
                        } else {
                            // Long parentheses are an explanation, short ones part of the title.
                            match split_label(comment) {
                                (title, Some(tip)) if tip.len() > 50 => {
                                    sec.title = title;
                                    sec.hint_top.get_or_insert(tip);
                                }
                                _ => sec.title = capitalize(comment),
                            }
                        }
                    } else if let Some(q) = comment.strip_prefix("section tweaks") {
                        let mut f = Flags::default();
                        for part in q.split(';') {
                            apply_flag(part, &mut f);
                        }
                        sec.enabled_if = f.enabled_if;
                        sec.visible_if = f.visible_if;
                    }
                    header_comments += 1;
                }
                continue;
            }
            let Some((key, rest)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let (value, comment) = split_value(rest);
            if info {
                let v = unquote(value);
                match key {
                    "FULLNAME" => m.name = v,
                    "SHORT_DESCRIPTION" => m.short = v,
                    "DESCRIPTION" => m.description = v,
                    "ORDER" => m.order = parse_f64(&v).unwrap_or(0.),
                    _ => {}
                }
                continue;
            }
            let Some(sec) = cur.as_mut() else { continue };
            seen_key = true;
            match key {
                "__HINT_TOP" => {
                    sec.hint_top = Some(unquote(value));
                    continue;
                }
                "__HINT_BOTTOM" => {
                    sec.hint_bottom = Some(unquote(value));
                    continue;
                }
                _ if key.starts_with("__") || !is_key(key) => continue,
                _ => {}
            }
            // Options without a description are internal; CM hides them as well.
            let Some(comment) = comment else { continue };
            // `;` inside the label's parentheses is prose, not a separator.
            let parts = split_top(comment, ';');
            let mut parts = parts.iter().map(String::as_str);
            let (label, tooltip) = split_label(parts.next().unwrap_or(""));
            let mut f = Flags::default();
            for p in parts {
                apply_flag(p, &mut f);
            }
            if f.hidden {
                continue;
            }
            sec.items.push(Item {
                section: sec.id.clone(),
                label: if label.is_empty() {
                    key_to_name(key)
                } else {
                    label
                },
                key: key.to_owned(),
                tooltip,
                default: value.to_owned(),
                kind: f.kind.unwrap_or(Kind::Text),
                new: f.new,
                enabled_if: f.enabled_if,
                visible_if: f.visible_if,
            });
        }
        flush(&mut cur, cur_hidden, &mut m);
        (!m.sections.is_empty()).then_some(m)
    }

    /// The module's master switch (`[BASIC] ENABLED`), if it has one.
    pub fn master(&self) -> Option<&Item> {
        let s = self.sections.iter().find(|s| s.id == "BASIC")?;
        s.items
            .iter()
            .find(|i| i.key == "ENABLED" && matches!(i.kind, Kind::Toggle { .. }))
    }
}

/// All CSP modules with the user's overrides on top.
pub struct CspConfig {
    pub version: Option<String>,
    pub config_dir: PathBuf,
    pub user_dir: PathBuf,
    pub modules: Vec<Module>,
    ac_root: PathBuf,
    /// Full default files, so hidden options can still drive conditions.
    defaults: BTreeMap<String, IniFile>,
    user: BTreeMap<String, IniFile>,
    saved: BTreeMap<String, String>,
}

/// `<ac_root>/extension/config`.
pub fn config_dir(ac_root: &Path) -> PathBuf {
    ac_root.join("extension").join("config")
}

/// `Documents/Assetto Corsa/cfg/extension`.
pub fn user_dir(ac_root: &Path) -> Option<PathBuf> {
    Some(
        paths::documents_ac_dir(ac_root)?
            .join("cfg")
            .join("extension"),
    )
}

pub fn installed(ac_root: &Path) -> bool {
    config_dir(ac_root).join("data_manifest.ini").is_file()
}

impl CspConfig {
    pub fn load(ac_root: &Path) -> Result<Self> {
        let user = user_dir(ac_root)
            .context("Documents/Assetto Corsa not found (run the game from Steam once)")?;
        Self::load_from(ac_root, &config_dir(ac_root), &user)
    }

    pub fn load_from(ac_root: &Path, config_dir: &Path, user_dir: &Path) -> Result<Self> {
        let mut names: Vec<String> = fs::read_dir(config_dir)
            .with_context(|| format!("reading {}", config_dir.display()))?
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                (name.ends_with(".ini") && !name.starts_with("data_") && e.path().is_file())
                    .then_some(name)
            })
            .collect();
        names.sort();
        let mut modules = Vec::new();
        let mut defaults = BTreeMap::new();
        for name in &names {
            let path = config_dir.join(name);
            let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let text = String::from_utf8_lossy(&bytes);
            if let Some(m) = Module::parse(name, &text) {
                defaults.insert(name.clone(), IniFile::parse(&text));
                modules.push(m);
            }
        }
        modules.sort_by(|a, b| {
            a.order
                .total_cmp(&b.order)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        let version = IniFile::load_or_default(&config_dir.join("data_manifest.ini"))?
            .get("VERSION", "SHADERS_PATCH")
            .map(str::to_owned);
        let mut user = BTreeMap::new();
        let mut saved = BTreeMap::new();
        for m in &modules {
            let ini = IniFile::load_or_default(&user_dir.join(&m.file))?;
            saved.insert(m.file.clone(), ini.to_string());
            user.insert(m.file.clone(), ini);
        }
        Ok(Self {
            version,
            config_dir: config_dir.to_path_buf(),
            user_dir: user_dir.to_path_buf(),
            modules,
            ac_root: ac_root.to_path_buf(),
            defaults,
            user,
            saved,
        })
    }

    /// The effective value: the override, else the default.
    pub fn value<'a>(&'a self, module: usize, item: &'a Item) -> &'a str {
        let m = &self.modules[module];
        self.user
            .get(&m.file)
            .and_then(|u| u.get(&item.section, &item.key))
            .unwrap_or(&item.default)
    }

    fn lookup(&self, module: usize, section: &str, key: &str) -> Option<String> {
        let file = &self.modules[module].file;
        self.user
            .get(file)
            .and_then(|u| u.get(section, key))
            .or_else(|| self.defaults.get(file)?.get(section, key))
            .map(|v| v.trim().to_owned())
    }

    fn test(&self, module: usize, section: &str, c: &Cond) -> bool {
        match c {
            Cond::Or(v) => v.iter().any(|c| self.test(module, section, c)),
            Cond::And(v) => v.iter().all(|c| self.test(module, section, c)),
            Cond::Not(c) => !self.test(module, section, c),
            Cond::Test {
                section: s,
                key,
                op,
            } => {
                let sec = s.as_deref().unwrap_or(section);
                let v = self.lookup(module, sec, key).or_else(|| {
                    // `only with ENABLED` may point at another section's key.
                    s.is_none().then(|| {
                        let m = &self.modules[module];
                        m.sections
                            .iter()
                            .find_map(|x| x.items.iter().find(|i| i.key == *key))
                            .map(|i| self.value(module, i).to_owned())
                    })?
                });
                let Some(v) = v else { return false };
                match op {
                    None => truthy(&v),
                    Some((c, want)) => {
                        let (a, b) = (parse_f64(&v), parse_f64(want));
                        match (c, a, b) {
                            ('<', Some(a), Some(b)) => a < b,
                            ('>', Some(a), Some(b)) => a > b,
                            ('≤', Some(a), Some(b)) => a <= b,
                            ('≥', Some(a), Some(b)) => a >= b,
                            (_, Some(a), Some(b)) => (a - b).abs() < 1e-9,
                            _ => unquote(&v).eq_ignore_ascii_case(want),
                        }
                    }
                }
            }
        }
    }

    pub fn section_visible(&self, module: usize, s: &Section) -> bool {
        s.visible_if
            .as_ref()
            .is_none_or(|c| self.test(module, &s.id, c))
    }

    pub fn section_enabled(&self, module: usize, s: &Section) -> bool {
        s.enabled_if
            .as_ref()
            .is_none_or(|c| self.test(module, &s.id, c))
    }

    pub fn visible(&self, module: usize, item: &Item) -> bool {
        item.visible_if
            .as_ref()
            .is_none_or(|c| self.test(module, &item.section, c))
    }

    pub fn enabled(&self, module: usize, item: &Item) -> bool {
        item.enabled_if
            .as_ref()
            .is_none_or(|c| self.test(module, &item.section, c))
    }

    /// Resolves a range bound that may name another key.
    pub fn bound(&self, module: usize, item: &Item, b: &Bound) -> Option<f64> {
        match b {
            Bound::Value(v) => Some(*v),
            Bound::Key(k) => parse_f64(&self.lookup(module, &item.section, k)?),
        }
    }

    /// Script choices: folders of `<ac_root>/<dir>` holding `file`.
    pub fn scripts(&self, dir: &str, file: &str) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(self.ac_root.join(dir))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join(file).is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        out.sort_by(|a, b| crate::content::natural_cmp(a, b));
        out
    }

    /// Sets a value; a value equal to the default drops the override.
    pub fn set(&mut self, module: usize, section: &str, key: &str, value: &str) {
        let file = self.modules[module].file.clone();
        let default = self.modules[module]
            .sections
            .iter()
            .find(|s| s.id == section)
            .and_then(|s| s.items.iter().find(|i| i.key == key))
            .map(|i| i.default.clone());
        let ini = self.user.entry(file).or_default();
        if default.as_deref().is_some_and(|d| same_value(d, value)) {
            if let Some(sec) = ini.sections().iter().position(|s| s.name == section) {
                ini.section_mut(section).remove(key);
                if ini.sections()[sec].entries().next().is_none() {
                    ini.remove_section(section);
                }
            }
        } else {
            ini.section_mut(section).set(key, value);
        }
    }

    /// Steps a range by `d` steps, snapped to the step grid and clamped.
    pub fn step(&mut self, module: usize, item: &Item, d: i32) {
        let Kind::Range { min, max, step, .. } = &item.kind else {
            return;
        };
        let lo = self.bound(module, item, min).unwrap_or(f64::MIN);
        let hi = self.bound(module, item, max).unwrap_or(f64::MAX);
        let cur = parse_f64(self.value(module, item)).unwrap_or(lo);
        let snapped = (cur / step).round() * step;
        let next = (snapped + *step * d as f64).clamp(lo.min(hi), hi.max(lo));
        let text = crate::game_cfg::fmt_number(next);
        self.set(module, &item.section, &item.key, &text);
    }

    /// Drops every override this module knows about.
    pub fn reset(&mut self, module: usize) {
        let items: Vec<(String, String)> = self.modules[module]
            .sections
            .iter()
            .flat_map(|s| s.items.iter().map(|i| (i.section.clone(), i.key.clone())))
            .collect();
        for (s, k) in items {
            let default = self.modules[module]
                .sections
                .iter()
                .find(|x| x.id == s)
                .and_then(|x| x.items.iter().find(|i| i.key == k))
                .map(|i| i.default.clone())
                .unwrap_or_default();
            self.set(module, &s, &k, &default);
        }
    }

    /// Options this module overrides.
    pub fn changed(&self, module: usize) -> usize {
        let m = &self.modules[module];
        m.sections
            .iter()
            .flat_map(|s| &s.items)
            .filter(|i| !same_value(self.value(module, i), &i.default))
            .count()
    }

    pub fn dirty_files(&self) -> Vec<String> {
        self.user
            .iter()
            .filter(|(n, f)| self.saved.get(*n) != Some(&f.to_string()))
            .map(|(n, _)| n.clone())
            .collect()
    }

    pub fn revert(&mut self) {
        for (name, text) in &self.saved {
            self.user.insert(name.clone(), IniFile::parse(text));
        }
    }

    /// Writes changed override files atomically.
    pub fn save(&mut self) -> Result<Vec<String>> {
        use std::io::Write;
        let dirty = self.dirty_files();
        if dirty.is_empty() {
            return Ok(dirty);
        }
        fs::create_dir_all(&self.user_dir)
            .with_context(|| format!("creating {}", self.user_dir.display()))?;
        for name in &dirty {
            let text = self.user[name].to_string();
            let path = self.user_dir.join(name);
            let mut temp = tempfile::NamedTempFile::new_in(&self.user_dir)?;
            temp.write_all(text.replace('\n', "\r\n").as_bytes())?;
            temp.as_file().sync_all()?;
            temp.persist(&path)
                .with_context(|| format!("writing {}", path.display()))?;
            self.saved.insert(name.clone(), text);
        }
        Ok(dirty)
    }
}

/// Compares values the way CSP reads them (`1` == `1.0`, quotes ignored).
fn same_value(a: &str, b: &str) -> bool {
    match (parse_f64(a), parse_f64(b)) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => unquote(a) == unquote(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAMEPAD: &str = "[ℹ]\r\nFULLNAME = Gamepad FX\r\nSHORT_DESCRIPTION = New gamepads\r\n\r\n\
[BASIC]\r\n; Extension\r\nENABLED=1 ; Active; 1 or 0\r\n\r\n\
[AXIS_REMAP]\r\nTHROTTLE=1 ; Throttle; 0 for \"L2\", 1 for \"R2 (default)\", 2 for \"Left stick (Y+)\"\r\n\r\n\
[ADVANCED_GAMEPADS]\r\n; PS4 DualShock, PS5 DualSense and Nintendo controllers integration\r\n\
ENABLED=1 ; Active; 1 or 0\r\nLIGHT_BAR=SMOOTH ; Light bar shifting color; Smooth is SMOOTH, Discrete is DISCRETE, Disabled is 0; only with ENABLED\r\n\
SECRET=1 ; Internal; 1 or 0; hidden\r\nNO_COMMENT=1\r\n\r\n\
[HAPTICS]\r\nGAIN=1 ; Volume gain; from 0 to 2, perc.\r\nSIZE=200 ;; from 60 px to 400, round to 20\r\n\
ANGLE_TO=0.8 ; Angle threshold, to; from ANGLE_FROM to 1\r\nANGLE_FROM=0.2 ; Angle threshold, from; from 0 to 1\r\n\
QUALITY=HIGH ; Quality; MEDIUM, HIGH or ULTRA; hidden with not ADVANCED_GAMEPADS/ENABLED\r\n\
SHUFFLE=AUTO ; Behavior (randomly limit; in automatic mode would affect only multi-lane traffic maps); ALWAYS, AUTO, NEVER\r\n\
COLOR='#000000' ; Color (with ; inside)\r\n\r\n\
[HIDDEN_SECTION]\r\n; hidden\r\nX=1 ; X; 1 or 0\r\n";

    fn item<'a>(m: &'a Module, sec: &str, key: &str) -> &'a Item {
        m.sections
            .iter()
            .find(|s| s.id == sec)
            .unwrap()
            .items
            .iter()
            .find(|i| i.key == key)
            .unwrap()
    }

    #[test]
    fn parses_annotations() {
        let m = Module::parse("gamepad_fx.ini", GAMEPAD).unwrap();
        assert_eq!(m.name, "Gamepad FX");
        assert_eq!(m.short, "New gamepads");
        let ids: Vec<&str> = m.sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["BASIC", "AXIS_REMAP", "ADVANCED_GAMEPADS", "HAPTICS"]);
        assert_eq!(
            m.sections[2].title,
            "PS4 DualShock, PS5 DualSense and Nintendo controllers integration"
        );

        assert_eq!(
            item(&m, "BASIC", "ENABLED").kind,
            Kind::Toggle {
                on: "1".into(),
                off: "0".into()
            }
        );
        let Kind::Choice(opts) = &item(&m, "AXIS_REMAP", "THROTTLE").kind else {
            panic!()
        };
        assert_eq!(opts[1], ("1".into(), "R2 (default)".into()));
        let Kind::Choice(opts) = &item(&m, "ADVANCED_GAMEPADS", "LIGHT_BAR").kind else {
            panic!()
        };
        assert_eq!(opts[2], ("0".into(), "Disabled".into()));

        let adv = &m.sections[2];
        assert!(
            adv.items
                .iter()
                .all(|i| i.key != "SECRET" && i.key != "NO_COMMENT")
        );

        let gain = item(&m, "HAPTICS", "GAIN");
        assert_eq!(
            gain.kind,
            Kind::Range {
                min: Bound::Value(0.),
                max: Bound::Value(2.),
                step: 0.01,
                percent: true,
                unit: String::new()
            }
        );
        let size = item(&m, "HAPTICS", "SIZE");
        assert_eq!(size.label, "Size");
        assert_eq!(
            size.kind,
            Kind::Range {
                min: Bound::Value(60.),
                max: Bound::Value(400.),
                step: 20.,
                percent: false,
                unit: "px".into()
            }
        );
        let Kind::Range { min, .. } = &item(&m, "HAPTICS", "ANGLE_TO").kind else {
            panic!()
        };
        assert_eq!(*min, Bound::Key("ANGLE_FROM".into()));
        let Kind::Choice(opts) = &item(&m, "HAPTICS", "QUALITY").kind else {
            panic!()
        };
        assert_eq!(opts.len(), 3);
        let shuffle = item(&m, "HAPTICS", "SHUFFLE");
        assert_eq!(shuffle.label, "Behavior");
        assert!(matches!(&shuffle.kind, Kind::Choice(o) if o.len() == 3));
        let color = item(&m, "HAPTICS", "COLOR");
        assert_eq!(
            (color.default.as_str(), &color.kind),
            ("'#000000'", &Kind::Text)
        );
    }

    fn fixture() -> (tempfile::TempDir, CspConfig) {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("extension/config");
        let user = dir.path().join("user");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&user).unwrap();
        fs::write(config.join("gamepad_fx.ini"), GAMEPAD).unwrap();
        fs::write(
            config.join("data_manifest.ini"),
            "[VERSION]\r\nSHADERS_PATCH=0.2.11\r\n",
        )
        .unwrap();
        fs::write(
            user.join("gamepad_fx.ini"),
            "[OTHER]\r\nKEEP=1\r\n\r\n[HAPTICS]\r\nGAIN=1.5\r\n",
        )
        .unwrap();
        let cfg = CspConfig::load_from(dir.path(), &config, &user).unwrap();
        (dir, cfg)
    }

    #[test]
    fn overrides_conditions_and_save() {
        let (dir, mut c) = fixture();
        assert_eq!(c.version.as_deref(), Some("0.2.11"));
        assert_eq!(c.modules.len(), 1);
        let m = c.modules[0].clone();
        let gain = item(&m, "HAPTICS", "GAIN");
        assert_eq!(c.value(0, gain), "1.5");
        assert_eq!(c.changed(0), 1);

        let light = item(&m, "ADVANCED_GAMEPADS", "LIGHT_BAR");
        let quality = item(&m, "HAPTICS", "QUALITY");
        assert!(c.enabled(0, light) && c.visible(0, quality));
        c.set(0, "ADVANCED_GAMEPADS", "ENABLED", "0");
        assert!(
            !c.enabled(0, light),
            "only with ENABLED reads its own section"
        );
        assert!(!c.visible(0, quality), "hidden with not SECTION/KEY");

        // Stepping snaps and clamps; the dependent bound follows the other key.
        for _ in 0..100 {
            c.step(0, gain, 1);
        }
        assert_eq!(c.value(0, gain), "2");
        let to = item(&m, "HAPTICS", "ANGLE_TO");
        for _ in 0..100 {
            c.step(0, to, -1);
        }
        assert_eq!(c.value(0, to), "0.2");

        assert_eq!(c.save().unwrap(), ["gamepad_fx.ini"]);
        let text = fs::read_to_string(dir.path().join("user/gamepad_fx.ini")).unwrap();
        assert!(
            text.contains("[OTHER]\r\nKEEP=1\r\n"),
            "unrelated sections survive: {text:?}"
        );
        assert!(
            text.contains("GAIN=2\r\n")
                && text.contains("ANGLE_TO=0.2\r\n")
                && text.contains("[ADVANCED_GAMEPADS]\r\nENABLED=0")
        );
        assert!(c.dirty_files().is_empty());

        // Back to defaults drops the overrides, and their sections when empty.
        c.reset(0);
        c.save().unwrap();
        let text = fs::read_to_string(dir.path().join("user/gamepad_fx.ini")).unwrap();
        assert_eq!(text, "[OTHER]\r\nKEEP=1\r\n");
        assert_eq!(c.changed(0), 0);
    }

    #[test]
    fn revert_restores_saved_state() {
        let (_dir, mut c) = fixture();
        c.set(0, "AXIS_REMAP", "THROTTLE", "2");
        assert_eq!(c.dirty_files(), ["gamepad_fx.ini"]);
        c.revert();
        assert!(c.dirty_files().is_empty());
        let m = c.modules[0].clone();
        assert_eq!(c.value(0, item(&m, "AXIS_REMAP", "THROTTLE")), "1");
    }

    #[test]
    fn conditions_parse() {
        assert_eq!(
            parse_cond("ENABLED and BASIC/MODE:CMAA or not X<2"),
            Some(Cond::Or(vec![
                Cond::And(vec![
                    Cond::Test {
                        section: None,
                        key: "ENABLED".into(),
                        op: None
                    },
                    Cond::Test {
                        section: Some("BASIC".into()),
                        key: "MODE".into(),
                        op: Some((':', "CMAA".into()))
                    },
                ]),
                Cond::Not(Box::new(Cond::Test {
                    section: None,
                    key: "X".into(),
                    op: Some(('<', "2".into()))
                })),
            ]))
        );
    }
}
