//! Online tab: servers from the Kunos lobby, favourite and recent servers, one
//! server's page with its cars, and joining it.
//!
//! The three sub-pages share this one view (`Source`), so the lobby download and
//! pings survive switching between them. Every network call runs off the UI
//! thread; results carry a generation number so a late answer never overwrites a
//! newer request.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ac_core::ai_line::RacingLine;
use ac_core::content::{self, Car, TrackEntry};
use ac_core::launch;
use ac_core::online::{self, CarSlot, Details, JoinOutcome, LobbyBusy, Pinger, Server, Store};
use ac_core::race::QuickDrive;
use ac_core::settings::ProtonInputSettings;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FontWeight, Render, ScrollHandle,
    ScrollStrategy, SharedString, Stateful, UniformListScrollHandle, Window, div, prelude::*, px,
    uniform_list,
};

use crate::events_view::Running;
use crate::nav::{InputDevice, NavAction};
use crate::text_input::TextInput;
use crate::theme;
use crate::widgets::{button, chip, focus_ring, panel, picture, section_title, segmented, track_map};

impl EventEmitter<Running> for OnlineView {}

/// Past this the rows only grow empty space between columns.
const MAX_W: f32 = 1800.;
const ROW_H: f32 = 64.;
const CAR_CARD_W: f32 = 210.;
const GAP: f32 = 12.;
/// Car stats shown in the server page's Details view, as `ui_car.json` spec keys.
const CAR_STATS: [(&str, &str); 6] =
    [("bhp", "Power"), ("torque", "Torque"), ("weight", "Weight"), ("pwratio", "Power/weight"), ("topspeed", "Top speed"), ("acceleration", "0–100")];
const STAT_W: f32 = 104.;
/// Revisiting the tab refreshes a list older than this.
const STALE_AFTER: Duration = Duration::from_secs(120);
const PING_POLL: Duration = Duration::from_millis(250);

/// Which list the page shows; each is its own sub-tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Lobby,
    Favourites,
    Recent,
}

impl Source {
    fn index(self) -> usize {
        match self {
            Source::Lobby => 0,
            Source::Favourites => 1,
            Source::Recent => 2,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    Installed,
    Players,
    Free,
    Open,
}

impl Filter {
    const ALL: [Filter; 4] = [Filter::Installed, Filter::Players, Filter::Free, Filter::Open];

    fn label(self) -> &'static str {
        match self {
            Filter::Installed => "Installed content",
            Filter::Players => "With drivers",
            Filter::Free => "Free slots",
            Filter::Open => "No password",
        }
    }
}

/// A sortable list column.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Ping,
    Name,
    Track,
    Drivers,
}

impl Sort {
    /// The sortable columns, in header order (also the pad's focus order).
    const ALL: [Sort; 4] = [Sort::Ping, Sort::Name, Sort::Track, Sort::Drivers];

    fn label(self) -> &'static str {
        match self {
            Sort::Ping => "Ping",
            Sort::Name => "Server",
            Sort::Track => "Track",
            Sort::Drivers => "Drivers",
        }
    }

    /// The direction a first click picks: most drivers first, else lowest / A to Z.
    fn default_desc(self) -> bool {
        self == Sort::Drivers
    }
}

/// How the server page lists its cars.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CarView {
    Cards,
    Details,
}

/// Focusable controls above the list, left to right.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BarItem {
    Filter(Filter),
    Refresh,
    AddServer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Bar(usize),
    /// A column header, by index into `Sort::ALL`.
    Header(usize),
    List,
}

/// Column widths of the server list, shared by the header and the rows.
struct Cols {
    star: f32,
    ping: f32,
    name: f32,
    track: f32,
    cars: f32,
    drivers: f32,
    info: f32,
}

impl Cols {
    fn new(width: f32) -> Self {
        let track = (width * 0.17).clamp(170., 300.);
        let cars = (width * 0.17).clamp(170., 300.);
        let (star, ping, drivers, info) = (24., 60., 72., 104.);
        // Six 12 px gaps between seven cells, 12 px padding each side.
        let fixed = star + ping + track + cars + drivers + info + 6. * GAP + 24.;
        Self { star, ping, name: (width - fixed).max(180.), track, cars, drivers, info }
    }

    fn of(&self, sort: Sort) -> f32 {
        match sort {
            Sort::Ping => self.ping,
            Sort::Name => self.name,
            Sort::Track => self.track,
            Sort::Drivers => self.drivers,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowRef {
    Lobby(usize),
    /// Index into the favourites or recent list, whichever is shown.
    Saved(usize),
}

enum Lookup {
    Loading,
    Ready(Box<Server>),
    Failed(String),
}

/// Installed content, keyed the way servers name it (lowercase ids).
#[derive(Default)]
pub struct Installed {
    cars: HashMap<String, InstalledCar>,
    tracks: HashMap<String, InstalledTrack>,
}

struct InstalledCar {
    id: String,
    name: String,
    /// Brand, class and year, for the Details view.
    about: String,
    specs: Vec<(String, String)>,
    /// Skin id and preview image, in the scan's order.
    skins: Vec<(String, Option<PathBuf>)>,
}

struct InstalledTrack {
    track_id: String,
    layout_id: Option<String>,
    name: String,
    preview: Option<PathBuf>,
    outline: Option<PathBuf>,
    racing_line: Option<Arc<RacingLine>>,
    length: Option<String>,
}

impl Installed {
    pub fn new(cars: &[Car], tracks: &[TrackEntry]) -> Self {
        let cars = cars
            .iter()
            .map(|c| {
                let about = [&c.brand, &c.class, &c.year].into_iter().flatten().map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                let car = InstalledCar {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    about,
                    specs: c.specs.clone(),
                    skins: c.skins.iter().map(|s| (s.id.clone(), s.preview.clone())).collect(),
                };
                (c.id.to_lowercase(), car)
            })
            .collect();
        let tracks = tracks
            .iter()
            .map(|t| {
                // Servers write `track` or `track-layout`.
                let key = match &t.layout.layout_id {
                    Some(l) => format!("{}-{l}", t.track_id),
                    None => t.track_id.clone(),
                };
                let track = InstalledTrack {
                    track_id: t.track_id.clone(),
                    layout_id: t.layout.layout_id.clone(),
                    name: t.layout.name.clone(),
                    preview: t.layout.preview.clone(),
                    outline: t.layout.outline.clone(),
                    racing_line: t.layout.racing_line.clone(),
                    length: t.layout.length.clone(),
                };
                (key.to_lowercase(), track)
            })
            .collect();
        Self { cars, tracks }
    }

    fn car(&self, id: &str) -> Option<&InstalledCar> {
        self.cars.get(&id.to_lowercase())
    }

    fn track(&self, id: &str) -> Option<&InstalledTrack> {
        self.tracks.get(&id.to_lowercase())
    }

    fn has_any_car(&self, s: &Server) -> bool {
        s.cars.iter().any(|c| self.car(c).is_some())
    }
}

/// Everything a launch needs from the app's settings.
#[derive(Clone, Default)]
pub struct LaunchConfig {
    pub driver: String,
    pub nationality: String,
    pub proton: Option<PathBuf>,
    pub input: ProtonInputSettings,
    /// The last Quick Drive, for preferences such as units; car and track are replaced.
    pub base: QuickDrive,
}

/// One server's page.
struct Detail {
    /// `host:port` as saved, or `ip:http_port` for lobby servers.
    address: String,
    server: Option<Server>,
    slots: Option<Result<Vec<CarSlot>, String>>,
    details: Option<Details>,
    error: Option<String>,
    loading: bool,
    /// The server's id of the chosen car.
    car: Option<String>,
    /// Password field (when needed) first, then the car cards.
    focus: usize,
    generation: u64,
}

/// A server car with what the page needs to show and pick it.
struct CarChoice {
    id: String,
    name: String,
    installed: bool,
    free: usize,
    total: usize,
    preview: Option<PathBuf>,
    about: String,
    specs: Vec<(String, String)>,
}

impl CarChoice {
    /// A `ui_car.json` spec as written, or a dash when the car doesn't say. Kunos writes
    /// unknown values as placeholders such as `--km/h` or `--s 0-100`, so no digit = unknown.
    fn spec(&self, key: &str) -> String {
        let value = self.specs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.trim()).unwrap_or_default();
        // The column title already says 0–100.
        let value = value.trim_end_matches("0-100").trim_end_matches("0–100").trim();
        if value.chars().any(|c| c.is_ascii_digit()) { value.to_owned() } else { "—".into() }
    }
}

pub struct OnlineView {
    pub root: PathBuf,
    source: Source,
    installed: Installed,
    config: LaunchConfig,
    steam_id: Option<String>,
    /// Installed CSP build; servers may require a minimum.
    csp_build: Option<u32>,
    input_device: InputDevice,
    running: bool,
    /// Page-level notice; `true` is a problem.
    message: Option<(String, bool)>,

    lobby: Vec<Server>,
    lobby_search: Vec<String>,
    lobby_loading: bool,
    lobby_error: Option<String>,
    lobby_at: Option<Instant>,
    /// Last request sent to the lobby, for its rate limit.
    lobby_asked: Option<Instant>,
    lobby_generation: u64,
    /// The list shown came from the disk cache, not this session's download.
    lobby_cached: bool,
    cache_tried: bool,

    pings: HashMap<SocketAddrV4, u32>,
    /// Addresses already sent to a pinger, so overlapping rounds don't ping twice (which
    /// doubles the rate and loses replies).
    ping_queued: HashSet<SocketAddrV4>,
    pingers: Vec<Pinger>,
    /// A poll loop is running; it stops itself once every pinger is done.
    ping_polling: bool,

    store: Store,
    /// Set when online.toml can't be read; saving is off until it is fixed.
    store_error: Option<String>,
    lookups: HashMap<String, Lookup>,
    /// Typed passwords, per server, until cm-rs closes. Never written to disk.
    passwords: HashMap<String, String>,

    search: Entity<TextInput>,
    filters: [bool; 4],
    /// Column and descending, per source; `None` keeps the list's own order.
    sorts: [Option<(Sort, bool)>; 3],
    visible: Vec<RowRef>,
    selected: usize,
    focus: Focus,
    scroll: UniformListScrollHandle,

    adding: bool,
    add_busy: bool,
    add_error: Option<String>,
    address: Entity<TextInput>,

    detail: Option<Detail>,
    detail_generation: u64,
    password: Entity<TextInput>,
    detail_scroll: ScrollHandle,
    car_view: CarView,
    car_cols: usize,
    /// Child index of the password row, the car view toggle and the first row of cars in
    /// the server page's scroll container, as last rendered.
    password_child: usize,
    view_child: usize,
    cars_child: usize,
}

impl OnlineView {
    pub fn new(root: PathBuf, source: Source, installed: Installed, config: LaunchConfig, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new(cx, "Search servers, tracks, cars…", ""));
        cx.observe(&search, |this, _, cx| this.refilter(cx)).detach();
        let address = cx.new(|cx| TextInput::new(cx, "host:port, e.g. 203.0.113.5:8081", ""));
        let password = cx.new(|cx| TextInput::new(cx, "Server password", "").masked());
        let (store, store_error) = match Store::load() {
            Ok(s) => (s, None),
            Err(e) => (Store::default(), Some(format!("{e:#}"))),
        };
        let csp_build = ac_core::csp::installed_build(&root);
        let mut view = Self {
            root,
            source,
            installed,
            config,
            steam_id: online::steam_id(),
            csp_build,
            input_device: InputDevice::Mouse,
            running: false,
            message: None,
            lobby: Vec::new(),
            lobby_search: Vec::new(),
            lobby_loading: false,
            lobby_error: None,
            lobby_at: None,
            lobby_asked: None,
            lobby_generation: 0,
            lobby_cached: false,
            cache_tried: false,
            pings: HashMap::new(),
            ping_queued: HashSet::new(),
            pingers: Vec::new(),
            ping_polling: false,
            store,
            store_error,
            lookups: HashMap::new(),
            passwords: HashMap::new(),
            search,
            filters: [true, false, false, false],
            sorts: [Some((Sort::Drivers, true)), None, None],
            visible: Vec::new(),
            selected: 0,
            focus: Focus::List,
            scroll: UniformListScrollHandle::new(),
            adding: false,
            add_busy: false,
            add_error: None,
            address,
            detail: None,
            detail_generation: 0,
            password,
            detail_scroll: ScrollHandle::new(),
            car_view: CarView::Cards,
            car_cols: 4,
            password_child: 0,
            view_child: 0,
            cars_child: 0,
        };
        view.set_source(source, cx);
        view
    }

    /// Whether the installed CSP is new enough for the server.
    fn csp_ok(&self, s: &Server) -> bool {
        s.required_csp.is_none_or(|need| self.csp_build.is_some_and(|have| have >= need))
    }

    fn csp_problem(&self, s: &Server) -> Option<String> {
        let need = s.required_csp.filter(|_| !self.csp_ok(s))?;
        Some(match self.csp_build {
            Some(have) => format!("Needs Custom Shaders Patch build {need} or newer; you have {have}."),
            None => format!("Needs Custom Shaders Patch build {need} or newer, which isn't installed."),
        })
    }

    // ---------- inputs from the app ----------

    pub fn set_source(&mut self, source: Source, cx: &mut Context<Self>) {
        if self.source != source {
            self.source = source;
            self.selected = 0;
            self.focus = Focus::List;
            self.adding = false;
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        match source {
            Source::Lobby => {
                if self.lobby.is_empty() && !self.cache_tried {
                    self.load_cached_lobby(cx);
                }
                let stale = self.lobby_at.is_none_or(|t| t.elapsed() > STALE_AFTER);
                if stale && !self.lobby_loading && (self.lobby_error.is_none() || !self.lobby.is_empty()) {
                    self.refresh_lobby(cx);
                }
            }
            Source::Favourites | Source::Recent => self.refresh_saved(false, cx),
        }
        self.refilter(cx);
    }

    pub fn set_installed(&mut self, installed: Installed, cx: &mut Context<Self>) {
        self.installed = installed;
        self.rebuild_search();
        self.refilter(cx);
    }

    pub fn configure(&mut self, config: LaunchConfig, cx: &mut Context<Self>) {
        self.config = config;
        cx.notify();
    }

    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        self.running = running;
        cx.notify();
    }

    pub fn set_input_device(&mut self, device: InputDevice, cx: &mut Context<Self>) {
        if self.input_device != device {
            self.input_device = device;
            cx.notify();
        }
    }

    /// Whether one of the page's text fields has keyboard focus (typed keys are not navigation).
    pub fn input_focused(&self, window: &Window, cx: &App) -> bool {
        [&self.search, &self.address, &self.password].iter().any(|i| i.read(cx).is_focused(window))
    }

    /// A key pressed inside one of the text fields. Returns true when focus should go back to the page.
    pub fn input_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let in_password = self.password.read(cx).is_focused(window);
        let in_address = self.address.read(cx).is_focused(window);
        match key {
            "escape" => true,
            "enter" if in_password => {
                self.join(cx);
                true
            }
            "enter" if in_address => {
                self.add_server(cx);
                true
            }
            "enter" | "down" if !in_password && !in_address => {
                self.focus = Focus::List;
                cx.notify();
                true
            }
            _ => false,
        }
    }

    /// Text search is keyboard-only (DESIGN.md), so a pad's Y cycles the sort instead.
    fn is_pad(&self) -> bool {
        matches!(self.input_device, InputDevice::Xbox | InputDevice::PlayStation)
    }

    fn shows_focus(&self) -> bool {
        self.input_device.shows_focus()
    }

    /// Controller hints for the footer, for the current screen.
    pub fn hints(&self) -> Vec<(NavAction, &'static str)> {
        if self.detail.is_some() {
            return vec![
                (NavAction::Up, "Move"),
                (NavAction::Confirm, "Pick car"),
                (NavAction::Secondary, "Favourite"),
                (NavAction::Search, "Refresh"),
                (NavAction::Primary, "Join"),
                (NavAction::Back, "Back"),
            ];
        }
        let mut hints = vec![(NavAction::SubPrev, "Prev page"), (NavAction::SubNext, "Next page"), (NavAction::Up, "Move"), (NavAction::Confirm, "Open")];
        if !self.visible.is_empty() {
            hints.push((NavAction::Secondary, "Favourite"));
        }
        hints.push((NavAction::Search, if self.is_pad() { "Sort" } else { "Search" }));
        hints.push((NavAction::Primary, "Refresh"));
        hints.push((NavAction::Back, "Back"));
        hints
    }

    /// The footer's main button: label and whether it can be pressed. Only the server page has one.
    pub fn primary(&self, cx: &App) -> Option<(SharedString, bool)> {
        self.detail.as_ref()?;
        if self.running {
            return Some(("Running…".into(), false));
        }
        Some(("Join".into(), self.join_blocker(cx).is_none()))
    }

    // ---------- lobby ----------

    /// Shows the last downloaded list while the fresh one downloads, and pings it meanwhile.
    fn load_cached_lobby(&mut self, cx: &mut Context<Self>) {
        self.cache_tried = true;
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { online::cached_lobby() }).await;
            let _ = this.update(cx, |this, cx| match result {
                // A fresh list that already arrived wins.
                Ok(Some((servers, age))) if this.lobby.is_empty() => {
                    this.lobby = servers;
                    this.lobby_cached = true;
                    this.lobby_at = Instant::now().checked_sub(age);
                    this.rebuild_search();
                    this.refilter(cx);
                    let order = this.lobby_ping_order();
                    this.start_ping(order, false, cx);
                }
                Ok(_) => {}
                // The download replaces the cache, so this only costs the head start.
                Err(e) => this.message = Some((format!("Couldn't read the saved server list: {e:#}"), true)),
            });
        })
        .detach();
    }

    /// Shown rows first, so the ones on screen get a ping within a second, then the rest by drivers.
    fn lobby_ping_order(&self) -> Vec<SocketAddrV4> {
        let mut rest: Vec<&Server> = self.lobby.iter().collect();
        rest.sort_by_key(|s| std::cmp::Reverse(s.clients));
        self.visible.iter().filter_map(|r| self.row_server(*r)).chain(rest).filter_map(Server::udp_addr).collect()
    }

    fn refresh_lobby(&mut self, cx: &mut Context<Self>) {
        if self.lobby_loading {
            return;
        }
        let Some(guid) = self.steam_id.clone() else {
            self.lobby_error = Some("No Steam account found. Start Steam and sign in once; servers identify drivers by their Steam account.".into());
            cx.notify();
            return;
        };
        self.lobby_loading = true;
        self.lobby_error = None;
        self.lobby_generation += 1;
        let generation = self.lobby_generation;
        // The lobby refuses a second request within ~13 s, so wait out the rest of the window.
        let wait = self.lobby_asked.map(|t| online::LOBBY_COOLDOWN.saturating_sub(t.elapsed())).unwrap_or_default();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut wait = wait;
            let mut retried = false;
            let result = loop {
                if !wait.is_zero() {
                    cx.background_executor().timer(wait).await;
                }
                let _ = this.update(cx, |this, _| this.lobby_asked = Some(Instant::now()));
                let guid = guid.clone();
                let result = cx.background_executor().spawn(async move { online::fetch_lobby(&guid) }).await;
                match result {
                    Err(e) if !retried && e.downcast_ref::<LobbyBusy>().is_some() => {
                        retried = true;
                        wait = online::LOBBY_COOLDOWN;
                    }
                    other => break other,
                }
            };
            let _ = this.update(cx, |this, cx| {
                if this.lobby_generation != generation {
                    return;
                }
                this.lobby_loading = false;
                match result {
                    Ok(list) => {
                        this.lobby = list.servers;
                        this.lobby_cached = false;
                        this.lobby_at = Some(Instant::now());
                        if let Some(e) = list.cache_error {
                            this.message = Some((format!("Couldn't save the server list for next time: {e:#}"), true));
                        }
                        this.rebuild_search();
                        this.refilter(cx);
                        // Servers pinged from the cached list keep their ping; only new ones are asked.
                        let order = this.lobby_ping_order();
                        this.start_ping(order, false, cx);
                    }
                    // Keep showing the previous list; the error explains why it is old.
                    Err(e) => {
                        this.lobby_error = Some(format!("{e:#}"));
                        this.refilter(cx);
                    }
                }
            });
        })
        .detach();
    }

    fn rebuild_search(&mut self) {
        let installed = &self.installed;
        self.lobby_search = self
            .lobby
            .iter()
            .map(|s| {
                let mut text = format!("{} {} {}", s.name, s.track, s.cars.join(" "));
                if let Some(t) = installed.track(&s.track) {
                    text.push(' ');
                    text.push_str(&t.name);
                }
                for c in s.cars.iter().filter_map(|c| installed.car(c)) {
                    text.push(' ');
                    text.push_str(&c.name);
                }
                text.to_lowercase()
            })
            .collect();
    }

    /// Pings the targets in order. Without `force`, servers that have a ping or are queued are
    /// skipped; `force` is for a few saved servers, where pinging one twice costs nothing.
    fn start_ping(&mut self, targets: Vec<SocketAddrV4>, force: bool, cx: &mut Context<Self>) {
        let targets: Vec<SocketAddrV4> = targets
            .into_iter()
            .filter(|a| {
                let fresh = self.ping_queued.insert(*a);
                force || (fresh && !self.pings.contains_key(a))
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        match Pinger::start(targets) {
            Ok(p) => self.pingers.push(p),
            Err(e) => {
                self.message = Some((format!("Ping is unavailable: {e:#}"), true));
                return;
            }
        }
        if self.ping_polling {
            return;
        }
        self.ping_polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PING_POLL).await;
                let more = this.update(cx, |this, cx| {
                    for p in &this.pingers {
                        this.pings.extend(p.results());
                    }
                    let before = this.pingers.len();
                    this.pingers.retain(|p| !p.is_done());
                    // Re-sort once a round finishes, not while rows would jump around.
                    if this.pingers.len() < before && this.sort().is_some_and(|(s, _)| s == Sort::Ping) {
                        this.refilter(cx);
                    }
                    cx.notify();
                    this.ping_polling = !this.pingers.is_empty();
                    this.ping_polling
                });
                if !matches!(more, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn ping_of(&self, s: &Server) -> Option<u32> {
        self.pings.get(&s.udp_addr()?).copied()
    }

    // ---------- favourites and recent ----------

    fn saved_list(&self) -> &[online::Saved] {
        match self.source {
            Source::Favourites => &self.store.favourites,
            Source::Recent => &self.store.recent,
            Source::Lobby => &[],
        }
    }

    /// Asks every shown saved server for its state. `force` also re-asks servers that answered.
    fn refresh_saved(&mut self, force: bool, cx: &mut Context<Self>) {
        let addresses: Vec<String> = self
            .saved_list()
            .iter()
            .map(|s| s.address.clone())
            .filter(|a| force || !matches!(self.lookups.get(a), Some(Lookup::Loading | Lookup::Ready(_))))
            .collect();
        if addresses.is_empty() {
            return;
        }
        for a in &addresses {
            self.lookups.insert(a.clone(), Lookup::Loading);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    let handles: Vec<_> = addresses
                        .into_iter()
                        .map(|a| {
                            std::thread::spawn(move || {
                                let r = online::resolve(&a).and_then(|(ip, port)| online::server_info(&ip, port));
                                (a, r.map_err(|e| format!("{e:#}")))
                            })
                        })
                        .collect();
                    handles.into_iter().filter_map(|h| h.join().ok()).collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let mut targets = Vec::new();
                for (address, r) in results {
                    let lookup = match r {
                        Ok(s) => {
                            targets.extend(s.udp_addr());
                            Lookup::Ready(Box::new(s))
                        }
                        Err(e) => Lookup::Failed(e),
                    };
                    this.lookups.insert(address, lookup);
                }
                this.start_ping(targets, force, cx);
                this.refilter(cx);
            });
        })
        .detach();
    }

    fn save_store(&mut self) {
        if let Some(e) = &self.store_error {
            self.message = Some((format!("Favourites aren't saved until online.toml is fixed: {e}"), true));
            return;
        }
        if let Err(e) = self.store.save() {
            self.message = Some((format!("Couldn't save favourites: {e:#}"), true));
        }
    }

    fn toggle_favourite(&mut self, address: String, name: String, cx: &mut Context<Self>) {
        self.store.toggle_favourite(&address, &name);
        self.save_store();
        self.refilter(cx);
    }

    fn toggle_selected_favourite(&mut self, cx: &mut Context<Self>) {
        let Some(&row) = self.visible.get(self.selected) else { return };
        let (address, name) = match row {
            RowRef::Lobby(i) => (self.lobby[i].address(), self.lobby[i].name.clone()),
            RowRef::Saved(i) => {
                let s = &self.saved_list()[i];
                (s.address.clone(), s.name.clone())
            }
        };
        self.toggle_favourite(address, name, cx);
    }

    fn open_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = true;
        self.add_error = None;
        self.address.read(cx).focus(window);
        cx.notify();
    }

    fn add_server(&mut self, cx: &mut Context<Self>) {
        if self.add_busy {
            return;
        }
        let address = self.address.read(cx).text().trim().to_owned();
        if address.is_empty() {
            self.add_error = Some("Type the server's address and HTTP port, for example 203.0.113.5:8081.".into());
            cx.notify();
            return;
        }
        self.add_busy = true;
        self.add_error = None;
        cx.notify();
        let query = address.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { online::resolve(&query).and_then(|(ip, port)| online::server_info(&ip, port)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.add_busy = false;
                match result {
                    Ok(server) => {
                        if !this.store.is_favourite(&address) {
                            this.store.toggle_favourite(&address, &server.name);
                            this.save_store();
                        }
                        this.adding = false;
                        this.address.update(cx, |i, cx| i.set_text("", cx));
                        this.lookups.insert(address.clone(), Lookup::Ready(Box::new(server.clone())));
                        this.refilter(cx);
                        this.open_detail(address, Some(server), cx);
                    }
                    Err(e) => {
                        this.add_error = Some(format!("No server there: {e:#}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    // ---------- list ----------

    fn passes(&self, s: &Server) -> bool {
        Filter::ALL.iter().enumerate().all(|(i, f)| {
            !self.filters[i]
                || match f {
                    Filter::Installed => self.installed.track(&s.track).is_some() && self.installed.has_any_car(s) && self.csp_ok(s),
                    Filter::Players => s.clients > 0,
                    Filter::Free => !s.is_full(),
                    Filter::Open => !s.password,
                }
        })
    }

    fn row_server(&self, row: RowRef) -> Option<&Server> {
        match row {
            RowRef::Lobby(i) => self.lobby.get(i),
            RowRef::Saved(i) => match self.lookups.get(&self.saved_list().get(i)?.address)? {
                Lookup::Ready(s) => Some(s.as_ref()),
                _ => None,
            },
        }
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let q = self.search.read(cx).text().trim().to_lowercase();
        let mut rows: Vec<RowRef> = match self.source {
            Source::Lobby => (0..self.lobby.len())
                .filter(|&i| (q.is_empty() || self.lobby_search[i].contains(&q)) && self.passes(&self.lobby[i]))
                .map(RowRef::Lobby)
                .collect(),
            Source::Favourites | Source::Recent => (0..self.saved_list().len())
                .filter(|&i| {
                    let s = &self.saved_list()[i];
                    q.is_empty()
                        || s.name.to_lowercase().contains(&q)
                        || s.address.contains(&q)
                        || self.row_server(RowRef::Saved(i)).is_some_and(|srv| srv.track.to_lowercase().contains(&q))
                })
                .map(RowRef::Saved)
                .collect(),
        };
        if let Some((key, desc)) = self.sort() {
            rows = self.sorted(rows, key, desc);
        }
        self.visible = rows;
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        cx.notify();
    }

    fn sort(&self) -> Option<(Sort, bool)> {
        self.sorts[self.source.index()]
    }

    fn row_title(&self, row: RowRef) -> String {
        match (self.row_server(row), row) {
            (Some(s), _) => s.name.clone(),
            (None, RowRef::Saved(i)) => self.saved_list()[i].name.clone(),
            (None, RowRef::Lobby(_)) => String::new(),
        }
    }

    /// Rows without the value (no ping yet, server offline) go last in both directions.
    fn sorted(&self, rows: Vec<RowRef>, key: Sort, desc: bool) -> Vec<RowRef> {
        // Keys are computed once per row; search re-sorts on every keystroke.
        let mut keyed: Vec<(RowRef, Option<u32>, Option<u32>, String)> = rows
            .into_iter()
            .map(|r| {
                let s = self.row_server(r);
                let ping = s.and_then(|s| self.ping_of(s));
                let drivers = s.map(|s| s.clients);
                let text = match key {
                    Sort::Name => self.row_title(r).to_lowercase(),
                    Sort::Track => s.map(|s| self.track_name(&s.track).to_lowercase()).unwrap_or_default(),
                    _ => String::new(),
                };
                (r, ping, drivers, text)
            })
            .collect();
        let flip = |o: Ordering| if desc { o.reverse() } else { o };
        keyed.sort_by(|a, b| match key {
            Sort::Ping => missing_last(a.1, b.1, flip),
            // Equal driver counts: the nearer server first.
            Sort::Drivers => missing_last(a.2, b.2, flip).then_with(|| missing_last(a.1, b.1, |o| o)),
            Sort::Name | Sort::Track => match (a.3.is_empty(), b.3.is_empty()) {
                (false, false) => flip(content::natural_cmp(&a.3, &b.3)),
                (x, y) => x.cmp(&y),
            },
        });
        keyed.into_iter().map(|k| k.0).collect()
    }

    /// A column header was pressed: the same column flips direction, another starts at its default.
    fn click_sort(&mut self, key: Sort, cx: &mut Context<Self>) {
        let slot = &mut self.sorts[self.source.index()];
        *slot = Some(match *slot {
            Some((k, desc)) if k == key => (key, !desc),
            _ => (key, key.default_desc()),
        });
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.refilter(cx);
    }

    fn bar_items(&self) -> Vec<BarItem> {
        let mut items = Vec::new();
        if self.source == Source::Lobby {
            items.extend(Filter::ALL.map(BarItem::Filter));
        }
        items.push(BarItem::Refresh);
        if self.source == Source::Favourites {
            items.push(BarItem::AddServer);
        }
        items
    }

    fn activate(&mut self, item: BarItem, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            BarItem::Filter(f) => {
                let i = Filter::ALL.iter().position(|x| *x == f).unwrap_or(0);
                self.filters[i] = !self.filters[i];
                self.selected = 0;
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                self.refilter(cx);
            }
            BarItem::Refresh => self.refresh(cx),
            BarItem::AddServer => self.open_add(window, cx),
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        match self.source {
            Source::Lobby => self.refresh_lobby(cx),
            Source::Favourites | Source::Recent => self.refresh_saved(true, cx),
        }
    }

    fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.filters = [false; 4];
        self.search.update(cx, |i, cx| i.set_text("", cx));
        self.refilter(cx);
    }

    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let Some(&row) = self.visible.get(self.selected) else { return };
        let (address, server) = match row {
            RowRef::Lobby(i) => (self.lobby[i].address(), Some(self.lobby[i].clone())),
            RowRef::Saved(i) => (self.saved_list()[i].address.clone(), self.row_server(row).cloned()),
        };
        self.open_detail(address, server, cx);
    }

    // ---------- server page ----------

    fn open_detail(&mut self, address: String, seed: Option<Server>, cx: &mut Context<Self>) {
        self.message = None;
        let password = self.passwords.get(&address).cloned().unwrap_or_default();
        self.password.update(cx, |i, cx| i.set_text(password, cx));
        self.detail_generation += 1;
        self.detail = Some(Detail {
            address,
            server: seed,
            slots: None,
            details: None,
            error: None,
            loading: false,
            car: None,
            focus: 0,
            generation: self.detail_generation,
        });
        self.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.load_detail(cx);
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.detail = None;
        self.detail_generation += 1;
        self.message = None;
        self.refilter(cx);
    }

    fn load_detail(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.detail.as_mut() else { return };
        let Some(guid) = self.steam_id.clone() else {
            d.error = Some("No Steam account found. Start Steam and sign in once.".into());
            cx.notify();
            return;
        };
        d.loading = true;
        d.error = None;
        let generation = d.generation;
        let address = d.address.clone();
        // Known from the lobby row; lets the extended info load alongside /INFO.
        let seed_port = d.server.as_ref().map(Server::details_port);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let (ip, port) = online::resolve(&address)?;
                    let (ip, guid) = (&ip, &guid);
                    // Three requests to the same server, side by side instead of one after another.
                    std::thread::scope(|scope| {
                        let slots = scope.spawn(move || online::car_slots(ip, port, guid).map_err(|e| format!("{e:#}")));
                        // Vanilla servers have no extended info; that is not an error.
                        let early = seed_port.map(|p| (p, scope.spawn(move || online::details(ip, p, guid).ok())));
                        let server = online::server_info(ip, port)?;
                        let details = match early {
                            Some((p, h)) if p == server.details_port() => h.join().ok().flatten(),
                            _ => online::details(ip, server.details_port(), guid).ok(),
                        };
                        let slots = slots.join().unwrap_or_else(|_| Err("the car list request crashed".into()));
                        anyhow::Ok((server, slots, details))
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(d) = this.detail.as_mut().filter(|d| d.generation == generation) else { return };
                d.loading = false;
                match result {
                    Ok((server, slots, details)) => {
                        d.server = Some(server);
                        d.slots = Some(slots);
                        d.details = details;
                    }
                    Err(e) => d.error = Some(format!("The server didn't answer ({e:#}). It may be offline; try Refresh.")),
                }
                this.pick_default_car();
                cx.notify();
            });
        })
        .detach();
    }

    fn car_choices(&self) -> Vec<CarChoice> {
        let Some(d) = &self.detail else { return Vec::new() };
        let Some(s) = &d.server else { return Vec::new() };
        let slots: &[CarSlot] = match &d.slots {
            Some(Ok(slots)) => slots,
            _ => &[],
        };
        s.cars
            .iter()
            .map(|id| {
                let of: Vec<&CarSlot> = slots.iter().filter(|c| c.model.eq_ignore_ascii_case(id)).collect();
                let free_skin = of.iter().find(|c| !c.connected).map(|c| c.skin.as_str());
                let car = self.installed.car(id);
                let preview = car.and_then(|c| {
                    let by_skin = free_skin.and_then(|k| c.skins.iter().find(|(s, _)| s.eq_ignore_ascii_case(k)));
                    by_skin.or(c.skins.first()).and_then(|(_, p)| p.clone())
                });
                CarChoice {
                    id: id.clone(),
                    name: car.map(|c| c.name.clone()).unwrap_or_else(|| id.clone()),
                    installed: car.is_some(),
                    free: of.iter().filter(|c| !c.connected).count(),
                    // Without the entry list, assume the car can be tried.
                    total: if d.slots.as_ref().is_some_and(|r| r.is_ok()) { of.len() } else { usize::MAX },
                    preview,
                    about: car.map(|c| c.about.clone()).unwrap_or_default(),
                    specs: car.map(|c| c.specs.clone()).unwrap_or_default(),
                }
            })
            .collect()
    }

    fn choice_available(c: &CarChoice) -> bool {
        c.installed && (c.total == usize::MAX || c.free > 0)
    }

    /// Keeps the chosen car when it still has room; else the Quick Drive car, else the first that fits.
    fn pick_default_car(&mut self) {
        let choices = self.car_choices();
        let keep = |id: &str| choices.iter().any(|c| c.id.eq_ignore_ascii_case(id) && Self::choice_available(c));
        let Some(d) = self.detail.as_mut() else { return };
        if d.car.as_deref().is_some_and(keep) {
            return;
        }
        let base = self.config.base.car_id.clone();
        d.car = choices
            .iter()
            .find(|c| c.id.eq_ignore_ascii_case(&base) && Self::choice_available(c))
            .or_else(|| choices.iter().find(|c| Self::choice_available(c)))
            .map(|c| c.id.clone());
    }

    fn needs_password(&self) -> bool {
        self.detail.as_ref().and_then(|d| d.server.as_ref()).is_some_and(|s| s.password)
    }

    /// Why Join can't be pressed right now, in words for the page.
    fn join_blocker(&self, cx: &App) -> Option<String> {
        let d = self.detail.as_ref()?;
        if self.running {
            return Some("The game is running.".into());
        }
        if self.steam_id.is_none() {
            return Some("Start Steam and sign in; servers identify drivers by their Steam account.".into());
        }
        if self.config.driver.trim().is_empty() {
            return Some("Set your driver name in Settings → App first.".into());
        }
        let Some(s) = &d.server else {
            return Some(if d.loading { "Asking the server…".into() } else { "The server didn't answer.".into() });
        };
        if !s.pickup {
            return Some("This server uses booking, which cm-rs can't do yet.".into());
        }
        if self.installed.track(&s.track).is_none() {
            return Some(format!("Install the track {} to join. Get it from Mods → Get mods.", s.track));
        }
        if !self.installed.has_any_car(s) {
            return Some("None of this server's cars are installed.".into());
        }
        if let Some(problem) = self.csp_problem(s) {
            return Some(problem);
        }
        let choices = self.car_choices();
        let Some(car) = d.car.as_ref().and_then(|id| choices.iter().find(|c| &c.id == id)) else {
            return Some("Every installed car is taken. Try again later.".into());
        };
        if !Self::choice_available(car) {
            return Some(format!("No free {} on this server.", car.name));
        }
        if s.password && self.password.read(cx).text().is_empty() {
            return Some("This server needs a password.".into());
        }
        None
    }

    fn select_car(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(d) = self.detail.as_mut() {
            d.car = Some(id);
        }
        cx.notify();
    }

    fn toggle_detail_favourite(&mut self, cx: &mut Context<Self>) {
        let Some(d) = &self.detail else { return };
        let name = d.server.as_ref().map(|s| s.name.clone()).unwrap_or_else(|| d.address.clone());
        self.toggle_favourite(d.address.clone(), name, cx);
    }

    pub fn join(&mut self, cx: &mut Context<Self>) {
        if let Some(reason) = self.join_blocker(cx) {
            self.message = Some((reason, true));
            cx.notify();
            return;
        }
        let (Some(d), Some(guid)) = (self.detail.as_ref(), self.steam_id.clone()) else { return };
        let (Some(s), Some(car_id)) = (d.server.clone(), d.car.clone()) else { return };
        let (Some(track), Some(car)) = (self.installed.track(&s.track), self.installed.car(&car_id)) else { return };
        let address = d.address.clone();
        let free_skin = match &d.slots {
            Some(Ok(slots)) => slots.iter().find(|c| c.model.eq_ignore_ascii_case(&car_id) && !c.connected).map(|c| c.skin.clone()),
            _ => None,
        };
        let skin = free_skin
            .and_then(|k| car.skins.iter().find(|(s, _)| s.eq_ignore_ascii_case(&k)))
            .or(car.skins.first())
            .map(|(s, _)| s.clone())
            .unwrap_or_default();
        let mut q = online::quick_drive_for(&self.config.base, &car.id, &skin, &track.track_id, track.layout_id.as_deref());
        q.driver_name = self.config.driver.clone();
        q.nationality = self.config.nationality.clone();
        let password = if s.password { self.password.read(cx).text() } else { String::new() };
        let join = online::Join {
            server_ip: s.ip.clone(),
            tcp_port: s.tcp_port,
            http_port: s.http_port,
            server_name: s.name.clone(),
            // The server's own spelling, which its entry list matches.
            car: car_id,
            driver_name: self.config.driver.clone(),
            guid,
            password: password.clone(),
            features: d.details.as_ref().map(|x| x.features.clone()).unwrap_or_default(),
        };
        let root = self.root.clone();
        if let Err(e) = launch::write_online_race_ini(&root, &q, &join) {
            self.message = Some((format!("Couldn't write race.ini: {e:#}"), true));
            cx.notify();
            return;
        }
        if s.password {
            self.passwords.insert(address.clone(), password);
        }
        let since = SystemTime::now();
        let opts = launch::LaunchOptions { proton: self.config.proton.clone(), input: self.config.input.clone() };
        let mut child = match launch::start(&root, &opts) {
            Ok(child) => child,
            Err(e) => {
                let _ = online::clear_remote(&root);
                self.message = Some((format!("Couldn't start the game: {e:#}"), true));
                cx.notify();
                return;
            }
        };
        self.running = true;
        self.message = Some((format!("Joining {}…", s.name), false));
        cx.emit(Running(true));
        cx.notify();
        let name = s.name.clone();
        cx.spawn(async move |this, cx| {
            let (exit, outcome, cleared) = cx
                .background_executor()
                .spawn(async move {
                    let exit = child.wait();
                    let outcome = online::join_outcome(&root, since);
                    let cleared = online::clear_remote(&root);
                    (exit, outcome, cleared)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.running = false;
                cx.emit(Running(false));
                let joined = matches!(outcome, Some(JoinOutcome::Connected));
                this.message = Some(match (&outcome, exit) {
                    (Some(JoinOutcome::Connected), _) => (format!("Back from {name}."), false),
                    (Some(o), _) => (o.message(), true),
                    (None, Err(e)) => (format!("Couldn't wait for the game: {e}"), true),
                    (None, Ok(_)) => ("The game closed without writing a log, so cm-rs can't tell whether it connected.".into(), true),
                });
                if let Err(e) = cleared {
                    this.message = Some((format!("Couldn't reset race.ini after the session: {e:#}"), true));
                }
                if joined || outcome.is_none() {
                    this.store.push_recent(&address, &name);
                    this.save_store();
                }
                if matches!(outcome, Some(JoinOutcome::WrongPassword)) {
                    this.passwords.remove(&address);
                    if let Some(d) = this.detail.as_mut() {
                        d.focus = 0;
                    }
                }
                // Slots and session changed while driving.
                this.load_detail(cx);
            });
        })
        .detach();
    }

    // ---------- pad ----------

    /// One controller press, already mapped from keys. Returns false to let the app handle it.
    pub fn pad(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.detail.is_some() {
            return self.detail_pad(name, window, cx);
        }
        if self.adding && name == "B" {
            self.adding = false;
            cx.notify();
            return true;
        }
        let items = self.bar_items();
        if self.visible.is_empty() && matches!(self.focus, Focus::List | Focus::Header(_)) {
            self.focus = Focus::Bar(0);
        }
        // Favourites and Recent have fewer bar items than the lobby.
        if let Focus::Bar(i) = self.focus {
            self.focus = Focus::Bar(i.min(items.len().saturating_sub(1)));
        }
        let handled = match (name, self.focus) {
            ("START", _) => {
                self.refresh(cx);
                true
            }
            ("Y", _) => {
                if !self.is_pad() {
                    self.search.read(cx).focus(window);
                } else {
                    // Next column at its default direction; A on a header flips it.
                    let at = self.sort().and_then(|(k, _)| Sort::ALL.iter().position(|s| *s == k));
                    let next = Sort::ALL[at.map_or(0, |i| (i + 1) % Sort::ALL.len())];
                    self.sorts[self.source.index()] = Some((next, next.default_desc()));
                    self.selected = 0;
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    self.refilter(cx);
                }
                true
            }
            ("DPAD_LEFT", Focus::Header(i)) => {
                self.focus = Focus::Header(i.saturating_sub(1));
                true
            }
            ("DPAD_RIGHT", Focus::Header(i)) => {
                self.focus = Focus::Header((i + 1).min(Sort::ALL.len() - 1));
                true
            }
            ("DPAD_UP", Focus::Header(_)) => {
                self.focus = Focus::Bar(0);
                true
            }
            ("DPAD_DOWN", Focus::Header(_)) => {
                self.focus = Focus::List;
                self.scroll.scroll_to_item(self.selected, ScrollStrategy::Center);
                true
            }
            ("A", Focus::Header(i)) => {
                self.click_sort(Sort::ALL[i], cx);
                true
            }
            ("DPAD_LEFT", Focus::Bar(i)) => {
                self.focus = Focus::Bar(i.saturating_sub(1));
                true
            }
            ("DPAD_RIGHT", Focus::Bar(i)) => {
                self.focus = Focus::Bar((i + 1).min(items.len().saturating_sub(1)));
                true
            }
            ("DPAD_DOWN", Focus::Bar(_)) => {
                // The header row only shows above rows.
                if !self.visible.is_empty() {
                    self.focus = Focus::Header(self.header_focus());
                }
                true
            }
            ("A", Focus::Bar(i)) => {
                if let Some(&item) = items.get(i) {
                    self.activate(item, window, cx);
                }
                true
            }
            ("DPAD_UP", Focus::Bar(_)) => true,
            ("DPAD_UP", Focus::List) => {
                if self.selected == 0 {
                    self.focus = Focus::Header(self.header_focus());
                } else {
                    self.selected -= 1;
                    self.scroll.scroll_to_item(self.selected, ScrollStrategy::Center);
                }
                true
            }
            ("DPAD_DOWN", Focus::List) => {
                if self.selected + 1 < self.visible.len() {
                    self.selected += 1;
                    self.scroll.scroll_to_item(self.selected, ScrollStrategy::Center);
                }
                true
            }
            ("A", Focus::List) => {
                self.open_selected(cx);
                true
            }
            ("X", Focus::List) => {
                self.toggle_selected_favourite(cx);
                true
            }
            _ => false,
        };
        if handled {
            cx.notify();
        }
        handled
    }

    /// Server page focus stops: the password field when needed (0), the car view toggle, then the cars.
    fn detail_stops(&self) -> (bool, usize, usize) {
        let password = self.needs_password();
        let toggle = usize::from(password);
        (password, toggle, toggle + 1)
    }

    fn car_columns(&self) -> usize {
        match self.car_view {
            CarView::Cards => self.car_cols.max(1),
            CarView::Details => 1,
        }
    }

    fn set_car_view(&mut self, view: CarView, cx: &mut Context<Self>) {
        self.car_view = view;
        cx.notify();
    }

    fn detail_pad(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (password, toggle, offset) = self.detail_stops();
        let cars = self.car_choices();
        let n = offset + cars.len();
        let cols = self.car_columns();
        let view = self.car_view;
        let Some(d) = self.detail.as_mut() else { return false };
        d.focus = d.focus.min(n.saturating_sub(1));
        let at = d.focus;
        let car_at = at.checked_sub(offset);
        let mut new_view = None;
        match name {
            "B" => self.close_detail(cx),
            "START" => self.join(cx),
            "X" => self.toggle_detail_favourite(cx),
            "Y" => self.load_detail(cx),
            "DPAD_LEFT" if at == toggle => new_view = Some(CarView::Cards),
            "DPAD_RIGHT" if at == toggle => new_view = Some(CarView::Details),
            "DPAD_LEFT" => {
                if let Some(c) = car_at.filter(|c| c % cols > 0) {
                    d.focus = offset + c - 1;
                }
            }
            "DPAD_RIGHT" => {
                if let Some(c) = car_at.filter(|c| c % cols + 1 < cols && c + 1 < cars.len()) {
                    d.focus = offset + c + 1;
                }
            }
            "DPAD_UP" => match car_at {
                Some(c) if c >= cols => d.focus = offset + c - cols,
                Some(_) => d.focus = toggle,
                None if at == toggle && password => d.focus = 0,
                None => {}
            },
            "DPAD_DOWN" => match car_at {
                None if at < toggle => d.focus = toggle,
                None if !cars.is_empty() => d.focus = offset,
                Some(c) if c + cols < cars.len() => d.focus = offset + c + cols,
                // Last row: drop to the last card so Down always moves when there is a row below.
                Some(c) if (c / cols + 1) * cols < cars.len() => d.focus = offset + cars.len() - 1,
                _ => {}
            },
            "A" => match car_at {
                None if password && at == 0 => self.password.read(cx).focus(window),
                None => {
                    new_view = Some(match view {
                        CarView::Cards => CarView::Details,
                        CarView::Details => CarView::Cards,
                    })
                }
                Some(c) => {
                    if let Some(choice) = cars.get(c).filter(|c| Self::choice_available(c)) {
                        let id = choice.id.clone();
                        self.select_car(id, cx);
                    }
                }
            },
            _ => return false,
        }
        if let Some(v) = new_view {
            self.set_car_view(v, cx);
        }
        if let Some(d) = &self.detail {
            let cols = self.car_columns();
            let child = match d.focus.checked_sub(offset) {
                Some(c) => self.cars_child + c / cols,
                None if d.focus == toggle => self.view_child,
                None => self.password_child,
            };
            self.detail_scroll.scroll_to_item(child);
        }
        cx.notify();
        true
    }

    /// Header focus starts on the sorted column.
    fn header_focus(&self) -> usize {
        self.sort().and_then(|(k, _)| Sort::ALL.iter().position(|s| *s == k)).unwrap_or(0)
    }

    // ---------- rendering ----------

    fn render_row(&self, vis: usize, cols: &Cols, cx: &mut Context<Self>) -> AnyElement {
        let row = self.visible[vis];
        let focused = self.shows_focus() && self.focus == Focus::List && self.selected == vis;
        let (address, saved_name) = match row {
            RowRef::Lobby(i) => (self.lobby[i].address(), None),
            RowRef::Saved(i) => {
                let s = &self.saved_list()[i];
                (s.address.clone(), Some(s.name.clone()))
            }
        };
        let favourite = self.store.is_favourite(&address);
        let lookup = match row {
            RowRef::Saved(_) => self.lookups.get(&address),
            RowRef::Lobby(_) => None,
        };
        let server = self.row_server(row);
        let title: SharedString = server.map(|s| s.name.clone()).or(saved_name).unwrap_or_else(|| address.clone()).into();

        let star = {
            let (address, name) = (address.clone(), title.to_string());
            div()
                .id(("online-star", vis))
                .w(px(cols.star))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .text_lg()
                .cursor_pointer()
                .text_color(if favourite { theme::accent() } else { theme::faint() })
                .hover(|s| s.text_color(theme::accent()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    // The row behind opens the server; the star only toggles.
                    cx.stop_propagation();
                    this.toggle_favourite(address.clone(), name.clone(), cx);
                }))
                .child(if favourite { "★" } else { "☆" })
        };

        let ping = server.and_then(|s| self.ping_of(s));
        let pinging = !self.pingers.is_empty();
        let ping_el = div()
            .w(px(cols.ping))
            .flex_shrink_0()
            .text_sm()
            .text_right()
            .text_color(match ping {
                Some(ms) if ms <= 80 => theme::ok(),
                Some(ms) if ms <= 150 => theme::text(),
                _ => theme::muted(),
            })
            .child(match ping {
                Some(ms) => format!("{ms} ms"),
                None if pinging && server.is_some() => "…".into(),
                None => "—".into(),
            });

        let (sub, sub_warn): (String, bool) = match (server, lookup) {
            (Some(s), _) => {
                let mut sub = session_text(s);
                if !s.country.is_empty() && !s.country.eq_ignore_ascii_case("na") {
                    sub.push_str(" · ");
                    sub.push_str(&s.country);
                }
                (sub, false)
            }
            (None, Some(Lookup::Failed(e))) => (format!("Offline · {e}"), true),
            _ => (format!("{address} · asking the server…"), false),
        };
        let name_el = two_lines(cols.name, title, sub, sub_warn, true);

        let installed_track = server.and_then(|s| self.installed.track(&s.track));
        let track_el = match server {
            Some(s) => {
                let (line2, warn) = match installed_track {
                    Some(t) => (t.length.as_deref().map(crate::app::format_track_length).unwrap_or_default(), false),
                    None => ("Not installed".to_owned(), true),
                };
                two_lines(cols.track, self.track_name(&s.track).into(), line2, warn, false)
            }
            None => div().w(px(cols.track)).flex_shrink_0(),
        };

        let cars_el = match server {
            Some(s) => {
                // One name plus a count: GPUI clipped two names without an ellipsis.
                let first = s.cars.first().map(|c| self.installed.car(c).map(|x| x.name.clone()).unwrap_or_else(|| c.clone())).unwrap_or_default();
                let mut text: String = first.chars().take(30).collect();
                if first.chars().count() > 30 {
                    text.push('…');
                }
                let more = s.cars.len().saturating_sub(1);
                if more > 0 {
                    text.push_str(&format!(" +{more}"));
                }
                let (line2, warn) = if !self.installed.has_any_car(s) {
                    ("Not installed".to_owned(), true)
                } else if !self.csp_ok(s) {
                    (format!("Needs CSP {}+", s.required_csp.unwrap_or(0)), true)
                } else {
                    let have = s.cars.iter().filter(|c| self.installed.car(c).is_some()).count();
                    (if have == s.cars.len() { String::new() } else { format!("{have} of {} installed", s.cars.len()) }, false)
                };
                two_lines(cols.cars, text.into(), line2, warn, false)
            }
            None => div().w(px(cols.cars)).flex_shrink_0(),
        };

        let drivers_el = div()
            .w(px(cols.drivers))
            .flex_shrink_0()
            .text_sm()
            .text_right()
            .text_color(if server.is_some_and(|s| s.clients > 0) { theme::text() } else { theme::muted() })
            .child(server.map(|s| format!("{}/{}", s.clients, s.max_clients)).unwrap_or_default());

        let mut badges = div().w(px(cols.info)).flex_shrink_0().flex().items_center().justify_end().gap_1();
        if let Some(s) = server {
            if s.password {
                badges = badges.child(badge("Password"));
            }
            if !s.pickup {
                badges = badges.child(badge("Booking"));
            }
        }

        let el = div()
            .id(("online-row", vis))
            .h(px(ROW_H))
            .w_full()
            .px_3()
            .flex()
            .items_center()
            .gap(px(GAP))
            .rounded(px(theme::RADIUS_SM))
            .cursor_pointer()
            .hover(|s| s.bg(theme::panel_hover()))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selected = vis;
                this.focus = Focus::List;
                this.open_selected(cx);
            }))
            .child(star)
            .child(ping_el)
            .child(name_el)
            .child(track_el)
            .child(cars_el)
            .child(drivers_el)
            .child(badges);
        focus_ring(el, focused).into_any_element()
    }

    /// Column titles above the rows; the sortable ones sort on click and show the direction.
    fn header_row(&self, cols: &Cols, cx: &mut Context<Self>) -> gpui::Div {
        let sort = self.sort();
        let label = |text: &'static str, w: f32, right: bool| {
            div().w(px(w)).flex_shrink_0().flex().when(right, |d| d.justify_end()).text_xs().text_color(theme::faint()).child(text.to_uppercase())
        };
        let mut row = div().h(px(36.)).px_3().flex().items_center().gap(px(GAP)).border_b_1().border_color(theme::border());
        row = row.child(div().w(px(cols.star)).flex_shrink_0());
        for (i, key) in Sort::ALL.into_iter().enumerate() {
            let active = sort.filter(|(k, _)| *k == key);
            let right = matches!(key, Sort::Ping | Sort::Drivers);
            let arrow = active.map(|(_, desc)| if desc { " ↓" } else { " ↑" }).unwrap_or("");
            let cell = div()
                .id(("online-col", i))
                .w(px(cols.of(key)))
                .flex_shrink_0()
                .flex()
                .when(right, |d| d.justify_end())
                .rounded(px(theme::RADIUS_SM))
                .cursor_pointer()
                .text_xs()
                .font_weight(if active.is_some() { FontWeight::BOLD } else { FontWeight::NORMAL })
                .text_color(if active.is_some() { theme::accent() } else { theme::muted() })
                .hover(|s| s.bg(theme::panel_hover()).text_color(theme::text()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.focus = Focus::Header(i);
                    this.click_sort(key, cx);
                }))
                .child(format!("{}{arrow}", key.label().to_uppercase()));
            let focused = self.shows_focus() && self.focus == Focus::Header(i);
            row = row.child(focus_ring(cell, focused));
            if key == Sort::Track {
                row = row.child(label("Cars", cols.cars, false));
            }
        }
        row.child(label("", cols.info, true))
    }

    fn track_name(&self, id: &str) -> String {
        self.installed.track(id).map(|t| t.name.clone()).unwrap_or_else(|| id.to_owned())
    }

    fn bar(&self, cx: &mut Context<Self>) -> gpui::Div {
        let items = self.bar_items();
        let focused = |i: usize| self.shows_focus() && self.focus == Focus::Bar(i);
        let mut filters = div().flex().flex_wrap().items_center().gap_2();
        let mut actions = div().flex().items_center().gap_2().ml_auto();
        if items.iter().any(|i| matches!(i, BarItem::Filter(_))) {
            filters = filters.child(div().mr_1().text_xs().text_color(theme::faint()).child("SHOW ONLY"));
        }
        for (i, item) in items.iter().enumerate() {
            let el: Stateful<gpui::Div> = match *item {
                BarItem::Filter(f) => {
                    let on = self.filters[Filter::ALL.iter().position(|x| *x == f).unwrap_or(0)];
                    filter_chip(("online-filter", i), f.label(), on)
                }
                BarItem::Refresh => {
                    let busy = match self.source {
                        Source::Lobby => self.lobby_loading,
                        _ => self.lookups.values().any(|l| matches!(l, Lookup::Loading)),
                    };
                    button("online-refresh", if busy { "Refreshing…" } else { "Refresh" }).when(busy, |d| d.opacity(0.6))
                }
                BarItem::AddServer => button("online-add", "Add server"),
            };
            let item = *item;
            let el = focus_ring(el, focused(i)).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.focus = Focus::Bar(i);
                this.activate(item, window, cx);
            }));
            match item {
                BarItem::Filter(_) => filters = filters.child(el),
                _ => actions = actions.child(el),
            }
        }
        div().flex().items_center().gap_3().child(filters).child(actions)
    }

    fn summary(&self) -> String {
        match self.source {
            Source::Lobby => {
                if self.lobby.is_empty() {
                    return if self.lobby_loading { "Downloading the server list…".into() } else { String::new() };
                }
                let with_drivers = self.lobby.iter().filter(|s| s.clients > 0).count();
                let mut text = format!(
                    "{} servers · {} with drivers · {} shown",
                    thousands(self.lobby.len()),
                    thousands(with_drivers),
                    thousands(self.visible.len())
                );
                if let Some(t) = self.lobby_at {
                    text.push_str(&format!(" · updated {}", ago(t)));
                }
                if self.lobby_loading {
                    text.push_str(" · refreshing…");
                }
                text
            }
            Source::Favourites => format!("{} favourite servers", self.store.favourites.len()),
            Source::Recent => format!("{} servers you joined", self.store.recent.len()),
        }
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let note = |title: &str, body: String| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .py_8()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(title.to_owned()))
                .child(div().max_w(px(560.)).text_sm().text_center().text_color(theme::muted()).child(body))
        };
        let el = match self.source {
            Source::Lobby if self.lobby.is_empty() && self.lobby_loading => {
                note("Loading servers", "Downloading the list from the Kunos lobby. This takes a few seconds.".into())
            }
            Source::Lobby if self.lobby.is_empty() => {
                let error = self.lobby_error.clone().unwrap_or_default();
                note("Couldn't load servers", error).child(button("online-retry", "Try again").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.refresh_lobby(cx))))
            }
            Source::Lobby => note("No servers match", "Turn off a filter or clear the search.".into())
                .child(button("online-clear", "Clear filters").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear_filters(cx)))),
            Source::Favourites if self.store.favourites.is_empty() => note(
                "No favourite servers yet",
                "Press X on a server to add it here, or use Add server with its address.".into(),
            ),
            Source::Recent if self.store.recent.is_empty() => note("No recent servers", "Servers you join show up here.".into()),
            _ => note("No servers match", "Clear the search to see them all.".into()),
        };
        el.into_any_element()
    }

    fn add_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        panel()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Add a server by address"))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::muted())
                    .child("Use the server's HTTP port, the one in its join link or /INFO address."),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(360.)).child(self.address.clone()))
                    .child(
                        button("online-add-go", if self.add_busy { "Checking…" } else { "Add" })
                            .when(self.add_busy, |d| d.opacity(0.6))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.add_server(cx))),
                    )
                    .child(button("online-add-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.adding = false;
                        this.add_error = None;
                        cx.notify();
                    }))),
            )
            .when_some(self.add_error.clone(), |d, e| d.child(div().text_sm().text_color(theme::error()).child(e)))
    }

    fn render_list(&mut self, col_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let title = match self.source {
            Source::Lobby => "Servers",
            Source::Favourites => "Favourites",
            Source::Recent => "Recent",
        };
        let header = panel()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xl().font_weight(FontWeight::BOLD).child(title))
                            .child(div().text_sm().text_color(theme::muted()).child(self.summary())),
                    )
                    .child(div().w(px(320.)).flex_shrink_0().child(self.search.clone())),
            )
            .child(self.bar(cx));

        let notices = [
            self.message.clone(),
            // A refresh that failed while an older list is still shown.
            self.lobby_error.clone().filter(|_| self.source == Source::Lobby && !self.lobby.is_empty()).map(|e| (format!("Couldn't refresh: {e}"), true)),
            self.store_error.clone().map(|e| (format!("online.toml can't be read, so favourites won't be saved: {e}"), true)),
        ];

        let body = if self.visible.is_empty() {
            self.empty_state(cx)
        } else {
            // Panel padding and border, and the rows' 2 px focus border on each side.
            let row_w = col_w - 14.;
            let list = uniform_list(
                "online-servers",
                self.visible.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                    let cols = Cols::new(row_w);
                    range.map(|vis| this.render_row(vis, &cols, cx)).collect()
                }),
            )
            .track_scroll(self.scroll.clone())
            .flex_1()
            .size_full();
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.header_row(&Cols::new(row_w), cx).mx(px(2.)))
                .child(list)
                .into_any_element()
        };

        div()
            .w(px(col_w))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .when(self.adding && self.source == Source::Favourites, |d| d.child(self.add_panel(cx)))
            .children(notices.into_iter().flatten().map(|(text, warn)| notice(text, warn)))
            .child(panel().p_1().flex_1().min_h_0().flex().flex_col().child(body))
            .into_any_element()
    }

    fn render_detail(&mut self, col_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(d) = &self.detail else { return div().into_any_element() };
        let server = d.server.as_ref();
        let favourite = self.store.is_favourite(&d.address);
        let track = server.and_then(|s| self.installed.track(&s.track));
        let (_, toggle, offset) = self.detail_stops();
        let focus = d.focus;
        let show_focus = self.shows_focus();

        // Hero: track picture, name, facts, badges, description.
        let pic_w = 300.;
        let picture_el = match track {
            Some(t) if t.preview.is_some() => picture(t.preview.clone(), 168.).w(px(pic_w)),
            Some(t) => div().w(px(pic_w)).flex().justify_center().child(track_map(t.racing_line.clone(), t.outline.clone(), 168.)),
            None => div()
                .w(px(pic_w))
                .h(px(168.))
                .flex_shrink_0()
                .rounded(px(theme::RADIUS_SM))
                .bg(theme::bg())
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme::muted())
                .child(if server.is_some() { "Track not installed" } else { "" }),
        };
        let name: SharedString = server.map(|s| s.name.clone()).unwrap_or_else(|| d.address.clone()).into();
        let mut facts: Vec<String> = Vec::new();
        if let Some(s) = server {
            let mut place = self.track_name(&s.track);
            if let Some(len) = track.and_then(|t| t.length.as_deref()).map(crate::app::format_track_length).filter(|l| !l.is_empty()) {
                place.push_str(&format!(" · {len}"));
            }
            facts.push(place);
            facts.push(format!("{} · {}/{} drivers", session_text(s), s.clients, s.max_clients));
            let mut net = Vec::new();
            if let Some(ms) = self.ping_of(s) {
                net.push(format!("{ms} ms"));
            }
            // Servers without a GeoIP match report ["na", "na"].
            if !s.country.is_empty() && !s.country.eq_ignore_ascii_case("na") {
                net.push(s.country.clone());
            }
            net.push(d.address.clone());
            facts.push(net.join(" · "));
        } else if d.loading {
            facts.push("Asking the server…".into());
        }
        let conditions = d.details.as_ref().map(conditions_text).filter(|t| !t.is_empty());
        let description = d.details.as_ref().map(|x| x.description.clone()).filter(|t| !t.is_empty());

        let mut badges = div().flex().flex_wrap().gap_1();
        if let Some(s) = server {
            if s.password {
                badges = badges.child(badge("Password"));
            }
            if !s.pickup {
                badges = badges.child(badge("Booking"));
            }
            if let Some(p) = &s.powered_by {
                badges = badges.child(badge_owned(p.clone()));
            }
        }
        let hero = panel()
            .flex()
            .gap_4()
            .child(picture_el)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xl().font_weight(FontWeight::BOLD).child(name))
                    .children(facts.into_iter().map(|f| div().text_sm().text_color(theme::muted()).child(f)))
                    .when_some(conditions, |el, c| el.child(div().text_sm().child(c)))
                    .child(badges.mt_1())
                    .when_some(description, |el, t| el.child(div().mt_2().max_h(px(96.)).overflow_hidden().text_xs().text_color(theme::muted()).child(t))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .flex_shrink_0()
                    .child(button("online-back", "Back").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_detail(cx))))
                    .child(
                        button("online-fav", if favourite { "★" } else { "☆" })
                            .justify_center()
                            .text_lg()
                            .text_color(if favourite { theme::accent() } else { theme::muted() })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_detail_favourite(cx))),
                    )
                    .child(
                        button("online-reload", if d.loading { "Refreshing…" } else { "Refresh" })
                            .when(d.loading, |b| b.opacity(0.6))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.load_detail(cx))),
                    ),
            );

        // Problems first (amber), then how the last join went, then what blocks Join.
        let mut notices: Vec<(String, bool)> = Vec::new();
        if let Some(e) = &d.error {
            notices.push((e.clone(), true));
        }
        if let Some(Err(e)) = &d.slots {
            notices.push((format!("The server didn't send its car list ({e}). Free slots are unknown."), true));
        }
        if let Some(m) = self.message.clone() {
            notices.push(m);
        }
        let blocker = self.join_blocker(cx);
        // A failed lookup already has its notice; repeating "didn't answer" below it adds nothing.
        let blocker_shown =
            notices.iter().any(|(t, _)| Some(t) == blocker.as_ref()) || (d.server.is_none() && d.error.is_some());
        let reachable = d.server.is_some() || d.loading;

        let password_row = self.needs_password().then(|| {
            let focused = show_focus && focus == 0;
            let el = div()
                .id("online-password")
                .flex()
                .items_center()
                .gap_3()
                .p_2()
                .rounded(px(theme::RADIUS_SM))
                .child(div().w(px(320.)).child(self.password.clone()))
                .child(div().text_xs().text_color(theme::muted()).child("Kept until you close cm-rs, never saved."));
            div().flex().flex_col().gap_1().child(section_title("Password")).child(focus_ring(el, focused))
        });

        let cars = self.car_choices();
        let chosen = d.car.clone();
        let cols = ((col_w + GAP) / (CAR_CARD_W + GAP)).floor().max(1.) as usize;
        self.car_cols = cols;
        let card_w = (col_w - GAP * (cols as f32 - 1.)) / cols as f32;
        let view = self.car_view;
        let mut cards: Vec<Stateful<gpui::Div>> = Vec::new();
        for (i, c) in cars.iter().enumerate() {
            let available = Self::choice_available(c);
            let selected = chosen.as_deref() == Some(c.id.as_str());
            let status: (String, gpui::Rgba) = if !c.installed {
                ("Not installed".into(), theme::error())
            } else if c.total == usize::MAX {
                ("Slots unknown".into(), theme::muted())
            } else if c.free == 0 {
                (format!("Full · {} slots", c.total), theme::muted())
            } else {
                (format!("{} of {} free", c.free, c.total), theme::ok())
            };
            let id = c.id.clone();
            let name_line = div()
                .flex()
                .items_center()
                .gap_1()
                .child(div().flex_1().min_w_0().text_sm().font_weight(FontWeight::SEMIBOLD).truncate().child(c.name.clone()))
                .when(selected, |d| d.child(div().text_sm().text_color(theme::accent()).child("✓")));
            let base = div()
                .id(("online-car", i))
                .rounded(px(theme::RADIUS_MD))
                .bg(if selected { theme::selected() } else { theme::panel() })
                .when(!available, |d| d.opacity(0.55))
                .when(available, |d| {
                    d.cursor_pointer().hover(|s| s.bg(theme::panel_hover())).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(det) = this.detail.as_mut() {
                            det.focus = offset + i;
                        }
                        this.select_car(id.clone(), cx);
                    }))
                });
            let card = match view {
                CarView::Cards => base
                    .w(px(card_w))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .child(picture(c.preview.clone(), 110.).w_full())
                    .child(name_line)
                    .child(div().text_xs().text_color(status.1).child(status.0)),
                CarView::Details => {
                    let mut row = base
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(px(GAP))
                        .p_2()
                        .child(picture(c.preview.clone(), 56.).w(px(100.)).flex_shrink_0())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(name_line)
                                .child(div().text_xs().text_color(theme::muted()).truncate().child(c.about.clone())),
                        );
                    for (key, _) in CAR_STATS {
                        row = row.child(div().w(px(STAT_W)).flex_shrink_0().text_sm().text_right().truncate().child(c.spec(key)));
                    }
                    row.child(div().w(px(120.)).flex_shrink_0().text_xs().text_right().text_color(status.1).child(status.0))
                }
            };
            let focused = show_focus && focus == offset + i;
            // The focus ring is the accent, so with a pad or keyboard the picked car gets a neutral
            // outline (plus its ✓); with a mouse there is no ring and the pick takes the accent.
            let card = if focused || !selected {
                focus_ring(card, focused)
            } else {
                card.border_2().border_color(if show_focus { theme::muted() } else { theme::accent() })
            };
            cards.push(card);
        }
        let view_toggle = {
            let item = |i: usize, label: &'static str, v: CarView| {
                chip(("online-carview", i), label, view == v).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(det) = this.detail.as_mut() {
                        det.focus = toggle;
                    }
                    this.set_car_view(v, cx);
                }))
            };
            let el = div().id("online-carview").p(px(2.)).rounded(px(theme::RADIUS_SM)).child(segmented(vec![
                item(0, "Cards", CarView::Cards),
                item(1, "Details", CarView::Details),
            ]));
            focus_ring(el, show_focus && focus == toggle)
        };
        let loading = d.loading;

        // One child per row so the pad can scroll a focused row into view.
        let mut children: Vec<AnyElement> = vec![hero.into_any_element()];
        children.extend(notices.into_iter().map(|(t, warn)| notice(t, warn).into_any_element()));
        if let Some(b) = blocker.filter(|_| !blocker_shown) {
            children.push(div().text_sm().text_color(theme::error()).child(b).into_any_element());
        }
        self.password_child = children.len();
        children.extend(password_row.map(|r| r.into_any_element()));
        self.view_child = children.len();
        if reachable {
            children.push(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(section_title("Pick a car"))
                    .child(view_toggle)
                    .into_any_element(),
            );
            if view == CarView::Details && !cars.is_empty() {
                // Column titles, aligned with the rows' picture, name and stat cells.
                let mut head = div().flex().items_center().gap(px(GAP)).px(px(10.)).text_xs().text_color(theme::faint());
                head = head.child(div().w(px(100.)).flex_shrink_0()).child(div().flex_1().child("CAR"));
                for (_, label) in CAR_STATS {
                    head = head.child(div().w(px(STAT_W)).flex_shrink_0().text_right().child(label.to_uppercase()));
                }
                children.push(head.child(div().w(px(120.)).flex_shrink_0().text_right().child("SLOTS")).into_any_element());
            }
        }
        self.cars_child = children.len();
        if cards.is_empty() && reachable {
            let text = if loading { "Loading cars…" } else { "The server lists no cars." };
            children.push(div().text_sm().text_color(theme::muted()).child(text).into_any_element());
        }
        let per_row = if view == CarView::Details { 1 } else { cols };
        let mut cards = cards.into_iter().peekable();
        while cards.peek().is_some() {
            let row: Vec<_> = cards.by_ref().take(per_row).collect();
            children.push(div().flex().gap(px(GAP)).children(row).into_any_element());
        }

        let body = div()
            .id("online-detail")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.detail_scroll)
            .flex()
            .flex_col()
            .gap_3()
            .children(children);

        div().w(px(col_w)).flex_1().min_h_0().flex().flex_col().child(body).into_any_element()
    }
}

impl Render for OnlineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vp_w = f32::from(window.viewport_size().width);
        let col_w = (vp_w - 32.).clamp(640., MAX_W);
        let page = if self.detail.is_some() { self.render_detail(col_w, cx) } else { self.render_list(col_w, cx) };
        div().size_full().flex().flex_col().items_center().p_4().child(page)
    }
}

fn badge(label: &'static str) -> gpui::Div {
    badge_owned(label.to_owned())
}

fn badge_owned(label: String) -> gpui::Div {
    div()
        .flex_shrink_0()
        .px_2()
        .py(px(2.))
        .rounded(px(theme::RADIUS_SM))
        .bg(theme::raised())
        .text_xs()
        .text_color(theme::muted())
        .child(label)
}

fn notice(text: String, warn: bool) -> gpui::Div {
    let color = if warn { theme::error() } else { theme::ok() };
    div()
        .px_3()
        .py_2()
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(color)
        .bg(theme::surface())
        .text_sm()
        .text_color(color)
        .child(text)
}

fn session_text(s: &Server) -> String {
    let label = s.session.label();
    if !s.shows_countdown() {
        return format!("{label} · lap race");
    }
    match s.seconds_left() {
        None => label.to_owned(),
        Some(left) if left <= 0 => format!("{label} · ending"),
        Some(left) if left < 3600 => format!("{label} · {} min left", (left + 59) / 60),
        Some(left) => format!("{label} · {}h {:02}m left", left / 3600, (left % 3600) / 60),
    }
}

/// `ScatteredClouds` or `3_clear` -> `Scattered clouds` / `Clear`.
fn weather_name(id: &str) -> String {
    let id = id.trim_start_matches(|c: char| c.is_ascii_digit() || c == '_');
    let mut words = String::new();
    let mut prev_lower = false;
    for c in id.chars() {
        if c == '_' || c == '-' {
            words.push(' ');
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            words.push(' ');
        }
        prev_lower = c.is_lowercase();
        words.extend(c.to_lowercase());
    }
    let mut chars = words.trim().chars();
    chars.next().map(|f| f.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn conditions_text(d: &Details) -> String {
    let mut parts = Vec::new();
    if let Some(w) = &d.weather {
        parts.push(weather_name(w));
    }
    if let Some(t) = d.ambient_c {
        parts.push(format!("air {t:.0} °C"));
    }
    if let Some(t) = d.road_c {
        parts.push(format!("road {t:.0} °C"));
    }
    if let Some(g) = d.grip {
        // AssettoServer reports grip as 0–1 or as a percentage depending on version.
        let pct = if g <= 1. { g * 100. } else { g };
        parts.push(format!("grip {pct:.0}%"));
    }
    parts.join(" · ")
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn ago(t: Instant) -> String {
    match t.elapsed().as_secs() {
        0..=9 => "just now".into(),
        s @ 10..=59 => format!("{s} s ago"),
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s => format!("{} h ago", s / 3600),
    }
}

/// Orders present values with `dir` and puts missing ones after them, whatever the direction.
fn missing_last<T: Ord>(a: Option<T>, b: Option<T>, dir: impl Fn(Ordering) -> Ordering) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => dir(a.cmp(&b)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// A filter toggle drawn as a checkbox, so it reads as "show only these" rather than a button.
fn filter_chip(id: impl Into<gpui::ElementId>, label: &'static str, on: bool) -> Stateful<gpui::Div> {
    let check = div()
        .size(px(16.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .when(on, |d| d.bg(theme::accent()).border_color(theme::accent()).text_color(theme::on_accent()).child("✓"))
        .when(!on, |d| d.border_color(theme::muted()));
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .min_h(px(theme::MIN_TARGET_H))
        .px_3()
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(if on { theme::accent() } else { theme::border() })
        .text_sm()
        .text_color(if on { theme::text() } else { theme::muted() })
        .cursor_pointer()
        .hover(|s| s.bg(theme::panel_hover()).text_color(theme::text()))
        .child(check)
        .child(label)
}

/// A fixed-width list cell: a title and an optional small second line (amber when `warn`).
fn two_lines(w: f32, title: SharedString, sub: String, warn: bool, strong: bool) -> gpui::Div {
    div()
        .w(px(w))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .child(div().w(px(w)).text_sm().when(strong, |d| d.font_weight(FontWeight::SEMIBOLD)).truncate().child(title))
        .when(!sub.is_empty(), |d| {
            d.child(div().w(px(w)).text_xs().text_color(if warn { theme::error() } else { theme::muted() }).truncate().child(sub))
        })
}
