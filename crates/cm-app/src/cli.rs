//! Command-line tools, handy for scripting and headless checks.

use std::path::{Path, PathBuf};

use ac_core::{acd, content, install, paths};

const USAGE: &str = "\
usage: cm-rs [command]

With no command, opens the GUI.

  acd unpack <data.acd> [out_dir]   decrypt data.acd (default: <car>/data)
  acd pack <data_dir> [data.acd]    encrypt a data folder (default: <car>/data.acd)
  list cars|tracks|events [ac_root]        list installed content
  install <mod.zip> [ac_root]       install cars/tracks from a zip
  where                             show detected paths";

fn root(arg: Option<&&str>) -> anyhow::Result<PathBuf> {
    let configured = match arg {
        Some(arg) => Some(PathBuf::from(arg)),
        None => ac_core::settings::Settings::load()?.ac_root,
    };
    configured.or_else(paths::find_ac_root).ok_or_else(|| anyhow::anyhow!("Assetto Corsa not found; pass its folder"))
}

pub fn run(args: &[String]) -> i32 {
    match run_inner(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

fn run_inner(args: &[String]) -> anyhow::Result<()> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["acd", "unpack", file, rest @ ..] => {
            let file = Path::new(file);
            let out = rest.first().map(PathBuf::from).unwrap_or_else(|| file.with_file_name("data"));
            let n = acd::unpack(file, &out)?;
            println!("unpacked {n} files to {}", out.display());
        }
        ["acd", "pack", dir, rest @ ..] => {
            let dir = Path::new(dir);
            let out = rest.first().map(PathBuf::from).unwrap_or_else(|| dir.with_file_name("data.acd"));
            let n = acd::pack(dir, &out)?;
            println!("packed {n} files into {}", out.display());
        }
        ["list", "cars", rest @ ..] => {
            for c in content::scan_cars(&root(rest.first())?) {
                println!("{:<40} {}", c.id, c.name);
            }
        }
        ["list", "tracks", rest @ ..] => {
            let r = root(rest.first())?;
            for t in content::track_entries(&content::scan_tracks(&r)) {
                println!("{:<40} {}", t.key(), t.layout.name);
            }
        }
        ["list", "events", rest @ ..] => {
            let root = root(rest.first())?;
            let catalog = ac_core::events::scan(&root);
            let progress = ac_core::events::Progress::load(&root)?;
            println!("{} series / {} career events / {} special events", catalog.series.len(), catalog.series.iter().map(|s| s.events.len()).sum::<usize>(), catalog.special.len());
            for series in &catalog.series {
                println!("{}: {} — {}", series.id, series.name, if series.completed(&progress) { "complete" } else if series.unlocked(&progress) { "unlocked" } else { "locked" });
                for opponent in &series.opponents {
                    println!("  {}: {} / {} / AI {}%", opponent.id, opponent.name, opponent.car, opponent.level);
                }
                for event in &series.events {
                    println!("  {}: {} / {} / {} — medal {}", event.id, event.name, event.car, event.track, progress.get(&event.id).medal);
                }
            }
            for event in &catalog.special {
                println!("{}: {} / {} / {} — medal {}", event.id, event.name, event.car, event.track, progress.get(&event.id).medal);
            }
            for error in &catalog.errors { eprintln!("{error}"); }
            anyhow::ensure!(catalog.errors.is_empty(), "Catalog contains unreadable events");
        }
        ["install", zip, rest @ ..] => {
            let r = root(rest.first())?;
            let zip = Path::new(zip);
            let items = install::plan_zip(zip, &r)?;
            for i in &items {
                println!("{:?} {} -> {}{}", i.kind, i.id, i.target.display(), if i.exists { " (update)" } else { "" });
            }
            let n = install::install_zip(zip, &items)?;
            if let Err(e) = ac_core::mods::record_install(&r, zip, &items) {
                eprintln!("warning: couldn't update mod manifest: {e:#}");
            }
            println!("wrote {n} files");
        }
        ["where"] => {
            println!("steam roots: {:?}", paths::steam_roots());
            println!("libraries:   {:?}", paths::steam_libraries());
            let r = paths::find_ac_root();
            println!("ac root:     {r:?}");
            if let Some(r) = r {
                println!("race.ini:    {:?}", paths::race_ini_path(&r));
                println!("compatdata:  {:?}", paths::compat_data_dir(&r));
                println!("proton:      {:?}", ac_core::launch::find_proton(&r));
                println!("steam-run:   {:?}", ac_core::launch::find_steam_run());
                println!("settings:    {:?}", ac_core::settings::path());
            }
        }
        _ => println!("{USAGE}"),
    }
    Ok(())
}
