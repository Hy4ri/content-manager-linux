//! Managing installed car and track mods: scanning, enable/disable, deletion, and manifest tracking.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::content::{self, natural_cmp};
use crate::install::{Item, Kind};

const MANIFEST_FILE: &str = ".cm-rs-mods.json";
const DISABLED_DIR: &str = ".cm-rs-disabled";
const MANIFEST_VERSION: u32 = 1;

/// Official Kunos Simulazioni base game and DLC car folder IDs (lowercased, sorted).
const STOCK_CARS: &[&str] = &[
    "abarth500",
    "abarth500_s1",
    "alfa_romeo_giulietta_qv",
    "alfa_romeo_giulietta_qv_le",
    "bmw_1m",
    "bmw_1m_s3",
    "bmw_m3_e30",
    "bmw_m3_e30_drift",
    "bmw_m3_e30_dtm",
    "bmw_m3_e30_gra",
    "bmw_m3_e30_s1",
    "bmw_m3_e92",
    "bmw_m3_e92_drift",
    "bmw_m3_e92_s1",
    "bmw_m3_gt2",
    "bmw_z4",
    "bmw_z4_drift",
    "bmw_z4_gt3",
    "bmw_z4_s1",
    "ferrari_312t",
    "ferrari_458",
    "ferrari_458_gt2",
    "ferrari_458_s3",
    "ferrari_599xxevo",
    "ferrari_f40",
    "ferrari_f40_s3",
    "ferrari_laferrari",
    "ks_abarth500_assetto_corse",
    "ks_abarth_595ss",
    "ks_abarth_595ss_s1",
    "ks_abarth_595ss_s2",
    "ks_alfa_33_stradale",
    "ks_alfa_giulia_qv",
    "ks_alfa_mito_qv",
    "ks_alfa_romeo_155_v6",
    "ks_alfa_romeo_4c",
    "ks_alfa_romeo_gta",
    "ks_audi_a1s1",
    "ks_audi_r18_etron_quattro",
    "ks_audi_r8_lms",
    "ks_audi_r8_lms_2016",
    "ks_audi_r8_plus",
    "ks_audi_sport_quattro",
    "ks_audi_sport_quattro_rally",
    "ks_audi_sport_quattro_s1",
    "ks_audi_tt_cup",
    "ks_audi_tt_vln",
    "ks_bmw_m235i_racing",
    "ks_bmw_m4",
    "ks_bmw_m4_akrapovic",
    "ks_corvette_c7_stingray",
    "ks_corvette_c7r",
    "ks_ferrari_250_gto",
    "ks_ferrari_288_gto",
    "ks_ferrari_312_67",
    "ks_ferrari_330_p4",
    "ks_ferrari_488_challenge_evo",
    "ks_ferrari_488_gt3",
    "ks_ferrari_488_gt3_2020",
    "ks_ferrari_488_gtb",
    "ks_ferrari_812_superfast",
    "ks_ferrari_f138",
    "ks_ferrari_f2004",
    "ks_ferrari_fxx_k",
    "ks_ferrari_sf15t",
    "ks_ferrari_sf70h",
    "ks_ford_escort_mk1",
    "ks_ford_gt40",
    "ks_ford_mustang_2015",
    "ks_glickenhaus_scg003",
    "ks_lamborghini_aventador_sv",
    "ks_lamborghini_countach",
    "ks_lamborghini_countach_s1",
    "ks_lamborghini_gallardo_sl",
    "ks_lamborghini_gallardo_sl_s3",
    "ks_lamborghini_huracan_gt3",
    "ks_lamborghini_huracan_performante",
    "ks_lamborghini_huracan_st",
    "ks_lamborghini_miura_sv",
    "ks_lamborghini_sesto_elemento",
    "ks_lotus_25",
    "ks_lotus_3_eleven",
    "ks_lotus_72d",
    "ks_maserati_250f_12cyl",
    "ks_maserati_250f_6cyl",
    "ks_maserati_alfieri",
    "ks_maserati_gt_mc_gt4",
    "ks_maserati_levante",
    "ks_maserati_mc12_gt1",
    "ks_maserati_quattroporte",
    "ks_mazda_787b",
    "ks_mazda_miata",
    "ks_mazda_mx5_cup",
    "ks_mazda_mx5_nd",
    "ks_mazda_rx7_spirit_r",
    "ks_mazda_rx7_tuned",
    "ks_mclaren_570s",
    "ks_mclaren_650_gt3",
    "ks_mclaren_f1_gtr",
    "ks_mclaren_p1",
    "ks_mclaren_p1_gtr",
    "ks_mercedes_190_evo2",
    "ks_mercedes_amg_gt3",
    "ks_mercedes_c9",
    "ks_nissan_370z",
    "ks_nissan_gtr",
    "ks_nissan_gtr_gt3",
    "ks_nissan_skyline_r34",
    "ks_pagani_huayra_bc",
    "ks_porsche_718_boxster_s",
    "ks_porsche_718_boxster_s_pdk",
    "ks_porsche_718_cayman_s",
    "ks_porsche_718_spyder_rs",
    "ks_porsche_908_lh",
    "ks_porsche_911_carrera_rsr",
    "ks_porsche_911_gt1",
    "ks_porsche_911_gt3_cup_2017",
    "ks_porsche_911_gt3_r_2016",
    "ks_porsche_911_gt3_rs",
    "ks_porsche_911_r",
    "ks_porsche_911_rsr_2017",
    "ks_porsche_917_30",
    "ks_porsche_917_k",
    "ks_porsche_918_spyder",
    "ks_porsche_919_hybrid_2015",
    "ks_porsche_919_hybrid_2016",
    "ks_porsche_935_78_moby_dick",
    "ks_porsche_962c_longtail",
    "ks_porsche_962c_shorttail",
    "ks_porsche_991_carrera_s",
    "ks_porsche_991_turbo_s",
    "ks_porsche_cayenne",
    "ks_porsche_cayman_gt4_clubsport",
    "ks_porsche_cayman_gt4_std",
    "ks_porsche_macan",
    "ks_porsche_panamera",
    "ks_praga_r1",
    "ks_ruf_rt12r",
    "ks_ruf_rt12r_awd",
    "ks_toyota_ae86",
    "ks_toyota_ae86_drift",
    "ks_toyota_ae86_tuned",
    "ks_toyota_celica_st185",
    "ks_toyota_gt86",
    "ks_toyota_supra_mkiv",
    "ks_toyota_supra_mkiv_drift",
    "ks_toyota_supra_mkiv_tuned",
    "ks_toyota_ts040",
    "ktm_xbow_r",
    "lotus_2_eleven",
    "lotus_2_eleven_gt4",
    "lotus_49",
    "lotus_98t",
    "lotus_elise_sc",
    "lotus_elise_sc_s1",
    "lotus_elise_sc_s2",
    "lotus_evora_gtc",
    "lotus_evora_gte",
    "lotus_evora_gte_carbon",
    "lotus_evora_gx",
    "lotus_evora_s",
    "lotus_evora_s_s2",
    "lotus_exige_240",
    "lotus_exige_240_s3",
    "lotus_exige_s",
    "lotus_exige_s_roadster",
    "lotus_exige_scura",
    "lotus_exige_v6_cup",
    "lotus_exos_125",
    "lotus_exos_125_s1",
    "mclaren_mp412c",
    "mclaren_mp412c_gt3",
    "mercedes_sls",
    "mercedes_sls_gt3",
    "p4-5_2011",
    "pagani_huayra",
    "pagani_zonda_r",
    "ruf_yellowbird",
    "shelby_cobra_427sc",
    "tatuusfa1",
];

/// Official Kunos Simulazioni base game and DLC track folder IDs (lowercased, sorted).
const STOCK_TRACKS: &[&str] = &[
    "drift",
    "imola",
    "ks_barcelona",
    "ks_black_cat_county",
    "ks_brands_hatch",
    "ks_drag",
    "ks_highlands",
    "ks_laguna_seca",
    "ks_monza66",
    "ks_nordschleife",
    "ks_nurburgring",
    "ks_red_bull_ring",
    "ks_silverstone",
    "ks_silverstone1967",
    "ks_vallelunga",
    "ks_zandvoort",
    "magione",
    "monza",
    "mugello",
    "spa",
    "trento-bondone",
];

pub fn is_stock(kind: Kind, id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    let table = match kind {
        Kind::Car => STOCK_CARS,
        Kind::Track => STOCK_TRACKS,
    };
    table.binary_search(&lower.as_str()).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModOrigin {
    /// Installed through cm-rs and recorded in the manifest.
    Tracked,
    /// Non-Kunos content installed outside cm-rs.
    Manual,
    /// Official Kunos base game or DLC content.
    Stock,
}

#[derive(Debug, Clone)]
pub struct InstalledMod {
    pub kind: Kind,
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    pub brand_or_country: Option<String>,
    /// Skin count for cars, layout count for tracks.
    pub detail_count: usize,
    pub preview: Option<PathBuf>,
    pub dir: PathBuf,
    pub enabled: bool,
    pub origin: ModOrigin,
    pub size_bytes: u64,
    pub archive_name: Option<String>,
    /// Unix timestamp in seconds (from manifest, or directory mtime fallback).
    pub installed_at: u64,
    pub search_text: String,
}

#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub items: Vec<InstalledMod>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    entries: BTreeMap<String, ManifestEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestEntry {
    archive_name: Option<String>,
    installed_at: u64,
}

fn manifest_key(kind: Kind, id: &str) -> String {
    let prefix = match kind {
        Kind::Car => "car",
        Kind::Track => "track",
    };
    format!("{prefix}:{id}")
}

fn manifest_path(ac_root: &Path) -> PathBuf {
    ac_root.join("content").join(MANIFEST_FILE)
}

fn validate_id(id: &str) -> Result<()> {
    let mut components = Path::new(id).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        bail!("unsafe content identifier: {id}");
    }
    Ok(())
}

pub fn enabled_parent(ac_root: &Path, kind: Kind) -> PathBuf {
    ac_root.join("content").join(kind.folder())
}

pub fn disabled_parent(ac_root: &Path, kind: Kind) -> PathBuf {
    ac_root.join("content").join(DISABLED_DIR).join(kind.folder())
}

fn load_manifest(ac_root: &Path) -> Result<Manifest> {
    let path = manifest_path(ac_root);
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("reading mod manifest {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(e) => Err(e).with_context(|| format!("reading mod manifest {}", path.display())),
    }
}

fn save_manifest(ac_root: &Path, manifest: &Manifest) -> Result<()> {
    let path = manifest_path(ac_root);
    let parent = path.parent().context("manifest path has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{MANIFEST_FILE}.{}.tmp",
        std::process::id()
    ));
    let data = serde_json::to_string_pretty(manifest)?;
    fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn dir_mtime_unix(dir: &Path) -> u64 {
    fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Records newly installed archive items in `<ac_root>/content/.cm-rs-mods.json`.
pub fn record_install(ac_root: &Path, archive: &Path, items: &[Item]) -> Result<()> {
    let mut manifest = load_manifest(ac_root)?;
    let archive_name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    let ts = now_unix();
    for item in items {
        manifest.entries.insert(
            manifest_key(item.kind, &item.id),
            ManifestEntry {
                archive_name: archive_name.clone(),
                installed_at: ts,
            },
        );
    }
    save_manifest(ac_root, &manifest)
}

/// Recursively computes directory size in bytes without following symlinks.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            let ft = meta.file_type();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let p = e.path();
            let ft = fs::symlink_metadata(&p).ok()?.file_type();
            (ft.is_dir() && !ft.is_symlink()).then_some(p)
        })
        .collect()
}

fn inspect_car(dir: &Path, enabled: bool, manifest: &Manifest) -> Option<InstalledMod> {
    let car = content::load_car(dir);
    if !car.has_data_acd && !car.has_data_dir && !dir.join("ui").join("ui_car.json").is_file() {
        return None;
    }
    let key = manifest_key(Kind::Car, &car.id);
    let entry = manifest.entries.get(&key);
    let stock = is_stock(Kind::Car, &car.id);
    let origin = if stock {
        ModOrigin::Stock
    } else if entry.is_some() {
        ModOrigin::Tracked
    } else {
        ModOrigin::Manual
    };
    let archive_name = entry.and_then(|e| e.archive_name.clone());
    let installed_at = entry.map(|e| e.installed_at).unwrap_or_else(|| dir_mtime_unix(dir));
    let preview = car.preview().map(Path::to_path_buf);
    let brand = car
        .brand
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let author = car
        .author
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let search_text = format!(
        "{} {} {} {} {}",
        car.name,
        car.id,
        brand.as_deref().unwrap_or(""),
        author.as_deref().unwrap_or(""),
        archive_name.as_deref().unwrap_or("")
    )
    .to_lowercase();
    Some(InstalledMod {
        kind: Kind::Car,
        id: car.id,
        name: car.name,
        author,
        brand_or_country: brand,
        detail_count: car.skins.len(),
        preview,
        dir: dir.to_path_buf(),
        enabled,
        origin,
        size_bytes: dir_size(dir),
        archive_name,
        installed_at,
        search_text,
    })
}

fn inspect_track(dir: &Path, enabled: bool, manifest: &Manifest) -> Option<InstalledMod> {
    let track = content::load_track(dir);
    if track.layouts.is_empty() && !dir.join("ui").is_dir() {
        return None;
    }
    let key = manifest_key(Kind::Track, &track.id);
    let entry = manifest.entries.get(&key);
    let stock = is_stock(Kind::Track, &track.id);
    let origin = if stock {
        ModOrigin::Stock
    } else if entry.is_some() {
        ModOrigin::Tracked
    } else {
        ModOrigin::Manual
    };
    let archive_name = entry.and_then(|e| e.archive_name.clone());
    let installed_at = entry.map(|e| e.installed_at).unwrap_or_else(|| dir_mtime_unix(dir));
    let first = track.layouts.first();
    let name = first
        .map(|l| l.name.clone())
        .unwrap_or_else(|| track.id.clone());
    let country = first
        .and_then(|l| l.country.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let preview = first.and_then(|l| l.preview.clone().or_else(|| l.outline.clone()));
    let author = read_track_author(dir);
    let search_text = format!(
        "{} {} {} {} {}",
        name,
        track.id,
        country.as_deref().unwrap_or(""),
        author.as_deref().unwrap_or(""),
        archive_name.as_deref().unwrap_or("")
    )
    .to_lowercase();
    Some(InstalledMod {
        kind: Kind::Track,
        id: track.id,
        name,
        author,
        brand_or_country: country,
        detail_count: track.layouts.len(),
        preview,
        dir: dir.to_path_buf(),
        enabled,
        origin,
        size_bytes: dir_size(dir),
        archive_name,
        installed_at,
        search_text,
    })
}

fn read_track_author(dir: &Path) -> Option<String> {
    let ui = dir.join("ui");
    let direct = ui.join("ui_track.json");
    let candidates = std::iter::once(direct).chain(
        subdirs(&ui)
            .into_iter()
            .map(|sub| sub.join("ui_track.json")),
    );
    for path in candidates {
        if let Ok(v) = crate::json::parse_file(&path)
            && let Some(author) = crate::json::text(&v, "author")
        {
            let trimmed = author.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// Scans all enabled and disabled cars and tracks in `ac_root`.
pub fn scan(ac_root: &Path) -> ScanResult {
    let (manifest, warning) = match load_manifest(ac_root) {
        Ok(m) => (m, None),
        Err(e) => (Manifest::default(), Some(format!("Mod manifest error: {e:#}"))),
    };
    let mut items = Vec::new();
    for dir in subdirs(&enabled_parent(ac_root, Kind::Car)) {
        if let Some(m) = inspect_car(&dir, true, &manifest) {
            items.push(m);
        }
    }
    for dir in subdirs(&disabled_parent(ac_root, Kind::Car)) {
        if let Some(m) = inspect_car(&dir, false, &manifest) {
            items.push(m);
        }
    }
    for dir in subdirs(&enabled_parent(ac_root, Kind::Track)) {
        if let Some(m) = inspect_track(&dir, true, &manifest) {
            items.push(m);
        }
    }
    for dir in subdirs(&disabled_parent(ac_root, Kind::Track)) {
        if let Some(m) = inspect_track(&dir, false, &manifest) {
            items.push(m);
        }
    }
    items.sort_by(|a, b| {
        natural_cmp(&a.name, &b.name)
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| b.enabled.cmp(&a.enabled))
    });
    ScanResult { items, warning }
}

/// Moves a car or track folder between `content/{cars,tracks}/<id>` and
/// `content/.cm-rs-disabled/{cars,tracks}/<id>`.
pub fn set_enabled(ac_root: &Path, kind: Kind, id: &str, enable: bool) -> Result<PathBuf> {
    validate_id(id)?;
    let (src_parent, dst_parent) = if enable {
        (disabled_parent(ac_root, kind), enabled_parent(ac_root, kind))
    } else {
        (enabled_parent(ac_root, kind), disabled_parent(ac_root, kind))
    };
    let src = src_parent.join(id);
    let dst = dst_parent.join(id);

    let meta = fs::symlink_metadata(&src)
        .with_context(|| format!("missing content folder {}", src.display()))?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        bail!("refusing to move non-directory or symlink {}", src.display());
    }
    if dst.exists() || fs::symlink_metadata(&dst).is_ok() {
        bail!(
            "destination already exists: {} (resolve the duplicate folder first)",
            dst.display()
        );
    }
    fs::create_dir_all(&dst_parent)
        .with_context(|| format!("creating {}", dst_parent.display()))?;
    fs::rename(&src, &dst)
        .with_context(|| format!("moving {} to {}", src.display(), dst.display()))?;
    Ok(dst)
}

/// Permanently deletes an installed mod folder and removes it from the manifest.
/// Refuses to delete official Kunos stock content or symlinks.
pub fn delete_mod(ac_root: &Path, kind: Kind, id: &str, enabled: bool) -> Result<()> {
    validate_id(id)?;
    if is_stock(kind, id) {
        bail!("refusing to delete official Kunos stock content: {id}");
    }
    let parent = if enabled {
        enabled_parent(ac_root, kind)
    } else {
        disabled_parent(ac_root, kind)
    };
    let dir = parent.join(id);
    let meta = fs::symlink_metadata(&dir)
        .with_context(|| format!("missing mod folder {}", dir.display()))?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        bail!("refusing to delete symlink or non-directory {}", dir.display());
    }
    let canon_parent = fs::canonicalize(&parent)
        .with_context(|| format!("resolving parent {}", parent.display()))?;
    let canon_dir = fs::canonicalize(&dir)
        .with_context(|| format!("resolving mod folder {}", dir.display()))?;
    if canon_dir.parent() != Some(canon_parent.as_path()) {
        bail!("refusing to delete path outside content directory: {}", dir.display());
    }

    fs::remove_dir_all(&dir)
        .with_context(|| format!("deleting {}", dir.display()))?;

    // Prune manifest entry if the manifest is readable and contains this key.
    if let Ok(mut manifest) = load_manifest(ac_root)
        && manifest.entries.remove(&manifest_key(kind, id)).is_some()
    {
        let _ = save_manifest(ac_root, &manifest);
    }
    Ok(())
}

/// Opens a folder in the OS file manager.
pub fn open_folder(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        bail!("folder does not exist: {}", dir.display());
    }
    os_open(dir.as_os_str()).with_context(|| format!("opening folder {}", dir.display()))
}

/// Opens an `https://` URL in the default browser.
pub fn open_url(url: &str) -> Result<()> {
    if !url.starts_with("https://") {
        bail!("refusing to open non-https URL {url}");
    }
    os_open(url.as_ref()).with_context(|| format!("opening {url}"))
}

/// Hands a path or URL to the desktop's default handler.
fn os_open(target: &std::ffi::OsStr) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    let mut cmd = Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = Command::new("explorer");
    cmd.arg(target).spawn().map(drop)
}

/// Formats byte sizes concisely for display (e.g. `128 MB`, `1.4 GB`).
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}
