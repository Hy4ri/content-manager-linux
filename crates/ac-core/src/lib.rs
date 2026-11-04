//! Core Assetto Corsa support shared by the GUI and CLI.
//!
//! Formats and behavior follow Content Manager (AcTools, MS-PL) and, for
//! `data.acd` key derivation, the AcTools.AcdEncryption module shipped inside
//! Content Manager 0.8.2782.

pub mod acd;
pub mod ai_line;
pub mod bookmarks;
pub mod browser_session;
pub mod content;
pub mod events;
pub mod game_cfg;
#[cfg(target_os = "linux")]
pub mod gamepad;
pub mod ini;
pub mod install;
pub mod json;
pub mod launch;
pub mod paths;
pub mod race;
pub mod settings;

/// Steam app id of Assetto Corsa.
pub const AC_APP_ID: u32 = 244210;
