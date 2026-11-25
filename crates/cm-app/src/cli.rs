//! Command-line tools, handy for scripting and headless checks.

use std::path::{Path, PathBuf};

use ac_core::{acd, content, install, online, paths};

const USAGE: &str = "\
usage: cm-rs [command]

With no command, opens the GUI.

  acd unpack <data.acd> [out_dir]   decrypt data.acd (default: <car>/data)
  acd pack <data_dir> [data.acd]    encrypt a data folder (default: <car>/data.acd)
  list cars|tracks|events [ac_root]        list installed content
  install <mod.zip|7z|rar> [ac_root] install cars/tracks from an archive
  online list [count]               busiest lobby servers with ping
  online info <host:http_port>      one server: session, cars, free slots
  online join <host:http_port> <car> [password]
                                    join a server with your Drive settings
  where                             show detected paths";

fn steam_id() -> anyhow::Result<String> {
    online::steam_id().ok_or_else(|| anyhow::anyhow!("no Steam account found; sign in to Steam once"))
}

fn online_list(count: usize) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let mut servers = online::fetch_lobby(&steam_id()?)?;
    let fetched = started.elapsed();
    servers.sort_by_key(|s| std::cmp::Reverse(s.clients));
    let pinger = online::Pinger::start(servers.iter().filter_map(|s| s.udp_addr()).collect())?;
    while !pinger.is_done() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let pings = pinger.results();
    servers.sort_by(|a, b| b.clients.cmp(&a.clients).then_with(|| a.name.cmp(&b.name)));
    let with_players = servers.iter().filter(|s| s.clients > 0).count();
    println!("{} servers ({with_players} with players, {} answered ping), lobby {:.1}s, total {:.1}s", servers.len(), pings.len(), fetched.as_secs_f64(), started.elapsed().as_secs_f64());
    for s in servers.iter().take(count) {
        let ping = s.udp_addr().and_then(|a| pings.get(&a)).map(|ms| format!("{ms}ms")).unwrap_or_else(|| "-".into());
        let name: String = s.name.chars().take(48).collect();
        println!("{:>5} {:>3}/{:<3} {:<3}{:<21} {:<30} {}", ping, s.clients, s.max_clients, if s.password { "pw " } else { "" }, s.address(), s.track, name);
    }
    Ok(())
}

fn online_info(address: &str) -> anyhow::Result<()> {
    let (ip, port) = online::resolve(address)?;
    let s = online::server_info(&ip, port)?;
    println!("{}
  {} · {} · {}/{} drivers · {} · tcp {} udp {}", s.name, s.track, s.session.label(), s.clients, s.max_clients, if s.password { "password" } else { "open" }, s.tcp_port, s.udp_port);
    if let Some(p) = &s.powered_by { println!("  powered by {p}"); }
    let guid = steam_id()?;
    let slots = online::car_slots(&ip, port, &guid)?;
    for car in &s.cars {
        let of: Vec<_> = slots.iter().filter(|c| c.model.eq_ignore_ascii_case(car)).collect();
        println!("  {car:<40} {} free of {}", of.iter().filter(|c| !c.connected).count(), of.len());
    }
    match online::details(&ip, s.details_port(), &guid) {
        Ok(d) => println!("  CSP features: {}", if d.features.is_empty() { "none".into() } else { d.features.join(", ") }),
        Err(e) => println!("  no extended info ({e:#})"),
    }
    Ok(())
}

fn online_join(address: &str, car: &str, password: &str) -> anyhow::Result<()> {
    let settings = ac_core::settings::Settings::load()?;
    let r = root(None)?;
    let (ip, port) = online::resolve(address)?;
    let guid = steam_id()?;
    let s = online::server_info(&ip, port)?;
    anyhow::ensure!(s.pickup, "{} uses booking, which cm-rs doesn't support yet", s.name);
    anyhow::ensure!(!s.password || !password.is_empty(), "{} needs a password", s.name);
    let tracks = content::track_entries(&content::scan_tracks(&r));
    let (track, layout) = online::match_track(&s.track, tracks.iter().map(|t| (t.track_id.as_str(), t.layout.layout_id.as_deref())))
        .ok_or_else(|| anyhow::anyhow!("track {} isn't installed", s.track))?;
    let slots = online::car_slots(&ip, port, &guid)?;
    let slot = slots.iter().find(|c| c.model.eq_ignore_ascii_case(car) && !c.connected)
        .ok_or_else(|| anyhow::anyhow!("no free {car} on this server"))?;
    let installed = content::scan_cars(&r).into_iter().find(|c| c.id.eq_ignore_ascii_case(car))
        .ok_or_else(|| anyhow::anyhow!("car {car} isn't installed"))?;
    let skin = installed.skins.iter().find(|k| k.id.eq_ignore_ascii_case(&slot.skin)).or(installed.skins.first())
        .map(|k| k.id.clone()).unwrap_or_default();
    let features = online::details(&ip, s.details_port(), &guid).map(|d| d.features).unwrap_or_default();
    let base = settings.last_quick_drive.clone().unwrap_or_default();
    let mut q = online::quick_drive_for(&base, &installed.id, &skin, &track, layout.as_deref());
    q.driver_name = settings.driver_name.clone();
    q.nationality = settings.nationality.clone();
    let join = online::Join {
        server_ip: ip.clone(), tcp_port: s.tcp_port, http_port: port, server_name: s.name.clone(),
        car: installed.id.clone(), driver_name: settings.driver_name.clone(), guid, password: password.to_owned(), features,
    };
    ac_core::launch::write_online_race_ini(&r, &q, &join)?;
    let since = std::time::SystemTime::now();
    let opts = ac_core::launch::LaunchOptions { proton: settings.proton.clone(), input: settings.proton_input.clone() };
    println!("joining {} with {}/{skin}…", s.name, installed.id);
    let status = ac_core::launch::start(&r, &opts)?.wait()?;
    let outcome = online::join_outcome(&r, since);
    online::clear_remote(&r)?;
    println!("game exited ({status}); {}", outcome.map(|o| o.message()).unwrap_or_else(|| "no game log for this launch".into()));
    Ok(())
}

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
            let items = install::plan(zip, &r)?;
            for i in &items {
                println!("{:?} {} -> {}{}", i.kind, i.id, i.target.display(), if i.exists { " (update)" } else { "" });
            }
            let n = install::install(zip, &items)?;
            if let Err(e) = ac_core::mods::record_install(&r, zip, &items) {
                eprintln!("warning: couldn't update mod manifest: {e:#}");
            }
            println!("wrote {n} files");
        }
        ["online", "list", rest @ ..] => online_list(rest.first().map(|n| n.parse()).transpose()?.unwrap_or(30))?,
        ["online", "info", address] => online_info(address)?,
        ["online", "join", address, car, rest @ ..] => online_join(address, car, rest.first().copied().unwrap_or(""))?,
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
                println!("sdl2 dirs:   {:?}", paths::steam_sdl_lib_dirs());
                println!("settings:    {:?}", ac_core::settings::path());
            }
        }
        _ => println!("{USAGE}"),
    }
    Ok(())
}
