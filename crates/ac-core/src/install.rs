//! Installing cars and tracks from mod archives (zip).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Car,
    Track,
}

impl Kind {
    pub fn folder(self) -> &'static str {
        match self {
            Kind::Car => "cars",
            Kind::Track => "tracks",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub kind: Kind,
    pub id: String,
    /// Directory inside the archive ("" for the archive root).
    pub prefix: String,
    pub target: PathBuf,
    pub exists: bool,
}

fn parent(p: &str) -> &str {
    p.rsplit_once('/').map(|(a, _)| a).unwrap_or("")
}

fn last(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Finds content roots in a list of archive paths (using `/`).
fn detect(names: &[String], archive_stem: &str) -> BTreeSet<(Kind, String, String)> {
    let mut roots = BTreeSet::new();
    for name in names {
        let lower = name.to_lowercase();
        let file = last(&lower);
        let dir = parent(name);
        let root = match file {
            "ui_car.json" if last(&dir.to_lowercase()) == "ui" => Some((Kind::Car, parent(dir))),
            "data.acd" => Some((Kind::Car, dir)),
            "ui_track.json" => {
                // ui/ui_track.json or ui/<layout>/ui_track.json
                let d = dir.to_lowercase();
                if last(&d) == "ui" {
                    Some((Kind::Track, parent(dir)))
                } else if last(parent(&d)) == "ui" {
                    Some((Kind::Track, parent(parent(dir))))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some((kind, prefix)) = root {
            let id = if prefix.is_empty() {
                archive_stem
            } else {
                last(prefix)
            };
            roots.insert((kind, id.to_owned(), prefix.to_owned()));
        }
    }
    // A car root inside a track root (or vice versa) is noise; keep outermost.
    let all: Vec<_> = roots.iter().cloned().collect();
    roots.retain(|(_, _, p)| {
        !all.iter().any(|(_, _, other)| {
            other != p && !other.is_empty() && p.starts_with(&format!("{other}/"))
        })
    });
    roots
}

fn archive_name(name: &str) -> Result<String> {
    let name = name.replace('\\', "/");
    if std::path::Path::new(&name).components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        bail!("unsafe archive path: {name}");
    }
    Ok(name)
}

pub fn plan_zip(archive: &Path, ac_root: &Path) -> Result<Vec<Item>> {
    let mut zip = zip::ZipArchive::new(File::open(archive)?).context("not a zip archive")?;
    let names: Vec<String> = (0..zip.len())
        .map(|i| archive_name(zip.by_index(i)?.name()))
        .collect::<Result<Vec<_>>>()?;
    let stem = archive
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let items: Vec<Item> = detect(&names, &stem)
        .into_iter()
        .map(|(kind, id, prefix)| {
            let mut components = Path::new(&id).components();
            if !matches!(components.next(), Some(std::path::Component::Normal(_)))
                || components.next().is_some()
            {
                bail!("unsafe content identifier: {id}");
            }
            let target = ac_root.join("content").join(kind.folder()).join(&id);
            Ok(Item {
                exists: target.exists(),
                kind,
                id,
                prefix,
                target,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if items.is_empty() {
        bail!("no cars or tracks found in {}", archive.display());
    }
    Ok(items)
}

/// Extracts the planned items. Existing files are overwritten (an update).
pub fn install_zip(archive: &Path, items: &[Item]) -> Result<usize> {
    let mut zip = zip::ZipArchive::new(File::open(archive)?)?;
    let mut written = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let name = archive_name(f.name())?;
        for item in items {
            let rel = if item.prefix.is_empty() {
                Some(name.as_str())
            } else {
                name.strip_prefix(&item.prefix)
                    .and_then(|r| r.strip_prefix('/'))
            };
            let Some(rel) = rel.filter(|r| !r.is_empty()) else {
                continue;
            };
            let out = item.target.join(rel);
            let mut checked = item.target.clone();
            for component in std::iter::once(None).chain(Path::new(rel).components().map(Some)) {
                if let Some(component) = component {
                    checked.push(component);
                }
                if fs::symlink_metadata(&checked)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    bail!(
                        "refusing to overwrite symlink destination: {}",
                        checked.display()
                    );
                }
            }
            if f.is_dir() {
                fs::create_dir_all(&out)?;
            } else {
                if let Some(p) = out.parent() {
                    fs::create_dir_all(p)?;
                }
                io::copy(&mut f, &mut File::create(&out)?)?;
                written += 1;
            }
            break;
        }
    }
    Ok(written)
}
