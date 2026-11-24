//! Career and special-event browsing, with persistent, result-backed progress.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ac_core::{
    content,
    events::{self, Catalog, Event, EventProgress, Progress, Series},
    launch, paths,
};
use gpui::{
    AnyElement, Context, Div, Entity, EventEmitter, FontWeight, ObjectFit, Render, ScrollStrategy,
    UniformListScrollHandle, Window, div, img, prelude::*, px, rgb, uniform_list,
};

use crate::{
    text_input::TextInput,
    theme,
    widgets::{button, chip, field, focus_ring, panel, picture, primary_button, section_title},
};

pub struct Running(pub bool);
impl EventEmitter<Running> for EventsView {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CareerScreen {
    Series,
    Events,
    Detail,
}

#[derive(Default)]
struct MetaCache {
    cars: BTreeMap<String, (String, Option<PathBuf>)>,
    tracks: BTreeMap<(String, String), String>,
}

impl MetaCache {
    fn build(root: &Path, catalog: &Catalog) -> Self {
        let mut cache = Self::default();
        let all_events = catalog
            .series
            .iter()
            .flat_map(|s| s.events.iter())
            .chain(catalog.special.iter());
        for e in all_events {
            if !e.car.is_empty() && !cache.cars.contains_key(&e.car) {
                let car = content::load_car(&root.join("content/cars").join(&e.car));
                let preview = car.preview().map(Path::to_path_buf);
                cache.cars.insert(e.car.clone(), (car.name, preview));
            }
            let key = (e.track.clone(), e.layout.clone());
            if !e.track.is_empty() && !cache.tracks.contains_key(&key) {
                let track = content::load_track(&root.join("content/tracks").join(&e.track));
                let name = track
                    .layouts
                    .iter()
                    .find(|l| l.layout_id.as_deref().unwrap_or("") == e.layout)
                    .map(|l| l.name.clone())
                    .unwrap_or_else(|| track.id.clone());
                cache.tracks.insert(key, name);
            }
        }
        cache
    }

    fn car_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.cars.get(id).map(|(n, _)| n.as_str()).unwrap_or(id)
    }

    fn car_preview(&self, id: &str) -> Option<PathBuf> {
        self.cars.get(id).and_then(|(_, p)| p.clone())
    }

    fn track_name<'a>(&'a self, track: &'a str, layout: &str) -> &'a str {
        self.tracks
            .get(&(track.to_string(), layout.to_string()))
            .map(String::as_str)
            .unwrap_or(track)
    }
}

pub struct EventsView {
    pub root: PathBuf,
    catalog: Catalog,
    progress: Progress,
    meta: MetaCache,
    career: bool,
    screen: CareerScreen,
    series: usize,
    selected: usize,
    series_cols: usize,
    event_cols: usize,
    search: Entity<TextInput>,
    visible: Vec<usize>,
    filter: u8,
    difficulty: usize,
    restart_confirm: bool,
    show_config: bool,
    scroll: UniformListScrollHandle,
    loading: bool,
    pub running: bool,
    error: Option<String>,
    message: Option<String>,
    driver: String,
    nationality: String,
    proton: Option<PathBuf>,
}

fn medal(n: u8) -> &'static str {
    match n {
        1 => "Bronze",
        2 => "Silver",
        3 => "Gold",
        _ => "Not earned",
    }
}

fn medal_short(n: u8) -> &'static str {
    match n {
        1 => "Bronze",
        2 => "Silver",
        3 => "Gold",
        _ => "—",
    }
}

fn medal_color(n: u8) -> gpui::Rgba {
    match n {
        1 => rgb(0xcd9360),
        2 => rgb(0xc6cbd4),
        3 => rgb(0xe7bc53),
        _ => theme::muted(),
    }
}

fn score_heading(event: &Event, championship: bool) -> &'static str {
    if championship {
        return "Finish";
    }
    match event
        .goals
        .first()
        .map(|g| g.kind.to_uppercase())
        .as_deref()
    {
        Some("TIME") => "Best time",
        Some("POSITION") => "Best finish",
        Some("POINTS") => "Best score",
        Some("WINS") => "Best wins",
        _ => "Best result",
    }
}

fn score_display(event: &Event, p: &EventProgress, championship: bool) -> String {
    if championship {
        return p
            .last_position
            .map(|pos| format!("P{pos}"))
            .unwrap_or_else(|| "—".into());
    }
    if let Some(score) = p.best_score {
        if let Some(g) = event.goals.first() {
            if g.kind.eq_ignore_ascii_case("POSITION") {
                return format!("P{}", score.round() as u32);
            }
            return events::goal_label(&events::Goal {
                kind: g.kind.clone(),
                objective: score,
            });
        }
        return format!("{score:.0}");
    }
    if let Some(pos) = p.last_position {
        return format!("P{pos}");
    }
    "—".into()
}

fn line(text: impl Into<gpui::SharedString>, width: f32) -> Div {
    div().w(px(width.max(80.))).text_sm().child(text.into())
}

fn badge(label: impl Into<gpui::SharedString>, color: gpui::Rgba, bg: gpui::Rgba) -> Div {
    div()
        .px_2()
        .py_0p5()
        .rounded(px(theme::RADIUS_SM))
        .bg(bg)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(color)
        .flex_shrink_0()
        .child(label.into())
}

fn series_icon(series: &Series, side: f32, locked: bool) -> Div {
    let frame = div()
        .size(px(side))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .rounded(px(theme::RADIUS_SM))
        .bg(theme::bg())
        .border_1()
        .border_color(theme::border());
    if let Some(path) = &series.preview {
        frame.child(
            img(path.clone())
                .size(px(side))
                .rounded(px(theme::RADIUS_SM))
                .object_fit(ObjectFit::Contain)
                .when(locked, |el| el.opacity(0.45)),
        )
    } else {
        frame.child(
            div()
                .text_base()
                .font_weight(FontWeight::BOLD)
                .text_color(if locked { theme::faint() } else { theme::text() })
                .child(series.code.clone()),
        )
    }
}

impl EventsView {
    pub fn new(
        root: PathBuf,
        career: bool,
        driver: String,
        nationality: String,
        proton: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| TextInput::new(cx, "Search events, cars, tracks…", ""));
        cx.observe(&search, |this, _, cx| {
            this.refilter(cx);
            cx.notify();
        })
        .detach();
        let mut view = Self {
            root,
            catalog: Catalog::default(),
            progress: Progress::default(),
            meta: MetaCache::default(),
            career,
            screen: CareerScreen::Series,
            series: 0,
            selected: 0,
            series_cols: 3,
            event_cols: 3,
            search,
            visible: Vec::new(),
            filter: 0,
            difficulty: 2,
            restart_confirm: false,
            show_config: false,
            scroll: UniformListScrollHandle::new(),
            loading: false,
            running: false,
            error: None,
            message: None,
            driver,
            nationality,
            proton,
        };
        view.reload(cx);
        view
    }

    pub fn configure(
        &mut self,
        career: bool,
        driver: String,
        nationality: String,
        proton: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.restart_confirm = false;
        let mode_changed = self.career != career;
        self.career = career;
        self.driver = driver;
        self.nationality = nationality;
        self.proton = proton;
        if mode_changed {
            self.message = None;
            if career {
                self.screen = CareerScreen::Series;
            }
        }
        self.refilter(cx);
        cx.notify();
    }

    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        self.running = running;
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.running || self.loading {
            return;
        }
        self.loading = true;
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let (catalog, progress, meta) = cx
                .background_executor()
                .spawn(async move {
                    let catalog = events::scan(&root);
                    let progress = Progress::load(&root);
                    let meta = MetaCache::build(&root, &catalog);
                    (catalog, progress, meta)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.catalog = catalog;
                this.meta = meta;
                match progress {
                    Ok(p) => {
                        this.progress = p;
                        this.error = None;
                    }
                    Err(e) => {
                        this.error = Some(format!(
                            "Cannot load progress: {e:#}. Launching is disabled to protect the save."
                        ))
                    }
                }
                this.loading = false;
                this.series = this
                    .series
                    .min(this.catalog.series.len().saturating_sub(1));
                if this.career
                    && this
                        .current_series()
                        .is_none_or(|s| !s.unlocked_in(&this.catalog, &this.progress))
                {
                    this.screen = CareerScreen::Series;
                }
                this.refilter(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn current_series(&self) -> Option<&Series> {
        self.career
            .then(|| self.catalog.series.get(self.series))
            .flatten()
    }

    fn list(&self) -> &[Event] {
        if self.career {
            self.current_series()
                .map(|s| s.events.as_slice())
                .unwrap_or(&[])
        } else {
            &self.catalog.special
        }
    }

    fn current_event(&self) -> Option<&Event> {
        self.list().get(self.selected)
    }

    fn requirement_names(&self, series: &Series) -> String {
        series
            .requires
            .iter()
            .map(|id| {
                self.catalog
                    .series
                    .iter()
                    .find(|s| s.id == *id)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| id.clone())
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn open_series(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(series) = self.catalog.series.get(index) else {
            return;
        };
        if !series.unlocked_in(&self.catalog, &self.progress) {
            let req = self.requirement_names(series);
            let mode = if series.requires_any {
                "any of"
            } else {
                "all of"
            };
            self.message = Some(format!(
                "{} is locked. Complete {} {} first.",
                series.name, mode, req
            ));
            cx.notify();
            return;
        }
        self.series = index;
        self.restart_confirm = false;
        self.show_config = false;
        self.message = None;
        let first_unplayed = series
            .events
            .iter()
            .enumerate()
            .find(|(i, e)| {
                let p = self.progress.get(&e.id);
                let done = if series.championship() {
                    p.last_position.is_some()
                } else {
                    p.medal > 0
                };
                !done && series.event_unlocked(*i, &self.catalog, &self.progress)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.selected = first_unplayed;
        self.screen = CareerScreen::Events;
        self.refilter(cx);
        cx.notify();
    }

    fn open_event(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(series) = self.current_series() else {
            return;
        };
        if !series.event_unlocked(index, &self.catalog, &self.progress) {
            self.message = Some(
                "This round is locked. Complete the previous championship round first.".into(),
            );
            cx.notify();
            return;
        }
        self.selected = index;
        self.show_config = false;
        self.message = None;
        self.screen = CareerScreen::Detail;
        cx.notify();
    }

    fn step_event(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(series) = self.current_series() else {
            return;
        };
        let next = self.selected as isize + delta;
        if next < 0 || next as usize >= series.events.len() {
            return;
        }
        let idx = next as usize;
        if !series.event_unlocked(idx, &self.catalog, &self.progress) {
            self.message = Some(
                "Next round is locked. Complete the current championship round first.".into(),
            );
            cx.notify();
            return;
        }
        self.selected = idx;
        self.show_config = false;
        self.message = None;
        cx.notify();
    }

    fn refilter(&mut self, cx: &Context<Self>) {
        self.restart_confirm = false;
        if self.career {
            let len = self.list().len();
            if len == 0 {
                self.selected = 0;
            } else if self.selected >= len {
                self.selected = len - 1;
            }
            self.visible = (0..len).collect();
            return;
        }
        let q = self.search.read(cx).text().to_lowercase();
        self.visible = self
            .list()
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let record = self.progress.get(&e.id);
                let earned = record.medal;
                let completed = earned > 0;
                let matches = q.split_whitespace().all(|word| {
                    format!("{} {} {} {}", e.name, e.car, e.track, e.description)
                        .to_lowercase()
                        .contains(word)
                });
                matches
                    && match self.filter {
                        1 => !completed,
                        2 => completed,
                        3 => earned == 3,
                        _ => true,
                    }
            })
            .map(|(i, _)| i)
            .collect();
        if !self.visible.contains(&self.selected) {
            self.selected = self.visible.first().copied().unwrap_or(usize::MAX);
        }
    }

    /// Returns true if the button press was consumed by this view.
    pub fn pad(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        if self.career {
            return self.pad_career(name, cx);
        }
        if name == "B" {
            return false;
        }
        match name {
            "DPAD_UP" | "DPAD_DOWN" if !self.visible.is_empty() => {
                let p = self
                    .visible
                    .iter()
                    .position(|&i| i == self.selected)
                    .unwrap_or(0);
                let next = if name == "DPAD_UP" {
                    p.saturating_sub(1)
                } else {
                    (p + 1).min(self.visible.len() - 1)
                };
                self.selected = self.visible[next];
                self.scroll.scroll_to_item(next, ScrollStrategy::Center);
            }
            "X" => self.difficulty = (self.difficulty + 1) % 3,
            "START" | "A" => self.start(cx),
            _ => {}
        }
        cx.notify();
        true
    }

    fn pad_career(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        match self.screen {
            CareerScreen::Series => {
                let n = self.catalog.series.len();
                let cols = self.series_cols.max(1);
                match name {
                    "B" => return false,
                    "DPAD_LEFT" if n > 0 => self.series = self.series.saturating_sub(1),
                    "DPAD_RIGHT" if n > 0 => self.series = (self.series + 1).min(n - 1),
                    "DPAD_UP" if n > 0 => self.series = self.series.saturating_sub(cols),
                    "DPAD_DOWN" if n > 0 => self.series = (self.series + cols).min(n - 1),
                    "A" | "START" if n > 0 => {
                        self.open_series(self.series, cx);
                        return true;
                    }
                    _ => {}
                }
                cx.notify();
                true
            }
            CareerScreen::Events => {
                let n = self.list().len();
                let cols = self.event_cols.max(1);
                match name {
                    "B" => {
                        self.screen = CareerScreen::Series;
                        self.message = None;
                        cx.notify();
                        return true;
                    }
                    "DPAD_LEFT" if n > 0 => self.selected = self.selected.saturating_sub(1),
                    "DPAD_RIGHT" if n > 0 => self.selected = (self.selected + 1).min(n - 1),
                    "DPAD_UP" if n > 0 => self.selected = self.selected.saturating_sub(cols),
                    "DPAD_DOWN" if n > 0 => self.selected = (self.selected + cols).min(n - 1),
                    "A" | "START" if n > 0 => {
                        self.open_event(self.selected, cx);
                        return true;
                    }
                    _ => {}
                }
                cx.notify();
                true
            }
            CareerScreen::Detail => {
                match name {
                    "B" => {
                        self.screen = CareerScreen::Events;
                        self.message = None;
                        cx.notify();
                        return true;
                    }
                    "DPAD_LEFT" => {
                        self.step_event(-1, cx);
                        return true;
                    }
                    "DPAD_RIGHT" => {
                        self.step_event(1, cx);
                        return true;
                    }
                    "A" | "START" => {
                        self.start(cx);
                        return true;
                    }
                    _ => {}
                }
                cx.notify();
                true
            }
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.running || self.loading || self.error.is_some() {
            return;
        }
        let Some(event) = self.current_event().cloned() else {
            return;
        };
        let series = self.current_series().cloned();
        if let Some(s) = series.as_ref() {
            if !s.unlocked_in(&self.catalog, &self.progress) {
                self.message = Some("Complete the required series first.".into());
                cx.notify();
                return;
            }
            if !s.event_unlocked(self.selected, &self.catalog, &self.progress) {
                self.message = Some("Complete the previous championship round first.".into());
                cx.notify();
                return;
            }
            if s.championship() && self.progress.get(&event.id).last_position.is_some() {
                self.message = Some("This championship round is already scored. Use Restart championship to run it again.".into());
                cx.notify();
                return;
            }
        }
        let root = self.root.clone();
        if let Err(e) = events::prepare(
            &root,
            &event,
            series.as_ref(),
            self.difficulty,
            &self.driver,
            &self.nationality,
        ) {
            self.message = Some(format!("Cannot launch: {e:#}"));
            cx.notify();
            return;
        }
        let result_path = paths::documents_ac_dir(&root).map(|d| d.join("out/race_out.json"));
        let previous = result_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        let began = SystemTime::now();
        match launch::start(
            &root,
            &launch::LaunchOptions {
                proton: self.proton.clone(),
            },
        ) {
            Err(e) => {
                self.message = Some(format!("Cannot start game: {e:#}"));
                cx.notify();
            }
            Ok(mut child) => {
                self.running = true;
                self.message = Some(format!("Running {}…", event.name));
                cx.emit(Running(true));
                cx.notify();
                cx.spawn(async move |this, cx| {
                    let exit = cx
                        .background_executor()
                        .spawn(async move { child.wait() })
                        .await;
                    let result = (|| -> anyhow::Result<serde_json::Value> {
                        let path =
                            result_path.ok_or_else(|| anyhow::anyhow!("AC Documents not found"))?;
                        let changed = std::fs::metadata(&path)?.modified()?;
                        anyhow::ensure!(
                            Some(changed) != previous && changed >= began,
                            "No new race result; progress was not changed"
                        );
                        let data: serde_json::Value =
                            serde_json::from_slice(&std::fs::read(&path)?)?;
                        let expected_track = if event.layout.is_empty() {
                            event.track.clone()
                        } else {
                            format!("{}-{}", event.track, event.layout)
                        };
                        anyhow::ensure!(
                            data["track"].as_str() == Some(expected_track.as_str()),
                            "Result track does not match this event"
                        );
                        anyhow::ensure!(
                            data["players"][0]["car"].as_str() == Some(event.car.as_str()),
                            "Result car does not match this event"
                        );
                        Ok(data)
                    })();
                    let _ = this.update(cx, |this, cx| {
                        this.running = false;
                        cx.emit(Running(false));
                        this.message = Some(match exit {
                            Ok(code) if code.success() => match result {
                                Ok(data) => {
                                    let mut next = this.progress.clone();
                                    match next.record(&event, series.as_ref(), &data).and_then(
                                        |msg| {
                                            next.save(&root)?;
                                            Ok(msg)
                                        },
                                    ) {
                                        Ok(msg) => {
                                            this.progress = next;
                                            msg
                                        }
                                        Err(e) => format!("Progress not saved: {e:#}"),
                                    }
                                }
                                Err(e) => format!("Game closed. {e:#}"),
                            },
                            Ok(code) => format!("Game exited with {code}; progress unchanged."),
                            Err(e) => format!("Cannot wait for game: {e}"),
                        });
                        this.refilter(cx);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        if self.running || self.error.is_some() {
            return;
        }
        let Some(series) = self.current_series().cloned() else {
            return;
        };
        if !self.restart_confirm {
            self.restart_confirm = true;
            self.message = Some("Restart clears this championship run’s points and round results. Press Confirm restart to continue.".into());
            cx.notify();
            return;
        }
        self.restart_confirm = false;
        let id = series.id.clone();
        let ids: Vec<_> = series.events.iter().map(|e| e.id.clone()).collect();
        let mut next = self.progress.clone();
        next.championship_points.remove(&id);
        // Preserve previously earned completion/unlocks, reset only this run's rounds.
        for key in ids {
            next.events.remove(&key);
        }
        match next.save(&self.root) {
            Ok(()) => {
                self.progress = next;
                self.selected = 0;
                self.message =
                    Some("Championship restarted. Previous series unlocks are preserved.".into());
            }
            Err(e) => self.message = Some(format!("Cannot restart: {e:#}")),
        }
        self.refilter(cx);
        cx.notify();
    }

    fn overview(&self) -> String {
        let all: Vec<&Event> = if self.career {
            self.catalog
                .series
                .iter()
                .flat_map(|s| s.events.iter())
                .collect()
        } else {
            self.catalog.special.iter().collect()
        };
        let mut counts = [0usize; 3];
        for e in &all {
            let m = self.progress.get(&e.id).medal;
            if (1..=3).contains(&m) {
                counts[m as usize - 1] += 1;
            }
        }
        if self.career {
            let unlocked = self
                .catalog
                .series
                .iter()
                .filter(|s| s.unlocked_in(&self.catalog, &self.progress))
                .count();
            format!(
                "{} / {} series complete  ·  {} unlocked  ·  Gold {}  Silver {}  Bronze {}",
                self.catalog
                    .series
                    .iter()
                    .filter(|s| s.completed(&self.progress))
                    .count(),
                self.catalog.series.len(),
                unlocked,
                counts[2],
                counts[1],
                counts[0]
            )
        } else {
            format!(
                "{} events  ·  Gold {}  Silver {}  Bronze {}  ·  {} not earned",
                all.len(),
                counts[2],
                counts[1],
                counts[0],
                all.len() - counts.iter().sum::<usize>()
            )
        }
    }

    fn render_banners(&self, main_w: f32) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .when_some(self.error.clone(), |d, e| {
                d.child(
                    line(e, main_w - 32.)
                        .px_6()
                        .py_2()
                        .text_color(theme::error()),
                )
            })
            .when_some(self.message.clone(), |d, m| {
                d.child(
                    line(m, main_w - 32.)
                        .px_6()
                        .py_2()
                        .text_color(theme::accent()),
                )
            })
            .when(!self.catalog.errors.is_empty(), |d| {
                d.child(
                    line(self.catalog.errors.join(" · "), main_w - 32.)
                        .px_6()
                        .py_2()
                        .text_color(theme::error()),
                )
            })
            .when(self.loading, |d| {
                d.child(
                    div()
                        .px_6()
                        .py_3()
                        .text_sm()
                        .text_color(theme::muted())
                        .child("Loading career and progress…"),
                )
            })
    }

    fn standings_panel(&self, s: &Series, width: f32) -> Div {
        let mut standings = panel()
            .flex()
            .flex_col()
            .gap_1()
            .child(section_title("Championship standings"))
            .child(
                line(
                    format!(
                        "Scoring: {}",
                        s.points
                            .iter()
                            .map(u32::to_string)
                            .collect::<Vec<_>>()
                            .join(" / ")
                    ),
                    width - 28.,
                )
                .text_color(theme::muted()),
            );
        for (i, (id, points)) in s.standings(&self.progress).into_iter().enumerate() {
            let is_player = id == "player";
            let driver_name = if is_player {
                self.driver.clone()
            } else {
                s.opponents
                    .iter()
                    .find(|o| o.id == id)
                    .map(|o| o.name.clone())
                    .unwrap_or(id)
            };
            standings = standings.child(
                div()
                    .flex()
                    .justify_between()
                    .gap_2()
                    .text_sm()
                    .when(is_player, |d| {
                        d.text_color(theme::accent()).font_weight(FontWeight::BOLD)
                    })
                    .child(format!("{}. {}", i + 1, driver_name))
                    .child(format!("{points} pts")),
            );
        }
        standings
    }

    fn render_career_series(&mut self, main_w: f32, cx: &mut Context<Self>) -> Div {
        let pad_x = 24.0f32;
        let gap = 14.0f32;
        let avail_w = (main_w - 2.0 * pad_x).max(320.0);
        let cols = ((avail_w + gap) / 310.0).floor().clamp(1.0, 5.0) as usize;
        self.series_cols = cols;
        let card_w = ((avail_w - gap * (cols.saturating_sub(1) as f32)) / cols as f32)
            .floor()
            .max(220.0);
        let text_w = (card_w - 64.0 - 12.0 - 28.0).max(110.0);

        let mut grid = div()
            .flex()
            .flex_col()
            .gap(px(gap))
            .px(px(pad_x))
            .py_4();

        for (row_idx, chunk) in self.catalog.series.chunks(cols).enumerate() {
            let mut row = div().flex().gap(px(gap));
            for (col_idx, s) in chunk.iter().enumerate() {
                let i = row_idx * cols + col_idx;
                let unlocked = s.unlocked_in(&self.catalog, &self.progress);
                let completed = s.completed(&self.progress);
                let counts = s.medal_counts(&self.progress);
                let req = self.requirement_names(s);
                let focused = i == self.series;

                let status_pill = if !unlocked {
                    badge("LOCKED", theme::faint(), theme::raised())
                } else if completed {
                    badge("COMPLETE", theme::on_accent(), theme::ok())
                } else if s.championship() {
                    badge("CHAMPIONSHIP", theme::muted(), theme::raised())
                } else {
                    badge("OPEN", theme::text(), theme::raised())
                };

                let progress_line = if !unlocked {
                    format!(
                        "Requires{}: {}",
                        if s.requires_any { " any" } else { "" },
                        if req.is_empty() {
                            "previous series".into()
                        } else {
                            req
                        }
                    )
                } else if s.championship() {
                    let done = s
                        .events
                        .iter()
                        .filter(|e| self.progress.get(&e.id).last_position.is_some())
                        .count();
                    let pts = self
                        .progress
                        .championship_points
                        .get(&s.id)
                        .and_then(|m| m.get("player"))
                        .copied()
                        .unwrap_or(0);
                    format!(
                        "{done}/{} rounds · {pts} pts · Goal: {}",
                        s.events.len(),
                        s.goal_summary()
                    )
                } else {
                    format!(
                        "{} events · Goal: {}",
                        s.events.len(),
                        s.goal_summary()
                    )
                };

                let base = div()
                    .id(("career-series-card", i))
                    .w(px(card_w))
                    .p_3()
                    .rounded(px(theme::RADIUS_MD))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when(unlocked, |d| {
                        d.bg(theme::surface())
                            .cursor_pointer()
                            .hover(|st| st.bg(theme::raised()))
                    })
                    .when(!unlocked, |d| {
                        d.bg(theme::bg())
                            .border_1()
                            .border_color(theme::border())
                            .opacity(0.42)
                    })
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_3()
                            .child(series_icon(s, 64., !unlocked))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .w(px(text_w))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_2()
                                            .child(badge(
                                                s.code.clone(),
                                                if unlocked {
                                                    theme::text()
                                                } else {
                                                    theme::faint()
                                                },
                                                theme::overlay(),
                                            ))
                                            .child(status_pill),
                                    )
                                    .child(
                                        div()
                                            .w(px(text_w))
                                            .text_sm()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(if unlocked {
                                                theme::text()
                                            } else {
                                                theme::muted()
                                            })
                                            .child(s.name.clone()),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .pt_2()
                            .border_t_1()
                            .border_color(theme::border())
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .w(px(card_w - 28.))
                                    .text_xs()
                                    .text_color(if unlocked {
                                        theme::muted()
                                    } else {
                                        theme::faint()
                                    })
                                    .child(progress_line),
                            )
                            .when(unlocked && !s.championship(), |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .text_xs()
                                        .child(
                                            div()
                                                .text_color(medal_color(3))
                                                .child(format!("Gold {}", counts[2])),
                                        )
                                        .child(
                                            div()
                                                .text_color(medal_color(2))
                                                .child(format!("Silver {}", counts[1])),
                                        )
                                        .child(
                                            div()
                                                .text_color(medal_color(1))
                                                .child(format!("Bronze {}", counts[0])),
                                        ),
                                )
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_series(i, cx);
                    }));

                let card = if unlocked {
                    focus_ring(base, focused)
                } else {
                    base
                };
                row = row.child(card);
            }
            grid = grid.child(row);
        }

        if self.catalog.series.is_empty() && !self.loading {
            grid = grid.child(
                div()
                    .p_6()
                    .text_color(theme::muted())
                    .child("No career series found in content/career."),
            );
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_6()
                    .py_4()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("Career"),
                            )
                            .child(
                                line(self.overview(), main_w - 220.).text_color(theme::muted()),
                            ),
                    )
                    .child(
                        button("events-refresh", "Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .child(
                line(events::import_notice(), main_w - 48.)
                    .px_6()
                    .py_1()
                    .text_color(theme::faint())
                    .text_xs(),
            )
            .child(self.render_banners(main_w))
            .child(
                div()
                    .id("career-series-grid")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(grid),
            )
    }

    fn render_career_events(&mut self, main_w: f32, cx: &mut Context<Self>) -> Div {
        let Some(series) = self.current_series().cloned() else {
            self.screen = CareerScreen::Series;
            return self.render_career_series(main_w, cx);
        };

        let pad_x = 24.0f32;
        let gap = 14.0f32;
        let avail_w = (main_w - 2.0 * pad_x).max(320.0);
        let cols = ((avail_w + gap) / 320.0).floor().clamp(1.0, 4.0) as usize;
        self.event_cols = cols;
        let card_w = ((avail_w - gap * (cols.saturating_sub(1) as f32)) / cols as f32)
            .floor()
            .max(240.0);

        let completed = series.completed(&self.progress);
        let counts = series.medal_counts(&self.progress);
        let player_pts = self
            .progress
            .championship_points
            .get(&series.id)
            .and_then(|m| m.get("player"))
            .copied()
            .unwrap_or(0);

        let done_rounds = series
            .events
            .iter()
            .filter(|e| self.progress.get(&e.id).last_position.is_some())
            .count();

        let progress_summary = div()
            .flex()
            .flex_col()
            .items_end()
            .gap_1()
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme::accent())
                    .child(format!("Goal: {}", series.goal_summary())),
            )
            .child(if series.championship() {
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme::text())
                    .child(format!(
                        "{player_pts} pts  ·  {done_rounds}/{} rounds",
                        series.events.len()
                    ))
            } else {
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .child(
                        div()
                            .text_color(medal_color(3))
                            .child(format!("Gold {}", counts[2])),
                    )
                    .child(
                        div()
                            .text_color(medal_color(2))
                            .child(format!("Silver {}", counts[1])),
                    )
                    .child(
                        div()
                            .text_color(medal_color(1))
                            .child(format!("Bronze {}", counts[0])),
                    )
            });

        let mut content_col = div()
            .flex()
            .flex_col()
            .gap_4()
            .px(px(pad_x))
            .py_4();

        // Unified series header card (navigation, title, goal + medals on right, actions, and description)
        let mut header_card = panel()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                button("career-back-series", "← All Series").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.screen = CareerScreen::Series;
                                        this.message = None;
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(series_icon(&series, 48., false))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(badge(
                                                series.code.clone(),
                                                theme::text(),
                                                theme::overlay(),
                                            ))
                                            .child(
                                                div()
                                                    .text_xl()
                                                    .font_weight(FontWeight::BOLD)
                                                    .child(series.name.clone()),
                                            )
                                            .when(completed, |d| {
                                                d.child(badge(
                                                    "COMPLETE",
                                                    theme::on_accent(),
                                                    theme::ok(),
                                                ))
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::muted())
                                            .child(if series.championship() {
                                                format!("{} rounds · Championship", series.events.len())
                                            } else {
                                                format!("{} events", series.events.len())
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .child(progress_summary)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .when(series.championship(), |d| {
                                        d.child(
                                            button(
                                                "championship-restart",
                                                if self.restart_confirm {
                                                    "Confirm restart"
                                                } else {
                                                    "Restart championship"
                                                },
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| this.restart(cx))),
                                        )
                                    })
                                    .child(
                                        button("events-refresh", "Refresh")
                                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                                    ),
                            ),
                    ),
            );
        if !series.description.is_empty() {
            header_card = header_card.child(
                div()
                    .pt_3()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(
                        line(series.description.clone(), avail_w - 32.)
                            .text_color(theme::muted()),
                    ),
            );
        }
        content_col = content_col.child(header_card);

        // Events grid
        let mut grid = div().flex().flex_col().gap(px(gap));
        for (row_idx, chunk) in series.events.chunks(cols).enumerate() {
            let mut row = div().flex().gap(px(gap));
            for (col_idx, e) in chunk.iter().enumerate() {
                let i = row_idx * cols + col_idx;
                let ev_unlocked = series.event_unlocked(i, &self.catalog, &self.progress);
                let p = self.progress.get(&e.id);
                let mode = e
                    .sessions
                    .last()
                    .map(|s| events::mode_label(s.kind))
                    .unwrap_or("Event");
                let car_name = self.meta.car_name(&e.car).to_string();
                let track_name = self.meta.track_name(&e.track, &e.layout).to_string();
                let preview = e
                    .preview
                    .clone()
                    .or_else(|| self.meta.car_preview(&e.car));
                let best_label = score_heading(e, series.championship());
                let best_val = score_display(e, &p, series.championship());
                let gold_hint = (!series.championship())
                    .then(|| e.goals.get(2).map(events::goal_label))
                    .flatten();

                let status_badge = if !ev_unlocked {
                    badge("LOCKED", theme::faint(), theme::raised())
                } else if series.championship() {
                    if let Some(pos) = p.last_position {
                        badge(format!("P{pos}"), theme::on_accent(), theme::ok())
                    } else {
                        badge("NEXT ROUND", theme::on_accent(), theme::accent())
                    }
                } else if p.medal > 0 {
                    badge(
                        medal(p.medal).to_uppercase(),
                        theme::on_accent(),
                        medal_color(p.medal),
                    )
                } else {
                    badge("NO MEDAL", theme::muted(), theme::raised())
                };

                let base = div()
                    .id(("career-event-card", i))
                    .w(px(card_w))
                    .p_3()
                    .rounded(px(theme::RADIUS_MD))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(ev_unlocked, |d| {
                        d.bg(theme::surface())
                            .cursor_pointer()
                            .hover(|st| st.bg(theme::raised()))
                    })
                    .when(!ev_unlocked, |d| {
                        d.bg(theme::bg())
                            .border_1()
                            .border_color(theme::border())
                            .opacity(0.42)
                    })
                    .child(picture(preview, 112.).when(!ev_unlocked, |d| d.opacity(0.5)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(badge(
                                        format!("#{}", i + 1),
                                        theme::text(),
                                        theme::overlay(),
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::muted())
                                            .child(mode),
                                    ),
                            )
                            .child(status_badge),
                    )
                    .child(
                        div()
                            .w(px(card_w - 28.))
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .text_color(if ev_unlocked {
                                theme::text()
                            } else {
                                theme::muted()
                            })
                            .child(e.name.clone()),
                    )
                    .child(
                        div()
                            .w(px(card_w - 28.))
                            .text_xs()
                            .text_color(theme::muted())
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .overflow_hidden()
                            .child(format!("{car_name}  ·  {track_name}")),
                    )
                    .child(
                        div()
                            .pt_2()
                            .border_t_1()
                            .border_color(theme::border())
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .when(!ev_unlocked, |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::faint())
                                        .child("Finish previous round to unlock"),
                                )
                            })
                            .when(ev_unlocked, |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::muted())
                                                .child(best_label),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(if best_val == "—" {
                                                    theme::muted()
                                                } else if p.medal > 0 {
                                                    medal_color(p.medal)
                                                } else {
                                                    theme::text()
                                                })
                                                .child(best_val),
                                        ),
                                )
                                .when_some(gold_hint, |d, g| {
                                    d.child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::faint())
                                            .child(format!("Gold {g}")),
                                    )
                                })
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_event(i, cx);
                    }));

                let card = if ev_unlocked {
                    focus_ring(base, i == self.selected)
                } else {
                    base
                };
                row = row.child(card);
            }
            grid = grid.child(row);
        }

        content_col = content_col.child(grid);

        if series.championship() {
            content_col = content_col.child(self.standings_panel(&series, avail_w));
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_banners(main_w))
            .child(
                div()
                    .id("career-events-grid")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(content_col),
            )
    }

    fn render_career_detail(&mut self, main_w: f32, cx: &mut Context<Self>) -> Div {
        let Some(series) = self.current_series().cloned() else {
            self.screen = CareerScreen::Series;
            return self.render_career_series(main_w, cx);
        };
        let Some(e) = series.events.get(self.selected).cloned() else {
            self.screen = CareerScreen::Events;
            return self.render_career_events(main_w, cx);
        };

        let p = self.progress.get(&e.id);
        let ev_count = series.events.len();
        let has_prev = self.selected > 0
            && series.event_unlocked(self.selected - 1, &self.catalog, &self.progress);
        let has_next = self.selected + 1 < ev_count
            && series.event_unlocked(self.selected + 1, &self.catalog, &self.progress);

        // Event jumper strip (jump to any event in this series)
        let mut jumper = div()
            .id("career-event-jumper")
            .flex()
            .items_center()
            .gap_2()
            .px_6()
            .py_2()
            .flex_shrink_0()
            .overflow_x_scroll()
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::surface());
        for (i, ev) in series.events.iter().enumerate() {
            let unlocked = series.event_unlocked(i, &self.catalog, &self.progress);
            let active = i == self.selected;
            let ep = self.progress.get(&ev.id);
            let tag = if !unlocked {
                "Locked".to_string()
            } else if series.championship() {
                ep.last_position
                    .map(|pos| format!("P{pos}"))
                    .unwrap_or_else(|| "Open".into())
            } else if ep.medal > 0 {
                medal_short(ep.medal).to_string()
            } else {
                "—".into()
            };
            let label = format!("#{} {} · {}", i + 1, ev.name, tag);
            let item = div()
                .id(("career-jump-event", i))
                .flex()
                .items_center()
                .flex_shrink_0()
                .min_h(px(34.))
                .px_3()
                .py_1()
                .rounded(px(theme::RADIUS_SM))
                .border_1()
                .text_xs()
                .when(active, |d| {
                    d.bg(theme::accent())
                        .border_color(theme::accent())
                        .text_color(theme::on_accent())
                        .font_weight(FontWeight::BOLD)
                        .cursor_pointer()
                })
                .when(!active && unlocked, |d| {
                    d.bg(theme::raised())
                        .border_color(theme::border())
                        .text_color(theme::text())
                        .cursor_pointer()
                        .hover(|st| st.bg(theme::overlay()))
                })
                .when(!unlocked, |d| {
                    d.bg(theme::bg())
                        .border_color(theme::border())
                        .text_color(theme::faint())
                        .opacity(0.45)
                })
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_event(i, cx);
                }));
            jumper = jumper.child(item);
        }

        // Main aligned dashboard layout
        let pad_x = 24.0f32;
        let col_gap = 16.0f32;
        let avail_w = (main_w - 2.0 * pad_x).max(320.0);
        let two_col = avail_w >= 860.0;
        let side_w = if two_col {
            (avail_w * 0.42).floor().clamp(360.0, 520.0)
        } else {
            avail_w
        };
        let left_w = if two_col {
            (avail_w - side_w - col_gap).max(360.0)
        } else {
            avail_w
        };

        let car_name = self.meta.car_name(&e.car).to_string();
        let track_name = self.meta.track_name(&e.track, &e.layout).to_string();
        let preview = e
            .preview
            .clone()
            .or_else(|| self.meta.car_preview(&e.car));
        let mode = e
            .sessions
            .last()
            .map(|s| events::mode_label(s.kind))
            .unwrap_or("Event");
        let counts = series.medal_counts(&self.progress);

        // Row 1: Hero card (event title + description on left, compact preview on right)
        let thumb_w = 250.0f32;
        let hero_text_w = if two_col {
            (avail_w - thumb_w - 48.0).max(240.0)
        } else {
            (avail_w - 32.0).max(200.0)
        };
        let status_pill = if series.championship() {
            p.last_position
                .map(|pos| badge(format!("P{pos}"), theme::on_accent(), theme::ok()))
        } else if p.medal > 0 {
            Some(badge(
                medal(p.medal).to_uppercase(),
                theme::on_accent(),
                medal_color(p.medal),
            ))
        } else {
            None
        };
        let hero_text = div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(badge(
                        format!("{} · #{}", series.code, self.selected + 1),
                        theme::text(),
                        theme::overlay(),
                    ))
                    .child(badge(mode, theme::muted(), theme::raised()))
                    .when_some(status_pill, |d, b| d.child(b)),
            )
            .child(
                line(e.name.clone(), hero_text_w)
                    .text_xl()
                    .font_weight(FontWeight::BOLD),
            )
            .when(!e.description.is_empty(), |d| {
                d.child(line(e.description.clone(), hero_text_w).text_color(theme::muted()))
            });
        let hero_card = panel()
            .p_4()
            .flex()
            .when(two_col, |d| d.items_center().justify_between().gap_4())
            .when(!two_col, |d| d.flex_col().gap_3())
            .child(hero_text)
            .child(picture(preview, 126.).w(px(if two_col { thumb_w } else { avail_w - 32. })));

        // Row 2 Left: Event details card
        let details_card = panel()
            .p_4()
            .w(px(left_w))
            .flex()
            .flex_col()
            .gap_2()
            .child(section_title("Event details"))
            .child(field("Car", car_name))
            .child(field(
                "Track",
                format!(
                    "{}{}",
                    track_name,
                    if e.layout.is_empty() {
                        String::new()
                    } else {
                        format!(" / {}", e.layout)
                    }
                ),
            ))
            .child(field("Weather", e.weather.clone()))
            .child(field(
                "Temperature",
                format!(
                    "Air {}°C · Road {}°C",
                    e.ambient
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "—".into()),
                    e.road.map(|v| v.to_string()).unwrap_or_else(|| "—".into())
                ),
            ))
            .child(field(
                "Rules",
                format!(
                    "Penalties {} · Fixed setup {}",
                    if e.penalties { "on" } else { "off" },
                    if e.fixed_setup { "on" } else { "off" }
                ),
            ))
            .child(field("AI strength", format!("{}%", e.ai_level)));

        // Row 2 Right: Medal targets (or Championship standings)
        let targets_card = if !e.goals.is_empty() {
            let mut goals = panel()
                .p_4()
                .w(px(side_w))
                .flex()
                .flex_col()
                .gap_3()
                .child(section_title("Medal targets"));
            for (i, g) in e.goals.iter().enumerate() {
                let n = (i + 1) as u8;
                goals = goals.child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .gap_2()
                        .py_1()
                        .px_2()
                        .rounded(px(theme::RADIUS_SM))
                        .bg(theme::bg())
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::BOLD)
                                .text_color(medal_color(n))
                                .child(format!("{}  ·  {}", medal(n), events::goal_label(g))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(if p.medal >= n {
                                    theme::ok()
                                } else {
                                    theme::muted()
                                })
                                .child(if p.medal >= n { "Earned" } else { "Not earned" }),
                        ),
                );
            }
            goals
        } else if series.championship() {
            self.standings_panel(&series, side_w).p_4().w(px(side_w))
        } else {
            panel()
                .p_4()
                .w(px(side_w))
                .flex()
                .flex_col()
                .gap_2()
                .child(section_title("Targets"))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::muted())
                        .child("Complete the session to record your result."),
                )
        };

        // Row 3 Left: Session schedule + race config toggle in card header
        let mut sessions_card = panel()
            .p_4()
            .w(px(left_w))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(section_title("Session schedule"))
                    .child(
                        button(
                            "toggle-event-config",
                            if self.show_config {
                                "Hide race config"
                            } else {
                                "Show race config"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_config = !this.show_config;
                            cx.notify();
                        })),
                    ),
            );
        for s in &e.sessions {
            sessions_card = sessions_card.child(line(
                format!(
                    "{} · {}{}{}",
                    s.name,
                    events::mode_label(s.kind),
                    if s.laps > 0 {
                        format!(" · {} laps", s.laps)
                    } else {
                        String::new()
                    },
                    if s.minutes > 0 {
                        format!(" · {} min", s.minutes)
                    } else {
                        String::new()
                    }
                ),
                left_w - 32.,
            ));
        }

        // Row 3 Right: Unified Progress card (Your progress + Series goal)
        let half_w = ((side_w - 48.0) / 2.0).max(140.0);
        let progress_card = panel()
            .p_4()
            .w(px(side_w))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .w(px(half_w))
                            .child(section_title("Your progress"))
                            .when(!series.championship(), |d| {
                                d.child(field("Best medal", medal(p.medal)))
                            })
                            .child(field(
                                score_heading(&e, series.championship()),
                                score_display(&e, &p, series.championship()),
                            ))
                            .child(field("Attempts", p.attempts.to_string()))
                            .when_some(p.last_position, |d, pos| {
                                d.child(field("Last finish", format!("Position {pos}")))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .w(px(half_w))
                            .pl_3()
                            .border_l_1()
                            .border_color(theme::border())
                            .child(section_title("Series progress"))
                            .child(
                                line(format!("Goal: {}", series.goal_summary()), half_w - 16.)
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme::accent()),
                            )
                            .when(!series.championship(), |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .text_xs()
                                        .child(
                                            div()
                                                .text_color(medal_color(3))
                                                .child(format!("Gold {}", counts[2])),
                                        )
                                        .child(
                                            div()
                                                .text_color(medal_color(2))
                                                .child(format!("Silver {}", counts[1])),
                                        )
                                        .child(
                                            div()
                                                .text_color(medal_color(1))
                                                .child(format!("Bronze {}", counts[0])),
                                        ),
                                )
                            })
                            .when(series.completed(&self.progress), |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(theme::ok())
                                        .child("Series complete"),
                                )
                            })
                            .when(series.championship(), |d| {
                                d.child(
                                    div().pt_1().child(
                                        button(
                                            "championship-restart",
                                            if self.restart_confirm {
                                                "Confirm restart"
                                            } else {
                                                "Restart championship"
                                            },
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.restart(cx)),
                                        ),
                                    ),
                                )
                            }),
                    ),
            );

        let row2 = if two_col {
            div()
                .flex()
                .gap(px(col_gap))
                .child(details_card)
                .child(targets_card)
        } else {
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(details_card)
                .child(targets_card)
        };

        let row3 = if two_col {
            div()
                .flex()
                .gap(px(col_gap))
                .child(sessions_card)
                .child(progress_card)
        } else {
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(sessions_card)
                .child(progress_card)
        };

        let mut body_grid = div()
            .flex()
            .flex_col()
            .gap_3()
            .px(px(pad_x))
            .py_4()
            .child(hero_card)
            .child(row2)
            .child(row3);

        if series.championship() && !e.goals.is_empty() {
            body_grid = body_grid.child(self.standings_panel(&series, avail_w).p_4());
        }

        if self.show_config {
            let mut config = panel()
                .p_4()
                .flex()
                .flex_col()
                .gap_1()
                .child(section_title("Event configuration (event.ini)"));
            for sec in e.ini.sections() {
                config = config.child(
                    line(format!("[{}]", sec.name), avail_w - 32.).text_color(theme::accent()),
                );
                for (k, v) in sec.entries() {
                    config = config.child(
                        line(format!("{k} = {v}"), avail_w - 32.)
                            .text_xs()
                            .text_color(theme::muted()),
                    );
                }
            }
            body_grid = body_grid.child(config);
        }

        // Footer launch bar
        let unavailable = events::available(&self.root, &e, Some(&series))
            .err()
            .map(|err| format!("Missing content: {err:#}"));
        let ev_unlocked = series.event_unlocked(self.selected, &self.catalog, &self.progress);
        let already_scored = series.championship() && p.last_position.is_some();
        let blocked = self.running || self.loading || self.error.is_some() || !ev_unlocked;
        let disabled = blocked || unavailable.is_some() || already_scored;

        let footer = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_6()
            .py_3()
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(
                line(
                    if already_scored {
                        "Round scored. Restart championship to replay.".into()
                    } else if let Some(err) = unavailable {
                        err
                    } else if !ev_unlocked {
                        "Complete the previous round to unlock.".into()
                    } else {
                        "D-pad Left/Right: prev/next · A / Start: launch · B: events".into()
                    },
                    (main_w - 260.).max(180.),
                )
                .text_color(theme::muted()),
            )
            .child(
                primary_button(
                    "event-start",
                    if self.running {
                        "Game running…"
                    } else {
                        "Start event"
                    },
                )
                .when(disabled, |d| d.opacity(0.4))
                .when(!disabled, |d| {
                    d.on_click(cx.listener(|this, _, _, cx| this.start(cx)))
                }),
            );

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_6()
                    .py_3()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                button(
                                    "career-back-events",
                                    format!("← {}", series.name),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.screen = CareerScreen::Events;
                                    this.message = None;
                                    cx.notify();
                                })),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme::muted())
                                    .child(format!("Event {} / {}", self.selected + 1, ev_count)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                button("career-prev-event", "← Prev")
                                    .when(!has_prev, |d| d.opacity(0.4))
                                    .when(has_prev, |d| {
                                        d.on_click(cx.listener(|this, _, _, cx| {
                                            this.step_event(-1, cx);
                                        }))
                                    }),
                            )
                            .child(
                                button("career-next-event", "Next →")
                                    .when(!has_next, |d| d.opacity(0.4))
                                    .when(has_next, |d| {
                                        d.on_click(cx.listener(|this, _, _, cx| {
                                            this.step_event(1, cx);
                                        }))
                                    }),
                            ),
                    ),
            )
            .child(jumper)
            .child(self.render_banners(main_w))
            .child(
                div()
                    .id("career-event-detail-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body_grid),
            )
            .child(footer)
    }

    fn special_details(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(e) = self.current_event() else {
            return div()
                .p_4()
                .text_color(theme::muted())
                .child("No events match your filter.")
                .into_any_element();
        };
        let p = self.progress.get(&e.id);
        let mut body = div().flex().flex_col().gap_3().p_4().w(px(width));
        if !e.goals.is_empty() {
            let mut goals = panel()
                .flex()
                .flex_col()
                .gap_2()
                .child(section_title("Medal targets"));
            for (i, g) in e.goals.iter().enumerate() {
                let n = (i + 1) as u8;
                goals = goals.child(
                    div()
                        .flex()
                        .justify_between()
                        .gap_2()
                        .child(
                            line(
                                format!("{}  ·  {}", medal(n), events::goal_label(g)),
                                width - 155.,
                            )
                            .text_color(medal_color(n)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(if p.medal >= n {
                                    theme::ok()
                                } else {
                                    theme::muted()
                                })
                                .child(if p.medal >= n { "Earned" } else { "Not earned" }),
                        ),
                );
            }
            if e.goals.first().is_some_and(|g| g.kind == "AI") {
                goals = goals.child(
                    div().flex().gap_1().children(
                        (0..e.goals.len().min(3))
                            .map(|i| {
                                chip(
                                    ("event-difficulty", i),
                                    format!("{} · {}%", medal(i as u8 + 1), e.goals[i].objective),
                                    self.difficulty == i,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.difficulty = i;
                                    cx.notify();
                                }))
                                .into_any_element()
                            })
                            .collect::<Vec<_>>(),
                    ),
                );
            }
            body = body.child(goals);
        }
        body = body.child(
            panel()
                .flex()
                .flex_col()
                .gap_1()
                .child(section_title("Your progress"))
                .child(field("Best medal", medal(p.medal)))
                .child(field("Attempts", p.attempts.to_string()))
                .child(field("Best score", score_display(e, &p, false)))
                .when_some(p.last_position, |d, pos| {
                    d.child(field("Last finish", format!("Position {pos}")))
                }),
        );
        let car_name = self.meta.car_name(&e.car).to_string();
        let track_name = self.meta.track_name(&e.track, &e.layout).to_string();
        let preview = e
            .preview
            .clone()
            .or_else(|| self.meta.car_preview(&e.car));
        body = body
            .child(picture(preview, 180.))
            .child(
                line(e.name.clone(), width - 32.)
                    .text_xl()
                    .font_weight(FontWeight::BOLD),
            )
            .child(line(e.description.clone(), width - 32.).text_color(theme::muted()))
            .child(
                panel()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field("Car", car_name))
                    .child(field(
                        "Track",
                        format!(
                            "{}{}",
                            track_name,
                            if e.layout.is_empty() {
                                String::new()
                            } else {
                                format!(" / {}", e.layout)
                            }
                        ),
                    ))
                    .child(field("Weather", e.weather.clone()))
                    .child(field(
                        "Temperature",
                        format!(
                            "Air {}°C · Road {}°C",
                            e.ambient
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "—".into()),
                            e.road.map(|v| v.to_string()).unwrap_or_else(|| "—".into())
                        ),
                    ))
                    .child(field(
                        "Rules",
                        format!(
                            "Penalties {} · Fixed setup {}",
                            if e.penalties { "on" } else { "off" },
                            if e.fixed_setup { "on" } else { "off" }
                        ),
                    ))
                    .child(field("AI strength", format!("{}%", e.ai_level))),
            );
        let mut sessions = panel()
            .flex()
            .flex_col()
            .gap_1()
            .child(section_title("Session schedule"));
        for s in &e.sessions {
            sessions = sessions.child(line(
                format!(
                    "{} · {}{}{}",
                    s.name,
                    events::mode_label(s.kind),
                    if s.laps > 0 {
                        format!(" · {} laps", s.laps)
                    } else {
                        String::new()
                    },
                    if s.minutes > 0 {
                        format!(" · {} min", s.minutes)
                    } else {
                        String::new()
                    }
                ),
                width - 58.,
            ));
        }
        body = body.child(sessions).child(
            div().child(
                button(
                    "toggle-event-config",
                    if self.show_config {
                        "Hide race config"
                    } else {
                        "Show race config"
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_config = !this.show_config;
                    cx.notify();
                })),
            ),
        );
        if self.show_config {
            let mut config = panel()
                .flex()
                .flex_col()
                .gap_1()
                .child(section_title("Event configuration (event.ini)"));
            for sec in e.ini.sections() {
                config = config.child(
                    line(format!("[{}]", sec.name), width - 58.).text_color(theme::accent()),
                );
                for (k, v) in sec.entries() {
                    config = config.child(
                        line(format!("{k} = {v}"), width - 58.)
                            .text_xs()
                            .text_color(theme::muted()),
                    );
                }
            }
            body = body.child(config);
        }
        body.into_any_element()
    }

    fn render_special_events(&mut self, main_w: f32, cx: &mut Context<Self>) -> Div {
        let list_w = 310.;
        let detail_w = (main_w - list_w).max(300.);
        let selected = self.selected;
        let list = uniform_list(
            "events-list",
            self.visible.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                range
                    .filter_map(|row| {
                        let i = *this.visible.get(row)?;
                        let e = this.list().get(i)?;
                        let p = this.progress.get(&e.id);
                        Some(
                            div()
                                .id(("event-row", i))
                                .h(px(70.))
                                .px_3()
                                .py_2()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .cursor_pointer()
                                .border_b_1()
                                .border_color(theme::border())
                                .when(i == selected, |d| d.bg(theme::selected()))
                                .hover(|s| s.bg(theme::panel_hover()))
                                .child(
                                    div()
                                        .w(px(280.))
                                        .text_sm()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .overflow_hidden()
                                        .child(e.name.clone()),
                                )
                                .child(
                                    div()
                                        .w(px(280.))
                                        .text_xs()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .overflow_hidden()
                                        .text_color(medal_color(p.medal))
                                        .child(format!(
                                            "{} · {}",
                                            e.sessions
                                                .last()
                                                .map(|s| events::mode_label(s.kind))
                                                .unwrap_or("Event"),
                                            medal(p.medal)
                                        )),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.selected = i;
                                    cx.notify();
                                })),
                        )
                    })
                    .collect()
            }),
        )
        .track_scroll(self.scroll.clone())
        .flex_1()
        .min_h_0();

        let left = div()
            .flex()
            .flex_col()
            .w(px(list_w))
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme::border())
            .child(div().p_2().child(self.search.clone()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .children(
                        [(0, "All"), (1, "Open"), (2, "Earned"), (3, "Gold")]
                            .into_iter()
                            .map(|(code, label)| {
                                chip(("event-filter", code as usize), label, self.filter == code)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.filter = code;
                                        this.refilter(cx);
                                        cx.notify();
                                    }))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>(),
                    ),
            )
            .child(list);

        let unavailable = self
            .current_event()
            .and_then(|e| events::available(&self.root, e, None).err())
            .map(|e| format!("Missing content: {e:#}"));
        let blocked = self.running
            || self.loading
            || self.error.is_some()
            || self.current_event().is_none();
        let disabled = blocked || unavailable.is_some();

        let footer = div()
            .flex()
            .items_center()
            .gap_3()
            .p_3()
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme::border())
            .child(
                primary_button(
                    "event-start",
                    if self.running {
                        "Game running…"
                    } else {
                        "Start event"
                    },
                )
                .when(disabled, |d| d.opacity(0.4))
                .when(!disabled, |d| {
                    d.on_click(cx.listener(|this, _, _, cx| this.start(cx)))
                }),
            )
            .child(line(
                unavailable.unwrap_or_else(|| {
                    "D-pad: select · A / Start: launch · X: difficulty".into()
                }),
                (detail_w - 230.).max(160.),
            ));

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .p_4()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("Special Events"),
                            )
                            .child(
                                line(self.overview(), main_w - 230.).text_color(theme::muted()),
                            ),
                    )
                    .child(
                        button("events-refresh", "Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .child(
                line(events::import_notice(), main_w - 32.)
                    .px_4()
                    .py_1()
                    .text_color(theme::muted())
                    .text_xs(),
            )
            .child(self.render_banners(main_w))
            .child(
                div().flex().flex_1().min_h_0().child(left).child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .id("event-details")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .child(self.special_details(detail_w, cx)),
                        )
                        .child(footer),
                ),
            )
    }
}

impl Render for EventsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main_w = f32::from(window.viewport_size().width) - crate::app::SIDEBAR_W;
        if !self.career {
            return self.render_special_events(main_w, cx);
        }
        match self.screen {
            CareerScreen::Series => self.render_career_series(main_w, cx),
            CareerScreen::Events => self.render_career_events(main_w, cx),
            CareerScreen::Detail => self.render_career_detail(main_w, cx),
        }
    }
}
