//! Dark palette in the spirit of Content Manager's default theme.

use gpui::{Rgba, rgb, rgba};

// Radius / spacing / sizing constants
pub const RADIUS_SM: f32 = 6.0;
pub const RADIUS_MD: f32 = 10.0;
pub const MIN_TARGET_H: f32 = 40.0;

// Base canvas and surface tokens
pub fn bg() -> Rgba {
    rgb(0x0f1012)
}
pub fn surface() -> Rgba {
    rgb(0x17181b)
}
pub fn raised() -> Rgba {
    rgb(0x1f2125)
}
pub fn overlay() -> Rgba {
    rgb(0x26282d)
}
pub fn border() -> Rgba {
    rgb(0x2c2f35)
}

// Text & foreground tokens
pub fn text() -> Rgba {
    rgb(0xedeef0)
}
pub fn muted() -> Rgba {
    rgb(0x9a9da4)
}
pub fn faint() -> Rgba {
    rgb(0x6b6e75)
}

// Accent tokens
pub fn accent() -> Rgba {
    rgb(0xf73152)
}
pub fn accent_hover() -> Rgba {
    rgb(0xff5470)
}
pub fn on_accent() -> Rgba {
    rgb(0x111111)
}

// Focus token (accent with alpha ~0.35)
pub fn focus_glow() -> Rgba {
    rgba(0xf7315259)
}

// Semantic status tokens
pub fn ok() -> Rgba {
    rgb(0x5fc77a)
}
pub fn error() -> Rgba {
    // Amber, deliberately NOT red so errors never look like the accent
    rgb(0xffa23d)
}

// Input background
pub fn input_bg() -> Rgba {
    rgb(0x0b0c0d)
}

// Legacy name mappings
pub fn sidebar() -> Rgba {
    rgb(0x0b0c0d)
}
pub fn panel() -> Rgba {
    surface()
}
pub fn panel_hover() -> Rgba {
    raised()
}
pub fn selected() -> Rgba {
    overlay()
}
