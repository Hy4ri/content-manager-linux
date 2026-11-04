//! The main window: Quick Drive, Cars, Tracks, Install and Settings pages.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use ac_core::content::{self, Car, TrackEntry, Weather};
use ac_core::game_cfg::{self, GameConfig, Kind, Setting, Tab};
use ac_core::race::{self, Conditions, Mode, QuickDrive};
use ac_core::settings::Settings;
use ac_core::{acd, install, launch, paths};
use gpui::{
    ClickEvent, Context, Div, Entity, FocusHandle, FontWeight, KeyDownEvent, ModifiersChangedEvent,
    MouseMoveEvent,
    ObjectFit, PathPromptOptions, ScrollStrategy, SharedString, UniformListScrollHandle, Window,
    div, img, prelude::*, px, uniform_list,
};

use crate::browser_downloads::{download_base_dir, manage_archives};
use crate::browser_tabs::BrowserTabs as BrowserView;
use crate::browser_view::PreviewDownload;
use crate::content_sort::{CarSort, TrackSort};
use crate::events_view::{EventsView, Running};
use crate::nav::NavAction;
use crate::text_input::TextInput;
use crate::theme;
use crate::widgets::{
    button, chip, field, focus_ring, hint, hint_bar, panel, picture, primary_button, section_title, segmented,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    QuickDrive,
    Cars,
    Tracks,
    Career,
    SpecialEvents,
    Install,
    Browser,
    Game,
    Settings,
}

/// Top-level tabs in bar order; L1/R1 walk these. Each tab owns one or more pages, and
/// L2/R2 walk the pages inside the active tab (shown as a sub-tab strip when there are several).
const TABS: [(&str, &[Page]); 7] = [
    ("Drive", &[Page::QuickDrive]),
    ("Garage", &[Page::Cars]),
    ("Tracks", &[Page::Tracks]),
    ("Career", &[Page::Career]),
    ("Events", &[Page::SpecialEvents]),
    ("Mods", &[Page::Browser, Page::Install]),
    ("Settings", &[Page::Game, Page::Settings]),
];

fn page_label(page: Page) -> &'static str {
    match page {
        Page::Browser => "Get mods",
        Page::Install => "Install",
        Page::Game => "Game",
        Page::Settings => "App",
        Page::QuickDrive => "Drive",
        Page::Cars => "Garage",
        Page::Tracks => "Tracks",
        Page::Career => "Career",
        Page::SpecialEvents => "Events",
    }
}

fn tab_of(page: Page) -> usize {
    TABS.iter().position(|(_, pages)| pages.contains(&page)).unwrap_or(0)
}

/// One focusable row on the Drive page, in top-to-bottom order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DriveRow {
    Car,
    Track,
    Mode,
    Minutes,
    Laps,
    Opponents,
    Ai,
    Grid,
    Penalties,
    Time,
    Temp,
    Weather,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerKind {
    Car,
    Track,
}

const PICK_CARD_MIN_W: f32 = 200.;
const PICK_CARD_H: f32 = 176.;
const PICK_ROW_H: f32 = PICK_CARD_H + 12.;

/// Columns and card width that fill a picker grid of `viewport_w` (24 px side padding, 12 px gaps).
fn picker_geometry(viewport_w: f32) -> (usize, f32) {
    let avail = (viewport_w - 48.).max(PICK_CARD_MIN_W);
    let cols = (((avail + 12.) / (PICK_CARD_MIN_W + 12.)).floor() as usize).max(1);
    (cols, (avail - 12. * (cols as f32 - 1.)) / cols as f32)
}

/// Next list index after moving `delta` rows, clamped; starts at the top with no selection.
fn step_index(pos: Option<usize>, delta: isize, len: usize) -> usize {
    match pos {
        None => 0,
        Some(p) => (p as isize + delta).clamp(0, len as isize - 1) as usize,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModeKind {
    Practice,
    Hotlap,
    Race,
}

#[derive(Default)]
struct Content {
    cars: Vec<Car>,
    tracks: Vec<TrackEntry>,
    weather: Vec<Weather>,
    car_search: Vec<String>,
    car_brands: Vec<(String, usize)>,
    track_search: Vec<String>,
}

impl Content {
    fn scan(root: &std::path::Path) -> Self {
        let cars = content::scan_cars(root);
        let mut car_brands: Vec<_> = content::brands(&cars).into_iter().collect();
        car_brands.sort_by(|(a, _), (b, _)| content::natural_cmp(a, b));
        let tracks = content::track_entries(&content::scan_tracks(root));
        Content {
            car_search: cars.iter().map(|c| c.search_text()).collect(),
            car_brands,
            track_search: tracks.iter().map(|t| t.search_text()).collect(),
            weather: content::scan_weather(root),
            cars,
            tracks,
        }
    }
}

struct InstallState {
    root: PathBuf,
    archive: PathBuf,
    items: Vec<install::Item>,
}

pub struct CmApp {
    settings: Settings,
    ac_root: Option<PathBuf>,
    content: Arc<Content>,
    loading: bool,
    page: Page,
    browser: Option<Entity<BrowserView>>,
    events: Option<Entity<EventsView>>,

    car: Option<usize>,
    skin: usize,
    track: Option<usize>,
    car_filter: Entity<TextInput>,
    track_filter: Entity<TextInput>,
    cars_view: Vec<usize>,
    car_scroll: UniformListScrollHandle,
    track_scroll: UniformListScrollHandle,
    /// Controller navigation task and which Quick Drive list the D-pad drives.
    pad_nav: Option<gpui::Task<()>>,
    /// The press that finishes a bind capture is also seen by the nav reader; ignore it until this time.
    pad_mute_until: Option<std::time::Instant>,
    root_focus: FocusHandle,
    drive_focus: usize,
    picker: Option<PickerKind>,
    picker_focus: usize,
    picker_scroll: UniformListScrollHandle,
    picker_cols: usize,
    picker_card_w: f32,
    /// Last device that produced input; decides hint glyphs and focus-ring visibility.
    input_device: crate::nav::InputDevice,
    car_brand: Option<String>,
    tracks_view: Vec<usize>,
    car_sort: CarSort,
    track_sort: TrackSort,
    car_sort_desc: bool,
    track_sort_desc: bool,

    mode: ModeKind,
    practice_minutes: u32,
    laps: u32,
    opponents: u32,
    ai_level: u32,
    start_position: u32,
    time_seconds: u32,
    ambient: i32,
    weather_id: String,
    penalties: bool,

    install: Option<InstallState>,
    install_generation: u64,
    busy: bool,
    status: Option<(String, bool)>,
    game_running: bool,

    in_root: Entity<TextInput>,
    in_driver: Entity<TextInput>,
    in_nation: Entity<TextInput>,
    in_proton: Entity<TextInput>,
    in_download_dir: Entity<TextInput>,
    download_usage: String,
    download_working: bool,
    clear_download_confirm: Option<PathBuf>,

    game_cfg: Option<GameConfig>,
    game_error: Option<String>,
    game_tab: Tab,
    game_settings: Vec<Setting>,
    /// Index into [`game_cfg::BINDS`] waiting for a key press.
    capturing: Option<usize>,
    capture_focus: FocusHandle,
    /// Index into [`game_cfg::BINDS`] waiting for a controller button, and the task reading it.
    pad_capturing: Option<usize>,
    pad_task: Option<gpui::Task<()>>,
}

/// Left chrome width; the sidebar is gone (top tab bar), kept so page-width maths stays explicit.
pub(crate) const SIDEBAR_W: f32 = 0.;
fn count_label(shown: usize, total: usize, noun: &str) -> String {
    if shown == total {
        format!("{total} {noun}")
    } else {
        format!("{shown} of {total} {noun}")
    }
}

/// Title + muted subtitle, both truncated with an ellipsis.
///
/// GPUI caches the first measurement of nowrap text, and inside flex layouts
/// that first pass has no width limit, so `truncate` only works when the
/// text's own box has a fixed pixel width. Pass it when known (list rows);
/// `None` fills the remaining space and simply clips.
fn two_lines(
    title: impl Into<SharedString>,
    sub: impl Into<SharedString>,
    width: Option<f32>,
) -> gpui::Div {
    let line = |d: gpui::Div| match width {
        Some(w) => d.w(px(w)),
        None => d.w_full(),
    };
    div()
        .flex()
        .flex_col()
        .map(|d| match width {
            Some(w) => d.w(px(w)).flex_shrink_0(),
            None => d.flex_1().min_w_0(),
        })
        .child(line(div()).text_sm().truncate().child(title.into()))
        .child(
            line(div())
                .text_xs()
                .text_color(theme::muted())
                .truncate()
                .child(sub.into()),
        )
}

const INSTALL_MAX_W: f32 = 760.;
const SETTINGS_MAX_W: f32 = 860.;
/// Horizontal space a `panel()` takes from its content: `p_3` on both sides plus the border.
const PANEL_INSET: f32 = 2. * 12. + 2.;

/// Width for wrapping text inside a page column that is `p_4` padded and
/// capped at `max_w`.
///
/// GPUI measures text once and caches the result, so text that wraps in a
/// flex layout keeps its unwrapped (one line) height and spills over what
/// follows. A definite pixel width makes the first measurement the right one.
fn text_w(main_w: f32, max_w: f32) -> f32 {
    (main_w.min(max_w) - 2. * 16.).max(120.)
}

/// A text input that takes the remaining width of a row.
///
/// The wrapper must be a flex container: under a plain block wrapper the
/// input's `w_full` resolves before the flex item has grown and collapses to
/// a few pixels.
fn grow_input(input: Entity<TextInput>) -> gpui::Div {
    div().flex().flex_1().min_w_0().child(input)
}

fn fmt_time(seconds: u32) -> String {
    format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

impl CmApp {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (settings, settings_error) = match Settings::load() {
            Ok(settings) => (settings, None),
            Err(e) => (
                Settings::default(),
                Some(format!(
                    "Settings could not be loaded: {e:#}. Fix the file and restart."
                )),
            ),
        };
        let ac_root = settings
            .ac_root
            .clone()
            .filter(|p| paths::is_ac_root(p))
            .or_else(paths::find_ac_root);

        let car_filter = cx.new(|cx| TextInput::new(cx, "Search cars…", ""));
        let track_filter = cx.new(|cx| TextInput::new(cx, "Search tracks…", ""));
        cx.observe(&car_filter, |this, _, cx| this.refilter(cx))
            .detach();
        cx.observe(&track_filter, |this, _, cx| this.refilter(cx))
            .detach();

        let root_text = ac_root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let in_root = cx.new(|cx| TextInput::new(cx, "Assetto Corsa folder", root_text));
        let in_driver =
            cx.new(|cx| TextInput::new(cx, "Driver name", settings.driver_name.clone()));
        let in_nation = cx.new(|cx| TextInput::new(cx, "ITA", settings.nationality.clone()));
        let proton_text = settings
            .proton
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let in_proton = cx.new(|cx| TextInput::new(cx, "Auto-detect", proton_text));
        let download_text = settings.download_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let in_download_dir = cx.new(|cx| TextInput::new(cx, "Default cache directory", download_text));

        let last = settings.last_quick_drive.clone().unwrap_or_default();
        let (mode, practice_minutes, laps, opponents, ai_level, start_position) = match last.mode {
            Mode::Practice { minutes } => (ModeKind::Practice, minutes, 5, 7, 95, 8),
            Mode::Hotlap => (ModeKind::Hotlap, 0, 5, 7, 95, 8),
            Mode::Race {
                laps,
                opponents,
                ai_level,
                starting_position,
            } => (
                ModeKind::Race,
                0,
                laps,
                opponents,
                ai_level,
                starting_position,
            ),
        };
        let c = if last.conditions.weather_id.is_empty() {
            Conditions::default()
        } else {
            last.conditions.clone()
        };

        let mut this = Self {
            page: if ac_root.is_some() {
                Page::QuickDrive
            } else {
                Page::Settings
            },
            browser: None,
            events: None,
            settings,
            ac_root,
            content: Arc::default(),
            loading: false,
            car: None,
            skin: 0,
            track: None,
            car_filter,
            track_filter,
            cars_view: Vec::new(),
            car_scroll: UniformListScrollHandle::new(),
            track_scroll: UniformListScrollHandle::new(),
            pad_nav: None,
            pad_mute_until: None,
            root_focus: cx.focus_handle(),
            drive_focus: 0,
            picker: None,
            picker_focus: 0,
            picker_scroll: UniformListScrollHandle::new(),
            picker_cols: 1,
            picker_card_w: PICK_CARD_MIN_W,
            input_device: crate::nav::InputDevice::default(),
            car_brand: None,
            tracks_view: Vec::new(),
            car_sort: CarSort::default(),
            track_sort: TrackSort::default(),
            car_sort_desc: false,
            track_sort_desc: false,
            mode,
            practice_minutes,
            laps,
            opponents,
            ai_level,
            start_position,
            time_seconds: c.time_seconds,
            ambient: c.ambient_c as i32,
            weather_id: c.weather_id,
            penalties: last.penalties || last.car_id.is_empty(),
            install: None,
            install_generation: 0,
            busy: false,
            status: None,
            game_running: false,
            in_root,
            in_driver,
            in_nation,
            in_proton,
            in_download_dir,
            download_usage: "Not measured yet".into(),
            download_working: false,
            clear_download_confirm: None,
            game_cfg: None,
            game_error: None,
            game_tab: Tab::Video,
            game_settings: game_cfg::settings(),
            capturing: None,
            capture_focus: cx.focus_handle(),
            pad_capturing: None,
            pad_task: None,
        };
        if this.ac_root.is_none() {
            this.status = Some((
                "Assetto Corsa wasn't found. Set its folder in Settings.".into(),
                true,
            ));
        }
        if let Some(error) = settings_error {
            this.page = Page::Settings;
            this.status = Some((error, true));
        } else {
            this.reload(cx);
        }
        this.refresh_download_usage(cx);
        this
    }

    // ---------- state ----------

    fn set_status(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.status = Some((text.into(), error));
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.ac_root.clone() else {
            return;
        };
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let content = cx
                .background_executor()
                .spawn(async move { Content::scan(&root) })
                .await;
            this.update(cx, |this, cx| {
                this.content = Arc::new(content);
                this.loading = false;
                this.restore_selection();
                this.refilter(cx);
                let n = (this.content.cars.len(), this.content.tracks.len());
                this.set_status(
                    format!("Loaded {} cars and {} track layouts", n.0, n.1),
                    false,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    fn restore_selection(&mut self) {
        let last = self.settings.last_quick_drive.clone().unwrap_or_default();
        let c = &self.content;
        self.car = c
            .cars
            .iter()
            .position(|x| x.id == last.car_id)
            .or((!c.cars.is_empty()).then_some(0));
        self.skin = self
            .car
            .and_then(|i| c.cars[i].skins.iter().position(|s| s.id == last.skin_id))
            .unwrap_or(0);
        self.track = c
            .tracks
            .iter()
            .position(|t| t.track_id == last.track_id && t.layout.layout_id == last.layout_id)
            .or((!c.tracks.is_empty()).then_some(0));
        if !c.weather.iter().any(|w| w.id == self.weather_id) {
            if let Some(w) = c.weather.first() {
                self.weather_id = w.id.clone();
            }
        }
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let car_q = self.car_filter.read(cx).text().to_lowercase();
        let track_q = self.track_filter.read(cx).text().to_lowercase();
        let matches = |hay: &str, q: &str| q.split_whitespace().all(|w| hay.contains(w));
        // A brand that vanished after a rescan would hide every car.
        if let Some(b) = &self.car_brand
            && !self
                .content
                .car_brands
                .iter()
                .any(|(name, _)| name.to_lowercase() == b.to_lowercase())
        {
            self.car_brand = None;
        }
        let brand = self.car_brand.as_ref().map(|b| b.to_lowercase());
        let cars_view: Vec<_> = (0..self.content.cars.len())
            .filter(|&i| {
                brand
                    .as_deref()
                    .is_none_or(|b| self.content.cars[i].brand_name().to_lowercase() == b)
                    && matches(&self.content.car_search[i], &car_q)
            })
            .collect();
        if cars_view != self.cars_view && !cars_view.is_empty() {
            self.car_scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        self.cars_view = cars_view;
        self.tracks_view = (0..self.content.tracks.len())
            .filter(|&i| matches(&self.content.track_search[i], &track_q))
            .collect();
        self.cars_view.sort_by(|&a, &b| {
            self.car_sort.compare(
                &self.content.cars[a],
                &self.content.cars[b],
                self.car_sort_desc,
            )
        });
        self.tracks_view.sort_by(|&a, &b| {
            self.track_sort.compare(
                &self.content.tracks[a],
                &self.content.tracks[b],
                self.track_sort_desc,
            )
        });
        cx.notify();
    }

    fn selected_car(&self) -> Option<&Car> {
        self.car.and_then(|i| self.content.cars.get(i))
    }

    fn selected_track(&self) -> Option<&TrackEntry> {
        self.track.and_then(|i| self.content.tracks.get(i))
    }

    fn quick_drive(&self, cx: &Context<Self>) -> Option<QuickDrive> {
        let car = self.selected_car()?;
        let track = self.selected_track()?;
        let skin_id = car
            .skins
            .get(self.skin)
            .map(|s| s.id.clone())
            .unwrap_or_default();
        let mode = match self.mode {
            ModeKind::Practice => Mode::Practice {
                minutes: self.practice_minutes,
            },
            ModeKind::Hotlap => Mode::Hotlap,
            ModeKind::Race => Mode::Race {
                laps: self.laps,
                opponents: self.opponents,
                ai_level: self.ai_level,
                starting_position: self.start_position,
            },
        };
        let mut q = QuickDrive {
            car_id: car.id.clone(),
            skin_id: skin_id.clone(),
            track_id: track.track_id.clone(),
            layout_id: track.layout.layout_id.clone(),
            driver_name: self.in_driver.read(cx).text(),
            nationality: self.in_nation.read(cx).text().to_uppercase(),
            mode,
            conditions: Conditions {
                time_seconds: self.time_seconds,
                ambient_c: self.ambient as f64,
                weather_id: self.weather_id.clone(),
                time_multiplier: 1.0,
            },
            penalties: self.penalties,
            fixed_setup: false,
            use_mph: false,
            opponents: Vec::new(),
        };
        if self.mode == ModeKind::Race {
            let skins: Vec<String> = car.skins.iter().map(|s| s.id.clone()).collect();
            q.opponents = race::pick_opponents(&car.id, &skin_id, &skins, self.opponents);
        }
        Some(q)
    }

    // ---------- actions ----------

    fn go(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.start_game(cx);
    }

    fn start_game(&mut self, cx: &mut Context<Self>) {
        if self.game_running {
            return;
        }
        let (Some(root), Some(q)) = (self.ac_root.clone(), self.quick_drive(cx)) else {
            return self.set_status("Pick a car and a track first.", true, cx);
        };
        if self.mode == ModeKind::Race {
            if let Some(t) = self.selected_track().and_then(|t| t.layout.pitboxes) {
                if self.opponents + 1 > t {
                    return self.set_status(
                        format!("This track has only {t} pit boxes; lower the opponent count."),
                        true,
                        cx,
                    );
                }
            }
        }
        self.settings.last_quick_drive = Some(q.clone());
        if let Err(e) = self.settings.save() {
            return self.set_status(format!("Couldn't save settings: {e:#}"), true, cx);
        }

        if let Err(e) = launch::write_race_ini(&root, &q) {
            return self.set_status(format!("Couldn't write race.ini: {e:#}"), true, cx);
        }
        let opts = launch::LaunchOptions {
            proton: self.settings.proton.clone(),
        };
        match launch::start(&root, &opts) {
            Ok(mut child) => {
                self.game_running = true;
                if let Some(events) = &self.events { events.update(cx, |view, cx| view.set_running(true, cx)); }
                self.set_status("Assetto Corsa is starting…", false, cx);
                cx.spawn(async move |this, cx| {
                    let code = cx
                        .background_executor()
                        .spawn(async move { child.wait() })
                        .await;
                    this.update(cx, |this, cx| {
                        this.game_running = false;
                        if let Some(events) = &this.events { events.update(cx, |view, cx| view.set_running(false, cx)); }
                        match code {
                            Ok(c) if c.success() => this.set_status("Game closed.", false, cx),
                            Ok(c) => this.set_status(
                                format!("Game exited with {c}. Is Steam running?"),
                                true,
                                cx,
                            ),
                            Err(e) => {
                                this.set_status(format!("Lost track of the game: {e}"), true, cx)
                            }
                        }
                    })
                    .ok();
                })
                .detach();
            }
            Err(e) => self.set_status(format!("{e:#}"), true, cx),
        }
    }

    fn save_settings(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let root_text = self.in_root.read(cx).text();
        let root = PathBuf::from(root_text.trim());
        if !paths::is_ac_root(&root) {
            return self.set_status(
                "That folder doesn't contain acs.exe and content/cars.",
                true,
                cx,
            );
        }
        let proton = self.in_proton.read(cx).text();
        self.settings.ac_root = Some(root.clone());
        self.settings.driver_name = self.in_driver.read(cx).text();
        self.settings.nationality = self.in_nation.read(cx).text().to_uppercase();
        self.settings.proton = (!proton.trim().is_empty()).then(|| PathBuf::from(proton.trim()));
        if let Err(e) = self.settings.save() {
            return self.set_status(format!("Couldn't save settings: {e:#}"), true, cx);
        }
        let changed = self.ac_root.as_ref() != Some(&root);
        self.ac_root = Some(root);
        if changed {
            self.game_cfg = None;
        }
        self.set_status("Settings saved.", false, cx);
        if changed {
            self.reload(cx);
        }
    }

    fn refresh_download_usage(&mut self, cx: &mut Context<Self>) {
        self.run_download_maintenance(false, cx);
    }

    fn run_download_maintenance(&mut self, clear: bool, cx: &mut Context<Self>) {
        if self.download_working || (clear && self.busy) {
            return;
        }
        let path = download_base_dir(self.settings.download_dir.as_deref());
        if clear && self.clear_download_confirm.as_ref() != Some(&path) {
            return;
        }
        self.clear_download_confirm = None;
        if clear {
            self.install_generation = self.install_generation.wrapping_add(1);
            self.install = None;
        }
        self.download_working = true;
        self.download_usage = if clear {
            "Clearing ZIPs…"
        } else {
            "Measuring ZIPs…"
        }
        .into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                let removed = if clear { Some(manage_archives(&path, true)?) } else { None };
                let remaining = manage_archives(&path, false)?;
                Ok::<_, std::io::Error>((removed, remaining))
            }).await;
            this.update(cx, |this, cx| {
                this.download_working = false;
                match result {
                    Ok((removed, (count, bytes))) => {
                        this.download_usage = format!("{count} ZIP files · {}", crate::browser_view::format_bytes(bytes as i64));
                        if let Some((count, bytes)) = removed {
                            // A preview of a removed archive is no longer installable.
                            this.install = None;
                            this.set_status(format!("Cleared {count} ZIP files ({}). Installed mods were not touched.", crate::browser_view::format_bytes(bytes as i64)), false, cx);
                        }
                    }
                    Err(error) => {
                        this.download_usage = format!("Could not read download storage: {error}");
                        if clear {
                            this.set_status(format!("Could not finish clearing ZIPs: {error}. Some files may already have been removed."), true, cx);
                        }
                    }
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    fn save_download_directory(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.download_working {
            return;
        }
        let text = self.in_download_dir.read(cx).text();
        let path = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
        if let Some(path) = &path {
            if !path.is_absolute() {
                return self.set_status(
                    "Choose an absolute download directory, or leave it empty for the default.",
                    true,
                    cx,
                );
            }
            if let Err(error) =
                std::fs::create_dir_all(path).and_then(|_| std::fs::read_dir(path).map(|_| ()))
            {
                return self.set_status(
                    format!("Cannot use download directory: {error}"),
                    true,
                    cx,
                );
            }
        }
        let mut settings = self.settings.clone();
        settings.download_dir = path;
        if let Err(error) = settings.save() {
            return self.set_status(
                format!("Couldn't save download directory: {error:#}"),
                true,
                cx,
            );
        }
        self.settings = settings;
        self.clear_download_confirm = None;
        self.set_status("Download directory saved. New downloads use it immediately; existing files have not been moved.", false, cx);
        self.refresh_download_usage(cx);
    }

    fn browse_download_directory(
        &mut self,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select browser download directory".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    this.in_download_dir.update(cx, |input, cx| {
                        input.set_text(path.display().to_string(), cx)
                    });
                    this.clear_download_confirm = None;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn browse_root(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select Assetto Corsa folder".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    this.update(cx, |this, cx| {
                        this.in_root
                            .update(cx, |i, cx| i.set_text(p.display().to_string(), cx));
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn detect_root(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        match paths::find_ac_root() {
            Some(p) => {
                self.in_root
                    .update(cx, |i, cx| i.set_text(p.display().to_string(), cx));
                self.set_status(format!("Found {}", p.display()), false, cx);
            }
            None => self.set_status(
                "Couldn't find Assetto Corsa in any Steam library.",
                true,
                cx,
            ),
        }
    }

    fn ensure_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.is_some() {
            return;
        }
        let browser = cx.new(|cx| BrowserView::new(window, cx));
        cx.subscribe(&browser, |this, _, event: &PreviewDownload, cx| {
            if this.busy {
                this.set_status("Wait for the current installation to finish.", true, cx);
                return;
            }
            this.preview_archive(event.0.clone(), cx);
        })
        .detach();
        self.browser = Some(browser);
    }

    fn preview_archive(&mut self, archive: PathBuf, cx: &mut Context<Self>) {
        if self.download_working {
            return self.set_status("Wait for download storage maintenance to finish.", true, cx);
        }
        let Some(root) = self.ac_root.clone() else {
            self.set_status(
                "Set your Assetto Corsa folder in Settings before installing mods.",
                true,
                cx,
            );
            return;
        };
        if self.busy {
            return;
        }
        if let Some(browser) = &self.browser {
            browser.update(cx, |browser, cx| browser.set_active(false, cx));
        }
        self.install_generation = self.install_generation.wrapping_add(1);
        let generation = self.install_generation;
        self.install = None;
        self.page = Page::Install;
        self.set_status("Inspecting archive…", false, cx);
        cx.spawn(async move |this, cx| {
            let a = archive.clone();
            let planned_root = root.clone();
            let plan = cx
                .background_executor()
                .spawn(async move { install::plan_zip(&a, &planned_root) })
                .await;
            this.update(cx, |this, cx| {
                if this.install_generation != generation
                    || this.ac_root.as_ref() != Some(&root)
                    || this.busy
                {
                    return;
                }
                match plan {
                    Ok(items) => {
                        this.install = Some(InstallState {
                            root: root.clone(),
                            archive,
                            items,
                        });
                        this.status = None;
                        cx.notify();
                    }
                    Err(e) => this.set_status(format!("{e:#}"), true, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn choose_archive(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.ac_root.is_none() {
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select a mod archive (.zip)".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(archive) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| this.preview_archive(archive, cx))
                .ok();
        })
        .detach();
    }

    fn do_install(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.download_working {
            return self.set_status("Wait for download storage maintenance to finish.", true, cx);
        }
        if self.busy {
            return;
        }
        let Some(state) = self.install.take() else {
            return;
        };
        if self.ac_root.as_ref() != Some(&state.root) {
            self.set_status(
                "Game folder changed. Preview the archive again before installing.",
                true,
                cx,
            );
            return;
        }
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let InstallState { archive, items, .. } = state;
            let n = items.len();
            let result = cx
                .background_executor()
                .spawn(async move { install::install_zip(&archive, &items) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(files) => {
                        this.set_status(
                            format!("Installed {n} item(s), {files} files."),
                            false,
                            cx,
                        );
                        this.reload(cx);
                    }
                    Err(e) => this.set_status(format!("Install failed: {e:#}"), true, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn unpack_acd(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(car) = self.selected_car() else {
            return;
        };
        let out = car.dir.join("data");
        if out.exists() {
            return self.set_status(
                "This car already has a data folder; not overwriting it.",
                true,
                cx,
            );
        }
        match acd::unpack(&car.dir.join("data.acd"), &out) {
            Ok(n) => self.set_status(
                format!("Unpacked {n} files to {}", out.display()),
                false,
                cx,
            ),
            Err(e) => self.set_status(format!("Unpack failed: {e:#}"), true, cx),
        }
    }

    fn pack_acd(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(car) = self.selected_car() else {
            return;
        };
        let acd_path = car.dir.join("data.acd");
        if acd_path.exists() {
            let backup = car.dir.join("data.acd~cm-rs-backup");
            if let Err(e) = std::fs::copy(&acd_path, &backup) {
                return self.set_status(format!("Couldn't back up data.acd: {e}"), true, cx);
            }
        }
        match acd::pack(&car.dir.join("data"), &acd_path) {
            Ok(n) => self.set_status(
                format!("Packed {n} files into data.acd (old one kept as data.acd~cm-rs-backup)"),
                false,
                cx,
            ),
            Err(e) => self.set_status(format!("Pack failed: {e:#}"), true, cx),
        }
    }

    // ---------- game settings ----------

    /// Loads the cfg files the first time the page opens (and after the AC folder changes).
    fn open_game_page(&mut self, cx: &mut Context<Self>) {
        if self.game_cfg.is_some() {
            return;
        }
        let Some(root) = self.ac_root.clone() else {
            self.game_error = Some("Set the Assetto Corsa folder in Settings first.".into());
            return;
        };
        match GameConfig::load(&root) {
            Ok(c) => {
                self.game_cfg = Some(c);
                self.game_error = None;
            }
            Err(e) => self.game_error = Some(format!("{e:#}")),
        }
        cx.notify();
    }

    fn save_game(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.game_cfg.as_mut() else {
            return;
        };
        match c.save() {
            Ok(files) if files.is_empty() => self.set_status("Nothing to save.", false, cx),
            Ok(files) => {
                let note = if self.game_running {
                    " Restart the game to apply."
                } else {
                    ""
                };
                self.set_status(format!("Saved {}.{note}", files.join(", ")), false, cx)
            }
            Err(e) => self.set_status(format!("Couldn't save game settings: {e:#}"), true, cx),
        }
    }

    fn revert_game(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.game_cfg = None;
        self.capturing = None;
        self.open_game_page(cx);
        self.set_status("Reverted to the saved game settings.", false, cx);
    }

    fn assign_key(&mut self, vk: Option<u32>, cx: &mut Context<Self>) {
        let (Some(i), Some(c)) = (self.capturing.take(), self.game_cfg.as_mut()) else {
            return;
        };
        let bind = &game_cfg::BINDS[i];
        c.set_bind(bind, vk);
        let conflicts = c.conflicts(bind);
        if !conflicts.is_empty() {
            self.status = Some((
                format!(
                    "{} shares its key with {}.",
                    bind.label,
                    conflicts.join(", ")
                ),
                true,
            ));
        }
        cx.notify();
    }

    fn capture_key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.capturing.is_none() {
            return;
        }
        cx.stop_propagation();
        if e.keystroke.key == "escape" {
            self.capturing = None;
            return cx.notify();
        }
        match game_cfg::vk_from_key(&e.keystroke.key) {
            Some(vk) => self.assign_key(Some(vk), cx),
            None => self.set_status(
                format!("AC can't bind “{}”; try another key.", e.keystroke.key),
                true,
                cx,
            ),
        }
    }

    /// Modifier keys arrive as modifier changes, not key presses. AC wants the
    /// left/right variant; GPUI doesn't say which side, so the left one is used.
    fn capture_modifier(
        &mut self,
        e: &ModifiersChangedEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.capturing.is_none() {
            return;
        }
        let m = e.modifiers;
        let vk = if m.control {
            0xA2
        } else if m.shift {
            0xA0
        } else if m.alt {
            0xA4
        } else {
            return;
        };
        self.assign_key(Some(vk), cx);
    }

    fn game_control(
        &self,
        idx: usize,
        s: &Setting,
        c: &GameConfig,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let value = c.get(s.file, s.section, s.key).unwrap_or("").to_owned();
        let base = idx * 100;
        fn set(
            idx: usize,
            v: String,
            cx: &mut Context<CmApp>,
        ) -> impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static {
            cx.listener(move |this, _, _, cx| {
                let s = &this.game_settings[idx];
                let (file, section, key) = (s.file, s.section, s.key);
                if let Some(c) = this.game_cfg.as_mut() {
                    c.set(file, section, key, &v);
                }
                cx.notify();
            })
        }
        match &s.kind {
            Kind::Toggle => {
                let on = value.trim() != "0" && !value.is_empty();
                segmented(vec![
                    chip(("gs", base), "On", on).on_click(set(idx, "1".into(), cx)),
                    chip(("gs", base + 1), "Off", !on).on_click(set(idx, "0".into(), cx)),
                ])
                .into_any_element()
            }
            Kind::Choice(opts) => {
                let known = opts.iter().any(|(v, _)| *v == value);
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .children(opts.iter().enumerate().map(|(j, (v, label))| {
                        chip(("gs", base + j), *label, *v == value).on_click(set(
                            idx,
                            (*v).to_owned(),
                            cx,
                        ))
                    }))
                    .when(!known && !value.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme::muted())
                                .child(format!("current: {value}")),
                        )
                    })
                    .into_any_element()
            }
            Kind::Filter => {
                div()
                    .flex()
                    .flex_1()
                    .flex_wrap()
                    .gap_1()
                    .children(c.filters.iter().enumerate().map(|(j, f)| {
                        chip(("gs", base + j), f.clone(), f.eq_ignore_ascii_case(&value))
                            .on_click(set(idx, f.clone(), cx))
                    }))
                    .into_any_element()
            }
            Kind::Number { scale, suffix, .. } => {
                let shown = format!("{}{suffix}", game_cfg::fmt_number(c.number(s) * scale));
                fn step(
                    idx: usize,
                    d: i32,
                    cx: &mut Context<CmApp>,
                ) -> impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static {
                    cx.listener(move |this, _, _, cx| {
                        let s = this.game_settings[idx].clone();
                        if let Some(c) = this.game_cfg.as_mut() {
                            c.step(&s, d);
                        }
                        cx.notify();
                    })
                }
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(button(("gs", base), "−").on_click(step(idx, -1, cx)))
                    .child(div().w(px(96.)).text_center().text_sm().child(shown))
                    .child(button(("gs", base + 1), "+").on_click(step(idx, 1, cx)))
                    .into_any_element()
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn start_pad_capture(&mut self, i: usize, cx: &mut Context<Self>) {
        use ac_core::gamepad::{Press, Reader};
        self.capturing = None;
        let mut reader = match Reader::open() {
            Ok(r) => r,
            Err(e) => return self.set_status(format!("{e:#}"), true, cx),
        };
        self.pad_capturing = Some(i);
        self.status = Some((format!("Listening on {}…", reader.names.join(", ")), false));
        cx.notify();
        self.pad_task = Some(cx.spawn(async move |this, cx| {
            for _ in 0..(15_000 / 16) {
                cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                let Some(press) = reader.poll() else { continue };
                let done = this
                    .update(cx, |this, cx| {
                        if this.pad_capturing != Some(i) {
                            return true;
                        }
                        match press {
                            Press::Button(name) => {
                                this.pad_capturing = None;
                                this.pad_mute_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(400));
                                let b = &game_cfg::BINDS[i];
                                if let Some(c) = this.game_cfg.as_mut() {
                                    c.set_pad(b, Some(name));
                                    let others = c.pad_conflicts(b);
                                    this.status = Some(if others.is_empty() {
                                        (format!("{} → {}", b.label, game_cfg::pad_label(name)), false)
                                    } else {
                                        (format!("{} shares {} with {}.", b.label, game_cfg::pad_label(name), others.join(", ")), true)
                                    });
                                }
                                cx.notify();
                                true
                            }
                            Press::Unbindable(_) => {
                                this.set_status("AC can't bind that button (L2/R2 are brake/throttle). Try another.", true, cx);
                                false
                            }
                        }
                    })
                    .unwrap_or(true);
                if done {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                if this.pad_capturing == Some(i) {
                    this.pad_capturing = None;
                    this.set_status("No button pressed.", true, cx);
                }
            })
            .ok();
        }));
    }

    #[cfg(not(target_os = "linux"))]
    fn start_pad_capture(&mut self, _: usize, cx: &mut Context<Self>) {
        self.set_status("Controller capture is Linux-only for now.", true, cx);
    }

    fn keybinds(&self, c: &GameConfig, cx: &mut Context<Self>) -> Vec<Div> {
        let mut panels: Vec<Div> = Vec::new();
        let mut current = "";
        for (i, b) in game_cfg::BINDS.iter().enumerate() {
            if b.group != current {
                current = b.group;
                panels.push(
                    panel()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(section_title(b.group)),
                );
            }
            let capturing = self.capturing == Some(i);
            let bound = c.bind(b);
            let label = if capturing {
                "Press a key… (Esc cancels)".to_string()
            } else {
                bound.map(game_cfg::vk_name).unwrap_or_else(|| "—".into())
            };
            let mut conflicts = c.conflicts(b);
            conflicts.extend(c.pad_conflicts(b));
            let pad_capturing = self.pad_capturing == Some(i);
            let pad = c.pad(b);
            let pad_cell = match b.pad_axis() {
                Some(axis) => div()
                    .w(px(200.))
                    .px_3()
                    .text_sm()
                    .text_color(theme::muted())
                    .child(format!("{axis} (automatic)"))
                    .into_any_element(),
                None => div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("pad", i))
                            .w(px(140.))
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .border_1()
                            .text_sm()
                            .cursor_pointer()
                            .when(pad_capturing, |d| {
                                d.border_color(theme::accent()).text_color(theme::accent())
                            })
                            .when(!pad_capturing, |d| {
                                d.border_color(theme::border())
                                    .bg(theme::panel())
                                    .hover(|s| s.bg(theme::panel_hover()))
                            })
                            .when(pad.is_none() && !pad_capturing, |d| {
                                d.text_color(theme::muted())
                            })
                            .child(if pad_capturing {
                                "Press a button…".to_string()
                            } else {
                                pad.map(|p| game_cfg::pad_label(p).to_owned())
                                    .unwrap_or_else(|| "—".into())
                            })
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.start_pad_capture(i, cx)),
                            ),
                    )
                    .when(pad.is_some(), |d| {
                        d.child(button(("unpad", i), "Clear").on_click(cx.listener(
                            move |this, _, _, cx| {
                                if let Some(c) = this.game_cfg.as_mut() {
                                    c.set_pad(&game_cfg::BINDS[i], None);
                                }
                                cx.notify();
                            },
                        )))
                    })
                    .into_any_element(),
            };
            let key_button = div()
                .id(("bind", i))
                .w(px(200.))
                .px_3()
                .py_1()
                .rounded_sm()
                .border_1()
                .text_sm()
                .cursor_pointer()
                .when(capturing, |d| {
                    d.border_color(theme::accent()).text_color(theme::accent())
                })
                .when(!capturing, |d| {
                    d.border_color(theme::border())
                        .bg(theme::panel())
                        .hover(|s| s.bg(theme::panel_hover()))
                })
                .when(bound.is_none() && !capturing, |d| {
                    d.text_color(theme::muted())
                })
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.capturing = Some(i);
                    this.pad_capturing = None;
                    this.pad_task = None;
                    window.focus(&this.capture_focus);
                    cx.notify();
                }));
            let row = div()
                .flex()
                .items_center()
                .gap_2()
                .min_h(px(28.))
                .child(
                    div()
                        .w(px(170.))
                        .flex_shrink_0()
                        .text_sm()
                        .text_color(theme::muted())
                        .child(b.label),
                )
                .child(key_button)
                .child(div().w(px(56.)).flex_shrink_0().child(if bound.is_some() {
                    button(("unbind", i), "Clear")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.capturing = Some(i);
                            this.assign_key(None, cx);
                        }))
                        .into_any_element()
                } else {
                    div().into_any_element()
                }))
                .child(pad_cell)
                .when(!conflicts.is_empty(), |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme::error())
                            .child(format!("also {}", conflicts.join(", "))),
                    )
                });
            let last = panels.pop().unwrap();
            panels.push(last.child(row));
        }
        panels
    }

    fn game_page(&self, text_w: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(c) = self.game_cfg.as_ref() else {
            let msg = self.game_error.clone().unwrap_or_else(|| "Loading…".into());
            return div()
                .id("game")
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::BOLD)
                        .child("Game settings"),
                )
                .child(div().text_sm().text_color(theme::error()).child(msg));
        };

        let tabs = segmented(
            Tab::ALL
                .iter()
                .map(|&t| {
                    chip(t.label(), t.label(), self.game_tab == t).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.game_tab = t;
                            this.capturing = None;
                            this.pad_capturing = None;
                            this.pad_task = None;
                            cx.notify();
                        },
                    ))
                })
                .collect(),
        );

        let mut panels: Vec<Div> = Vec::new();
        let mut current = "";
        for (idx, s) in self
            .game_settings
            .iter()
            .enumerate()
            .filter(|(_, s)| s.tab == self.game_tab)
        {
            if s.group != current {
                current = s.group;
                panels.push(
                    panel()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(section_title(s.group)),
                );
            }
            let row = div()
                .flex()
                .items_center()
                .gap_3()
                .min_h(px(28.))
                .child(
                    div()
                        .w(px(170.))
                        .flex_shrink_0()
                        .text_sm()
                        .text_color(theme::muted())
                        .child(s.label),
                )
                .child(self.game_control(idx, s, c, cx));
            let last = panels.pop().unwrap();
            panels.push(last.child(row));
        }
        if self.game_tab == Tab::Keys {
            panels.push(
                panel().child(
                    div()
                        .w(px(text_w - PANEL_INSET))
                        .text_sm()
                        .text_color(theme::muted())
                        .child(
                            "Keyboard: click a key, then press the new one (Esc cancels; Ctrl, Shift and Alt bind as the \
                             left-hand key). Controller: click its column, then press the button. Set Input method to \
                             Gamepad on the Controls tab to drive with it.",
                        ),
                ),
            );
            panels.extend(self.keybinds(c, cx));
        }

        let dirty = c.dirty_files();
        let footer = div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::sidebar())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(theme::muted())
                    .truncate()
                    .child(if dirty.is_empty() {
                        format!("Saved in {}", c.dir.display())
                    } else {
                        format!("Unsaved: {}", dirty.join(", "))
                    }),
            )
            .when(!dirty.is_empty(), |d| {
                d.child(button("game-revert", "Revert").on_click(cx.listener(Self::revert_game)))
            })
            .child(
                primary_button("game-save", "Save")
                    .when(dirty.is_empty(), |d| d.opacity(0.5))
                    .when(!dirty.is_empty(), |d| {
                        d.on_click(cx.listener(Self::save_game))
                    }),
            );

        div()
            .id("game")
            .track_focus(&self.capture_focus)
            .on_key_down(cx.listener(Self::capture_key))
            .on_modifiers_changed(cx.listener(Self::capture_modifier))
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .id("game-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .w_full()
                            .max_w(px(SETTINGS_MAX_W))
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("Game settings"),
                            )
                            .child(tabs)
                            .children(panels),
                    ),
            )
            .child(footer)
    }

    // ---------- views ----------

    fn ensure_events(&mut self, career: bool, cx: &mut Context<Self>) {
        let Some(root) = self.ac_root.clone() else { return; };
        if self.events.as_ref().is_some_and(|e| e.read(cx).root != root) && !self.game_running {
            self.events = None;
        }
        if self.events.is_none() {
            let driver = self.settings.driver_name.clone();
            let nationality = self.settings.nationality.clone();
            let proton = self.settings.proton.clone();
            let view = cx.new(|cx| EventsView::new(root, career, driver, nationality, proton, cx));
            cx.subscribe(&view, |this, _, event: &Running, cx| {
                this.game_running = event.0;
                cx.notify();
            }).detach();
            view.update(cx, |view, cx| view.set_running(self.game_running, cx));
            self.events = Some(view);
        } else if let Some(view) = &self.events {
            view.update(cx, |view, cx| view.configure(career, self.settings.driver_name.clone(), self.settings.nationality.clone(), self.settings.proton.clone(), cx));
        }
    }

    fn goto_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(page, Page::Career | Page::SpecialEvents) {
            self.ensure_events(page == Page::Career, cx);
        }
        if page == Page::Browser {
            self.ensure_browser(window, cx);
        }
        if let Some(browser) = &self.browser {
            browser.update(cx, |browser, cx| browser.set_active(page == Page::Browser, cx));
        }
        if page == Page::Settings {
            self.clear_download_confirm = None;
            self.refresh_download_usage(cx);
        }
        self.page = page;
        if page == Page::Game {
            self.open_game_page(cx);
        }
        cx.notify();
    }

    /// Moves the car or track selection (whichever list the page shows) by `delta` rows.
    fn pad_move(&mut self, delta: isize) {
        let tracks = match self.page {
            Page::Tracks => true,
            Page::Cars => false,
            _ => return,
        };
        if tracks {
            if self.tracks_view.is_empty() {
                return;
            }
            let pos = self.track.and_then(|t| self.tracks_view.iter().position(|&i| i == t));
            let next = step_index(pos, delta, self.tracks_view.len());
            self.track = Some(self.tracks_view[next]);
            self.track_scroll.scroll_to_item(next, ScrollStrategy::Center);
        } else {
            if self.cars_view.is_empty() {
                return;
            }
            let pos = self.car.and_then(|c| self.cars_view.iter().position(|&i| i == c));
            let next = step_index(pos, delta, self.cars_view.len());
            self.car = Some(self.cars_view[next]);
            self.skin = 0;
            self.car_scroll.scroll_to_item(next, ScrollStrategy::Center);
        }
    }

    /// One controller press aimed at the launcher UI. Ignored while a bind is
    /// being captured, the game runs, or the window is in the background.
    fn pad_press(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mute = std::time::Instant::now() + std::time::Duration::from_millis(400);
        if self.pad_capturing.is_some() || self.capturing.is_some() {
            self.pad_mute_until = Some(mute);
            return;
        }
        if self.pad_mute_until.is_some_and(|t| std::time::Instant::now() < t) {
            return;
        }
        if self.game_running || !window.is_window_active() {
            return;
        }
        if self.picker.is_some() {
            self.picker_press(name, window, cx);
            cx.notify();
            return;
        }
        if matches!(self.page, Page::Career | Page::SpecialEvents) && !matches!(name, "LSHOULDER" | "RSHOULDER" | "LTRIGGER" | "RTRIGGER" | "B") {
            if let Some(events) = &self.events { events.update(cx, |view, cx| view.pad(name, cx)); }
            return;
        }
        let here = tab_of(self.page);
        match name {
            "LSHOULDER" => self.goto_page(TABS[(here + TABS.len() - 1) % TABS.len()].1[0], window, cx),
            "RSHOULDER" => self.goto_page(TABS[(here + 1) % TABS.len()].1[0], window, cx),
            "LTRIGGER" | "RTRIGGER" => {
                let pages = TABS[here].1;
                if pages.len() > 1 {
                    let at = pages.iter().position(|p| *p == self.page).unwrap_or(0);
                    let next = if name == "RTRIGGER" { (at + 1) % pages.len() } else { (at + pages.len() - 1) % pages.len() };
                    self.goto_page(pages[next], window, cx);
                }
            }
            _ if self.page == Page::QuickDrive => self.drive_press(name, window, cx),
            "DPAD_UP" => self.pad_move(-1),
            "DPAD_DOWN" => self.pad_move(1),
            "A" if self.page == Page::Cars => self.open_picker(PickerKind::Car, window),
            "A" if self.page == Page::Tracks => self.open_picker(PickerKind::Track, window),
            "B" if self.page != Page::QuickDrive => self.goto_page(Page::QuickDrive, window, cx),
            "X" => {
                let n = self.selected_car().map(|c| c.skins.len()).unwrap_or(0);
                if n > 0 {
                    self.skin = (self.skin + 1) % n;
                }
            }
            "START" if matches!(self.page, Page::QuickDrive | Page::Cars | Page::Tracks) => self.start_game(cx),
            _ => {}
        }
        cx.notify();
    }

    #[cfg(target_os = "linux")]
    pub fn start_pad_nav(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use ac_core::gamepad::Nav;
        self.pad_nav = Some(cx.spawn_in(window, async move |this, cx| {
            let mut nav: Option<Nav> = None;
            let mut ticks = 0u32;
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                // Reopen every ~5 s so a pad that connects or reconnects is picked up.
                if ticks % 312 == 0 {
                    nav = Nav::open().ok();
                }
                ticks = ticks.wrapping_add(1);
                let Some(nav) = nav.as_mut() else { continue };
                let presses = nav.poll(std::time::Instant::now());
                if presses.is_empty() {
                    continue;
                }
                let device = if nav.is_playstation() {
                    crate::nav::InputDevice::PlayStation
                } else {
                    crate::nav::InputDevice::Xbox
                };
                for name in presses {
                    let ok = this.update_in(cx, |this, window, cx| {
                        this.input_device = device;
                        this.pad_press(name, window, cx)
                    });
                    if ok.is_err() {
                        return;
                    }
                }
            }
        }));
    }

    /// One pill in the top tab bar or the sub-tab strip.
    fn tab_button(&self, id: impl Into<gpui::ElementId>, label: &'static str, active: bool, page: Page, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        div()
            .id(id)
            .flex()
            .items_center()
            .h(px(theme::MIN_TARGET_H))
            .px_4()
            .rounded(px(theme::RADIUS_SM))
            .text_sm()
            .cursor_pointer()
            .when(active, |d| d.bg(theme::raised()).text_color(theme::text()).font_weight(FontWeight::BOLD))
            .when(!active, |d| d.text_color(theme::muted()).hover(|s| s.bg(theme::surface()).text_color(theme::text())))
            .on_click(cx.listener(move |this, _, window, cx| this.goto_page(page, window, cx)))
            .child(label)
    }

    /// Brand, the seven tabs flanked by the L1/R1 glyphs, and a sub-tab strip when the tab has several pages.
    fn tab_bar(&self, cx: &mut Context<Self>) -> Div {
        let here = tab_of(self.page);
        let dev = self.input_device;
        let glyph = |a: NavAction| {
            div().px_2().py_0p5().rounded(px(theme::RADIUS_SM)).border_1().border_color(theme::border()).text_xs().text_color(theme::muted()).child(dev.label(a))
        };
        let mut tabs = div().flex().items_center().gap_1();
        for (i, (label, pages)) in TABS.iter().enumerate() {
            tabs = tabs.child(self.tab_button(("tab", i), label, i == here, pages[0], cx));
        }
        let mut bar = div().flex().flex_col().flex_shrink_0().bg(theme::surface()).border_b_1().border_color(theme::border()).child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .h(px(56.))
                .child(div().mr_4().text_lg().font_weight(FontWeight::BOLD).text_color(theme::accent()).child("Content Manager"))
                .child(glyph(NavAction::TabPrev))
                .child(tabs)
                .child(glyph(NavAction::TabNext)),
        );
        let pages = TABS[here].1;
        if pages.len() > 1 {
            let mut subs = div().flex().items_center().gap_1().px_4().pb_2().child(div().mr_2().child(glyph(NavAction::SubPrev)));
            for (i, page) in pages.iter().enumerate() {
                subs = subs.child(self.tab_button(("subtab", i), page_label(*page), *page == self.page, *page, cx));
            }
            bar = bar.child(subs.child(div().ml_2().child(glyph(NavAction::SubNext))));
        }
        bar
    }

    /// Bottom bar: the buttons that work on this page, then the status line.
    fn footer_bar(&self, status: SharedString, status_err: bool, cx: &mut Context<Self>) -> Div {
        let dev = self.input_device;
        let h = |a: NavAction, text: &str| hint(dev.label(a), text);
        let mut hints = vec![h(NavAction::TabPrev, "Prev tab"), h(NavAction::TabNext, "Next tab")];
        match self.page {
            Page::Browser => hints.push(h(NavAction::Back, "Back to Drive")),
            Page::QuickDrive => {
                hints.push(hint(&format!("{} ↕↔", dev.label(NavAction::Up)), "Move / change"));
                hints.push(h(NavAction::Confirm, "Open"));
                hints.push(h(NavAction::Secondary, "Next skin"));
                hints.push(h(NavAction::Primary, "Go!"));
            }
            Page::Cars | Page::Tracks => {
                hints.push(h(NavAction::Confirm, "Change"));
                hints.push(h(NavAction::Back, "Drive"));
                hints.push(h(NavAction::Primary, "Go!"));
            }
            Page::Career | Page::SpecialEvents => {
                hints.push(h(NavAction::Up, "Move"));
                hints.push(h(NavAction::Confirm, "Start"));
                hints.push(h(NavAction::Back, "Back"));
            }
            Page::Install | Page::Game | Page::Settings => {
                if TABS[tab_of(self.page)].1.len() > 1 {
                    hints.push(h(NavAction::SubPrev, "Prev page"));
                    hints.push(h(NavAction::SubNext, "Next page"));
                }
                hints.push(h(NavAction::Back, "Back"));
            }
        }
        let status_el = div()
            .text_xs()
            .text_color(if status_err { theme::error() } else { theme::muted() })
            .child(status);
        let go = matches!(self.page, Page::QuickDrive | Page::Cars | Page::Tracks).then(|| {
            let label = if self.game_running { "Running…" } else { "Go!" };
            primary_button("go", label)
                .when(self.game_running, |d| d.opacity(0.6))
                .when(!self.game_running, |d| d.on_click(cx.listener(Self::go)))
        });
        let primary = div()
            .flex()
            .items_center()
            .gap_4()
            .child(status_el)
            .children(go)
            .into_any_element();
        hint_bar(hints, Some(primary))
    }

    /// `−  value  +` control; the row it edits is the single source of truth for keyboard, pad and mouse.
    fn stepper(&self, id: &'static str, value: String, row: DriveRow, value_w: f32, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(button((id, 0usize), "−").on_click(cx.listener(move |this, _, _, cx| this.drive_adjust(row, -1, cx))))
            .child(div().w(px(value_w)).text_center().text_sm().child(value))
            .child(button((id, 1usize), "+").on_click(cx.listener(move |this, _, _, cx| this.drive_adjust(row, 1, cx))))
    }

    fn open_quick_drive(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::QuickDrive;
        cx.notify();
    }

    /// Focusable Drive rows for the current mode, top to bottom.
    fn drive_rows(&self) -> Vec<DriveRow> {
        use DriveRow::*;
        let mut rows = vec![Car, Track, Mode];
        match self.mode {
            ModeKind::Practice => rows.push(Minutes),
            ModeKind::Hotlap => {}
            ModeKind::Race => rows.extend([Laps, Opponents, Ai, Grid]),
        }
        rows.extend([Penalties, Time, Temp, Weather]);
        rows
    }

    /// Changes the value behind `row` by `dir` (-1 / +1); Car cycles the skin.
    fn drive_adjust(&mut self, row: DriveRow, dir: i32, cx: &mut Context<Self>) {
        match row {
            DriveRow::Car => {
                let n = self.selected_car().map(|c| c.skins.len()).unwrap_or(0);
                if n > 0 {
                    self.skin = (self.skin as i32 + dir).rem_euclid(n as i32) as usize;
                }
            }
            DriveRow::Track => {}
            DriveRow::Mode => {
                let all = [ModeKind::Practice, ModeKind::Hotlap, ModeKind::Race];
                let at = all.iter().position(|m| *m == self.mode).unwrap_or(0) as i32;
                self.mode = all[(at + dir).rem_euclid(3) as usize];
            }
            DriveRow::Minutes => self.practice_minutes = (self.practice_minutes as i32 + dir * 5).clamp(0, 240) as u32,
            DriveRow::Laps => self.laps = (self.laps as i32 + dir).clamp(1, 200) as u32,
            DriveRow::Opponents => {
                self.opponents = (self.opponents as i32 + dir).clamp(1, 40) as u32;
                self.start_position = self.start_position.min(self.opponents + 1);
            }
            DriveRow::Ai => self.ai_level = (self.ai_level as i32 + dir).clamp(70, 100) as u32,
            DriveRow::Grid => self.start_position = (self.start_position as i32 + dir).clamp(1, self.opponents as i32 + 1) as u32,
            DriveRow::Penalties => self.penalties = !self.penalties,
            DriveRow::Time => {
                self.time_seconds = (self.time_seconds as i32 + dir * 1800).clamp(race::TIME_MIN as i32, race::TIME_MAX as i32) as u32
            }
            DriveRow::Temp => self.ambient = (self.ambient + dir).clamp(0, 40),
            DriveRow::Weather => {
                let n = self.content.weather.len();
                if n > 0 {
                    let at = self.content.weather.iter().position(|w| w.id == self.weather_id).unwrap_or(0) as i32;
                    self.weather_id = self.content.weather[(at + dir).rem_euclid(n as i32) as usize].id.clone();
                }
            }
        }
        cx.notify();
    }

    /// One controller/keyboard press on the Drive page (tab/sub-tab buttons are handled earlier).
    fn drive_press(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.drive_rows();
        let at = self.drive_focus.min(rows.len() - 1);
        let row = rows[at];
        match name {
            "DPAD_UP" => self.drive_focus = at.saturating_sub(1),
            "DPAD_DOWN" => self.drive_focus = (at + 1).min(rows.len() - 1),
            "DPAD_LEFT" => self.drive_adjust(row, -1, cx),
            "DPAD_RIGHT" => self.drive_adjust(row, 1, cx),
            "A" => match row {
                DriveRow::Car => self.open_picker(PickerKind::Car, window),
                DriveRow::Track => self.open_picker(PickerKind::Track, window),
                DriveRow::Mode | DriveRow::Penalties | DriveRow::Weather => self.drive_adjust(row, 1, cx),
                _ => {}
            },
            "X" => self.drive_adjust(DriveRow::Car, 1, cx),
            "START" => self.start_game(cx),
            _ => {}
        }
    }

    fn picker_view(&self) -> &[usize] {
        match self.picker {
            Some(PickerKind::Track) => &self.tracks_view,
            _ => &self.cars_view,
        }
    }

    fn open_picker(&mut self, kind: PickerKind, window: &Window) {
        let (cols, w) = picker_geometry(f32::from(window.viewport_size().width));
        self.picker_cols = cols;
        self.picker_card_w = w;
        self.picker = Some(kind);
        let current = if kind == PickerKind::Car { self.car } else { self.track };
        let view = self.picker_view();
        self.picker_focus = current.and_then(|c| view.iter().position(|&i| i == c)).unwrap_or(0);
        self.picker_scroll.scroll_to_item(self.picker_focus / cols, ScrollStrategy::Center);
    }

    fn pick(&mut self, kind: PickerKind, ix: usize, cx: &mut Context<Self>) {
        match kind {
            PickerKind::Car => {
                self.car = Some(ix);
                self.skin = 0;
            }
            PickerKind::Track => self.track = Some(ix),
        }
        self.picker = None;
        cx.notify();
    }

    fn cycle_picker_sort(&mut self, cx: &mut Context<Self>) {
        match self.picker {
            Some(PickerKind::Car) => {
                let at = CarSort::ALL.iter().position(|s| *s == self.car_sort).unwrap_or(0);
                self.car_sort = CarSort::ALL[(at + 1) % CarSort::ALL.len()];
            }
            Some(PickerKind::Track) => {
                let at = TrackSort::ALL.iter().position(|s| *s == self.track_sort).unwrap_or(0);
                self.track_sort = TrackSort::ALL[(at + 1) % TrackSort::ALL.len()];
            }
            None => return,
        }
        self.picker_focus = 0;
        self.picker_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.refilter(cx);
    }

    fn toggle_picker_order(&mut self, cx: &mut Context<Self>) {
        match self.picker {
            Some(PickerKind::Car) => self.car_sort_desc = !self.car_sort_desc,
            Some(PickerKind::Track) => self.track_sort_desc = !self.track_sort_desc,
            None => return,
        }
        self.picker_focus = 0;
        self.picker_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.refilter(cx);
    }

    /// Steps the brand filter through All → each brand → All.
    fn cycle_picker_brand(&mut self, dir: i32, cx: &mut Context<Self>) {
        let brands = &self.content.car_brands;
        let at = self
            .car_brand
            .as_ref()
            .and_then(|b| brands.iter().position(|(n, _)| n.to_lowercase() == b.to_lowercase()))
            .map_or(0, |i| i as i32 + 1);
        let next = (at + dir).rem_euclid(brands.len() as i32 + 1);
        self.car_brand = (next > 0).then(|| brands[next as usize - 1].0.clone());
        self.picker_focus = 0;
        self.picker_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.refilter(cx);
    }

    fn picker_press(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let kind = self.picker.unwrap_or(PickerKind::Car);
        let len = self.picker_view().len();
        let cols = self.picker_cols.max(1);
        let at = self.picker_focus.min(len.saturating_sub(1));
        let moved = |to: usize, this: &mut Self| {
            this.picker_focus = to.min(len.saturating_sub(1));
            this.picker_scroll.scroll_to_item(this.picker_focus / cols, ScrollStrategy::Center);
        };
        match name {
            "DPAD_UP" if at >= cols => moved(at - cols, self),
            "DPAD_DOWN" if at + cols < len => moved(at + cols, self),
            "DPAD_LEFT" if at > 0 => moved(at - 1, self),
            "DPAD_RIGHT" if at + 1 < len => moved(at + 1, self),
            "A" => {
                if let Some(&ix) = self.picker_view().get(at) {
                    self.pick(kind, ix, cx);
                }
            }
            "B" => self.picker = None,
            "Y" => {
                let input = if kind == PickerKind::Car { self.car_filter.clone() } else { self.track_filter.clone() };
                input.read(cx).focus(window);
            }
            "X" => self.cycle_picker_sort(cx),
            "RSHOULDER" => self.toggle_picker_order(cx),
            "LTRIGGER" if kind == PickerKind::Car => self.cycle_picker_brand(-1, cx),
            "RTRIGGER" if kind == PickerKind::Car => self.cycle_picker_brand(1, cx),
            _ => {}
        }
    }

    /// Keyboard on the Drive/Garage/Tracks pages and the picker, mapped onto the pad buttons.
    fn root_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.capturing.is_some() || self.pad_capturing.is_some() {
            return;
        }
        if self.picker.is_none() && !matches!(self.page, Page::QuickDrive | Page::Cars | Page::Tracks) {
            return;
        }
        let key = e.keystroke.key.as_str();
        if self.car_filter.read(cx).is_focused(window) || self.track_filter.read(cx).is_focused(window) {
            if matches!(key, "escape" | "down" | "enter") {
                window.focus(&self.root_focus);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        let name = match (key, e.keystroke.modifiers.control) {
            ("up", _) => "DPAD_UP",
            ("down", _) => "DPAD_DOWN",
            ("left", _) => "DPAD_LEFT",
            ("right", _) => "DPAD_RIGHT",
            ("enter", true) => "START",
            ("enter", false) | ("space", _) => "A",
            ("escape", _) => "B",
            ("q", _) => "LSHOULDER",
            ("e", _) => "RSHOULDER",
            ("[", _) => "LTRIGGER",
            ("]", _) => "RTRIGGER",
            ("x", _) => "X",
            ("/", _) => "Y",
            _ => return,
        };
        self.input_device = crate::nav::InputDevice::Keyboard;
        cx.stop_propagation();
        self.pad_press(name, window, cx);
    }

    pub fn focus_root(&self, window: &mut Window) {
        window.focus(&self.root_focus);
    }

    /// A labelled, focusable Drive row; the ring only shows for keyboard/pad.
    fn drive_row(&self, i: usize, label: &'static str, control: impl IntoElement, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let focused = self.input_device.shows_focus() && self.picker.is_none() && self.drive_focus == i;
        focus_ring(
            div()
                .id(("drow", i))
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .min_h(px(theme::MIN_TARGET_H))
                .px_3()
                .rounded(px(theme::RADIUS_SM))
                .bg(theme::raised()),
            focused,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.drive_focus = i;
            cx.notify();
        }))
        .child(div().text_sm().text_color(theme::muted()).child(label))
        .child(control)
    }

    fn quick_drive_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let car = self.selected_car();
        let skin = car.and_then(|c| c.skins.get(self.skin));
        let track = self.selected_track();
        let skin_count = car.map(|c| c.skins.len()).unwrap_or(0);
        let shows = self.input_device.shows_focus() && self.picker.is_none();
        let rows = self.drive_rows();
        let at = self.drive_focus.min(rows.len() - 1);

        let skin_label = skin
            .map(|s| format!("{} · {}/{}", s.name, self.skin + 1, skin_count))
            .unwrap_or_else(|| "No skins".into());
        let car_sub = car
            .map(|c| [c.brand.clone(), c.class.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · "))
            .unwrap_or_default();
        let car_card = focus_ring(
            div().id("drive-car").flex().flex_col().gap_2().p_3().rounded(px(theme::RADIUS_MD)).bg(theme::surface()),
            shows && at == 0,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.drive_focus = 0;
            cx.notify();
        }))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_xs().text_color(theme::faint()).child("CAR"))
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::BOLD)
                                .truncate()
                                .child(car.map(|c| c.name.clone()).unwrap_or_else(|| "No car".into())),
                        )
                        .child(div().text_sm().text_color(theme::muted()).truncate().child(car_sub)),
                )
                .child(button("car-change", "Change car").on_click(cx.listener(|this, _, window, cx| {
                    this.drive_focus = 0;
                    this.open_picker(PickerKind::Car, window);
                    cx.notify();
                }))),
        )
        .child(picture(skin.and_then(|s| s.preview.clone()), 230.).w_full())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(button("skin-prev", "◀").on_click(cx.listener(|this, _, _, cx| this.drive_adjust(DriveRow::Car, -1, cx))))
                .child(div().flex_1().text_sm().text_color(theme::muted()).truncate().text_center().child(format!("Skin: {skin_label}")))
                .child(button("skin-next", "▶").on_click(cx.listener(|this, _, _, cx| this.drive_adjust(DriveRow::Car, 1, cx)))),
        );

        let track_place = track
            .map(|t| [t.layout.city.clone(), t.layout.country.clone()].into_iter().flatten().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let track_size = track
            .map(|t| {
                format!(
                    "{} · {} pit boxes",
                    t.layout.length.clone().map(|l| format!("{l} m")).unwrap_or_else(|| "length ?".into()),
                    t.layout.pitboxes.map(|p| p.to_string()).unwrap_or_else(|| "?".into())
                )
            })
            .unwrap_or_default();
        let track_card = focus_ring(
            div().id("drive-track").flex().items_center().gap_3().p_3().rounded(px(theme::RADIUS_MD)).bg(theme::surface()),
            shows && at == 1,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.drive_focus = 1;
            cx.notify();
        }))
        .child(picture(track.and_then(|t| t.layout.preview.clone()), 84.).w(px(150.)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().text_xs().text_color(theme::faint()).child("TRACK"))
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::BOLD)
                        .truncate()
                        .child(track.map(|t| t.layout.name.clone()).unwrap_or_else(|| "No track".into())),
                )
                .child(div().text_sm().text_color(theme::muted()).truncate().child(track_place))
                .child(div().text_sm().text_color(theme::muted()).child(track_size)),
        )
        .child(picture(track.and_then(|t| t.layout.outline.clone()), 72.).w(px(72.)))
        .child(button("track-change", "Change").on_click(cx.listener(|this, _, window, cx| {
            this.drive_focus = 1;
            this.open_picker(PickerKind::Track, window);
            cx.notify();
        })));

        let mut session = div().flex().flex_col().gap_2().child(section_title("Session"));
        for (i, row) in rows.iter().enumerate().skip(2) {
            let row = *row;
            let el = match row {
                DriveRow::Mode => self.drive_row(
                    i,
                    "Mode",
                    segmented(vec![
                        chip("mode-practice", "Practice", self.mode == ModeKind::Practice).on_click(cx.listener(|this, _, _, cx| {
                            this.mode = ModeKind::Practice;
                            cx.notify();
                        })),
                        chip("mode-hotlap", "Hotlap", self.mode == ModeKind::Hotlap).on_click(cx.listener(|this, _, _, cx| {
                            this.mode = ModeKind::Hotlap;
                            cx.notify();
                        })),
                        chip("mode-race", "Quick race", self.mode == ModeKind::Race).on_click(cx.listener(|this, _, _, cx| {
                            this.mode = ModeKind::Race;
                            cx.notify();
                        })),
                    ]),
                    cx,
                ),
                DriveRow::Minutes => {
                    let label = if self.practice_minutes == 0 { "Unlimited".to_string() } else { format!("{} min", self.practice_minutes) };
                    self.drive_row(i, "Duration", self.stepper("minutes", label, row, 96., cx), cx)
                }
                DriveRow::Laps => self.drive_row(i, "Laps", self.stepper("laps", self.laps.to_string(), row, 96., cx), cx),
                DriveRow::Opponents => self.drive_row(i, "Opponents", self.stepper("opponents", self.opponents.to_string(), row, 96., cx), cx),
                DriveRow::Ai => self.drive_row(i, "AI strength", self.stepper("ai", format!("{}%", self.ai_level), row, 96., cx), cx),
                DriveRow::Grid => self.drive_row(i, "Start position", self.stepper("grid", self.start_position.to_string(), row, 96., cx), cx),
                DriveRow::Penalties => self.drive_row(
                    i,
                    "Penalties",
                    segmented(vec![
                        chip("pen-on", "On", self.penalties).on_click(cx.listener(|this, _, _, cx| {
                            this.penalties = true;
                            cx.notify();
                        })),
                        chip("pen-off", "Off", !self.penalties).on_click(cx.listener(|this, _, _, cx| {
                            this.penalties = false;
                            cx.notify();
                        })),
                    ]),
                    cx,
                ),
                DriveRow::Time => self.drive_row(i, "Time", self.stepper("time", fmt_time(self.time_seconds), row, 96., cx), cx),
                DriveRow::Temp => {
                    let road = race::road_temperature(self.time_seconds as f64, self.ambient as f64, 1.0);
                    self.drive_row(
                        i,
                        "Temperature",
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(self.stepper("temp", format!("{}°C", self.ambient), row, 64., cx))
                            .child(div().text_xs().text_color(theme::muted()).child(format!("road {road:.0}°C"))),
                        cx,
                    )
                }
                DriveRow::Weather => {
                    let name = self
                        .content
                        .weather
                        .iter()
                        .find(|w| w.id == self.weather_id)
                        .map(|w| w.name.clone())
                        .unwrap_or_else(|| self.weather_id.clone());
                    self.drive_row(i, "Weather", self.stepper("weather", name, row, 150., cx), cx)
                }
                DriveRow::Car | DriveRow::Track => continue,
            };
            session = session.child(el);
        }

        div()
            .flex()
            .size_full()
            .gap_4()
            .p_4()
            .child(
                div()
                    .id("drive-left")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .gap_3()
                    .overflow_y_scroll()
                    .child(car_card)
                    .child(track_card),
            )
            .child(
                div()
                    .id("drive-right")
                    .flex()
                    .flex_col()
                    .w(px(460.))
                    .flex_shrink_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(session),
            )
    }

    fn picker_card(&self, pos: usize, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let kind = self.picker.unwrap_or(PickerKind::Car);
        let w = self.picker_card_w;
        let focused = self.input_device.shows_focus() && self.picker_focus == pos;
        let (ix, title, sub, image, current) = match kind {
            PickerKind::Car => {
                let ix = self.cars_view[pos];
                let c = &self.content.cars[ix];
                let sub = [c.brand.clone(), c.class.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                (ix, c.name.clone(), sub, c.preview().map(|p| p.to_path_buf()), self.car == Some(ix))
            }
            PickerKind::Track => {
                let ix = self.tracks_view[pos];
                let t = &self.content.tracks[ix];
                let sub = [t.layout.country.clone(), t.layout.length.clone().map(|l| format!("{l} m"))]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                (ix, t.layout.name.clone(), sub, t.layout.preview.clone().or_else(|| t.layout.outline.clone()), self.track == Some(ix))
            }
        };
        focus_ring(
            div()
                .id(("pcard", pos))
                .w(px(w))
                .h(px(PICK_CARD_H))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .rounded(px(theme::RADIUS_MD))
                .cursor_pointer()
                .when(current, |d| d.bg(theme::overlay()))
                .when(!current, |d| d.bg(theme::surface()).hover(|s| s.bg(theme::raised()))),
            focused,
        )
        .on_click(cx.listener(move |this, _, _, cx| this.pick(kind, ix, cx)))
        .child(picture(image, 108.).w_full())
        .child(two_lines(title, sub, Some(w - 28.)))
    }

    /// Full-screen car/track picker: search, sort, brand filter and a grid of cards.
    fn picker_sheet(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let kind = self.picker.unwrap_or(PickerKind::Car);
        let cars = kind == PickerKind::Car;
        let dev = self.input_device;
        let (title, filter, counter, sort_label, desc) = if cars {
            (
                "Choose a car",
                self.car_filter.clone(),
                count_label(self.cars_view.len(), self.content.cars.len(), "cars"),
                self.car_sort.label(),
                self.car_sort_desc,
            )
        } else {
            (
                "Choose a track",
                self.track_filter.clone(),
                count_label(self.tracks_view.len(), self.content.tracks.len(), "layouts"),
                self.track_sort.label(),
                self.track_sort_desc,
            )
        };
        let len = self.picker_view().len();
        let cols = self.picker_cols.max(1);
        let body: gpui::AnyElement = if len == 0 {
            div().flex_1().flex().items_center().justify_center().text_color(theme::muted()).child("No matches").into_any_element()
        } else {
            uniform_list(
                "picker-grid",
                len.div_ceil(cols),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    let cols = this.picker_cols.max(1);
                    let len = this.picker_view().len();
                    range
                        .map(|ri| {
                            let mut row = div().id(("prow", ri)).flex().gap_3().h(px(PICK_ROW_H)).w_full();
                            for pos in (ri * cols..(ri + 1) * cols).take_while(|p| *p < len) {
                                row = row.child(this.picker_card(pos, cx));
                            }
                            row
                        })
                        .collect()
                }),
            )
            .track_scroll(self.picker_scroll.clone())
            .flex_1()
            .px_6()
            .into_any_element()
        };
        let mut hints = vec![
            hint(dev.label(NavAction::Confirm), "Pick"),
            hint(dev.label(NavAction::Back), "Close"),
            hint(dev.label(NavAction::Search), "Search"),
            hint(dev.label(NavAction::Secondary), "Sort"),
            hint(dev.label(NavAction::TabNext), "Order"),
        ];
        if cars {
            hints.push(hint(&format!("{}/{}", dev.label(NavAction::SubPrev), dev.label(NavAction::SubNext)), "Brand"));
        }
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_6()
                    .h(px(72.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().text_xl().font_weight(FontWeight::BOLD).child(title))
                    .child(div().w(px(320.)).flex().child(grow_input(filter)))
                    .child(div().text_sm().text_color(theme::muted()).child(counter))
                    .child(div().flex_1())
                    .child(
                        button("pick-sort", format!("Sort: {sort_label}"))
                            .on_click(cx.listener(|this, _, _, cx| this.cycle_picker_sort(cx))),
                    )
                    .child(
                        button("pick-order", if desc { "↓ Descending" } else { "↑ Ascending" })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_picker_order(cx))),
                    )
                    .when(cars, |d| {
                        d.child(
                            button("pick-brand", format!("Brand: {}", self.car_brand.clone().unwrap_or_else(|| "All".into())))
                                .on_click(cx.listener(|this, _, _, cx| this.cycle_picker_brand(1, cx))),
                        )
                    })
                    .child(button("pick-close", "Close").on_click(cx.listener(|this, _, _, cx| {
                        this.picker = None;
                        cx.notify();
                    }))),
            )
            .child(div().h(px(12.)).flex_shrink_0())
            .child(body)
            .child(hint_bar(hints, None))
            .into_any_element()
    }

    fn cars_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let detail =
            match self.selected_car() {
                None => div()
                    .p_4()
                    .text_color(theme::muted())
                    .child("No car selected"),
                Some(car) => {
                    let dir = car.dir.clone();
                    let header = div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .when_some(car.badge.clone(), |d, b| {
                            d.child(
                                img(b)
                                    .size(px(40.))
                                    .flex_shrink_0()
                                    .object_fit(ObjectFit::Contain),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_xl()
                                        .font_weight(FontWeight::BOLD)
                                        .truncate()
                                        .child(car.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme::muted())
                                        .truncate()
                                        .child(car.id.clone()),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .items_center()
                                .gap_2()
                                .child(button("garage-change", "Change car").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.open_picker(PickerKind::Car, window);
                                        cx.notify();
                                    }),
                                ))
                                .child(button("open-folder", "Open folder").on_click(
                                    cx.listener(move |_, _, _, cx| cx.open_with_system(&dir)),
                                ))
                                .when(car.has_data_acd && !car.has_data_dir, |d| {
                                    d.child(
                                        button("unpack", "Unpack data.acd")
                                            .on_click(cx.listener(Self::unpack_acd)),
                                    )
                                })
                                .when(car.has_data_dir, |d| {
                                    d.child(
                                        button("pack", "Pack data.acd")
                                            .on_click(cx.listener(Self::pack_acd)),
                                    )
                                })
                                .child(
                                    primary_button("drive-car", "Drive")
                                        .on_click(cx.listener(Self::open_quick_drive)),
                                ),
                        );

                    let preview = car
                        .skins
                        .get(self.skin)
                        .and_then(|s| s.preview.clone())
                        .or(car.preview().map(|p| p.to_path_buf()));
                    let skins = div().flex().flex_wrap().gap_2().children(
                        car.skins.iter().enumerate().map(|(i, s)| {
                            let selected = i == self.skin;
                            div()
                                .id(("skin", i))
                                .w(px(112.))
                                .flex()
                                .flex_col()
                                .rounded_sm()
                                .overflow_hidden()
                                .border_1()
                                .border_color(if selected {
                                    theme::accent()
                                } else {
                                    theme::border()
                                })
                                .cursor_pointer()
                                .hover(|d| d.bg(theme::panel_hover()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.skin = i;
                                    cx.notify();
                                }))
                                .child(picture(s.preview.clone(), 63.).w_full())
                                .child(div().px_1().text_xs().truncate().child(s.name.clone()))
                        }),
                    );

                    let mut info = panel().grid().grid_cols(2).gap_x_6().gap_y_1();
                    for (k, v) in [
                        ("Brand", &car.brand),
                        ("Class", &car.class),
                        ("Year", &car.year),
                        ("Country", &car.country),
                        ("Author", &car.author),
                    ] {
                        if let Some(v) = v {
                            info = info.child(field(k, v.clone()));
                        }
                    }
                    for (k, v) in &car.specs {
                        info = info.child(field(content::spec_label(k).to_owned(), v.clone()));
                    }
                    info = info.child(field(
                        "Data",
                        match (car.has_data_acd, car.has_data_dir) {
                            (true, true) => "data.acd + data folder",
                            (true, false) => "data.acd (packed)",
                            (false, true) => "data folder (unpacked)",
                            (false, false) => "missing",
                        },
                    ));

                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_4()
                        .child(header)
                        .child(picture(preview, 300.).w_full())
                        .child(section_title(format!("Skins ({})", car.skins.len())))
                        .child(skins)
                        .child(section_title("Specs"))
                        .child(info)
                        .when(!car.tags.is_empty(), |d| {
                            d.child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_1()
                                    .children(car.tags.iter().map(|t| {
                                        div()
                                            .px_2()
                                            .rounded_sm()
                                            .bg(theme::panel())
                                            .text_xs()
                                            .text_color(theme::muted())
                                            .child(t.clone())
                                    })),
                            )
                        })
                        .when_some(car.description.clone(), |d, desc| {
                            d.child(div().text_sm().child(desc))
                        })
                        .when_some(car.ui_error.clone(), |d, err| {
                            d.child(
                                div()
                                    .text_sm()
                                    .text_color(theme::error())
                                    .child(format!("ui_car.json: {err}")),
                            )
                        })
                }
            };
        div()
            .id("car-detail")
            .size_full()
            .overflow_y_scroll()
            .child(detail)
    }

    fn tracks_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let detail =
            match self.selected_track() {
                None => div()
                    .p_4()
                    .text_color(theme::muted())
                    .child("No track selected"),
                Some(t) => {
                    let l = &t.layout;
                    let mut info = panel()
                        .grid()
                        .grid_cols(2)
                        .gap_x_6()
                        .gap_y_1()
                        .child(field("Folder", t.key()));
                    for (k, v) in [
                        ("Country", &l.country),
                        ("City", &l.city),
                        ("Length", &l.length.clone().map(|x| format!("{x} m"))),
                    ] {
                        if let Some(v) = v {
                            info = info.child(field(k, v.clone()));
                        }
                    }
                    if let Some(p) = l.pitboxes {
                        info = info.child(field("Pit boxes", p.to_string()));
                    }
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_4()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xl()
                                        .font_weight(FontWeight::BOLD)
                                        .truncate()
                                        .child(l.name.clone()),
                                )
                                .child(button("tracks-change", "Change track").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.open_picker(PickerKind::Track, window);
                                        cx.notify();
                                    }),
                                ))
                                .child(
                                    primary_button("drive-track", "Drive")
                                        .on_click(cx.listener(Self::open_quick_drive)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .child(picture(l.preview.clone(), 240.).flex_1().min_w_0())
                                .child(picture(l.outline.clone(), 240.).w(px(240.))),
                        )
                        .child(info)
                        .when(!l.tags.is_empty(), |d| {
                            d.child(div().flex().flex_wrap().gap_1().children(l.tags.iter().map(
                                |t| {
                                    div()
                                        .px_2()
                                        .rounded_sm()
                                        .bg(theme::panel())
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .child(t.clone())
                                },
                            )))
                        })
                        .when_some(l.description.clone(), |d, desc| {
                            d.child(div().text_sm().child(desc))
                        })
                }
            };
        div()
            .id("track-detail")
            .size_full()
            .overflow_y_scroll()
            .child(detail)
    }

    /// `text_w`: pixel width for wrapping text in the page column (see [`text_w`]).
    fn install_page(&self, text_w: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let dest = self
            .ac_root
            .as_ref()
            .map(|r| format!("Installs into {}", r.join("content").display()))
            .unwrap_or_else(|| "Set the Assetto Corsa folder in Settings first.".into());
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .w_full()
            .max_w(px(INSTALL_MAX_W))
            .child(div().text_xl().font_weight(FontWeight::BOLD).child("Install mods"))
            .child(div().w(px(text_w)).text_sm().text_color(theme::muted()).child(
                "Pick a .zip with cars or tracks. Folders are found by ui_car.json, data.acd or ui_track.json, \
                 wherever they sit in the archive.",
            ))
            .child(div().w(px(text_w)).text_sm().text_color(theme::muted()).child(dest))
            .child(
                div().flex().child(
                    primary_button("choose-zip", "Choose archive…")
                        .when(self.busy || self.ac_root.is_none(), |d| d.opacity(0.5))
                        .when(!self.busy && self.ac_root.is_some(), |d| d.on_click(cx.listener(Self::choose_archive))),
                ),
            );
        if let Some(state) = &self.install {
            let name = state
                .archive
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut list = panel()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(FontWeight::BOLD).child(name))
                .child(
                    div()
                        .w(px(text_w - PANEL_INSET))
                        .text_xs()
                        .text_color(theme::muted())
                        .child(state.archive.display().to_string()),
                )
                .child(section_title(format!("Found {}", state.items.len())));
            for item in &state.items {
                let kind = match item.kind {
                    install::Kind::Car => "Car",
                    install::Kind::Track => "Track",
                };
                list = list.child(
                    div()
                        .flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .w(px(60.))
                                .flex_shrink_0()
                                .text_color(theme::muted())
                                .child(kind),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(item.id.clone()))
                        .child(if item.exists {
                            div()
                                .flex_shrink_0()
                                .text_color(theme::accent())
                                .child("update, overwrites files")
                        } else {
                            div().flex_shrink_0().text_color(theme::ok()).child("new")
                        }),
                );
            }
            page = page.child(list).child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        primary_button(
                            "install",
                            if self.busy {
                                "Installing…"
                            } else {
                                "Install"
                            },
                        )
                        .when(self.busy, |d| d.opacity(0.6))
                        .when(!self.busy, |d| d.on_click(cx.listener(Self::do_install))),
                    )
                    .when(!self.busy, |d| {
                        d.child(button("cancel-install", "Cancel").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.install = None;
                                cx.notify();
                            },
                        )))
                    }),
            );
        }
        div()
            .id("install")
            .size_full()
            .overflow_y_scroll()
            .child(page)
    }

    fn settings_page(&self, text_w: f32, cx: &mut Context<Self>) -> impl IntoElement {
        const LABEL_W: f32 = 170.;
        let label = |text: &'static str| {
            div()
                .w(px(LABEL_W))
                .flex_shrink_0()
                .text_sm()
                .text_color(theme::muted())
                .child(text)
        };
        let row = |text: &'static str, control: gpui::AnyElement| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(label(text))
                .child(control)
        };
        // Label above value so long paths get the whole panel width to wrap in.
        let info = |text: &'static str, value: String| {
            div()
                .flex()
                .flex_col()
                .w_full()
                .child(div().text_xs().text_color(theme::muted()).child(text))
                .child(div().w(px(text_w - PANEL_INSET)).text_sm().child(value))
        };
        let hint = |text: &'static str| {
            div()
                .pl(px(LABEL_W + 12.))
                .text_xs()
                .text_color(theme::muted())
                .child(text)
        };

        let race_ini = self
            .ac_root
            .as_ref()
            .and_then(|r| paths::race_ini_path(r))
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found (on Linux: run AC from Steam once)".into());

        let game = panel()
            .flex()
            .flex_col()
            .gap_2()
            .child(section_title("Game"))
            .child(row(
                "Assetto Corsa folder",
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(grow_input(self.in_root.clone()))
                    .child(
                        button("browse-root", "Browse…").on_click(cx.listener(Self::browse_root)),
                    )
                    .child(
                        button("detect-root", "Auto-detect")
                            .on_click(cx.listener(Self::detect_root)),
                    )
                    .into_any_element(),
            ))
            .when(!cfg!(windows), |d| {
                d.child(row(
                    "Proton script",
                    grow_input(self.in_proton.clone()).into_any_element(),
                ))
                .child(hint(
                    "Leave empty to use the last Proton recorded in the prefix (shown below).",
                ))
            });

        let driver = panel()
            .flex()
            .flex_col()
            .gap_2()
            .child(section_title("Driver"))
            .child(row(
                "Name",
                div()
                    .w(px(280.))
                    .child(self.in_driver.clone())
                    .into_any_element(),
            ))
            .child(row(
                "Nationality",
                div()
                    .w(px(80.))
                    .child(self.in_nation.clone())
                    .into_any_element(),
            ))
            .child(hint("Three-letter code, for example ITA, GBR or JOR."));

        let downloads = panel()
            .flex().flex_col().gap_2()
            .child(section_title("Browser downloads"))
            .child(div().text_xs().text_color(theme::muted()).child("Download directory (empty = default cache)"))
            .child(div().flex().items_center().gap_2()
                .child(grow_input(self.in_download_dir.clone()))
                .child(button("browse-download-directory", "Browse…")
                    .on_click(cx.listener(Self::browse_download_directory)))
                .child(button("save-download-directory", "Save folder")
                    .when(!self.download_working, |d| d.on_click(cx.listener(Self::save_download_directory)))
                    .when(self.download_working, |d| d.opacity(0.5))))
            .child(info("Current download directory", download_base_dir(self.settings.download_dir.as_deref()).display().to_string()))
            .child(div().w(px(text_w - PANEL_INSET)).text_xs().text_color(theme::muted())
                .child("Folder changes apply to new downloads only. Existing ZIPs are not moved. Size and cleanup cover cm-rs ZIP downloads only; active downloads are skipped."))
            .child(div().flex().items_center().gap_2()
                .child(div().text_sm().child(self.download_usage.clone()))
                .child(button("refresh-download-size", "Refresh")
                    .when(!self.download_working, |d| d.on_click(cx.listener(|this, _, _, cx| this.refresh_download_usage(cx))))
                    .when(self.download_working, |d| d.opacity(0.5)))
                .child(button("clear-downloads", "Clear ZIP files…")
                    .when(!self.download_working && !self.busy, |d| d.on_click(cx.listener(|this, _, _, cx| {
                        this.clear_download_confirm = Some(download_base_dir(this.settings.download_dir.as_deref()));
                        cx.notify();
                    })))
                    .when(self.download_working || self.busy, |d| d.opacity(0.5))))
            .when_some(self.clear_download_confirm.as_ref(), |d, path| {
                d.child(div().flex().flex_col().gap_2()
                    .child(div().w(px(text_w - PANEL_INSET)).text_sm().text_color(theme::accent())
                        .child(format!("Permanently delete the downloaded ZIP files in {}? Installed cars, tracks and other mods will NOT be deleted. Unrelated files and active downloads are kept. This cannot be undone.", path.display())))
                    .child(div().flex().gap_2()
                        .child(button("confirm-clear-downloads", "Delete downloaded ZIPs")
                            .when(!self.download_working && !self.busy, |d| d.on_click(cx.listener(|this, _, _, cx| this.run_download_maintenance(true, cx)))))
                        .child(button("cancel-clear-downloads", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                            this.clear_download_confirm = None;
                            cx.notify();
                        })))))
            });

        let detected = panel()
            .flex()
            .flex_col()
            .gap_2()
            .child(section_title("Detected"))
            .child(info("race.ini", race_ini))
            .when(!cfg!(windows), |d| {
                let proton = self
                    .ac_root
                    .as_ref()
                    .and_then(|r| launch::find_proton(r))
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "none found".into());
                d.child(info("Proton", proton))
            })
            .child(info(
                "Settings file",
                ac_core::settings::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ));

        div().id("settings").size_full().overflow_y_scroll().child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .w_full()
                .max_w(px(SETTINGS_MAX_W))
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::BOLD)
                        .child("Settings"),
                )
                .child(downloads)
                .child(game)
                .child(driver)
                .child(
                    div().flex().justify_end().child(
                        primary_button("save-settings", "Save")
                            .on_click(cx.listener(Self::save_settings)),
                    ),
                )
                .child(detected),
        )
    }
}

impl Render for CmApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main_w = f32::from(window.viewport_size().width) - SIDEBAR_W;
        let page: gpui::AnyElement = if self.loading && self.page != Page::Browser {
            div()
                .p_4()
                .text_color(theme::muted())
                .child("Loading content…")
                .into_any_element()
        } else {
            match self.page {
                Page::QuickDrive => self.quick_drive_page(cx).into_any_element(),
                Page::Cars => self.cars_page(cx).into_any_element(),
                Page::Tracks => self.tracks_page(cx).into_any_element(),
                Page::Career | Page::SpecialEvents => div().size_full().children(self.events.clone()).into_any_element(),
                Page::Install => self
                    .install_page(text_w(main_w, INSTALL_MAX_W), cx)
                    .into_any_element(),
                Page::Browser => div()
                    .size_full()
                    .children(self.browser.clone())
                    .into_any_element(),
                Page::Game => self
                    .game_page(text_w(main_w, SETTINGS_MAX_W), cx)
                    .into_any_element(),
                Page::Settings => self
                    .settings_page(text_w(main_w, SETTINGS_MAX_W), cx)
                    .into_any_element(),
            }
        };
        let status: SharedString = self
            .status
            .as_ref()
            .map(|(s, _)| s.clone())
            .unwrap_or_default()
            .into();
        let status_err = self.status.as_ref().is_some_and(|(_, e)| *e);

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .relative()
            .track_focus(&self.root_focus)
            .on_key_down(cx.listener(Self::root_key))
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| {
                if this.input_device != crate::nav::InputDevice::Mouse {
                    this.input_device = crate::nav::InputDevice::Mouse;
                    cx.notify();
                }
            }))
            .child(self.tab_bar(cx))
            .child(div().flex_1().min_h_0().min_w_0().child(page))
            .child(self.footer_bar(status, status_err, cx))
            .children(self.picker.map(|_| self.picker_sheet(cx)))
    }
}
