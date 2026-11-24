//! Secure native download helpers for the embedded CEF browser.
//!
//! Each download has a unique subdirectory in the configured download directory.
//! Authentication cookies and redirects are preserved because the transfer
//! is handled natively by Chromium's download machinery.

use crate::browser_types::{BrowserDownload, DownloadStatus};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

static ACTIVE_DOWNLOADS: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn download_base_dir(configured: Option<&Path>) -> PathBuf {
    configured.map(Path::to_path_buf).unwrap_or_else(|| {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("cm-rs/browser/downloads")
    })
}

pub fn finish_download(path: &Path) {
    if let Ok(mut active) = ACTIVE_DOWNLOADS.lock() {
        active.remove(path);
    }
}

/// Only direct mod archives (.zip, .7z, .rar) in cm-rs's uniquely named download folders are managed.
/// Never recurse into arbitrary folders or follow symlinks, even in a shared directory.
fn archive_paths(base: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let parts: Vec<_> = name.split('-').collect();
        if !entry.file_type()?.is_dir()
            || parts.len() != 4
            || !parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        for file in std::fs::read_dir(entry.path())? {
            let file = file?;
            if file.file_type()?.is_file()
                && file
                    .path()
                    .extension()
                    .is_some_and(|ext| {
                        ["zip", "7z", "rar"]
                            .iter()
                            .any(|known| ext.eq_ignore_ascii_case(known))
                    })
            {
                paths.push(file.path());
            }
        }
    }
    Ok(paths)
}

/// Returns the number and size of downloaded archives (not installed content).
/// When clearing, active downloads and all non-archive files are left alone.
pub fn manage_archives(base: &Path, clear: bool) -> std::io::Result<(usize, u64)> {
    let active = ACTIVE_DOWNLOADS
        .lock()
        .map_err(|_| std::io::Error::other("download lock poisoned"))?;
    let mut count = 0;
    let mut bytes = 0;
    for path in archive_paths(base)? {
        if active.contains(&path) {
            continue;
        }
        // Recheck the leaf before deleting; never follow an archive symlink.
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() {
            continue;
        }
        if clear {
            std::fs::remove_file(&path)?;
            if let Some(parent) = path.parent() {
                // Only remove the download folder if it is now empty.
                let _ = std::fs::remove_dir(parent);
            }
        }
        count += 1;
        bytes += metadata.len();
    }
    Ok((count, bytes))
}

/// Sanitizes a suggested filename so it cannot traverse directories or
/// contain forbidden path separators. Returns a single safe filename component.
pub fn sanitize_filename(suggested: &str) -> String {
    let clean = suggested.trim();
    if clean.is_empty() {
        return "download.bin".to_string();
    }

    // Extract file name component only (strip any directory slashes)
    let p = Path::new(clean);
    let name = match p.file_name() {
        Some(f) => f.to_string_lossy().to_string(),
        None => "download.bin".to_string(),
    };

    // Filter out path traversal indicators and control characters
    let filtered: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\' && *c != '\0')
        .collect();

    let final_name = filtered.trim_matches(|c| c == '.' || c == ' ');
    if final_name.is_empty() {
        "download.bin".to_string()
    } else {
        final_name.to_string()
    }
}

/// Computes the isolated, unique download target path for an active download.
pub fn target_download_path(id: u32, suggested_name: &str) -> std::io::Result<PathBuf> {
    #[cfg(unix)]
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    // Read on every new download so a saved folder change applies to open tabs too.
    let settings = ac_core::settings::Settings::load().map_err(std::io::Error::other)?;
    let base = download_base_dir(settings.download_dir.as_deref());
    let mut active = ACTIVE_DOWNLOADS
        .lock()
        .map_err(|_| std::io::Error::other("download lock poisoned"))?;
    std::fs::create_dir_all(&base)?;
    loop {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!("{}-{stamp}-{id}-{sequence}", std::process::id()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(&dir) {
            Ok(()) => {
                let path = dir.join(sanitize_filename(suggested_name));
                active.insert(path.clone());
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// One download list for the whole process, so every browser tab shows the same
/// progress no matter which tab started the transfer.
static DOWNLOADS: LazyLock<Mutex<Vec<BrowserDownload>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Returns a copy of every download that has not been dismissed.
pub fn shared_downloads() -> Vec<BrowserDownload> {
    DOWNLOADS.lock().map(|list| list.clone()).unwrap_or_default()
}

/// Removes a finished, cancelled or interrupted download from every tab's list.
/// In-flight downloads stay listed until CEF reports a terminal state.
pub fn dismiss_download(id: u32) {
    if let Ok(mut list) = DOWNLOADS.lock() {
        list.retain(|d| d.id != id || d.status == DownloadStatus::Downloading);
    }
}

/// Updates or records an item in the shared download list.
pub fn record_download(
    id: u32,
    name: String,
    path: PathBuf,
    received_bytes: i64,
    total_bytes: i64,
    status: DownloadStatus,
) {
    let Ok(mut downloads) = DOWNLOADS.lock() else {
        return;
    };
    if let Some(pos) = downloads.iter().position(|d| d.id == id) {
        downloads[pos].received_bytes = received_bytes;
        downloads[pos].total_bytes = total_bytes;
        downloads[pos].status = status;
        if !name.is_empty() {
            downloads[pos].name = name;
        }
        if !path.as_os_str().is_empty() {
            downloads[pos].path = path;
        }
    } else if status == DownloadStatus::Downloading || !name.is_empty() {
        // A late terminal update for a dismissed item must not resurrect it.
        downloads.push(BrowserDownload {
            id,
            name,
            path,
            received_bytes,
            total_bytes,
            status,
        });
    }
}
