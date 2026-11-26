# Design System & UI Guidelines

This document defines the visual language, layout rules, and interaction patterns for `cm-rs`. All new screens and UI refactors should follow these conventions so the app feels cohesive across mouse, keyboard, and gamepad input.

## 1. Visual Tokens

Tokens live in `crates/cm-app/src/theme.rs`. Use token functions instead of hardcoding colors or radii.

### Surfaces & Borders
| Token | Hex | Role |
|---|---|---|
| `bg()` | `#0f1012` | App canvas and inset wells (such as medal rows and empty image frames) |
| `surface()` / `panel()` | `#17181b` | Primary cards, header panels, and section containers |
| `raised()` / `panel_hover()` | `#1f2125` | Hover states and secondary badges |
| `overlay()` / `selected()` | `#26282d` | Elevated chips, series codes, and active selections |
| `border()` | `#2c2f35` | 1 px panel, button, and divider borders |
| `input_bg()` | `#0b0c0d` | Text input fields |

### Text & Semantic Status
| Token | Hex | Role |
|---|---|---|
| `text()` | `#edeef0` | Primary titles, labels, and values |
| `muted()` | `#9a9da4` | Secondary metadata, descriptions, and field labels |
| `faint()` | `#6b6e75` | Disabled / locked hints and subtle dividers |
| `accent()` | `#f73152` | Primary CTAs, active tabs/pills, and focus rings |
| `accent_hover()` | `#ff5470` | Hover state for primary CTAs |
| `on_accent()` | `#111111` | High-contrast dark text rendered on `accent()` or bright status pills |
| `on_accent_divider()` | `rgba(17, 17, 17, 0.25)` | Subtle divider inside primary accent buttons |
| `ok()` | `#5fc77a` | Completed states, earned medals, and positive status |
| `error()` | `#ffa23d` | Errors and warnings (amber, never red, so errors never read as selected accent) |
| `medal_bronze()` / `medal_silver()` / `medal_gold()` | `#c87a4b` / `#c4c8cf` / `#e8bd4f` | Career and Special Event medal tiers |
| `scrim()` / `shadow()` | `rgba(0, 0, 0, 0.55)` / `rgba(0, 0, 0, 0.45)` | Modal backdrop scrim and dropdown shadow |

### Radii & Touch/Focus Targets
- `RADIUS_SM` (`6.0` px): buttons, chips, badges, inputs, and image frames.
- `RADIUS_MD` (`10.0` px): top-level panels and cards.
- `MIN_TARGET_H` (`40.0` px): minimum height for interactive buttons and chips.

## 2. Navigation & Input Model

- **Controller-first, keyboard/mouse native**: Top tab bar switches sections via `Q`/`E` or `LB`/`RB`. Keyboard and gamepad inputs map onto a single `pad_press` flow.
- **No fake pad support**: Never add an on-screen keyboard. Gamepads narrow lists with filter chips and sort controls; text search stays keyboard-only.
- **Focus rings follow the last input device**: Draw `focus_ring` only for keyboard/gamepad input, and hide it on mouse move. Reserve a transparent 2 px border when unfocused so focus never causes layout shift.
- **Multi-step drill-down over cramped split panes**: Prefer dedicated steps (for example, Career: Series menu -> Series events -> Event detail) rather than squeezing dense lists and detail info side-by-side.
- **Step-wise Back navigation**: `Esc` and gamepad `B` step back one level inside the active view (Detail -> Events -> Series) before falling back to Quick Drive.
- **Honest lock states**: Items gated by game progression rules (such as locked Career series or events) render visibly greyed out/muted with explicit unlock requirements and cannot be launched until unlocked.

## 3. Page & Card Layout Rules

### Unified Header Cards (Collection / List Screens)
- Combine back navigation, icon/code badge, title, item count/subtitle, progress/goal summary, action buttons (`Refresh`, `Restart`), and optional description inside **one unified header panel** rather than stacking multiple loose header bars.
- Place high-signal progress metrics (series goal, medal counts, championship points/rounds) on the **right side** of the header row, next to utility actions. Give them strong visual hierarchy (`text_base` bold for the goal, `text_sm` bold and color-coded for medals/points) so completion status is readable at a glance.

### Aligned Dashboard Grids (Detail Screens)
- **Full-width hero card on Row 1**: Place the event/item badges, earned status pill, title, and description on the left and a compact preview image on the right inside one full-width panel. Do not leave loose headings above one column while the other column starts with a tall card.
- **Paired horizontal rows**: Group detail cards into explicit rows (`Row 2`, `Row 3`, ...) so left and right cards share exact top and bottom edges.
  - Pair complementary content of similar visual weight (for example, `Event details` paired with `Medal targets`; `Session schedule` paired with a unified `Your progress | Series progress` card).
  - Combine closely related small summaries side-by-side inside one divided card (using a `border_l_1` divider) instead of stacking tiny single-metric boxes that leave dead space.
- **Keep secondary controls inside their owning card**: Place toggles like `Show race config` in the header row of the related card rather than floating between cards.
- **Responsive fallback**: Collapse paired rows into a single vertical column below the two-column breakpoint (`860 px` available width).

### Fast Peer Switching & Pinned Footers
- On detail screens belonging to a sequence (such as events in a series), provide both a top bar with `Prev` / `Next` buttons and a horizontal quick-jump strip showing each sibling's index, name, and earned medal/result.
- Keep the primary action (`Start event`, `Go!`) **right-aligned** in a pinned footer bar (~56 px tall) with contextual gamepad/keyboard hints on the left so the CTA never moves with scroll height.
- Center narrow form and settings pages inside a bounded max-width container so wide viewports do not stretch single-column controls across the screen.

## 4. GPUI 0.2.2 Implementation Rules

- **Images**: Always render previews through `widgets::picture(path, height)` with explicit parent width. Never use `img(..).size_full()`, which clips vertically.
- **Text truncation**: Give wrapped or truncated text rows explicit pixel widths (`line(text, max_w)` / `two_lines(..., Some(w))`).
- **Row height matching**: `gpui::Div` does not have an `items_stretch()` method; a standard `.flex()` row stretches children to match heights by default as long as `items_start()` / `items_center()` is not set on the row container.
- **No external GPUI widget crates**: Stay on `gpui 0.2.2` and local `widgets.rs` primitives (do not add `gpui-kit` or `gpui-component`).
- **Visual verification**: Verify every UI change with real renders (headless Sway screenshots or live Wayland run) and confirm no clipping, misalignment, or dead-code warnings before shipping.
