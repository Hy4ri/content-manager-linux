use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::content::{natural_cmp, scan_cars};
use crate::ini::IniFile;
use crate::paths::{documents_ac_dir, race_ini_path};
use crate::race;

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub series: Vec<Series>,
    pub special: Vec<Event>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Series {
    pub id: String,
    pub code: String,
    pub name: String,
    pub description: String,
    pub preview: Option<PathBuf>,
    pub requires: Vec<String>,
    pub requires_any: bool,
    pub points: Vec<u32>,
    pub goal_points: u32,
    pub goal_ranking: u32,
    pub goal_medals: [u32; 3],
    pub events: Vec<Event>,
    pub opponents: Vec<Opponent>,
}

#[derive(Debug, Clone)]
pub struct Opponent {
    pub id: String,
    pub name: String,
    pub car: String,
    pub skin: String,
    pub level: u32,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: String,
    pub name: String,
    pub description: String,
    pub preview: Option<PathBuf>,
    pub car: String,
    pub track: String,
    pub layout: String,
    pub weather: String,
    pub ambient: Option<f64>,
    pub road: Option<f64>,
    pub penalties: bool,
    pub fixed_setup: bool,
    pub ai_level: u32,
    pub sessions: Vec<Session>,
    pub goals: Vec<Goal>,
    pub ini: IniFile,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub name: String,
    pub kind: u32,
    pub laps: u32,
    pub minutes: u32,
}

#[derive(Debug, Clone)]
pub struct Goal {
    pub kind: String,
    pub objective: f64,
}

impl Series {
    pub fn championship(&self) -> bool {
        self.goal_points > 0 || self.goal_ranking > 0
    }

    pub fn unlocked(&self, p: &Progress) -> bool {
        if self.requires.is_empty() {
            return true;
        }
        if self.requires_any {
            self.requires
                .iter()
                .any(|req| p.completed_series.contains(req))
        } else {
            self.requires
                .iter()
                .all(|req| p.completed_series.contains(req))
        }
    }

    pub fn unlocked_in(&self, catalog: &Catalog, p: &Progress) -> bool {
        if self.requires.is_empty() {
            return true;
        }
        let satisfied = |req: &str| {
            p.completed_series.contains(req)
                || catalog
                    .series
                    .iter()
                    .find(|s| s.id == req)
                    .is_some_and(|s| s.completed(p))
        };
        if self.requires_any {
            self.requires.iter().any(|req| satisfied(req))
        } else {
            self.requires.iter().all(|req| satisfied(req))
        }
    }

    pub fn event_unlocked(&self, index: usize, catalog: &Catalog, p: &Progress) -> bool {
        if !self.unlocked_in(catalog, p) || index >= self.events.len() {
            return false;
        }
        if self.championship() && index > 0 {
            return self
                .events
                .get(index - 1)
                .is_some_and(|prev| p.get(&prev.id).last_position.is_some());
        }
        true
    }

    pub fn goal_summary(&self) -> String {
        if self.championship() {
            let mut parts = Vec::new();
            if self.goal_points > 0 {
                parts.push(format!("{} pts", self.goal_points));
            }
            if self.goal_ranking > 0 {
                parts.push(format!("Top {}", self.goal_ranking));
            }
            if parts.is_empty() {
                "Championship".into()
            } else {
                parts.join(" · ")
            }
        } else {
            let mut parts = Vec::new();
            if self.goal_medals[0] > 0 {
                parts.push(format!("{} Bronze", self.goal_medals[0]));
            }
            if self.goal_medals[1] > 0 {
                parts.push(format!("{} Silver", self.goal_medals[1]));
            }
            if self.goal_medals[2] > 0 {
                parts.push(format!("{} Gold", self.goal_medals[2]));
            }
            if parts.is_empty() {
                "Complete events".into()
            } else {
                parts.join(" · ")
            }
        }
    }

    pub fn completed(&self, p: &Progress) -> bool {
        if p.completed_series.contains(&self.id) {
            return true;
        }
        if self.championship() {
            let pts = p
                .championship_points
                .get(&self.id)
                .and_then(|pts_map| pts_map.get("player"))
                .copied()
                .unwrap_or(0);
            if self.goal_points > 0 && pts >= self.goal_points {
                return true;
            }
            if self.goal_ranking > 0 {
                let all_done = !self.events.is_empty()
                    && self
                        .events
                        .iter()
                        .all(|e| p.get(&e.id).last_position.is_some());
                if all_done {
                    let st = self.standings(p);
                    if let Some(rank) = st.iter().position(|(id, _)| id == "player")
                        && (rank as u32 + 1) <= self.goal_ranking
                    {
                        return true;
                    }
                }
            }
            false
        } else {
            if self.events.is_empty() {
                return false;
            }
            let counts = self.medal_counts(p);
            let g = self.goal_medals;
            let mult = self.events.len() as i64;

            let mut diff3 = counts[2] as i64 - g[2] as i64;
            if diff3 < 0 {
                diff3 *= mult;
            }
            let mut diff2 = counts[1] as i64 + diff3 - g[1] as i64;
            if diff2 < 0 {
                diff2 *= mult;
            }
            let mut diff1 = counts[0] as i64 + diff2 - g[0] as i64;
            if diff1 < 0 {
                diff1 *= mult;
            }

            diff3 + diff2 + diff1 >= 0
        }
    }

    pub fn medal_counts(&self, p: &Progress) -> [u32; 3] {
        let mut b = 0;
        let mut s = 0;
        let mut g = 0;
        for ev in &self.events {
            match p.get(&ev.id).medal {
                1 => b += 1,
                2 => s += 1,
                3 => g += 1,
                _ => {}
            }
        }
        [b, s, g]
    }

    pub fn standings(&self, p: &Progress) -> Vec<(String, u32)> {
        let default_empty = BTreeMap::new();
        let series_pts = p
            .championship_points
            .get(&self.id)
            .unwrap_or(&default_empty);
        let mut list: Vec<(String, u32)> = Vec::new();
        list.push((
            "player".to_string(),
            series_pts.get("player").copied().unwrap_or(0),
        ));
        for opp in &self.opponents {
            let pts = series_pts.get(&opp.id).copied().unwrap_or(0);
            list.push((opp.id.clone(), pts));
        }
        list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| natural_cmp(&a.0, &b.0)));
        list
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventProgress {
    pub medal: u8,
    pub attempts: u32,
    pub best_score: Option<f64>,
    pub last_score: Option<f64>,
    pub last_position: Option<u32>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Progress {
    pub events: BTreeMap<String, EventProgress>,
    pub championship_points: BTreeMap<String, BTreeMap<String, u32>>,
    pub completed_series: BTreeSet<String>,
}

pub fn import_notice() -> &'static str {
    "Career save is imported on first use. Special-event medals earned here are saved locally; existing Steam medals are not synchronized."
}

impl Progress {
    pub fn load(root: &Path) -> Result<Self> {
        let mut p = Self::default();
        if let Some(doc_dir) = documents_ac_dir(root) {
            let json_path = doc_dir.join("launcherdata").join("cm-rs-progress.json");
            if json_path.exists() {
                let bytes = fs::read(&json_path)
                    .with_context(|| format!("reading {}", json_path.display()))?;
                return serde_json::from_slice(&bytes)
                    .with_context(|| format!("invalid progress in {}", json_path.display()));
            }

            let career_ini_path = doc_dir
                .join("launcherdata")
                .join("filestore")
                .join("career.ini");
            if career_ini_path.is_file() {
                p.import_career_ini(&career_ini_path, root)?;
            }
        }
        Ok(p)
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let doc_dir = documents_ac_dir(root)
            .ok_or_else(|| anyhow!("Cannot resolve Documents/Assetto Corsa directory"))?;
        let launcher_dir = doc_dir.join("launcherdata");
        fs::create_dir_all(&launcher_dir)
            .with_context(|| format!("creating directory {}", launcher_dir.display()))?;

        let target_path = launcher_dir.join("cm-rs-progress.json");
        let tmp_file = tempfile::NamedTempFile::new_in(&launcher_dir)
            .with_context(|| format!("creating temp file in {}", launcher_dir.display()))?;

        let data = serde_json::to_vec_pretty(self)?;
        fs::write(tmp_file.path(), data)
            .with_context(|| format!("writing temp file {}", tmp_file.path().display()))?;

        tmp_file.as_file().sync_all()?;
        tmp_file
            .persist(&target_path)
            .with_context(|| format!("persisting {}", target_path.display()))?;

        Ok(())
    }

    pub fn get(&self, id: &str) -> EventProgress {
        self.events.get(id).cloned().unwrap_or_default()
    }

    pub fn record(
        &mut self,
        event: &Event,
        series: Option<&Series>,
        result: &serde_json::Value,
    ) -> Result<String> {
        let race_session = result["sessions"].as_array().and_then(|sessions| {
            sessions
                .iter()
                .rev()
                .find(|s| s["type"].as_u64() == Some(3))
        });
        let needs_race = event.sessions.iter().any(|s| s.kind == 3);
        if needs_race {
            let session = race_session.ok_or_else(|| anyhow!("No race session in result"))?;
            let required = event
                .sessions
                .iter()
                .find(|s| s.kind == 3)
                .map(|s| s.laps)
                .unwrap_or(0);
            let driven = session["lapstotal"][0].as_u64().unwrap_or(0);
            anyhow::ensure!(
                driven > 0 && (required == 0 || driven >= required as u64),
                "Race was cancelled or unfinished; progress unchanged"
            );
        }
        let extras = result["extras"].as_array();
        let is_completed = needs_race
            || extras.is_some_and(|extra| {
                extra.iter().any(|x| match x["name"].as_str() {
                    Some("bestlap") => x["time"].as_f64().is_some_and(|v| v > 0.),
                    Some("timeattack" | "drift") => x["points"].as_f64().is_some_and(|v| v > 0.),
                    Some("drag") => x["runs"].as_u64().is_some_and(|v| v > 0),
                    _ => false,
                })
            });
        if !is_completed {
            return Ok("No completed attempt; progress unchanged.".into());
        }

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut ep = self.get(&event.id);
        ep.attempts += 1;
        ep.updated_at = now;

        if let Some(ser) = series
            && ser.championship()
        {
            if ep.last_position.is_some() {
                return Ok("Championship round was already scored.".to_string());
            }

            let mut player_pos = 0u32;
            if let Some(sessions) = result.get("sessions").and_then(|s| s.as_array()) {
                for sess in sessions
                    .iter()
                    .rev()
                    .filter(|s| s["type"].as_u64() == Some(3))
                {
                    if let Some(rr) = sess.get("raceResult").and_then(|r| r.as_array()) {
                        for (i, v) in rr.iter().enumerate() {
                            if v.as_u64() == Some(0) {
                                player_pos = (i + 1) as u32;
                                break;
                            }
                        }
                        if player_pos > 0 {
                            let pts_table = &ser.points;
                            let pts_entry =
                                self.championship_points.entry(ser.id.clone()).or_default();

                            for (i, v) in rr.iter().enumerate() {
                                let pts = if i < pts_table.len() { pts_table[i] } else { 0 };
                                if let Some(c_idx) = v.as_u64() {
                                    if c_idx == 0 {
                                        *pts_entry.entry("player".to_string()).or_default() += pts;
                                    } else {
                                        let opp = ser
                                            .opponents
                                            .get(c_idx as usize - 1)
                                            .ok_or_else(|| {
                                                anyhow!("Unknown championship car index {c_idx}")
                                            })?;
                                        *pts_entry.entry(opp.id.clone()).or_default() += pts;
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
            }

            if player_pos == 0 {
                return Ok("Race finished without a valid finishing position.".to_string());
            }

            ep.last_position = Some(player_pos);
            ep.last_score = Some(player_pos as f64);
            ep.best_score = Some(match ep.best_score {
                Some(b) => b.min(player_pos as f64),
                None => player_pos as f64,
            });
            self.events.insert(event.id.clone(), ep);

            if ser.completed(self) {
                self.completed_series.insert(ser.id.clone());
            }

            return Ok(format!(
                "Championship round completed! Finished P{}. Standings updated.",
                player_pos
            ));
        }

        let mut scored_medal: u8 = 0;
        let mut score: Option<f64> = None;
        let mut pos_res: Option<u32> = None;

        let authoritative = if event.id.starts_with("SPECIAL_EVENT_") {
            let extra = result["extras"].as_array().and_then(|extras| {
                extras
                    .iter()
                    .find(|x| x["name"].as_str() == Some("special_event"))
            });
            if let Some(extra) = extra {
                let guid = extra["guid"]
                    .as_i64()
                    .map(|v| v.to_string())
                    .or_else(|| extra["guid"].as_str().map(str::to_owned));
                let expected = event
                    .ini
                    .get("SPECIAL_EVENT", "GUID")
                    .unwrap_or_else(|| event.id.trim_start_matches("SPECIAL_EVENT_"));
                anyhow::ensure!(
                    guid.as_deref() == Some(expected),
                    "Special-event result GUID does not match"
                );
                let tier = extra["tier"]
                    .as_i64()
                    .ok_or_else(|| anyhow!("Missing event result tier"))?;
                anyhow::ensure!((-1..=2).contains(&tier), "Invalid result tier");
                Some((tier + 1) as u8)
            } else {
                None
            }
        } else {
            None
        };

        if !event.goals.is_empty() {
            let primary_kind = event.goals[0].kind.to_uppercase();
            match primary_kind.as_str() {
                "TIME" => {
                    let mut best_lap: Option<f64> = None;
                    if let Some(extras) = result.get("extras").and_then(|e| e.as_array()) {
                        for x in extras {
                            if x.get("name").and_then(|n| n.as_str()) == Some("bestlap")
                                && let Some(t) = x.get("time").and_then(|t| t.as_f64())
                            {
                                if t > 0.0 {
                                    best_lap = Some(t);
                                }
                            }
                        }
                    }
                    if best_lap.is_none()
                        && let Some(sessions) = result.get("sessions").and_then(|s| s.as_array())
                    {
                        for sess in sessions {
                            if let Some(bls) = sess.get("bestLaps").and_then(|b| b.as_array()) {
                                for b in bls {
                                    if b.get("car").and_then(|c| c.as_u64()) == Some(0) {
                                        if let Some(t) = b.get("time").and_then(|t| t.as_f64()) {
                                            if t > 0.0 {
                                                best_lap = Some(match best_lap {
                                                    Some(cur) => cur.min(t),
                                                    None => t,
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if let Some(t) = best_lap {
                        score = Some(t);
                        let mut m: u8 = 0;
                        for (idx, goal) in event.goals.iter().enumerate() {
                            if t <= goal.objective && goal.objective > 0.0 {
                                m = (idx as u8 + 1).max(m);
                            }
                        }
                        scored_medal = m;
                    }
                }
                "POINTS" => {
                    let mut pts: Option<f64> = None;
                    if let Some(extras) = result.get("extras").and_then(|e| e.as_array()) {
                        for x in extras {
                            let name = x.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            if (name == "timeattack" || name == "drift")
                                && let Some(p) = x.get("points").and_then(|p| p.as_f64())
                            {
                                pts = Some(p);
                            }
                        }
                    }
                    if let Some(p) = pts {
                        score = Some(p);
                        let mut m: u8 = 0;
                        for (idx, goal) in event.goals.iter().enumerate() {
                            if p >= goal.objective && goal.objective > 0.0 {
                                m = (idx as u8 + 1).max(m);
                            }
                        }
                        scored_medal = m;
                    }
                }
                "POSITION" => {
                    let mut pos = 0u32;
                    if let Some(sessions) = result.get("sessions").and_then(|s| s.as_array()) {
                        for sess in sessions
                            .iter()
                            .rev()
                            .filter(|s| s["type"].as_u64() == Some(3))
                        {
                            if let Some(rr) = sess.get("raceResult").and_then(|r| r.as_array()) {
                                for (i, v) in rr.iter().enumerate() {
                                    if v.as_u64() == Some(0) {
                                        pos = (i + 1) as u32;
                                        break;
                                    }
                                }
                                if pos > 0 {
                                    break;
                                }
                            }
                        }
                    }
                    if pos > 0 {
                        pos_res = Some(pos);
                        score = Some(pos as f64);
                        let mut m: u8 = 0;
                        for (idx, goal) in event.goals.iter().enumerate() {
                            if (pos as f64) <= goal.objective && goal.objective > 0.0 {
                                m = (idx as u8 + 1).max(m);
                            }
                        }
                        scored_medal = m;
                    }
                }
                "WINS" => {
                    let mut wins: Option<f64> = None;
                    if let Some(extras) = result.get("extras").and_then(|e| e.as_array()) {
                        for x in extras {
                            if x.get("name").and_then(|n| n.as_str()) == Some("drag")
                                && let Some(w) = x.get("wins").and_then(|w| w.as_f64())
                            {
                                wins = Some(w);
                            }
                        }
                    }
                    if let Some(w) = wins {
                        score = Some(w);
                        let mut m: u8 = 0;
                        for (idx, goal) in event.goals.iter().enumerate() {
                            if w >= goal.objective && goal.objective > 0.0 {
                                m = (idx as u8 + 1).max(m);
                            }
                        }
                        scored_medal = m;
                    }
                }
                _ => {}
            }
        }

        if let Some(medal) = authoritative {
            scored_medal = medal;
        }
        if event.goals.first().is_some_and(|g| g.kind == "AI") && scored_medal > 0 {
            score = event
                .goals
                .get(scored_medal as usize - 1)
                .map(|g| g.objective);
        }
        ep.medal = ep.medal.max(scored_medal);
        ep.last_score = score.or(ep.last_score);
        ep.last_position = pos_res.or(ep.last_position);

        if let Some(sc) = score {
            let is_lower_better = event.goals.first().is_some_and(|g| {
                g.kind.to_uppercase() == "TIME" || g.kind.to_uppercase() == "POSITION"
            });
            ep.best_score = Some(match ep.best_score {
                Some(prev) => {
                    if is_lower_better {
                        prev.min(sc)
                    } else {
                        prev.max(sc)
                    }
                }
                None => sc,
            });
        }

        self.events.insert(event.id.clone(), ep);

        if let Some(ser) = series
            && ser.completed(self)
        {
            self.completed_series.insert(ser.id.clone());
        }

        let medal_str = match scored_medal {
            1 => "Bronze medal earned!",
            2 => "Silver medal earned!",
            3 => "Gold medal earned!",
            _ => "Event finished.",
        };
        Ok(medal_str.to_string())
    }

    fn import_career_ini(&mut self, path: &Path, root: &Path) -> Result<()> {
        let mut ini = IniFile::load(path)?;
        normalize_ini_uppercase(&mut ini);
        if let Some(comp) = ini.get("CAREER", "COMPLETE") {
            self.completed_series.extend(
                comp.split(',')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty()),
            );
        }
        let catalog = scan(root);
        for sec in ini.sections() {
            let id = sec.name.to_lowercase();
            let Some(series) = catalog.series.iter().find(|s| s.id == id) else {
                continue;
            };
            for (key, value) in sec.entries() {
                if let Some(index) = key
                    .strip_prefix("EVENT")
                    .and_then(|i| i.parse::<usize>().ok())
                {
                    let Some(event) = series.events.get(index) else {
                        continue;
                    };
                    let Ok(val) = value.parse::<u32>() else {
                        continue;
                    };
                    let ep = self.events.entry(event.id.clone()).or_default();
                    if series.championship() {
                        if val > 0 {
                            ep.last_position = Some(val);
                        }
                    } else {
                        ep.medal = val.min(3) as u8;
                    }
                } else if (key == "POINTS" || key.starts_with("AI"))
                    && let Ok(pts) = value.parse::<u32>()
                {
                    self.championship_points
                        .entry(id.clone())
                        .or_default()
                        .insert(
                            if key == "POINTS" {
                                "player".into()
                            } else {
                                key.to_lowercase()
                            },
                            pts,
                        );
                }
            }
        }
        Ok(())
    }
}

pub fn scan(root: &Path) -> Catalog {
    let mut cat = Catalog::default();
    let career_dir = root.join("content").join("career");
    if career_dir.is_dir()
        && let Ok(entries) = fs::read_dir(&career_dir)
    {
        let mut series_dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|de| de.path()))
            .filter(|p| {
                p.is_dir()
                    && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        n.starts_with("series") && n != "series0" && n != "series100"
                    })
            })
            .collect();
        series_dirs.sort_by(|a, b| {
            let na = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let nb = b.file_name().and_then(|n| n.to_str()).unwrap_or("");
            natural_cmp(na, nb)
        });

        for sdir in series_dirs {
            match load_series(&sdir) {
                Ok(s) => cat.series.push(s),
                Err(e) => cat
                    .errors
                    .push(format!("series {}: {:#}", sdir.display(), e)),
            }
        }
    }

    let special_dir = root.join("content").join("specialevents");
    if special_dir.is_dir()
        && let Ok(entries) = fs::read_dir(&special_dir)
    {
        let mut event_dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|de| de.path()))
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("SPECIAL_EVENT_"))
            })
            .collect();
        event_dirs.sort_by(|a, b| {
            let na = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let nb = b.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let num_a = na
                .trim_start_matches("SPECIAL_EVENT_")
                .parse::<u32>()
                .unwrap_or(u32::MAX);
            let num_b = nb
                .trim_start_matches("SPECIAL_EVENT_")
                .parse::<u32>()
                .unwrap_or(u32::MAX);
            num_a.cmp(&num_b).then_with(|| natural_cmp(na, nb))
        });

        for edir in event_dirs {
            let ev_id = edir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let ini_path = edir.join("event.ini");
            if ini_path.is_file() {
                match load_event(&ini_path, &ev_id, &edir) {
                    Ok(ev) => cat.special.push(ev),
                    Err(e) => cat
                        .errors
                        .push(format!("special event {}: {:#}", edir.display(), e)),
                }
            }
        }
    }

    cat
}

fn load_series(dir: &Path) -> Result<Series> {
    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let ini_path = dir.join("series.ini");
    let ini =
        IniFile::load(&ini_path).with_context(|| format!("loading {}", ini_path.display()))?;

    let code = ini
        .get("SERIES", "CODE")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&id)
        .to_string();
    let name = ini.get("SERIES", "NAME").unwrap_or(&id).to_string();
    let description = crate::content::plain_text(ini.get("SERIES", "DESCRIPTION").unwrap_or(""));
    let preview = {
        let p = dir.join("preview.png");
        if p.is_file() { Some(p) } else { None }
    };

    let requires: Vec<String> = ini
        .get("SERIES", "REQUIRES")
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let requires_any = ini
        .get("SERIES", "REQUIRESANY")
        .is_some_and(|v| v.trim() == "1");

    let points: Vec<u32> = ini
        .get("SERIES", "POINTS")
        .unwrap_or("")
        .split(',')
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .collect();

    let goal_points = ini
        .get("GOALS", "POINTS")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let goal_ranking = ini
        .get("GOALS", "RANKING")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0);

    let t1 = ini
        .get("GOALS", "TIER1")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let t2 = ini
        .get("GOALS", "TIER2")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let t3 = ini
        .get("GOALS", "TIER3")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let goal_medals = [t1, t2, t3];

    let mut events = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        let mut ev_dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|de| de.path()))
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("event"))
            })
            .collect();
        ev_dirs.sort_by(|a, b| {
            let na = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let nb = b.file_name().and_then(|n| n.to_str()).unwrap_or("");
            natural_cmp(na, nb)
        });

        for ed in ev_dirs {
            let ev_num = ed.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let event_id = format!("{}/{}", id, ev_num);
            let ev_ini = ed.join("event.ini");
            if ev_ini.is_file() {
                events.push(load_event(&ev_ini, &event_id, &ed)?);
            }
        }
    }

    let mut opponents = Vec::new();
    let opp_path = dir.join("opponents.ini");
    if opp_path.is_file()
        && let Ok(opp_ini) = IniFile::load(&opp_path)
    {
        for sec in opp_ini.sections() {
            let name = sec.name.as_str();
            if name.starts_with("AI") {
                let opp_id = name.to_lowercase();
                let opp_name = sec
                    .get("DRIVER_NAME")
                    .or_else(|| sec.get("NAME"))
                    .unwrap_or("")
                    .to_string();
                let opp_car = sec
                    .get("MODEL")
                    .or_else(|| sec.get("CAR"))
                    .unwrap_or("")
                    .to_string();
                let opp_skin = sec.get("SKIN").unwrap_or("").to_string();
                let opp_level = sec
                    .get("AI_LEVEL")
                    .or_else(|| sec.get("LEVEL"))
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or(98);
                opponents.push(Opponent {
                    id: opp_id,
                    name: opp_name,
                    car: opp_car,
                    skin: opp_skin,
                    level: opp_level,
                });
            }
        }
    }

    opponents.sort_by(|a, b| natural_cmp(&a.id, &b.id));
    Ok(Series {
        id,
        code,
        name,
        description,
        preview,
        requires,
        requires_any,
        points,
        goal_points,
        goal_ranking,
        goal_medals,
        events,
        opponents,
    })
}

fn load_event(path: &Path, id: &str, event_dir: &Path) -> Result<Event> {
    let mut ini = IniFile::load(path)?;
    normalize_ini_uppercase(&mut ini);

    let name = ini
        .get("EVENT", "NAME")
        .unwrap_or_else(|| ini.get("SESSION_0", "NAME").unwrap_or(id))
        .to_string();
    let description = crate::content::plain_text(ini.get("EVENT", "DESCRIPTION").unwrap_or(""));
    let preview = {
        let p = event_dir.join("preview.png");
        if p.is_file() { Some(p) } else { None }
    };

    let car = ini
        .get("RACE", "MODEL")
        .or_else(|| ini.get("CAR_0", "MODEL"))
        .filter(|m| *m != "-")
        .unwrap_or("")
        .to_string();
    let track = ini.get("RACE", "TRACK").unwrap_or("").to_string();
    let layout = ini.get("RACE", "CONFIG_TRACK").unwrap_or("").to_string();
    let weather = ini.get("WEATHER", "NAME").unwrap_or("3_clear").to_string();
    let ambient = ini
        .get("TEMPERATURE", "AMBIENT")
        .and_then(|v| v.trim().parse::<f64>().ok());
    let road = ini
        .get("TEMPERATURE", "ROAD")
        .and_then(|v| v.trim().parse::<f64>().ok());
    let penalties = ini.get("RACE", "PENALTIES").is_none_or(|v| v.trim() != "0");
    let fixed_setup = ini
        .get("RACE", "FIXED_SETUP")
        .is_some_and(|v| v.trim() == "1");
    let ai_level = ini
        .get("RACE", "AI_LEVEL")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(98);

    let mut sessions = Vec::new();
    for i in 0..10 {
        let sname = format!("SESSION_{}", i);
        if let Some(sec) = ini.section(&sname) {
            let name = sec.get("NAME").unwrap_or("Session").to_string();
            let kind = sec
                .get("TYPE")
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(1);
            let laps = sec
                .get("LAPS")
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(0);
            let minutes = sec
                .get("DURATION_MINUTES")
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(0);
            sessions.push(Session {
                name,
                kind,
                laps,
                minutes,
            });
        }
    }

    let cond0_type = ini
        .get("CONDITION_0", "TYPE")
        .map(|t| t.trim().to_uppercase());

    let mut goals = Vec::new();
    for i in 0..10 {
        let cname = format!("CONDITION_{}", i);
        if let Some(sec) = ini.section(&cname) {
            let mut k = sec
                .get("TYPE")
                .map(|t| t.trim().to_uppercase())
                .or_else(|| cond0_type.clone())
                .unwrap_or_else(|| "POINTS".to_string());
            if k.is_empty() {
                k = cond0_type.clone().unwrap_or_else(|| "POINTS".to_string());
            }
            let obj = sec
                .get("OBJECTIVE")
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            goals.push(Goal {
                kind: k,
                objective: obj,
            });
        }
    }

    Ok(Event {
        id: id.to_string(),
        name,
        description,
        preview,
        car,
        track,
        layout,
        weather,
        ambient,
        road,
        penalties,
        fixed_setup,
        ai_level,
        sessions,
        goals,
        ini,
    })
}

fn normalize_ini_uppercase(file: &mut IniFile) {
    let mut new_ini = IniFile::default();
    for sec in file.sections() {
        let sec_name = sec.name.to_uppercase();
        let target_sec = new_ini.section_mut(&sec_name);
        for (k, v) in sec.entries() {
            target_sec.set(&k.to_uppercase(), v);
        }
    }
    *file = new_ini;
}

pub fn goal_label(goal: &Goal) -> String {
    let kind = goal.kind.to_uppercase();
    match kind.as_str() {
        "TIME" => {
            let total_ms = goal.objective.round() as i64;
            if total_ms <= 0 {
                return "0:00.000".to_string();
            }
            let minutes = total_ms / 60_000;
            let rem_ms = total_ms % 60_000;
            let seconds = rem_ms / 1_000;
            let ms = rem_ms % 1_000;
            format!("{}:{:02}.{:03}", minutes, seconds, ms)
        }
        "POSITION" => {
            let p = goal.objective as u32;
            let suffix = match p % 10 {
                1 if p % 100 != 11 => "st",
                2 if p % 100 != 12 => "nd",
                3 if p % 100 != 13 => "rd",
                _ => "th",
            };
            format!("Finish {}{}", p, suffix)
        }
        "POINTS" => format!("{:.0} pts", goal.objective),
        "WINS" => format!("{:.0} wins", goal.objective),
        "AI" => format!("{:.0}% AI", goal.objective),
        _ => format!("{} {:.0}", goal.kind, goal.objective),
    }
}

pub fn mode_label(kind: u32) -> &'static str {
    match kind {
        1 => "Practice",
        2 => "Qualifying",
        3 => "Race",
        4 => "Hotlap",
        5 => "Time Attack",
        6 => "Drift",
        7 => "Drag",
        _ => "Special",
    }
}

pub fn available(root: &Path, event: &Event, series: Option<&Series>) -> Result<()> {
    let cars = root.join("content/cars");
    anyhow::ensure!(
        !event.car.is_empty() && cars.join(&event.car).is_dir(),
        "Car '{}' is not installed",
        event.car
    );
    let track_dir = root.join("content/tracks").join(&event.track);
    anyhow::ensure!(
        track_dir.is_dir(),
        "Track '{}' is not installed",
        event.track
    );
    let track = crate::content::load_track(&track_dir);
    anyhow::ensure!(
        track
            .layouts
            .iter()
            .any(|l| l.layout_id.as_deref().unwrap_or("") == event.layout),
        "Track layout '{}' is not installed",
        event.layout
    );
    anyhow::ensure!(
        root.join("content/weather").join(&event.weather).is_dir(),
        "Weather '{}' is not installed",
        event.weather
    );
    if let Some(series) = series.filter(|s| s.championship()) {
        for opponent in &series.opponents {
            anyhow::ensure!(
                !opponent.car.is_empty() && cars.join(&opponent.car).is_dir(),
                "Opponent car '{}' is not installed",
                opponent.car
            );
        }
    } else {
        for section in event
            .ini
            .sections()
            .iter()
            .filter(|s| s.name.starts_with("CAR_") && s.name != "CAR_0")
        {
            if let Some(model) = section.get("MODEL").filter(|m| *m != "-" && !m.is_empty()) {
                anyhow::ensure!(
                    cars.join(model).is_dir(),
                    "Opponent car '{}' is not installed",
                    model
                );
            }
        }
    }
    Ok(())
}

pub fn prepare(
    root: &Path,
    event: &Event,
    series: Option<&Series>,
    difficulty: usize,
    driver: &str,
    nationality: &str,
) -> Result<PathBuf> {
    available(root, event, series)?;

    let target_path =
        race_ini_path(root).ok_or_else(|| anyhow!("Cannot locate AC race.ini path"))?;

    let mut ini = IniFile::load_or_default(&root.join("cfg/race.ini"))?;
    if ini.sections().is_empty() {
        ini = IniFile::load_or_default(&target_path)?;
    }
    ini.remove_section("CAR_0");
    race::clear_up(&mut ini);

    let cond0_type = event
        .ini
        .get("CONDITION_0", "TYPE")
        .map(|t| t.trim().to_uppercase());

    for sec in event.ini.sections() {
        let sec_name = sec.name.to_uppercase();
        if sec_name == "EVENT" || sec_name == "ASSISTS" {
            continue;
        }

        let target_sec = ini.section_mut(&sec_name);
        for (k, v) in sec.entries() {
            let key = k.to_uppercase();
            if sec_name.starts_with("CONDITION_")
                && key == "TYPE"
                && v.trim().is_empty()
                && let Some(c0) = &cond0_type
            {
                target_sec.set("TYPE", c0);
                continue;
            }
            target_sec.set(&key, v);
        }

        if sec_name.starts_with("CONDITION_")
            && !sec.entries().any(|(k, _)| k.eq_ignore_ascii_case("TYPE"))
            && let Some(c0) = &cond0_type
        {
            target_sec.set("TYPE", c0);
        }
    }

    let is_special_ai = event
        .goals
        .first()
        .is_some_and(|g| g.kind.to_uppercase() == "AI");
    if is_special_ai {
        let diff_idx = difficulty.clamp(0, 2);
        let goal_obj = event
            .goals
            .get(diff_idx)
            .map(|g| g.objective)
            .or_else(|| event.goals.last().map(|g| g.objective))
            .unwrap_or(98.0) as u32;

        ini.section_mut("RACE").set("AI_LEVEL", goal_obj);
        let names: Vec<_> = ini.sections().iter().map(|s| s.name.clone()).collect();
        for sname in names {
            if sname.starts_with("CAR_") && sname != "CAR_0" {
                ini.section_mut(&sname).set("AI_LEVEL", goal_obj);
            }
        }
    }

    if let Some(ser) = series
        && ser.championship()
    {
        ini.remove_numbered("CAR", 1);
        let mut index = 1;
        for opp in &ser.opponents {
            let car_sec = format!("CAR_{}", index);
            let c = ini.section_mut(&car_sec);
            c.set("DRIVER_NAME", &opp.name);
            c.set("NATIONALITY", "");
            c.set("NATION_CODE", "");
            c.set("AI_LEVEL", opp.level);
            c.set("MODEL", opp.car.to_lowercase());
            c.set("MODEL_CONFIG", "");
            c.set("SKIN", opp.skin.to_lowercase());
            c.set("SETUP", "");
            index += 1;
        }
        ini.section_mut("RACE").set("CARS", index);
    }

    let race = ini.section_mut("RACE");
    if !event.car.is_empty() {
        race.set("MODEL", event.car.to_lowercase());
    }
    race.set("TRACK", event.track.to_lowercase());
    race.set("CONFIG_TRACK", event.layout.to_lowercase());
    race.set_bool("PENALTIES", event.penalties);
    race.set_bool("FIXED_SETUP", event.fixed_setup);

    if event.ini.get("WEATHER", "NAME").is_none() || event.ini.get("WEATHER", "NAME") == Some("") {
        let weather = if !event.weather.is_empty() {
            &event.weather
        } else {
            "3_clear"
        };
        ini.section_mut("WEATHER").set("NAME", weather);
    }

    let cars = scan_cars(root);
    let car_model = event.car.to_lowercase();
    let car_info = cars.iter().find(|c| c.id.eq_ignore_ascii_case(&car_model));

    let car0 = ini.section_mut("CAR_0");
    car0.set("MODEL", "-");
    car0.set("DRIVER_NAME", driver);
    car0.set("NATIONALITY", nationality);
    car0.set("NATION_CODE", nationality);
    car0.set("SETUP", "");

    let existing_skin = car0.get("SKIN").unwrap_or("").to_string();
    let skin_is_valid = if let Some(ci) = car_info {
        ci.skins
            .iter()
            .any(|s| s.id.eq_ignore_ascii_case(&existing_skin))
    } else {
        !existing_skin.is_empty()
    };

    if !skin_is_valid {
        let fallback_skin = car_info
            .and_then(|ci| ci.skins.first())
            .map(|s| s.id.clone())
            .unwrap_or_default();
        car0.set("SKIN", fallback_skin);
    }

    ini.save(&target_path)?;
    Ok(target_path)
}
