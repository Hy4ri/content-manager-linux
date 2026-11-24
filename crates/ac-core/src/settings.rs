//! Persistent app settings (`<config dir>/cm-rs/settings.toml`).
//! Legacy JSON is migrated on first load and retained as a backup.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::race::QuickDrive;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickerView {
    GridAuto,
    Grid3,
    Grid4,
    #[default]
    Grid5,
    List,
}

impl PickerView {
    pub fn is_list(self) -> bool {
        matches!(self, Self::List)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::GridAuto => "Grid",
            Self::Grid3 => "Grid · 3",
            Self::Grid4 => "Grid · 4",
            Self::Grid5 => "Grid · 5",
            Self::List => "List",
        }
    }

    pub fn id(self) -> usize {
        match self {
            Self::GridAuto => 0,
            Self::Grid3 => 1,
            Self::Grid4 => 2,
            Self::Grid5 => 3,
            Self::List => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ac_root: Option<PathBuf>,
    pub driver_name: String,
    pub nationality: String,
    /// Linux: explicit `proton` script; auto-detected when unset.
    pub proton: Option<PathBuf>,
    pub last_quick_drive: Option<QuickDrive>,
    /// Browser archive storage; unset uses the application's XDG cache directory.
    pub download_dir: Option<PathBuf>,
    pub car_picker_view: PickerView,
    pub track_picker_view: PickerView,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ac_root: None,
            driver_name: "Player".into(),
            nationality: "ITA".into(),
            proton: None,
            last_quick_drive: None,
            download_dir: None,
            car_picker_view: PickerView::default(),
            track_picker_view: PickerView::default(),
        }
    }
}

pub fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("cm-rs").join("settings.toml"))
}

impl Settings {
    pub fn load() -> Result<Self> {
        Self::load_from(&path().context("no config directory")?)
    }

    fn load_from(p: &std::path::Path) -> Result<Self> {
        match fs::read_to_string(p) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("reading {}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let legacy = p.with_extension("json");
                match fs::read_to_string(&legacy) {
                    Ok(text) => {
                        let settings: Self = serde_json::from_str(&text)
                            .with_context(|| format!("reading legacy {}", legacy.display()))?;
                        settings.save_to(p)?;
                        Ok(settings)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
                    Err(e) => Err(e).with_context(|| format!("reading {}", legacy.display())),
                }
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&path().context("no config directory")?)
    }

    fn save_to(&self, p: &std::path::Path) -> Result<()> {
        use std::io::Write;
        // Never replace an invalid hand-edited file with fallback defaults.
        if p.exists() {
            Self::load_from(p)?;
        }
        let parent = p.parent().context("settings path has no parent")?;
        fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        temp.as_file().sync_all()?;
        temp.persist(p).with_context(|| format!("writing {}", p.display()))?;
        Ok(())
    }
}
