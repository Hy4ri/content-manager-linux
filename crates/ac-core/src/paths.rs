//! Locating Steam, the Assetto Corsa install and its Documents folder on
//! Windows and on Linux (Steam + Proton).

use std::fs;
use std::path::{Path, PathBuf};

use crate::AC_APP_ID;

/// Candidate Steam install roots, most likely first.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam") {
            if let Ok(p) = k.get_value::<String, _>("SteamPath") {
                roots.push(PathBuf::from(p.replace('/', "\\")));
            }
        }
        if let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\WOW6432Node\Valve\Steam") {
            if let Ok(p) = k.get_value::<String, _>("InstallPath") {
                roots.push(PathBuf::from(p));
            }
        }
        roots.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
    }

    #[cfg(not(windows))]
    if let Some(home) = dirs::home_dir() {
        for rel in [
            ".steam/steam",
            ".steam/root",
            ".local/share/Steam",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
            "snap/steam/common/.local/share/Steam",
        ] {
            roots.push(home.join(rel));
        }
    }

    let mut seen = Vec::<PathBuf>::new();
    roots
        .into_iter()
        .filter(|p| p.join("steamapps").is_dir())
        .filter(|p| {
            let canon = fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            if seen.contains(&canon) {
                false
            } else {
                seen.push(canon);
                true
            }
        })
        .collect()
}

/// Every Steam library folder (each contains `steamapps`).
pub fn steam_libraries() -> Vec<PathBuf> {
    let mut libs: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        let canon = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if p.join("steamapps").is_dir() && !libs.iter().any(|l| fs::canonicalize(l).unwrap_or_else(|_| l.clone()) == canon) {
            libs.push(p);
        }
    };
    for root in steam_roots() {
        push(root.clone());
        let vdf = root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(text) = fs::read_to_string(&vdf) {
            for path in parse_library_paths(&text) {
                push(path);
            }
        }
    }
    libs
}

/// Directories holding only `libSDL2-2.0.so.0` (64- and 32-bit), which Wine's
/// gamepad bus needs and `steam-run` doesn't provide. Only SDL2 is exposed:
/// exposing whole Steam runtime directories breaks Wine (old FreeType) and
/// `steam-run`'s bwrap (old libattr/libselinux). Symlinks live in the cache dir.
///
/// Search order per architecture:
/// 1. Active Steam Linux Runtime (Scout) in any Steam library
///    (`steamapps/common/SteamLinuxRuntime/steam-runtime/usr/lib/<arch>`),
///    which ships standalone SDL2 (unlike Sniper/SteamRT4's `sdl2-compat`
///    wrapper that needs container-only `libSDL3.so.0` deps).
/// 2. Legacy active `ubuntu12_32/steam-runtime/usr/lib/<arch>` if still present
///    (never `steam-runtime.old`).
/// 3. Standard system library directories for non-NixOS distros.
pub fn steam_sdl_lib_dirs() -> Vec<PathBuf> {
    #[cfg(windows)]
    return Vec::new();
    #[cfg(not(windows))]
    {
        let Some(stage) = dirs::cache_dir().map(|d| d.join("cm-rs").join("steam-sdl")) else {
            return Vec::new();
        };
        let libs = steam_libraries();
        let roots = steam_roots();
        let dirs_64 = sdl_candidate_dirs(&libs, &roots, "x86_64-linux-gnu", &[
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib64",
            "/usr/lib",
        ]);
        let dirs_32 = sdl_candidate_dirs(&libs, &roots, "i386-linux-gnu", &[
            "/usr/lib/i386-linux-gnu",
            "/usr/lib32",
            "/usr/lib",
        ]);
        stage_sdl_lib_dirs(&stage, &dirs_64, &dirs_32)
    }
}

#[cfg(not(windows))]
fn sdl_candidate_dirs(
    steam_libs: &[PathBuf],
    steam_roots: &[PathBuf],
    arch: &str,
    system_dirs: &[&str],
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for lib in steam_libs {
        dirs.push(lib.join("steamapps/common/SteamLinuxRuntime/steam-runtime/usr/lib").join(arch));
    }
    for root in steam_roots {
        dirs.push(root.join("ubuntu12_32/steam-runtime/usr/lib").join(arch));
    }
    for sys in system_dirs {
        dirs.push(PathBuf::from(sys));
    }
    dirs
}

/// Checks that `path` resolves to a readable regular ELF file with the expected
/// `EI_CLASS` (`2` for 64-bit, `1` for 32-bit). Rejects broken symlinks.
#[cfg(not(windows))]
fn is_valid_sdl2_elf(path: &Path, expected_elf_class: u8) -> bool {
    use std::io::Read;
    if !fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Ok(mut f) = fs::File::open(path) else {
        return false;
    };
    let mut hdr = [0u8; 5];
    f.read_exact(&mut hdr).is_ok() && hdr[..4] == *b"ELF" && hdr[4] == expected_elf_class
}

#[cfg(not(windows))]
pub(crate) fn stage_sdl_lib_dirs(
    stage: &Path,
    dirs_64: &[PathBuf],
    dirs_32: &[PathBuf],
) -> Vec<PathBuf> {
    const LIB: &str = "libSDL2-2.0.so.0";
    let mut out = Vec::new();
    for (sub, elf_class, candidates) in [("64", 2u8, dirs_64), ("32", 1u8, dirs_32)] {
        let dst = stage.join(sub);
        let link = dst.join(LIB);
        // Always remove any previous symlink first so stale or dangling links
        // never survive when a runtime path disappears.
        if fs::symlink_metadata(&link).is_ok() {
            let _ = fs::remove_file(&link);
        }
        let Some(target) = candidates
            .iter()
            .map(|dir| dir.join(LIB))
            .find(|p| is_valid_sdl2_elf(p, elf_class))
        else {
            continue;
        };
        if fs::create_dir_all(&dst).is_err() {
            continue;
        }
        if std::os::unix::fs::symlink(&target, &link).is_ok() {
            out.push(dst);
        }
    }
    out
}

/// Extracts `"path" "..."` values from `libraryfolders.vdf`.
pub fn parse_library_paths(vdf: &str) -> Vec<PathBuf> {
    vdf.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('"').collect();
            // `"path"  "C:\\Games\\Steam"` splits into ["", path, ws, value, ""].
            (parts.len() >= 5 && parts[1].eq_ignore_ascii_case("path"))
                .then(|| PathBuf::from(parts[3].replace("\\\\", "\\")))
        })
        .collect()
}

pub fn is_ac_root(p: &Path) -> bool {
    p.join("acs.exe").is_file() && p.join("content").join("cars").is_dir()
}

pub fn find_ac_root() -> Option<PathBuf> {
    steam_libraries()
        .into_iter()
        .map(|lib| lib.join("steamapps").join("common").join("assettocorsa"))
        .find(|p| is_ac_root(p))
}

/// `.../steamapps` for an AC root inside a Steam library.
pub fn steamapps_of(ac_root: &Path) -> Option<PathBuf> {
    let common = ac_root.parent()?;
    let steamapps = common.parent()?;
    (common.file_name()?.eq_ignore_ascii_case("common")).then(|| steamapps.to_path_buf())
}

/// The Proton compatdata folder for AC (contains `pfx`). Looks next to the
/// game first, then in every library.
pub fn compat_data_dir(ac_root: &Path) -> Option<PathBuf> {
    let id = AC_APP_ID.to_string();
    let near = steamapps_of(ac_root).map(|s| s.join("compatdata").join(&id));
    near.into_iter()
        .chain(steam_libraries().into_iter().map(|l| l.join("steamapps").join("compatdata").join(&id)))
        .find(|p| p.join("pfx").is_dir())
}

/// `Documents/Assetto Corsa`, where AC reads `cfg/race.ini` and writes results.
pub fn documents_ac_dir(ac_root: &Path) -> Option<PathBuf> {
    if cfg!(windows) {
        let _ = ac_root;
        Some(dirs::document_dir()?.join("Assetto Corsa"))
    } else {
        Some(
            compat_data_dir(ac_root)?
                .join("pfx/drive_c/users/steamuser/Documents/Assetto Corsa"),
        )
    }
}

pub fn race_ini_path(ac_root: &Path) -> Option<PathBuf> {
    Some(documents_ac_dir(ac_root)?.join("cfg").join("race.ini"))
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write_fake_elf(dir: &Path, name: &str, elf_class: u8) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        fs::write(&p, [0x7f, b'E', b'L', b'F', elf_class, 1, 1, 0]).unwrap();
        p
    }

    #[test]
    fn stage_sdl_resolves_valid_and_skips_wrong_arch_and_broken_links() {
        let tmp = tempfile::tempdir().unwrap();
        let stage = tmp.path().join("stage");
        let broken_dir = tmp.path().join("broken");
        let wrong_arch = tmp.path().join("wrong32");
        let good64 = tmp.path().join("good64");
        let good32 = tmp.path().join("good32");

        fs::create_dir_all(&broken_dir).unwrap();
        symlink(tmp.path().join("nonexistent.so"), broken_dir.join("libSDL2-2.0.so.0")).unwrap();
        write_fake_elf(&wrong_arch, "libSDL2-2.0.so.0", 1);
        let elf64 = write_fake_elf(&good64, "libSDL2-2.0.so.0", 2);
        let elf32 = write_fake_elf(&good32, "libSDL2-2.0.so.0", 1);

        let dirs = stage_sdl_lib_dirs(
            &stage,
            &[broken_dir.clone(), wrong_arch.clone(), good64.clone()],
            &[broken_dir, good32.clone()],
        );
        assert_eq!(dirs, vec![stage.join("64"), stage.join("32")]);
        assert_eq!(fs::read_link(stage.join("64/libSDL2-2.0.so.0")).unwrap(), elf64);
        assert_eq!(fs::read_link(stage.join("32/libSDL2-2.0.so.0")).unwrap(), elf32);
    }

    #[test]
    fn stage_sdl_cleans_stale_links_when_libraries_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let stage = tmp.path().join("stage");
        fs::create_dir_all(stage.join("64")).unwrap();
        fs::create_dir_all(stage.join("32")).unwrap();
        let stale64 = stage.join("64/libSDL2-2.0.so.0");
        let stale32 = stage.join("32/libSDL2-2.0.so.0");
        symlink("/retired/steam-runtime/64/libSDL2-2.0.so.0", &stale64).unwrap();
        symlink("/retired/steam-runtime/32/libSDL2-2.0.so.0", &stale32).unwrap();

        let missing = tmp.path().join("does-not-exist");
        let dirs = stage_sdl_lib_dirs(&stage, &[missing.clone()], &[missing]);
        assert!(dirs.is_empty());
        assert!(fs::symlink_metadata(&stale64).is_err());
        assert!(fs::symlink_metadata(&stale32).is_err());
    }
}
