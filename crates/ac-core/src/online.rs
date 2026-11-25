//! Online play: the Kunos lobby, each server's HTTP endpoints, UDP ping, the
//! `[REMOTE]` part of race.ini, and reading why a join failed.
//!
//! Endpoints and race.ini keys follow Content Manager (AcTools `KunosApiProvider`,
//! `Game.OnlineProperties`, `ServerEntry.Join`). The lobby host is the one Kunos'
//! own launcher names (`launcher/themes/.base/ac.utils.js`). Handshake reply codes
//! come from AssettoServer's `ACServerProtocol`, which mirrors Kunos' server.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ini::IniFile;
use crate::paths;
use crate::race::{Mode, QuickDrive};

/// Plain HTTP like Content Manager and the dedicated server: the lobby's HTTPS only offers
/// TLS 1.2 with CBC ciphers, which rustls refuses. The list is public data.
const LOBBY_URL: &str = "http://lobby.assettocorsa.net/lobby.ashx/list";
/// The lobby redirects every other User-Agent to its home page.
const USER_AGENT: &str = "Assetto Corsa Launcher";
/// The full list is about 8 MB of JSON (1.6 MB gzipped) for 9000 servers.
const LOBBY_LIMIT: u64 = 64 << 20;
const SERVER_LIMIT: u64 = 4 << 20;
const LOBBY_TIMEOUT: Duration = Duration::from_secs(60);
const SERVER_TIMEOUT: Duration = Duration::from_secs(6);

/// Most recent servers kept in `online.toml`.
const RECENT_MAX: usize = 20;

// ---------- server list ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    Booking,
    Practice,
    Qualifying,
    Race,
    Unknown,
}

impl SessionType {
    fn from_code(n: i64) -> Self {
        match n {
            0 => Self::Booking,
            1 => Self::Practice,
            2 => Self::Qualifying,
            3 => Self::Race,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Booking => "Booking",
            Self::Practice => "Practice",
            Self::Qualifying => "Qualifying",
            Self::Race => "Race",
            Self::Unknown => "Session",
        }
    }
}

/// One server as the lobby or its own `/INFO` describes it.
#[derive(Debug, Clone)]
pub struct Server {
    pub ip: String,
    /// UDP port; pings go here.
    pub udp_port: u16,
    /// HTTP port (`cport`): `/INFO`, `/JSON|guid`, and the server's identity.
    pub http_port: u16,
    /// TCP port (`tport`), race.ini `SERVER_PORT`.
    pub tcp_port: u16,
    pub name: String,
    /// `/api/details` port, from Content Manager's `ℹ<port>` name suffix.
    pub details_port: Option<u16>,
    pub clients: u32,
    pub max_clients: u32,
    /// `track` or `track-layout`, without the `csp/<build>/../` prefix.
    pub track: String,
    /// Minimum CSP build, from a `csp/<build>/../track` track id.
    pub required_csp: Option<u32>,
    pub cars: Vec<String>,
    pub password: bool,
    /// False for booking servers, which need a reservation before joining.
    pub pickup: bool,
    pub session: SessionType,
    pub sessions: Vec<SessionType>,
    /// False when a race runs for a lap count rather than a time.
    pub timed: bool,
    /// Seconds left in the current session at `fetched`; `None` when the server doesn't say.
    pub time_left: Option<i64>,
    pub fetched: Instant,
    /// Seconds since midnight, from the server's sun angle.
    pub time_of_day: u32,
    pub country: String,
    pub country_code: String,
    pub powered_by: Option<String>,
}

impl Server {
    /// `ip:http_port`, which is also how favourites are stored.
    pub fn address(&self) -> String {
        format!("{}:{}", self.ip, self.http_port)
    }

    /// Where `/api/details` lives: the `ℹ<port>` port, else the HTTP port (AssettoServer).
    /// Vanilla servers answer there with an empty body.
    pub fn details_port(&self) -> u16 {
        self.details_port.unwrap_or(self.http_port)
    }

    pub fn udp_addr(&self) -> Option<SocketAddrV4> {
        Some(SocketAddrV4::new(self.ip.parse().ok()?, self.udp_port))
    }

    pub fn seconds_left(&self) -> Option<i64> {
        Some(self.time_left? - self.fetched.elapsed().as_secs() as i64)
    }

    pub fn is_full(&self) -> bool {
        self.max_clients > 0 && self.clients >= self.max_clients
    }

    /// Whether the session has a countdown worth showing.
    pub fn shows_countdown(&self) -> bool {
        self.timed || self.session != SessionType::Race
    }
}

fn int(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .or_else(|| v.as_str()?.trim().parse().ok())
}

fn string(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_owned()
}

fn port(v: &Value) -> Option<u16> {
    u16::try_from(int(v)?).ok().filter(|p| *p != 0)
}

/// Splits Content Manager's `ℹ<port>` suffix (the extended-info port) off a server name.
fn split_details_port(name: &str) -> (String, Option<u16>) {
    // Some names carry control bytes (0x04) to sort first in other launchers; fonts draw them as boxes.
    let clean: String = name.chars().filter(|c| !c.is_control()).collect();
    let trimmed = clean.trim();
    if let Some(at) = trimmed.rfind('ℹ')
        && let Ok(p) = trimmed[at + 'ℹ'.len_utf8()..].trim().parse::<u16>()
    {
        return (trimmed[..at].trim_end().to_owned(), Some(p));
    }
    (trimmed.to_owned(), None)
}

/// Servers that need CSP write the track as `csp/<build>/../<track>`, sometimes with
/// physics flags as a middle piece. The track is the last piece (Content Manager's `ServerEntry`).
fn split_csp_track(raw: &str) -> (String, Option<u32>) {
    let Some(rest) = raw.strip_prefix("csp/") else {
        return (raw.to_owned(), None);
    };
    let pieces: Vec<&str> = rest.split("/../").collect();
    if pieces.len() < 2 {
        return (raw.to_owned(), None);
    }
    let build = pieces[0].split('/').next().and_then(|b| b.trim().parse().ok());
    (pieces[pieces.len() - 1].to_owned(), build)
}

fn parse_server(v: &Value, fallback_ip: Option<&str>, fetched: Instant) -> Option<Server> {
    let mut ip = string(&v["ip"]);
    if ip.is_empty() {
        ip = fallback_ip?.to_owned();
    }
    let http_port = port(&v["cport"])?;
    let tcp_port = port(&v["tport"])?;
    let (name, details_port) = split_details_port(v["name"].as_str().unwrap_or_default());
    let (track, required_csp) = split_csp_track(&string(&v["track"]));
    let codes: Vec<i64> = v["sessiontypes"].as_array().map(|a| a.iter().filter_map(int).collect()).unwrap_or_default();
    let sessions = codes.iter().copied().map(SessionType::from_code).collect();
    // The lobby sends the current session's type code. A vanilla server's own /INFO sends its
    // index into `sessiontypes` (CM's KunosApiProvider maps it the same way; measured: `0` with
    // types `[1]` during practice); AssettoServer's /INFO sends the type code again.
    let mut session = int(&v["session"]).unwrap_or(-1);
    let direct = fallback_ip.is_some() && v["poweredBy"].is_null();
    if direct && let Some(&code) = usize::try_from(session).ok().and_then(|i| codes.get(i)) {
        session = code;
    }
    let country = v["country"].as_array();
    let country_part = |i: usize| country.and_then(|c| c.get(i)).map(string).unwrap_or_default();
    // Content Manager: session end = now + timeleft - timestamp / 1000.
    let time_left = int(&v["timeleft"]).map(|t| t - int(&v["timestamp"]).unwrap_or(0) / 1000);
    let sun = v["timeofday"].as_f64().unwrap_or(0.);
    Some(Server {
        udp_port: port(&v["port"]).unwrap_or(tcp_port),
        http_port,
        tcp_port,
        name,
        details_port,
        clients: int(&v["clients"]).unwrap_or(0).max(0) as u32,
        max_clients: int(&v["maxclients"]).unwrap_or(0).max(0) as u32,
        track,
        required_csp,
        cars: v["cars"]
            .as_array()
            .map(|a| a.iter().map(string).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default(),
        password: v["pass"].as_bool().unwrap_or(false),
        pickup: v["pickup"].as_bool().unwrap_or(true),
        session: SessionType::from_code(session),
        sessions,
        timed: v["timed"].as_bool().unwrap_or(true),
        time_left,
        fetched,
        // Inverse of `race::sun_angle`.
        time_of_day: (46800. + sun * 3600. / 16.).clamp(0., 86399.) as u32,
        country: country_part(0),
        country_code: country_part(1),
        powered_by: v["poweredBy"].as_str().map(str::to_owned),
        ip,
    })
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// The URL without the query or `JSON|<guid>` part, so errors don't print the Steam ID.
fn redact(url: &str) -> &str {
    let end = url.find(['?', '|']).or_else(|| url.find("%7C")).unwrap_or(url.len());
    &url[..end]
}

fn get_text(url: &str, timeout: Duration, limit: u64) -> Result<String> {
    let mut resp = match agent(timeout).get(url).call() {
        Ok(resp) => resp,
        Err(ureq::Error::Timeout(_)) => bail!("no answer from {} within {} s", redact(url), timeout.as_secs()),
        Err(ureq::Error::Io(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            bail!("{} refused the connection", redact(url))
        }
        Err(e) => return Err(e).with_context(|| format!("requesting {}", redact(url))),
    };
    resp.body_mut()
        .with_config()
        .limit(limit)
        .read_to_string()
        .with_context(|| format!("reading {}", redact(url)))
}

/// The lobby answers `SLOW DOWN` to a second request within about 13 s (measured);
/// callers should not refresh more often than this.
pub const LOBBY_COOLDOWN: Duration = Duration::from_secs(15);

/// Why a lobby request failed, so callers can retry throttling but not other errors.
#[derive(Debug)]
pub struct LobbyBusy;

impl std::fmt::Display for LobbyBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the lobby is busy (too many requests); try again in a moment")
    }
}

impl std::error::Error for LobbyBusy {}

/// Downloads every server registered with the Kunos lobby.
pub fn fetch_lobby(steam_id: &str) -> Result<Vec<Server>> {
    let body = get_text(&format!("{LOBBY_URL}?guid={steam_id}"), LOBBY_TIMEOUT, LOBBY_LIMIT)?;
    parse_list(&body)
}

fn parse_list(body: &str) -> Result<Vec<Server>> {
    let body = body.trim_start();
    // Throttling comes back as HTTP 200 with a plain-text body.
    if !body.starts_with('[') {
        if body.contains("SLOW DOWN") || body.contains("TOO MANY") {
            return Err(LobbyBusy.into());
        }
        let first: String = body.lines().next().unwrap_or("").chars().take(120).collect();
        bail!("the lobby sent something that isn't a server list: {first}");
    }
    let list: Vec<Value> = serde_json::from_str(body).context("parsing the server list")?;
    let fetched = Instant::now();
    Ok(list.iter().filter_map(|v| parse_server(v, None, fetched)).collect())
}

/// Asks one server for its current state (`/INFO` on the HTTP port).
pub fn server_info(ip: &str, http_port: u16) -> Result<Server> {
    let body = get_text(&format!("http://{ip}:{http_port}/INFO"), SERVER_TIMEOUT, SERVER_LIMIT)?;
    let v: Value = serde_json::from_str(&body).context("the server's /INFO isn't JSON")?;
    parse_server(&v, Some(ip), Instant::now()).context("the server's /INFO is missing its ports")
}

/// One entry-list slot.
#[derive(Debug, Clone)]
pub struct CarSlot {
    pub model: String,
    pub skin: String,
    pub driver: String,
    pub connected: bool,
}

/// The entry list with who is connected (`/JSON|<guid>`).
pub fn car_slots(ip: &str, http_port: u16, steam_id: &str) -> Result<Vec<CarSlot>> {
    let body = get_text(&format!("http://{ip}:{http_port}/JSON%7C{steam_id}"), SERVER_TIMEOUT, SERVER_LIMIT)?;
    let v: Value = serde_json::from_str(&body).context("the server's car list isn't JSON")?;
    Ok(v["Cars"]
        .as_array()
        .map(|cars| {
            cars.iter()
                .map(|c| CarSlot {
                    model: string(&c["Model"]),
                    skin: string(&c["Skin"]),
                    driver: string(&c["DriverName"]),
                    connected: c["IsConnected"].as_bool().unwrap_or(false),
                })
                .filter(|c| !c.model.is_empty())
                .collect()
        })
        .unwrap_or_default())
}

/// Extra information from AssettoServer and server wrappers (`/api/details`).
#[derive(Debug, Clone, Default)]
pub struct Details {
    /// CSP features the server expects the client to announce (race.ini `__FEATURES`).
    pub features: Vec<String>,
    pub description: String,
    pub ambient_c: Option<f64>,
    pub road_c: Option<f64>,
    pub grip: Option<f64>,
    pub weather: Option<String>,
    pub city: Option<String>,
}

pub fn details(ip: &str, port: u16, steam_id: &str) -> Result<Details> {
    let body = get_text(&format!("http://{ip}:{port}/api/details?guid={steam_id}"), SERVER_TIMEOUT, SERVER_LIMIT)?;
    if body.trim().is_empty() {
        bail!("no extended information");
    }
    let v: Value = serde_json::from_str(&body).context("the server's details aren't JSON")?;
    let text = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    Ok(Details {
        features: v["features"]
            .as_array()
            .map(|a| a.iter().map(string).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default(),
        description: strip_bbcode(v["description"].as_str().unwrap_or_default()),
        ambient_c: v["ambientTemperature"].as_f64(),
        road_c: v["roadTemperature"].as_f64(),
        grip: v["grip"].as_f64(),
        weather: text("currentWeatherId"),
        city: text("city"),
    })
}

/// Drops `[b]`, `[/size]`, `[color=#fff]`-style tags from a server description.
fn strip_bbcode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let tag_end = after.find(']');
        let is_tag = tag_end.is_some_and(|end| {
            let tag = after[..end].trim_start_matches('/');
            let name = tag.split('=').next().unwrap_or("");
            !name.is_empty() && name.len() <= 8 && name.chars().all(|c| c.is_ascii_alphabetic() || c == '*')
        });
        match (is_tag, tag_end) {
            (true, Some(end)) => rest = &after[end + 1..],
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let mut lines: Vec<&str> = Vec::new();
    for line in out.lines().map(str::trim_end) {
        if line.trim().is_empty() && lines.last().is_none_or(|l| l.trim().is_empty()) {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n").trim().to_owned()
}

// ---------- ping ----------

/// Pings sent per burst, and the pause between bursts: about 800 a second. Sending the
/// whole lobby in one second (measured) lost most replies and doubled the rest's times.
const PING_BURST: usize = 8;
const PING_GAP: Duration = Duration::from_millis(10);
/// How long to keep listening after the last ping went out.
const PING_GRACE: Duration = Duration::from_millis(1500);

/// UDP ping like Kunos' launcher: send `[200]`, the server answers `[200, http_port lo, hi]`.
/// One socket; a receiver thread timestamps replies while the sender is still sending,
/// so far-away servers late in the list are not charged for the send queue.
pub struct Pinger {
    results: Arc<Mutex<HashMap<SocketAddrV4, u32>>>,
    done: Arc<AtomicBool>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Pinger {
    /// Pings in the given order, so callers can put the servers that matter first.
    pub fn start(mut targets: Vec<SocketAddrV4>) -> Result<Self> {
        let mut seen = std::collections::HashSet::new();
        targets.retain(|t| seen.insert(*t));
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).context("opening a UDP socket for ping")?;
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let receiver = socket.try_clone()?;
        let sent: Arc<Mutex<HashMap<SocketAddrV4, Instant>>> = Arc::default();
        let results: Arc<Mutex<HashMap<SocketAddrV4, u32>>> = Arc::default();
        let done = Arc::new(AtomicBool::new(false));
        let sending = Arc::new(AtomicBool::new(true));

        {
            let (sent, results, done, sending) = (sent.clone(), results.clone(), done.clone(), sending.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 16];
                let mut quiet_since: Option<Instant> = None;
                loop {
                    match receiver.recv_from(&mut buf) {
                        Ok((3, SocketAddr::V4(from))) if buf[0] == 200 => {
                            let now = Instant::now();
                            if let Some(t) = lock(&sent).get(&from).copied() {
                                let ms = now.duration_since(t).as_millis().min(u32::MAX as u128) as u32;
                                lock(&results).entry(from).or_insert(ms);
                            }
                        }
                        Ok(_) => {}
                        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                        // ICMP errors (port unreachable) surface here; they only mean "no answer".
                        Err(_) => {}
                    }
                    if !sending.load(Ordering::Acquire)
                        && quiet_since.get_or_insert_with(Instant::now).elapsed() > PING_GRACE
                    {
                        break;
                    }
                }
                done.store(true, Ordering::Release);
            });
        }
        std::thread::spawn(move || {
            for burst in targets.chunks(PING_BURST) {
                for t in burst {
                    lock(&sent).insert(*t, Instant::now());
                    let _ = socket.send_to(&[200], t);
                }
                std::thread::sleep(PING_GAP);
            }
            sending.store(false, Ordering::Release);
        });
        Ok(Self { results, done })
    }

    /// Round trips so far, in milliseconds.
    pub fn results(&self) -> HashMap<SocketAddrV4, u32> {
        lock(&self.results).clone()
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

// ---------- Steam ID ----------

/// SteamID64 of the account Steam last signed in with (`config/loginusers.vdf`).
/// Servers identify drivers by it, and the lobby needs it to list servers.
pub fn steam_id() -> Option<String> {
    paths::steam_roots()
        .into_iter()
        .filter_map(|root| fs::read_to_string(root.join("config").join("loginusers.vdf")).ok())
        .find_map(|vdf| most_recent_user(&vdf))
}

fn vdf_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '{' | '}' => out.push(c.to_string()),
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => s.extend(chars.next()),
                        '"' => break,
                        _ => s.push(c),
                    }
                }
                out.push(s);
            }
            _ => {}
        }
    }
    out
}

fn most_recent_user(vdf: &str) -> Option<String> {
    let tokens = vdf_tokens(vdf);
    let is_id = |s: &str| s.len() == 17 && s.starts_with("7656119") && s.bytes().all(|b| b.is_ascii_digit());
    let (mut first, mut current, mut recent) = (None, None::<String>, None);
    let mut depth = 0;
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "{" => depth += 1,
            "}" => {
                depth -= 1;
                if depth <= 1 {
                    current = None;
                }
            }
            t if depth == 1 && tokens.get(i + 1).is_some_and(|n| n == "{") && is_id(t) => {
                first.get_or_insert_with(|| t.to_owned());
                current = Some(t.to_owned());
            }
            t if depth == 2 && t.eq_ignore_ascii_case("MostRecent") => {
                if tokens.get(i + 1).is_some_and(|v| v == "1") {
                    recent = current.clone();
                }
                i += 1;
            }
            // Skip the value of any other flat key; nested blocks are walked by depth.
            _ if depth == 2 && tokens.get(i + 1).is_some_and(|n| n != "{" && n != "}") => i += 1,
            _ => {}
        }
        i += 1;
    }
    recent.or(first)
}

// ---------- joining ----------

/// What race.ini `[REMOTE]` needs to join one server.
#[derive(Debug, Clone)]
pub struct Join {
    /// IPv4 address; AC does not resolve host names.
    pub server_ip: String,
    pub tcp_port: u16,
    pub http_port: u16,
    pub server_name: String,
    pub car: String,
    pub driver_name: String,
    pub guid: String,
    pub password: String,
    /// CSP features from `/api/details`; AssettoServer rejects the handshake without them.
    pub features: Vec<String>,
}

/// The quick-drive part of an online race.ini: the player's own car, skin and the
/// server's track on top of `base` (which keeps preferences such as `use_mph`).
/// Session and conditions come from the server once connected.
pub fn quick_drive_for(base: &QuickDrive, car: &str, skin: &str, track: &str, layout: Option<&str>) -> QuickDrive {
    QuickDrive {
        car_id: car.to_owned(),
        skin_id: skin.to_owned(),
        track_id: track.to_owned(),
        layout_id: layout.map(str::to_owned),
        mode: Mode::Practice { minutes: 0 },
        opponents: Vec::new(),
        ..base.clone()
    }
}

/// Mirrors `Game.OnlineProperties.Set` on top of a race.ini already prepared for the car and track.
pub fn apply_remote(file: &mut IniFile, j: &Join) {
    let r = file.section_mut("REMOTE");
    r.set("SERVER_IP", &j.server_ip);
    r.set("SERVER_PORT", j.tcp_port);
    r.set("SERVER_NAME", &j.server_name);
    r.set("SERVER_HTTP_PORT", j.http_port);
    r.set("REQUESTED_CAR", &j.car);
    r.set("NAME", &j.driver_name);
    r.set("TEAM", "");
    r.set("GUID", &j.guid);
    r.set("PASSWORD", &j.password);
    r.set_bool("ACTIVE", true);
    r.set_bool("__CM_EXTENDED", false);
    if j.features.is_empty() {
        r.remove("__FEATURES");
    } else {
        r.set("__FEATURES", j.features.join(","));
    }
}

/// Turns `[REMOTE]` off and blanks the password once an online session is over, so
/// the next plain launch of AC (from Steam too) does not reconnect.
pub fn clear_remote(ac_root: &Path) -> Result<()> {
    let path = paths::race_ini_path(ac_root).context("AC's Documents folder is missing")?;
    let mut ini = IniFile::load_or_default(&path)?;
    let r = ini.section_mut("REMOTE");
    r.set_bool("ACTIVE", false);
    r.set("PASSWORD", "");
    ini.save(&path)
}

/// Resolves `host:port` typed by the user (the server's HTTP port) to an IPv4 address.
pub fn resolve(address: &str) -> Result<(String, u16)> {
    let a = address.trim();
    let a = a.strip_prefix("http://").unwrap_or(a).trim_end_matches('/');
    let (host, port) = a.rsplit_once(':').context("use host:port, for example 203.0.113.5:8081")?;
    let port: u16 = port.trim().parse().context("the port must be a number between 1 and 65535")?;
    let host = host.trim();
    if host.is_empty() || port == 0 {
        bail!("use host:port, for example 203.0.113.5:8081");
    }
    let ip = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("can't resolve {host}"))?
        .find_map(|s| match s {
            SocketAddr::V4(v) => Some(*v.ip()),
            SocketAddr::V6(_) => None,
        })
        .with_context(|| format!("{host} has no IPv4 address"))?;
    Ok((ip.to_string(), port))
}

/// The installed track and layout for a server's `track` value. Track folders may
/// contain `-` themselves, so the split is decided by what is installed.
pub fn match_track<'a>(server_track: &str, installed: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> Option<(String, Option<String>)> {
    let want = server_track.to_lowercase();
    installed.into_iter().find_map(|(track, layout)| {
        let key = match layout {
            Some(l) => format!("{track}-{l}"),
            None => track.to_owned(),
        };
        (key.to_lowercase() == want).then(|| (track.to_owned(), layout.map(str::to_owned)))
    })
}

/// How the last online launch went, read from the game's own logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinOutcome {
    Connected,
    /// The game closed before it tried to connect.
    NotAttempted,
    Unreachable,
    WrongPassword,
    Banned,
    NoSlot,
    SessionClosed,
    WrongVersion,
    Rejected(Option<String>),
    Unknown(u8),
}

impl JoinOutcome {
    pub fn message(&self) -> String {
        match self {
            Self::Connected => "Connected.".into(),
            Self::NotAttempted => "The game closed before it connected to the server.".into(),
            Self::Unreachable => "The server didn't answer. It may be offline or blocked by a firewall.".into(),
            Self::WrongPassword => "Wrong password.".into(),
            Self::Banned => "This server has banned your Steam account.".into(),
            Self::NoSlot => "No free slot for that car. Pick another car or try again later.".into(),
            Self::SessionClosed => "The session is closed to new drivers. Wait for the next session.".into(),
            Self::WrongVersion => "The server runs a different version of Assetto Corsa.".into(),
            Self::Rejected(Some(reason)) => format!("The server refused the connection: {reason}"),
            Self::Rejected(None) => "The server refused the connection.".into(),
            Self::Unknown(code) => format!("The server refused the connection (code {code})."),
        }
    }
}

fn modified_since(p: &Path, since: SystemTime) -> bool {
    fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t >= since)
}

/// Reads `logs/log.txt` (and CSP's log for the refusal reason) written after `since`.
/// `None` when the game left no log for this launch.
pub fn join_outcome(ac_root: &Path, since: SystemTime) -> Option<JoinOutcome> {
    let logs = paths::documents_ac_dir(ac_root)?.join("logs");
    let log = logs.join("log.txt");
    if !modified_since(&log, since) {
        return None;
    }
    let text = String::from_utf8_lossy(&fs::read(&log).ok()?).into_owned();
    Some(parse_outcome(&text, || csp_refusal(&logs.join("custom_shaders_patch.log"), since)))
}

/// Markers are acs.exe's own log strings: `CONNECTING TO SERVER VIA TCP %S [%u]`, then
/// `RECV: %d` with the handshake reply, or `TCP connection failed`.
fn parse_outcome(log: &str, reason: impl FnOnce() -> Option<String>) -> JoinOutcome {
    let Some(at) = log.rfind("CONNECTING TO SERVER") else {
        return JoinOutcome::NotAttempted;
    };
    for line in log[at..].lines().map(str::trim) {
        if line.starts_with("TCP connection failed") {
            return JoinOutcome::Unreachable;
        }
        if line.contains("ACP_SERVER_STILL_BOOKING") || line.contains("ACP_SESSION_CLOSED") {
            return JoinOutcome::SessionClosed;
        }
        if let Some(rest) = line.strip_prefix("RECV:") {
            return match rest.trim().parse::<u8>() {
                Ok(0x3E) => JoinOutcome::Connected,
                Ok(0x3C) => JoinOutcome::WrongPassword,
                Ok(0x3B) => JoinOutcome::Banned,
                Ok(0x45) => JoinOutcome::NoSlot,
                Ok(0x41) => JoinOutcome::SessionClosed,
                Ok(0x42) => JoinOutcome::WrongVersion,
                Ok(0x6F) => JoinOutcome::Rejected(reason()),
                Ok(code) => JoinOutcome::Unknown(code),
                Err(_) => JoinOutcome::Rejected(reason()),
            };
        }
    }
    JoinOutcome::Unreachable
}

/// CSP logs the text of an AssettoServer refusal: `SERVER AUTH FAILED:<reason>`.
fn csp_refusal(path: &Path, since: SystemTime) -> Option<String> {
    if !modified_since(path, since) {
        return None;
    }
    let text = String::from_utf8_lossy(&fs::read(path).ok()?).into_owned();
    const MARK: &str = "SERVER AUTH FAILED:";
    let at = text.rfind(MARK)?;
    let reason = text[at + MARK.len()..].lines().next()?.trim();
    (!reason.is_empty()).then(|| reason.to_owned())
}

// ---------- favourites and recent servers ----------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// `host:http_port`.
    pub address: String,
    pub name: String,
}

/// Favourite and recently joined servers, kept in `online.toml` next to the settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub favourites: Vec<Saved>,
    #[serde(default)]
    pub recent: Vec<Saved>,
}

pub fn store_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("cm-rs").join("online.toml"))
}

impl Store {
    /// A missing file is an empty store. A file that doesn't parse is an error, so a
    /// hand-edited file is never replaced with an empty one.
    pub fn load() -> Result<Self> {
        let p = store_path().context("no config directory")?;
        match fs::read_to_string(&p) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("reading {}", p.display())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        use std::io::Write;
        let p = store_path().context("no config directory")?;
        if p.exists() {
            Self::load()?;
        }
        let parent = p.parent().context("online.toml has no parent folder")?;
        fs::create_dir_all(parent)?;
        let text = toml::to_string_pretty(self)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(text.as_bytes())?;
        temp.as_file().sync_all()?;
        temp.persist(&p).with_context(|| format!("writing {}", p.display()))?;
        Ok(())
    }

    pub fn is_favourite(&self, address: &str) -> bool {
        self.favourites.iter().any(|s| s.address == address)
    }

    pub fn toggle_favourite(&mut self, address: &str, name: &str) {
        if self.is_favourite(address) {
            self.favourites.retain(|s| s.address != address);
        } else {
            self.favourites.push(Saved { address: address.to_owned(), name: name.to_owned() });
        }
    }

    pub fn push_recent(&mut self, address: &str, name: &str) {
        self.recent.retain(|s| s.address != address);
        self.recent.insert(0, Saved { address: address.to_owned(), name: name.to_owned() });
        self.recent.truncate(RECENT_MAX);
    }
}
