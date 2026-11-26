#![allow(dead_code)]

//! Pure, UI-agnostic navigation and focus model for gamepad, keyboard, and mouse navigation.
//!
//! This module provides directional navigation across arbitrary multi-zone layouts
//! (lists, grids, toolbars, cards) and maps input device button glyphs / actions.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavAction {
    Up,
    Down,
    Left,
    Right,
    Confirm,
    Back,
    TabPrev,
    TabNext,
    SubPrev,
    SubNext,
    Secondary,
    Search,
    Primary,
    Menu,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PadKind {
    Xbox,
    PlayStation,
}

impl PadKind {
    pub fn input_device(self) -> InputDevice {
        match self {
            PadKind::Xbox => InputDevice::Xbox,
            PadKind::PlayStation => InputDevice::PlayStation,
        }
    }

    pub fn label(self, a: NavAction) -> &'static str {
        self.input_device().label(a)
    }

    /// Optional override for headless UI QA (`CM_RS_FORCE_PAD=xbox|ps`).
    pub fn from_env() -> Option<Self> {
        let v = std::env::var("CM_RS_FORCE_PAD").ok()?;
        match v.trim().to_ascii_lowercase().as_str() {
            "xbox" => Some(PadKind::Xbox),
            "ps" | "playstation" | "sony" | "dualsense" | "ds4" => Some(PadKind::PlayStation),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InputDevice {
    #[default]
    Mouse,
    Keyboard,
    Xbox,
    PlayStation,
}

impl InputDevice {
    /// Focus rings / indicators are shown for keyboard and gamepad, but hidden for mouse.
    pub fn shows_focus(self) -> bool {
        !matches!(self, InputDevice::Mouse)
    }

    /// Device whose button labels should be rendered in hints and tab bars.
    /// Whenever a controller is connected, its glyphs stay visible even after mouse movement.
    pub fn glyph_device(self, connected_pad: Option<PadKind>) -> InputDevice {
        match connected_pad {
            Some(pad) => pad.input_device(),
            None => self,
        }
    }

    /// Hint-bar glyph text per device:
    /// - Xbox: Confirm="A", Back="B", Secondary="X", Search="Y", Primary="Menu", Menu="View",
    ///   TabPrev="LB", TabNext="RB", SubPrev="LT", SubNext="RT", Up/Down/Left/Right="D-pad".
    /// - PlayStation: Confirm="✕", Back="○", Secondary="□", Search="△", Primary="Options", Menu="Share",
    ///   TabPrev="L1", TabNext="R1", SubPrev="L2", SubNext="R2", Up/Down/Left/Right="D-pad".
    /// - Keyboard: Confirm="Enter", Back="Esc", Secondary="X", Search="/", Primary="Ctrl+Enter", Menu="F1",
    ///   TabPrev="Q", TabNext="E", SubPrev="[", SubNext="]", Up/Down/Left/Right="Arrows".
    /// - Mouse: same labels as Keyboard.
    pub fn label(self, a: NavAction) -> &'static str {
        match self {
            InputDevice::Xbox => match a {
                NavAction::Confirm => "A",
                NavAction::Back => "B",
                NavAction::Secondary => "X",
                NavAction::Search => "Y",
                NavAction::Primary => "Menu",
                NavAction::Menu => "View",
                NavAction::TabPrev => "LB",
                NavAction::TabNext => "RB",
                NavAction::SubPrev => "LT",
                NavAction::SubNext => "RT",
                NavAction::Up | NavAction::Down | NavAction::Left | NavAction::Right => "D-pad",
            },
            InputDevice::PlayStation => match a {
                NavAction::Confirm => "✕",
                NavAction::Back => "○",
                NavAction::Secondary => "□",
                NavAction::Search => "△",
                NavAction::Primary => "Options",
                NavAction::Menu => "Share",
                NavAction::TabPrev => "L1",
                NavAction::TabNext => "R1",
                NavAction::SubPrev => "L2",
                NavAction::SubNext => "R2",
                NavAction::Up | NavAction::Down | NavAction::Left | NavAction::Right => "D-pad",
            },
            InputDevice::Keyboard | InputDevice::Mouse => match a {
                NavAction::Confirm => "Enter",
                NavAction::Back => "Esc",
                NavAction::Secondary => "X",
                NavAction::Search => "/",
                NavAction::Primary => "Ctrl+Enter",
                NavAction::Menu => "F1",
                NavAction::TabPrev => "Q",
                NavAction::TabNext => "E",
                NavAction::SubPrev => "[",
                NavAction::SubNext => "]",
                NavAction::Up | NavAction::Down | NavAction::Left | NavAction::Right => "Arrows",
            },
        }
    }
}

/// Map a gamepad button name (XInput names used by ac-core::gamepad: "A","B","X","Y",
/// "LSHOULDER","RSHOULDER","LTRIGGER","RTRIGGER","START","BACK","DPAD_UP","DPAD_DOWN",
/// "DPAD_LEFT","DPAD_RIGHT") to a NavAction.
/// A->Confirm, B->Back, X->Secondary, Y->Search, LSHOULDER->TabPrev, RSHOULDER->TabNext,
/// LTRIGGER->SubPrev, RTRIGGER->SubNext, START->Primary, BACK->Menu, dpad->directions; others None.
pub fn from_pad(name: &str) -> Option<NavAction> {
    match name {
        "A" => Some(NavAction::Confirm),
        "B" => Some(NavAction::Back),
        "X" => Some(NavAction::Secondary),
        "Y" => Some(NavAction::Search),
        "LSHOULDER" => Some(NavAction::TabPrev),
        "RSHOULDER" => Some(NavAction::TabNext),
        "LTRIGGER" => Some(NavAction::SubPrev),
        "RTRIGGER" => Some(NavAction::SubNext),
        "START" => Some(NavAction::Primary),
        "BACK" | "VIEW" => Some(NavAction::Menu),
        "DPAD_UP" => Some(NavAction::Up),
        "DPAD_DOWN" => Some(NavAction::Down),
        "DPAD_LEFT" => Some(NavAction::Left),
        "DPAD_RIGHT" => Some(NavAction::Right),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FocusPos {
    pub zone: usize,
    pub index: usize,
}

/// Static description of one focus zone on a page.
#[derive(Clone, Copy, Debug)]
pub struct ZoneSpec {
    pub len: usize,
    /// Number of columns >= 1. 1 = vertical list; for a horizontal row of N items, cols = N.
    pub cols: usize,
    /// Neighbouring zone indices when moving past an edge.
    pub up: Option<usize>,
    pub down: Option<usize>,
    pub left: Option<usize>,
    pub right: Option<usize>,
}

/// What a directional press did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Moved(FocusPos),
    /// No neighbour or edge hit without target; caller may treat Left/Right as value-edit.
    Stay,
}

/// Clamps a focus position into the valid bounds of the given zones.
///
/// If `zones` is empty, returns the default `FocusPos`.
/// If `pos.zone` is out of bounds, it is clamped to `zones.len() - 1`.
/// `pos.index` is clamped to `zone.len - 1` (or 0 if the zone is empty).
pub fn clamp(pos: FocusPos, zones: &[ZoneSpec]) -> FocusPos {
    if zones.is_empty() {
        return FocusPos::default();
    }
    let zone_idx = pos.zone.min(zones.len() - 1);
    let zone = &zones[zone_idx];
    let index = if zone.len == 0 {
        0
    } else {
        pos.index.min(zone.len - 1)
    };
    FocusPos {
        zone: zone_idx,
        index,
    }
}

/// Computes the landing index when entering `target_zone` from a given directional move.
///
/// - For `Up`: Entering from below into the bottom row. Keeps source column `src_col = src_pos.index % src_cols`
///   clamped to `target_cols - 1`, landing in target's bottom row and clamped to `target.len - 1`.
/// - For `Down`: Entering from above into the top row (row 0). Keeps `src_col` clamped to `target_cols - 1`
///   and clamped to `target.len - 1`.
/// - For `Left`: Moving left entering a zone to the left. Lands on the rightmost item of row 0 (or clamped to `target.len - 1`).
/// - For `Right`: Moving right entering a zone to the right. Lands on index 0.
fn landing_index(src_pos: FocusPos, src_zone: &ZoneSpec, target_zone: &ZoneSpec, action: NavAction) -> usize {
    if target_zone.len == 0 {
        return 0;
    }
    let src_cols = src_zone.cols.max(1);
    let target_cols = target_zone.cols.max(1);

    match action {
        NavAction::Up => {
            let col = (src_pos.index % src_cols).min(target_cols - 1);
            let num_rows = (target_zone.len + target_cols - 1) / target_cols;
            let bottom_row = num_rows.saturating_sub(1);
            let target_idx = bottom_row * target_cols + col;
            target_idx.min(target_zone.len - 1)
        }
        NavAction::Down => {
            let col = (src_pos.index % src_cols).min(target_cols - 1);
            col.min(target_zone.len - 1)
        }
        NavAction::Left => {
            // Last index of row 0
            let last_in_row0 = (target_cols - 1).min(target_zone.len - 1);
            last_in_row0
        }
        NavAction::Right => 0,
        _ => 0,
    }
}

/// Find the next non-empty zone in the given direction starting from `start_zone_idx`.
///
/// Follows the direction pointer chain across empty zones (`len == 0`).
/// Returns `None` if the chain terminates at `None` or enters a cycle.
fn find_target_zone(
    mut curr_zone_idx: usize,
    action: NavAction,
    zones: &[ZoneSpec],
) -> Option<usize> {
    let mut visited_count = 0;
    while visited_count < zones.len() {
        if curr_zone_idx >= zones.len() {
            return None;
        }
        let z = &zones[curr_zone_idx];
        let next = match action {
            NavAction::Up => z.up,
            NavAction::Down => z.down,
            NavAction::Left => z.left,
            NavAction::Right => z.right,
            _ => None,
        };
        match next {
            Some(next_idx) if next_idx < zones.len() => {
                if zones[next_idx].len > 0 {
                    return Some(next_idx);
                }
                // Skip empty zone
                curr_zone_idx = next_idx;
                visited_count += 1;
            }
            _ => return None,
        }
    }
    None
}

/// Advance focus based on a directional navigation action.
///
/// - Only handles `Up`, `Down`, `Left`, `Right`. Any other action returns `Step::Stay`.
/// - Within a zone, items are laid out row-major with `cols` columns.
/// - `Left` and `Right` move `index - 1` / `index + 1` strictly within the same row (no row wrapping).
/// - `Up` and `Down` move `index - cols` / `index + cols`.
/// - Moving past an edge jumps to the neighbouring zone in that direction.
/// - Empty zones (`len == 0`) are skipped if they have a further neighbour in the same direction.
/// - If `pos.zone` or `pos.index` is out of range, it is clamped first.
/// - Cols == 0 is guarded and treated as 1. Zero panics on any input.
pub fn step(pos: FocusPos, action: NavAction, zones: &[ZoneSpec]) -> Step {
    if zones.is_empty() {
        return Step::Stay;
    }

    // Only directional actions navigate focus
    if !matches!(action, NavAction::Up | NavAction::Down | NavAction::Left | NavAction::Right) {
        return Step::Stay;
    }

    let clamped = clamp(pos, zones);
    let zone = &zones[clamped.zone];

    if zone.len == 0 {
        // If current zone is empty, try to move to a non-empty neighbor in this direction
        if let Some(target_idx) = find_target_zone(clamped.zone, action, zones) {
            let target_zone = &zones[target_idx];
            let new_index = landing_index(clamped, zone, target_zone, action);
            return Step::Moved(FocusPos {
                zone: target_idx,
                index: new_index,
            });
        }
        return Step::Stay;
    }

    let cols = zone.cols.max(1);
    let curr_row = clamped.index / cols;
    let curr_col = clamped.index % cols;

    match action {
        NavAction::Left => {
            if curr_col > 0 {
                Step::Moved(FocusPos {
                    zone: clamped.zone,
                    index: clamped.index - 1,
                })
            } else if let Some(target_idx) = find_target_zone(clamped.zone, NavAction::Left, zones) {
                let target_zone = &zones[target_idx];
                let new_index = landing_index(clamped, zone, target_zone, NavAction::Left);
                Step::Moved(FocusPos {
                    zone: target_idx,
                    index: new_index,
                })
            } else {
                Step::Stay
            }
        }
        NavAction::Right => {
            let next_index = clamped.index + 1;
            let next_row = next_index / cols;
            if curr_col + 1 < cols && next_index < zone.len && next_row == curr_row {
                Step::Moved(FocusPos {
                    zone: clamped.zone,
                    index: next_index,
                })
            } else if let Some(target_idx) = find_target_zone(clamped.zone, NavAction::Right, zones) {
                let target_zone = &zones[target_idx];
                let new_index = landing_index(clamped, zone, target_zone, NavAction::Right);
                Step::Moved(FocusPos {
                    zone: target_idx,
                    index: new_index,
                })
            } else {
                Step::Stay
            }
        }
        NavAction::Up => {
            if clamped.index >= cols {
                Step::Moved(FocusPos {
                    zone: clamped.zone,
                    index: clamped.index - cols,
                })
            } else if let Some(target_idx) = find_target_zone(clamped.zone, NavAction::Up, zones) {
                let target_zone = &zones[target_idx];
                let new_index = landing_index(clamped, zone, target_zone, NavAction::Up);
                Step::Moved(FocusPos {
                    zone: target_idx,
                    index: new_index,
                })
            } else {
                Step::Stay
            }
        }
        NavAction::Down => {
            let next_index = clamped.index + cols;
            if next_index < zone.len {
                Step::Moved(FocusPos {
                    zone: clamped.zone,
                    index: next_index,
                })
            } else if let Some(target_idx) = find_target_zone(clamped.zone, NavAction::Down, zones) {
                let target_zone = &zones[target_idx];
                let new_index = landing_index(clamped, zone, target_zone, NavAction::Down);
                Step::Moved(FocusPos {
                    zone: target_idx,
                    index: new_index,
                })
            } else {
                Step::Stay
            }
        }
        _ => Step::Stay,
    }
}
