//! Installing cars and tracks from mod archives (zip, 7z, rar).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Zip,
    SevenZ,
    Rar,
}

/// Sniffs the format from the file signature; extensions on downloads lie.
fn format(archive: &Path) -> Result<Format> {
    let mut magic = [0u8; 8];
    let mut file = File::open(archive)
        .with_context(|| format!("cannot open {}", archive.display()))?;
    let n = file.read(&mut magic)?;
    let magic = &magic[..n];
    if magic.starts_with(b"PK\x03\x04") || magic.starts_with(b"PK\x05\x06") {
        Ok(Format::Zip)
    } else if magic.starts_with(b"7z\xBC\xAF\x27\x1C") {
        Ok(Format::SevenZ)
    } else if magic.starts_with(b"Rar!\x1A\x07") {
        Ok(Format::Rar)
    } else {
        bail!("unsupported archive format (expected .zip, .7z or .rar)")
    }
}

fn open_7z(archive: &Path) -> Result<sevenz_rust2::ArchiveReader<File>> {
    sevenz_rust2::ArchiveReader::open(archive, sevenz_rust2::Password::empty())
        .context("cannot read 7z archive (password-protected archives are not supported)")
}

fn rar_name(header: &unrar::FileHeader) -> String {
    header.filename.to_string_lossy().into_owned()
}

/// RAR is the only backend that writes files itself, and it recreates links;
/// skip them so extraction never creates a symlink inside game content.
fn rar_is_link(header: &unrar::FileHeader) -> bool {
    const UNIX_TYPE: u32 = 0o170000;
    const UNIX_LINK: u32 = 0o120000;
    const WIN_REPARSE_POINT: u32 = 0x400;
    header.file_attr & UNIX_TYPE == UNIX_LINK || header.file_attr & WIN_REPARSE_POINT != 0
}

fn entry_names(archive: &Path) -> Result<Vec<String>> {
    match format(archive)? {
        Format::Zip => {
            let mut zip =
                zip::ZipArchive::new(File::open(archive)?).context("not a zip archive")?;
            (0..zip.len())
                .map(|i| archive_name(zip.by_index(i)?.name()))
                .collect()
        }
        Format::SevenZ => open_7z(archive)?
            .archive()
            .files
            .iter()
            .filter(|e| !e.is_anti_item())
            .map(|e| archive_name(e.name()))
            .collect(),
        Format::Rar => unrar::Archive::new(archive)
            .open_for_listing()
            .context("cannot read rar archive")?
            .map(|header| {
                let header = header.context("cannot read rar archive")?;
                if header.is_encrypted() {
                    bail!("password-protected archives are not supported");
                }
                archive_name(&rar_name(&header))
            })
            .collect(),
    }
}

pub fn plan(archive: &Path, ac_root: &Path) -> Result<Vec<Item>> {
    let names = entry_names(archive)?;
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

/// Where an archive entry goes, or None when it is outside every planned item.
/// Refuses to write through a symlink already present in the destination.
fn destination(name: &str, items: &[Item]) -> Result<Option<PathBuf>> {
    for item in items {
        let rel = if item.prefix.is_empty() {
            Some(name)
        } else {
            name.strip_prefix(&item.prefix)
                .and_then(|r| r.strip_prefix('/'))
        };
        let Some(rel) = rel.filter(|r| !r.is_empty()) else {
            continue;
        };
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
        return Ok(Some(item.target.join(rel)));
    }
    Ok(None)
}

fn write_file(out: &Path, data: &mut dyn Read) -> Result<()> {
    if let Some(p) = out.parent() {
        fs::create_dir_all(p)?;
    }
    io::copy(data, &mut File::create(out)?)?;
    Ok(())
}

/// Extracts the planned items. Existing files are overwritten (an update).
pub fn install(archive: &Path, items: &[Item]) -> Result<usize> {
    let mut written = 0;
    match format(archive)? {
        Format::Zip => {
            let mut zip = zip::ZipArchive::new(File::open(archive)?)?;
            for i in 0..zip.len() {
                let mut f = zip.by_index(i)?;
                let Some(out) = destination(&archive_name(f.name())?, items)? else {
                    continue;
                };
                if f.is_dir() {
                    fs::create_dir_all(&out)?;
                } else {
                    write_file(&out, &mut f)?;
                    written += 1;
                }
            }
        }
        Format::SevenZ => {
            let mut reader = open_7z(archive)?;
            // The callback must return a sevenz error; keep ours intact instead.
            let mut failure = None;
            let result = reader.for_each_entries_with_decision(|entry, data| {
                if entry.is_anti_item() {
                    return Ok(sevenz_rust2::EntryDecision::Continue);
                }
                let step = (|| -> Result<()> {
                    let Some(out) = destination(&archive_name(entry.name())?, items)? else {
                        return Ok(());
                    };
                    if entry.is_directory() {
                        fs::create_dir_all(&out)?;
                    } else {
                        write_file(&out, data)?;
                        written += 1;
                    }
                    Ok(())
                })();
                match step {
                    Ok(()) => Ok(sevenz_rust2::EntryDecision::Continue),
                    Err(e) => {
                        failure = Some(e);
                        Ok(sevenz_rust2::EntryDecision::Stop)
                    }
                }
            });
            if let Some(e) = failure {
                return Err(e);
            }
            result.context("cannot extract 7z archive")?;
        }
        Format::Rar => {
            let mut open = unrar::Archive::new(archive)
                .open_for_processing()
                .context("cannot read rar archive")?;
            while let Some(header) = open.read_header().context("cannot read rar archive")? {
                let entry = header.entry();
                if entry.is_encrypted() {
                    bail!("password-protected archives are not supported");
                }
                let out = if rar_is_link(entry) {
                    None
                } else {
                    destination(&archive_name(&rar_name(entry))?, items)?
                };
                open = match out {
                    Some(out) if entry.is_directory() => {
                        fs::create_dir_all(&out)?;
                        header.skip()?
                    }
                    Some(out) => {
                        if let Some(p) = out.parent() {
                            fs::create_dir_all(p)?;
                        }
                        let next = header
                            .extract_to(&out)
                            .with_context(|| format!("cannot extract {}", out.display()))?;
                        written += 1;
                        next
                    }
                    None => header.skip()?,
                };
            }
        }
    }
    Ok(written)
}
