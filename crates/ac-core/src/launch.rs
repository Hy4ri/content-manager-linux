//! Starting `acs.exe`: directly on Windows, through Proton on Linux.
//!
//! Like Content Manager's direct starters we skip Kunos' launcher and run
//! `acs.exe` with `SteamAppId` set so the Steam API initializes. Steam itself
//! must be running.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use anyhow::{Context, Result, bail};

use crate::{AC_APP_ID, ini::IniFile, paths, race::QuickDrive};

#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// Linux only: a specific `proton` script. Auto-detected when `None`.
    pub proton: Option<PathBuf>,
}

/// Writes race.ini for `q` into AC's Documents folder. Returns the path.
pub fn write_race_ini(ac_root: &Path, q: &QuickDrive) -> Result<PathBuf> {
    let path = paths::race_ini_path(ac_root).context(
        "can't find AC's Documents folder. On Linux, start Assetto Corsa from Steam once so Proton creates its prefix.",
    )?;
    let mut ini = IniFile::load_or_default(&path)?;
    if ini.sections().is_empty() {
        // First run: start from Kunos' template so every section AC expects exists.
        ini = IniFile::load_or_default(&ac_root.join("cfg").join("race.ini"))?;
    }
    crate::race::apply(&mut ini, q);
    ini.save(&path)?;
    Ok(path)
}

pub fn start(ac_root: &Path, opts: &LaunchOptions) -> Result<Child> {
    let acs = ac_root.join("acs.exe");
    if !acs.is_file() {
        bail!("acs.exe not found in {}", ac_root.display());
    }
    let app_id = AC_APP_ID.to_string();
    let mut cmd = if cfg!(windows) {
        let _ = opts;
        Command::new(&acs)
    } else {
        let proton = match &opts.proton {
            Some(p) => p.clone(),
            None => find_proton(ac_root).context(
                "no Proton found. Set Assetto Corsa to use Proton in Steam and run it once, or pick the proton script in Settings.",
            )?,
        };
        let compat = paths::compat_data_dir(ac_root)
            .context("no Proton prefix for Assetto Corsa yet; run it from Steam once")?;
        let steam_root = paths::steam_roots().into_iter().next().context("Steam install not found")?;
        let runtime = find_steam_run();
        if Path::new("/etc/NIXOS").exists() && runtime.is_none() {
            bail!("steam-run is required on NixOS; enable/install it alongside Steam");
        }
        let mut c = proton_command(&acs, &proton, runtime.as_deref());
        // A Nix dev shell exports ~20 KB of stdenv build variables. Windows caps the
        // environment block at 32 KB, so Wine's steam.exe fails to create acs.exe
        // (error 731) when the launcher inherits them. Keep the system-level Nix vars
        // (nix-ld, profiles) and drop the rest.
        const KEEP: [&str; 5] = ["NIX_LD", "NIX_LD_LIBRARY_PATH", "NIX_PATH", "NIX_PROFILES", "NIX_USER_PROFILE_DIR"];
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            let build_var = (name.starts_with("NIX") && !KEEP.contains(&&*name)) || name == "IN_NIX_SHELL";
            if build_var {
                c.env_remove(key);
            }
        }
        c.env("STEAM_COMPAT_DATA_PATH", compat)
            .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", steam_root)
            .env("STEAM_COMPAT_APP_ID", &app_id);

        let sdl_dirs = paths::steam_sdl_lib_dirs();
        if let Ok(joined) = std::env::join_paths(&sdl_dirs) {
            if !joined.is_empty() {
                c.env("LD_LIBRARY_PATH", joined);
            }
        }
        if std::env::var_os("PROTON_DISABLE_HIDRAW").is_none() {
            c.env(
                "PROTON_DISABLE_HIDRAW",
                "0x054C/0x05C4,0x054C/0x09CC,0x054C/0x0BA0,0x054C/0x0CE6,0x0DF2,0x057E/0x2009",
            );
        }
        // Custom Shaders Patch hooks in as `dwrite.dll` next to acs.exe; Wine only
        // loads it over its builtin with a native-first override (what users set as
        // the Steam launch option). Keep any overrides the user already passes.
        if ac_root.join("dwrite.dll").is_file() {
            let user = std::env::var("WINEDLLOVERRIDES").unwrap_or_default();
            if !user.split(';').any(|o| o.trim_start().starts_with("dwrite")) {
                let all = if user.is_empty() { "dwrite=n,b".to_owned() } else { format!("{user};dwrite=n,b") };
                c.env("WINEDLLOVERRIDES", all);
            }
        }
        c
    };
    cmd.current_dir(ac_root).env("SteamAppId", &app_id).env("SteamGameId", &app_id);
    cmd.spawn().with_context(|| format!("starting {}", acs.display()))
}

/// Finds the Proton build Steam last used for AC (from the prefix's
/// `config_info`), falling back to the last installed path alphabetically.
pub fn find_proton(ac_root: &Path) -> Option<PathBuf> {
    if let Some(compat) = paths::compat_data_dir(ac_root) {
        if let Ok(info) = std::fs::read_to_string(compat.join("config_info")) {
            for line in info.lines() {
                let p = Path::new(line.trim());
                if let Some(dir) = p.ancestors().find(|a| a.join("proton").is_file()) {
                    return Some(dir.join("proton"));
                }
            }
        }
    }
    let mut found = Vec::new();
    for root in paths::steam_roots() {
        for dir in [root.join("compatibilitytools.d"), root.join("steamapps").join("common")] {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let script = e.path().join("proton");
                if script.is_file() {
                    found.push(script);
                }
            }
        }
    }
    found.sort();
    found.pop()
}

/// Steam's FHS runtime wrapper, available on NixOS. This is not SteamCMD
/// (the server downloader), nor `steam -applaunch` (which opens Kunos' launcher).
pub fn find_steam_run() -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|p| p.join("steam-run")).find(|p| p.is_file()))
}

fn proton_command(acs: &Path, proton: &Path, runtime: Option<&Path>) -> Command {
    let mut cmd = if let Some(runtime) = runtime {
        let mut c = Command::new(runtime);
        c.arg(proton);
        // The app's GPUI library paths are not the game's runtime. Let
        // steam-run provide its own libraries instead of leaking the Nix shell.
        c.env_remove("LD_LIBRARY_PATH").env_remove("LD_PRELOAD");
        c
    } else {
        Command::new(proton)
    };
    cmd.arg("run").arg(acs);
    cmd
}
