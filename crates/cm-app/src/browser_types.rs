//! Shared, engine-independent state for the embedded browser.
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct BrowserFrame {
    pub width: u32,
    pub height: u32,
    /// Owned BGRA pixels copied from CEF's borrowed paint buffer.
    pub pixels: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadStatus {
    Downloading,
    Complete,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug)]
pub struct BrowserDownload {
    pub id: u32,
    pub name: String,
    pub path: PathBuf,
    pub received_bytes: i64,
    pub total_bytes: i64,
    pub status: DownloadStatus,
}

#[derive(Clone, Debug, Default)]
pub struct BrowserSnapshot {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub error: Option<String>,
}

/// CEF-compatible event flags, kept engine-independent for the GPUI view.
#[derive(Clone, Copy, Debug, Default)]
pub struct BrowserModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub command: bool,
    pub left_button: bool,
    pub middle_button: bool,
    pub right_button: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum BrowserMouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Debug)]
pub struct BrowserKey {
    pub key: String,
    pub text: Option<String>,
    pub down: bool,
    pub modifiers: BrowserModifiers,
}
