//! Starting `acs.exe`: directly on Windows, through Proton on Linux.
//!
//! Like Content Manager's direct starters we skip Kunos' launcher and run
//! `acs.exe` with `SteamAppId` set so the Steam API initializes. Steam itself
//! must be running.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use anyhow::{Context, Result, bail};

use crate::{AC_APP_ID, ini::IniFile, paths, race::QuickDrive, settings::ProtonInputSettings};

#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// Linux only: a specific `proton` script. Auto-detected when `None`.
    pub proton: Option<PathBuf>,
    /// Linux only: Proton gamepad input flags.
    pub input: ProtonInputSettings,
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
        let has_dbghelp_proxy = has_prefix_dbghelp_proxy(&compat);
        c.env("STEAM_COMPAT_DATA_PATH", compat)
            .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", steam_root)
            .env("STEAM_COMPAT_APP_ID", &app_id);

        let sdl_dirs = paths::steam_sdl_lib_dirs();
        let has_dwrite = ac_root.join("dwrite.dll").is_file();
        apply_proton_env(
            &mut c,
            &sdl_dirs,
            &opts.input,
            has_dwrite,
            has_dwrite && has_dbghelp_proxy,
            |k| std::env::var_os(k),
        );
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

const DEFAULT_DISABLE_HIDRAW: &str =
    "0x054C/0x05C4,0x054C/0x09CC,0x054C/0x0BA0,0x054C/0x0CE6,0x0DF2,0x057E/0x2009";

/// True when the user has installed a local `dbghelp.dll` proxy alongside
/// Wine's renamed `dbghelp_wine.dll` and `acs.publics.tsv` in the prefix.
/// Symlinks (such as Proton's stock link to its builtin) are ignored.
fn has_prefix_dbghelp_proxy(compat: &Path) -> bool {
    let sys32 = compat.join("pfx/drive_c/windows/system32");
    let proxy = sys32.join("dbghelp.dll");
    let is_regular_file = std::fs::symlink_metadata(&proxy).is_ok_and(|m| m.file_type().is_file());
    is_regular_file && sys32.join("dbghelp_wine.dll").is_file() && sys32.join("acs.publics.tsv").is_file()
}

fn has_dll_override(overrides: &str, dll: &str) -> bool {
    overrides.split(';').any(|entry| {
        let name = entry.trim_start();
        name.strip_prefix(dll)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
    })
}

fn apply_proton_env(
    c: &mut Command,
    sdl_dirs: &[PathBuf],
    input: &ProtonInputSettings,
    has_dwrite: bool,
    has_dbghelp_proxy: bool,
    env_get: impl Fn(&str) -> Option<OsString>,
) {
    let mut has_sdl = false;
    if let Ok(joined) = std::env::join_paths(sdl_dirs)
        && !joined.is_empty()
    {
        c.env("LD_LIBRARY_PATH", joined);
        has_sdl = true;
    }
    // Only request Proton's SDL input backend when we actually resolved a real
    // SDL2 library: PROTON_USE_SDL=1 disables hidraw/SteamInput in CachyOS/GE
    // Proton, which would leave pads dead if libSDL2 cannot be loaded.
    if has_sdl && input.use_sdl {
        if env_get("PROTON_USE_SDL").is_none() {
            c.env("PROTON_USE_SDL", "1");
        }
        if input.emulate_steam_input && env_get("PROTON_EMULATE_STEAMINPUT").is_none() {
            c.env("PROTON_EMULATE_STEAMINPUT", "1");
        }
    }
    if env_get("PROTON_DISABLE_HIDRAW").is_none() {
        c.env("PROTON_DISABLE_HIDRAW", DEFAULT_DISABLE_HIDRAW);
    }
    // Custom Shaders Patch hooks in as `dwrite.dll` next to acs.exe; Wine only
    // loads it over its builtin with a native-first override (what users set as
    // the Steam launch option). Likewise, if the prefix has a user-installed
    // dbghelp proxy, request native-first `dbghelp`. Keep any user overrides.
    let mut needed = Vec::new();
    if has_dwrite {
        needed.push(("dwrite", "dwrite=n,b"));
    }
    if has_dbghelp_proxy {
        needed.push(("dbghelp", "dbghelp=n,b"));
    }
    if !needed.is_empty() {
        let user = env_get("WINEDLLOVERRIDES")
            .and_then(|v| v.into_string().ok())
            .unwrap_or_default();
        let mut all = user;
        let mut changed = false;
        for (dll, rule) in needed {
            if !has_dll_override(&all, dll) {
                if !all.is_empty() {
                    all.push(';');
                }
                all.push_str(rule);
                changed = true;
            }
        }
        if changed {
            c.env("WINEDLLOVERRIDES", all);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsStr;

    fn env_map(cmd: &Command) -> HashMap<String, Option<String>> {
        cmd.get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|s: &OsStr| s.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn sets_sdl_and_steam_input_flags_when_sdl2_available() {
        let mut c = Command::new("proton");
        let sdl = vec![PathBuf::from("/cache/sdl/64"), PathBuf::from("/cache/sdl/32")];
        apply_proton_env(&mut c, &sdl, &ProtonInputSettings::default(), true, false, |_| None);
        let envs = env_map(&c);

        assert_eq!(envs.get("LD_LIBRARY_PATH").unwrap().as_deref(), Some("/cache/sdl/64:/cache/sdl/32"));
        assert_eq!(envs.get("PROTON_USE_SDL").unwrap().as_deref(), Some("1"));
        assert_eq!(envs.get("PROTON_EMULATE_STEAMINPUT").unwrap().as_deref(), Some("1"));
        assert_eq!(envs.get("PROTON_DISABLE_HIDRAW").unwrap().as_deref(), Some(DEFAULT_DISABLE_HIDRAW));
        assert_eq!(envs.get("WINEDLLOVERRIDES").unwrap().as_deref(), Some("dwrite=n,b"));
        assert!(!envs.contains_key("SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT"));
    }

    #[test]
    fn adds_dbghelp_override_only_when_prefix_proxy_present() {
        let mut c = Command::new("proton");
        let sdl = vec![PathBuf::from("/cache/sdl/64")];
        apply_proton_env(&mut c, &sdl, &ProtonInputSettings::default(), true, true, |_| None);
        assert_eq!(
            env_map(&c).get("WINEDLLOVERRIDES").unwrap().as_deref(),
            Some("dwrite=n,b;dbghelp=n,b")
        );
    }

    #[test]
    fn omits_sdl_flags_when_sdl2_missing() {
        let mut c = Command::new("proton");
        apply_proton_env(&mut c, &[], &ProtonInputSettings::default(), false, false, |_| None);
        let envs = env_map(&c);

        assert!(!envs.contains_key("LD_LIBRARY_PATH"));
        assert!(!envs.contains_key("PROTON_USE_SDL"));
        assert!(!envs.contains_key("PROTON_EMULATE_STEAMINPUT"));
        assert_eq!(envs.get("PROTON_DISABLE_HIDRAW").unwrap().as_deref(), Some(DEFAULT_DISABLE_HIDRAW));
    }

    #[test]
    fn respects_input_settings_toggles() {
        let sdl = vec![PathBuf::from("/cache/sdl/64")];

        let mut c1 = Command::new("proton");
        apply_proton_env(
            &mut c1,
            &sdl,
            &ProtonInputSettings { use_sdl: false, emulate_steam_input: true },
            false,
            false,
            |_| None,
        );
        let e1 = env_map(&c1);
        assert!(!e1.contains_key("PROTON_USE_SDL"));
        assert!(!e1.contains_key("PROTON_EMULATE_STEAMINPUT"));

        let mut c2 = Command::new("proton");
        apply_proton_env(
            &mut c2,
            &sdl,
            &ProtonInputSettings { use_sdl: true, emulate_steam_input: false },
            false,
            false,
            |_| None,
        );
        let e2 = env_map(&c2);
        assert_eq!(e2.get("PROTON_USE_SDL").unwrap().as_deref(), Some("1"));
        assert!(!e2.contains_key("PROTON_EMULATE_STEAMINPUT"));
    }

    #[test]
    fn preserves_user_env_overrides() {
        let sdl = vec![PathBuf::from("/cache/sdl/64")];
        let mut user = HashMap::new();
        user.insert("PROTON_USE_SDL", OsString::from("0"));
        user.insert("PROTON_EMULATE_STEAMINPUT", OsString::from("0"));
        user.insert("PROTON_DISABLE_HIDRAW", OsString::from("0"));
        user.insert("WINEDLLOVERRIDES", OsString::from("dbghelp=b"));

        let mut c = Command::new("proton");
        apply_proton_env(&mut c, &sdl, &ProtonInputSettings::default(), true, true, |k| {
            user.get(k).cloned()
        });
        let envs = env_map(&c);

        // User-supplied flags are inherited rather than overwritten.
        assert!(!envs.contains_key("PROTON_USE_SDL"));
        assert!(!envs.contains_key("PROTON_EMULATE_STEAMINPUT"));
        assert!(!envs.contains_key("PROTON_DISABLE_HIDRAW"));
        // User's dbghelp override is preserved alongside automatic dwrite override.
        assert_eq!(envs.get("WINEDLLOVERRIDES").unwrap().as_deref(), Some("dbghelp=b;dwrite=n,b"));

        // If user already passed both overrides, leave WINEDLLOVERRIDES untouched.
        user.insert("WINEDLLOVERRIDES", OsString::from("dbghelp=n,b;dwrite=b"));
        let mut c2 = Command::new("proton");
        apply_proton_env(&mut c2, &sdl, &ProtonInputSettings::default(), true, true, |k| {
            user.get(k).cloned()
        });
        assert!(!env_map(&c2).contains_key("WINEDLLOVERRIDES"));
    }
}
