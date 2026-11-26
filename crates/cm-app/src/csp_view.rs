//! Custom Shaders Patch settings under Settings → CSP.
//!
//! Two levels: a grid of CSP modules, and one module's options. Every option and
//! its control come from CSP's own annotated config files (`ac_core::csp`), so new
//! CSP releases show up without changes here.

use std::collections::HashMap;
use std::path::PathBuf;

use ac_core::csp::{self, CspConfig, Item, Kind};
use ac_core::game_cfg::fmt_number;
use gpui::{
    AnyElement, ClickEvent, Context, Div, Entity, FontWeight, Render, ScrollHandle, SharedString,
    Stateful, Window, div, prelude::*, px,
};

use crate::nav::{InputDevice, NavAction, PadKind};
use crate::text_input::TextInput;
use crate::theme;
use crate::widgets::{button, button_pad, chip, focus_ring, panel, primary_button_pad, segmented};

const MAX_W: f32 = 1120.;
const CARD_MIN_W: f32 = 250.;
const CARD_H: f32 = 116.;
const GAP: f32 = 12.;
const LABEL_W: f32 = 280.;

/// One visible option row: section index and item index inside it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct RowRef {
    section: usize,
    item: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Top,
    Middle,
    Bottom,
}

pub struct CspView {
    pub root: PathBuf,
    cfg: Option<CspConfig>,
    error: Option<String>,
    open: Option<usize>,
    /// Module index in the grid, or row index inside an open module.
    focus: usize,
    hovered: Option<RowRef>,
    cols: usize,
    scroll: ScrollHandle,
    inputs: HashMap<RowRef, Entity<TextInput>>,
    input_device: InputDevice,
    connected_pad: Option<PadKind>,
    message: Option<(String, bool)>,
}

impl CspView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            root,
            cfg: None,
            error: None,
            open: None,
            focus: 0,
            hovered: None,
            cols: 3,
            scroll: ScrollHandle::new(),
            inputs: HashMap::new(),
            input_device: InputDevice::Mouse,
            connected_pad: None,
            message: None,
        };
        view.load(cx);
        view
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.cfg = None;
        self.error = None;
        self.open = None;
        self.focus = 0;
        self.inputs.clear();
        if csp::installed(&self.root) {
            match CspConfig::load(&self.root) {
                Ok(c) => self.cfg = Some(c),
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }
        cx.notify();
    }

    pub fn set_input_device(&mut self, device: InputDevice, cx: &mut Context<Self>) {
        if self.input_device != device {
            self.input_device = device;
            cx.notify();
        }
    }

    pub fn set_connected_pad(&mut self, pad: Option<PadKind>, cx: &mut Context<Self>) {
        if self.connected_pad != pad {
            self.connected_pad = pad;
            cx.notify();
        }
    }

    fn pad_label(&self, a: NavAction) -> Option<&'static str> {
        self.connected_pad.map(|p| p.label(a))
    }

    /// Whether a text field has keyboard focus (so typed keys are not navigation).
    pub fn input_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.inputs.values().any(|i| i.read(cx).is_focused(window))
    }

    fn rows(&self) -> Vec<RowRef> {
        let (Some(c), Some(m)) = (self.cfg.as_ref(), self.open) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (si, s) in c.modules[m].sections.iter().enumerate() {
            if !c.section_visible(m, s) {
                continue;
            }
            for (ii, item) in s.items.iter().enumerate() {
                if c.visible(m, item) {
                    out.push(RowRef {
                        section: si,
                        item: ii,
                    });
                }
            }
        }
        out
    }

    fn item(&self, r: RowRef) -> Option<&Item> {
        let c = self.cfg.as_ref()?;
        c.modules[self.open?]
            .sections
            .get(r.section)?
            .items
            .get(r.item)
    }

    fn row_enabled(&self, r: RowRef) -> bool {
        let (Some(c), Some(m)) = (self.cfg.as_ref(), self.open) else {
            return false;
        };
        let s = &c.modules[m].sections[r.section];
        c.section_enabled(m, s) && c.enabled(m, &s.items[r.item])
    }

    fn open_module(&mut self, m: usize, cx: &mut Context<Self>) {
        let Some(c) = self.cfg.as_ref() else { return };
        self.open = Some(m);
        self.focus = 0;
        self.hovered = None;
        self.message = None;
        self.inputs.clear();
        let mut texts = Vec::new();
        for (si, s) in c.modules[m].sections.iter().enumerate() {
            for (ii, item) in s.items.iter().enumerate() {
                if item.kind == Kind::Text {
                    texts.push((
                        RowRef {
                            section: si,
                            item: ii,
                        },
                        c.value(m, item).to_owned(),
                    ));
                }
            }
        }
        for (r, value) in texts {
            let input = cx.new(|cx| TextInput::new(cx, "", value));
            cx.observe(&input, move |this, input, cx| {
                let text = input.read(cx).text();
                let Some(m) = this.open else { return };
                let Some((section, key)) = this.item(r).map(|i| (i.section.clone(), i.key.clone()))
                else {
                    return;
                };
                if let Some(c) = this.cfg.as_mut()
                    && c.modules[m]
                        .sections
                        .get(r.section)
                        .and_then(|s| s.items.get(r.item))
                        .is_some_and(|i| c.value(m, i) != text)
                {
                    c.set(m, &section, &key, &text);
                    cx.notify();
                }
            })
            .detach();
            self.inputs.insert(r, input);
        }
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        cx.notify();
    }

    fn close_module(&mut self, cx: &mut Context<Self>) {
        if let Some(m) = self.open.take() {
            self.focus = m;
            self.inputs.clear();
            self.hovered = None;
            self.scroll.scroll_to_item(1 + m / self.cols.max(1));
            cx.notify();
        }
    }

    /// Re-reads text fields after a revert or reset changed values under them.
    fn sync_inputs(&mut self, cx: &mut Context<Self>) {
        let (Some(c), Some(m)) = (self.cfg.as_ref(), self.open) else {
            return;
        };
        let values: Vec<(Entity<TextInput>, String)> = self
            .inputs
            .iter()
            .filter_map(|(r, input)| {
                let item = c.modules[m].sections.get(r.section)?.items.get(r.item)?;
                Some((input.clone(), c.value(m, item).to_owned()))
            })
            .collect();
        for (input, v) in values {
            input.update(cx, |i, cx| {
                if i.text() != v {
                    i.set_text(v, cx);
                }
            });
        }
    }

    fn set_value(&mut self, r: RowRef, value: String, cx: &mut Context<Self>) {
        if !self.row_enabled(r) {
            return;
        }
        let Some(m) = self.open else { return };
        let Some((section, key)) = self.item(r).map(|i| (i.section.clone(), i.key.clone())) else {
            return;
        };
        if let Some(c) = self.cfg.as_mut() {
            c.set(m, &section, &key, &value);
        }
        self.message = None;
        cx.notify();
    }

    fn step(&mut self, r: RowRef, d: i32, cx: &mut Context<Self>) {
        if !self.row_enabled(r) {
            return;
        }
        let Some(m) = self.open else { return };
        let Some(item) = self.item(r).cloned() else {
            return;
        };
        if let Some(c) = self.cfg.as_mut() {
            c.step(m, &item, d);
        }
        self.message = None;
        cx.notify();
    }

    /// Option values in display order, for stepping choices with the D-pad.
    fn options(&self, item: &Item) -> Vec<String> {
        match &item.kind {
            Kind::Toggle { on, off } => vec![off.clone(), on.clone()],
            Kind::Choice(opts) => opts.iter().map(|(v, _)| v.clone()).collect(),
            Kind::Script { dir, file } => self
                .cfg
                .as_ref()
                .map(|c| c.scripts(dir, file))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Moves a toggle/choice/script by `d` options, or a range by `d` steps.
    fn adjust(&mut self, r: RowRef, d: i32, cx: &mut Context<Self>) {
        let (Some(c), Some(m), Some(item)) = (self.cfg.as_ref(), self.open, self.item(r)) else {
            return;
        };
        if matches!(item.kind, Kind::Range { .. }) {
            return self.step(r, d, cx);
        }
        let opts = self.options(item);
        if opts.is_empty() {
            return;
        }
        let cur = c.value(m, item);
        let at = opts
            .iter()
            .position(|o| o == cur)
            .map(|i| i as i32)
            .unwrap_or(-1);
        let next = if matches!(item.kind, Kind::Toggle { .. }) {
            (at + d).clamp(0, 1)
        } else {
            (at + d).rem_euclid(opts.len() as i32)
        };
        let v = opts[next as usize].clone();
        self.set_value(r, v, cx);
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.cfg.as_mut() else { return };
        self.message = Some(match c.save() {
            Ok(files) if files.is_empty() => ("Nothing to save.".into(), false),
            Ok(files) => (format!("Saved {}.", files.join(", ")), false),
            Err(e) => (format!("{e:#}"), true),
        });
        cx.notify();
    }

    fn revert(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.cfg.as_mut() {
            c.revert();
        }
        self.message = Some(("Reverted unsaved changes.".into(), false));
        self.sync_inputs(cx);
        cx.notify();
    }

    fn reset_module(&mut self, cx: &mut Context<Self>) {
        let Some(m) = self.open else { return };
        if let Some(c) = self.cfg.as_mut() {
            c.reset(m);
        }
        self.message = Some(("Module reset to CSP defaults. Save to apply.".into(), false));
        self.sync_inputs(cx);
        cx.notify();
    }

    /// Gamepad/keyboard input. Returns `false` to let the app handle it (tab walking, Back).
    pub fn pad(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let Some(c) = self.cfg.as_ref() else {
            return false;
        };
        if name == "START" {
            self.save(cx);
            return true;
        }
        if name == "X" && !c.dirty_files().is_empty() {
            self.revert(cx);
            return true;
        }
        let Some(m) = self.open else {
            if name == "Y" {
                self.load(cx);
                return true;
            }
            let n = c.modules.len();
            if n == 0 {
                return false;
            }
            let cols = self.cols.max(1);
            let next = match name {
                "DPAD_LEFT" => self.focus.saturating_sub(1),
                "DPAD_RIGHT" => (self.focus + 1).min(n - 1),
                "DPAD_UP" => self.focus.saturating_sub(cols),
                "DPAD_DOWN" => (self.focus + cols).min(n - 1),
                "A" => {
                    self.open_module(self.focus.min(n - 1), cx);
                    return true;
                }
                _ => return false,
            };
            self.focus = next;
            self.scroll.scroll_to_item(1 + next / cols);
            cx.notify();
            return true;
        };
        let rows = self.rows();
        let cur = rows
            .get(self.focus.min(rows.len().saturating_sub(1)))
            .copied();
        match name {
            "B" => {
                self.close_module(cx);
                true
            }
            "DPAD_UP" | "DPAD_DOWN" => {
                if rows.is_empty() {
                    return true;
                }
                self.focus = if name == "DPAD_UP" {
                    self.focus.saturating_sub(1)
                } else {
                    (self.focus + 1).min(rows.len() - 1)
                };
                self.scroll_to_row(rows[self.focus]);
                cx.notify();
                true
            }
            "DPAD_LEFT" | "DPAD_RIGHT" => {
                if let Some(r) = cur {
                    self.adjust(r, if name == "DPAD_LEFT" { -1 } else { 1 }, cx);
                }
                true
            }
            "A" => {
                if let Some(r) = cur {
                    let toggle = self
                        .item(r)
                        .is_some_and(|i| matches!(i.kind, Kind::Toggle { .. }));
                    if toggle {
                        let on = self.item(r).and_then(|i| match &i.kind {
                            Kind::Toggle { on, off } => {
                                let v = self.cfg.as_ref()?.value(m, i);
                                Some(if v == on { off.clone() } else { on.clone() })
                            }
                            _ => None,
                        });
                        if let Some(v) = on {
                            self.set_value(r, v, cx);
                        }
                    } else {
                        self.adjust(r, 1, cx);
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Scroll children of an open module: header, then per section a title block,
    /// one child per row, and a closing block. Mirrors `module_children`.
    fn scroll_to_row(&self, target: RowRef) {
        let (Some(c), Some(m)) = (self.cfg.as_ref(), self.open) else {
            return;
        };
        let mut ix = 1;
        for (si, s) in c.modules[m].sections.iter().enumerate() {
            if !c.section_visible(m, s) {
                continue;
            }
            ix += 1;
            for (ii, item) in s.items.iter().enumerate() {
                if !c.visible(m, item) {
                    continue;
                }
                if (RowRef {
                    section: si,
                    item: ii,
                }) == target
                {
                    self.scroll.scroll_to_item(ix);
                    return;
                }
                ix += 1;
            }
            ix += 1;
        }
    }

    // ---------- rendering ----------

    fn shows_focus(&self) -> bool {
        self.input_device.shows_focus()
    }

    fn header(&self, col_w: f32, cx: &mut Context<Self>) -> Div {
        let c = self.cfg.as_ref();
        let Some(m) = self.open else {
            let changed: usize = c
                .map(|c| (0..c.modules.len()).map(|i| c.changed(i)).sum())
                .unwrap_or(0);
            let sub = match c {
                Some(c) => format!(
                    "{} · {} modules",
                    c.version
                        .as_deref()
                        .map(|v| format!("v{v}"))
                        .unwrap_or_else(|| "Unknown version".into()),
                    c.modules.len()
                ),
                None => "Not installed".into(),
            };
            return panel()
                .w(px(col_w))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().text_xl().font_weight(FontWeight::BOLD).child("Custom Shaders Patch"))
                                .child(div().text_sm().text_color(theme::muted()).child(sub)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when(changed > 0, |d| {
                                    d.child(
                                        div()
                                            .text_sm()
                                            .text_color(theme::muted())
.child(format!("{changed} changed")),
                                    )
                                })
                                .child(button_pad("csp-reload", "Reload", self.pad_label(NavAction::Search)).on_click(cx.listener(|this, _, _, cx| this.load(cx)))),
                        ),
                )
                .when_some(c, |d, c| {
                    d.child(
                        div()
                            .w(px(col_w - 26.))
                            .text_xs()
                            .text_color(theme::muted())
                            .child(format!(
                                "Your changes are saved to {} on top of CSP's defaults, which stay untouched.",
                                c.user_dir.display()
                            )),
                    )
                });
        };
        let Some(c) = c else { return div() };
        let module = &c.modules[m];
        let changed = c.changed(m);
        panel()
            .w(px(col_w))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        button_pad("csp-back", "‹ Back", self.pad_label(NavAction::Back))
                            .on_click(cx.listener(|this, _, _, cx| this.close_module(cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child(module.name.clone()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme::muted())
                                    .child(module.short.clone()),
                            ),
                    )
                    .when(changed > 0, |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(theme::muted())
                                .child(format!("{changed} changed")),
                        )
                        .child(
                            button("csp-reset", "Reset to defaults")
                                .on_click(cx.listener(|this, _, _, cx| this.reset_module(cx))),
                        )
                    }),
            )
            .when(!module.description.is_empty(), |d| {
                d.child(
                    div()
                        .w(px(col_w - 26.))
                        .text_sm()
                        .text_color(theme::muted())
                        .child(module.description.clone()),
                )
            })
    }

    fn grid(&self, col_w: f32, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(c) = self.cfg.as_ref() else {
            return Vec::new();
        };
        let cols = self.cols.max(1);
        let card_w = (col_w - GAP * (cols as f32 - 1.)) / cols as f32;
        let text_w = card_w - 32.;
        c.modules
            .chunks(cols)
            .enumerate()
            .map(|(row, chunk)| {
                div()
                    .w(px(col_w))
                    .flex()
                    .gap(px(GAP))
                    .pt(px(GAP))
                    .children(chunk.iter().enumerate().map(|(j, module)| {
                        let i = row * cols + j;
                        let master = module.master().map(|item| match &item.kind {
                            Kind::Toggle { on, .. } => c.value(i, item) == on,
                            _ => true,
                        });
                        let changed = c.changed(i);
                        let card = div()
                            .id(("csp-mod", i))
                            .w(px(card_w))
                            .h(px(CARD_H))
                            .p_3()
                            .flex()
                            .flex_col()
                            .justify_between()
                            .rounded(px(theme::RADIUS_MD))
                            .bg(theme::panel())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::panel_hover()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.focus = i;
                                this.open_module(i, cx);
                            }))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .w(px(text_w))
                                            .truncate()
                                            .font_weight(FontWeight::BOLD)
                                            .child(module.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .w(px(text_w))
                                            .text_sm()
                                            .text_color(theme::muted())
                                            .line_clamp(2)
                                            .child(module.short.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_xs()
                                    .when_some(master, |d, on| {
                                        d.child(
                                            div()
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(if on {
                                                    theme::ok()
                                                } else {
                                                    theme::faint()
                                                })
                                                .child(if on { "● On" } else { "○ Off" }),
                                        )
                                    })
                                    .when(changed > 0, |d| {
                                        d.child(
                                            div()
                                                .text_color(theme::muted())
                                                .child(format!("{changed} changed")),
                                        )
                                    }),
                            );
                        focus_ring(card, self.shows_focus() && self.focus == i)
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn part(id: impl Into<gpui::ElementId>, col_w: f32, part: Part) -> Stateful<Div> {
        let r = px(theme::RADIUS_MD);
        div()
            .id(id)
            .w(px(col_w))
            .bg(theme::panel())
            .border_color(theme::border())
            .border_l_1()
            .border_r_1()
            .px_3()
            .when(part == Part::Top, |d| {
                d.border_t_1()
                    .rounded_tl(r)
                    .rounded_tr(r)
                    .pt_3()
                    .mt(px(GAP))
            })
            .when(part == Part::Bottom, |d| {
                d.border_b_1().rounded_bl(r).rounded_br(r).pb_3()
            })
    }

    /// Few short options show as chips; longer lists use a `‹ value ›` cycler
    /// (the same thing the D-pad does) so a row stays one line tall.
    fn options_control(
        &self,
        r: RowRef,
        base: usize,
        opts: Vec<(String, String)>,
        value: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let at = opts.iter().position(|(v, _)| v == value);
        let text_len: usize = opts.iter().map(|(_, l)| l.chars().count()).sum();
        if opts.len() <= 5 && text_len <= 48 {
            return div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .children(opts.into_iter().enumerate().map(|(j, (v, label))| {
                    chip(("csp-c", base + j), label, v == value).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.set_value(r, v.clone(), cx),
                    ))
                }))
                .when(at.is_none(), |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(format!("current: {value}")),
                    )
                })
                .into_any_element();
        }
        let shown = at.map_or_else(|| value.to_owned(), |i| opts[i].1.clone());
        let pos = at.map_or_else(
            || format!("? / {}", opts.len()),
            |i| format!("{} / {}", i + 1, opts.len()),
        );
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                button(("csp-c", base), "‹").on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.adjust(r, -1, cx)),
                ),
            )
            .child(
                div()
                    .w(px(220.))
                    .text_center()
                    .text_sm()
                    .truncate()
                    .child(shown),
            )
            .child(
                button(("csp-c", base + 1), "›").on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.adjust(r, 1, cx)),
                ),
            )
            .child(div().text_xs().text_color(theme::faint()).child(pos))
            .into_any_element()
    }

    fn control(&self, r: RowRef, base: usize, enabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let (Some(c), Some(m), Some(item)) = (self.cfg.as_ref(), self.open, self.item(r)) else {
            return div().into_any_element();
        };
        let value = c.value(m, item).to_owned();
        let el = match &item.kind {
            Kind::Toggle { on, off } => {
                let is_on = &value == on;
                segmented(vec![
                    chip(("csp-c", base), "On", is_on).on_click({
                        let v = on.clone();
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_value(r, v.clone(), cx)
                        })
                    }),
                    chip(("csp-c", base + 1), "Off", !is_on).on_click({
                        let v = off.clone();
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_value(r, v.clone(), cx)
                        })
                    }),
                ])
                .into_any_element()
            }
            Kind::Choice(opts) => self.options_control(r, base, opts.clone(), &value, cx),
            Kind::Script { dir, file } => {
                let mut opts: Vec<(String, String)> = c
                    .scripts(dir, file)
                    .into_iter()
                    .map(|v| (v.clone(), v))
                    .collect();
                if !opts.iter().any(|(v, _)| *v == value) {
                    opts.push((value.clone(), value.clone()));
                }
                self.options_control(r, base, opts, &value, cx)
            }
            Kind::Range {
                min,
                max,
                percent,
                unit,
                ..
            } => {
                let lo = c.bound(m, item, min);
                let hi = c.bound(m, item, max);
                let v = value.trim().parse::<f64>().ok();
                let shown = match v {
                    Some(v) if *percent => format!("{}%", fmt_number((v * 100.).round())),
                    Some(v) => {
                        let sep = if unit.starts_with(|ch: char| ch.is_alphabetic()) {
                            " "
                        } else {
                            ""
                        };
                        format!("{}{sep}{unit}", fmt_number(v))
                    }
                    None => value.clone(),
                };
                let frac = match (v, lo, hi) {
                    (Some(v), Some(lo), Some(hi)) if hi > lo => {
                        ((v - lo) / (hi - lo)).clamp(0., 1.) as f32
                    }
                    _ => 0.,
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(button(("csp-c", base), "−").on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.step(r, -1, cx)),
                    ))
                    .child(div().w(px(84.)).text_center().text_sm().child(shown))
                    .child(button(("csp-c", base + 1), "+").on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.step(r, 1, cx)),
                    ))
                    .child(
                        div()
                            .w(px(120.))
                            .h(px(4.))
                            .rounded(px(2.))
                            .bg(theme::raised())
                            .child(
                                div()
                                    .h_full()
                                    .w(px(120. * frac))
                                    .rounded(px(2.))
                                    .bg(theme::accent()),
                            ),
                    )
                    .into_any_element()
            }
            Kind::Text => match self.inputs.get(&r) {
                Some(input) if enabled => div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .child(input.clone())
                    .into_any_element(),
                _ => div()
                    .text_sm()
                    .child(if value.is_empty() {
                        "—".to_owned()
                    } else {
                        value
                    })
                    .into_any_element(),
            },
        };
        if enabled {
            el
        } else {
            // Disabled options keep their value visible but take no input.
            div().opacity(0.4).child(el).into_any_element()
        }
    }

    fn module_children(&self, col_w: f32, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (Some(c), Some(m)) = (self.cfg.as_ref(), self.open) else {
            return Vec::new();
        };
        let rows = self.rows();
        let focused = rows.get(self.focus).copied();
        let inner_w = col_w - 26.;
        let mut out = Vec::new();
        let mut row_n = 0usize;
        for (si, s) in c.modules[m].sections.iter().enumerate() {
            if !c.section_visible(m, s) {
                continue;
            }
            let sec_enabled = c.section_enabled(m, s);
            out.push(
                Self::part(("csp-sec", si), col_w, Part::Top)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .pb_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::muted())
                            .child(s.title.to_uppercase()),
                    )
                    .when_some(s.hint_top.clone(), |d, h| {
                        d.child(
                            div()
                                .w(px(inner_w))
                                .text_xs()
                                .text_color(theme::faint())
                                .child(h),
                        )
                    })
                    .into_any_element(),
            );
            for (ii, item) in s.items.iter().enumerate() {
                if !c.visible(m, item) {
                    continue;
                }
                let r = RowRef {
                    section: si,
                    item: ii,
                };
                let enabled = sec_enabled && c.enabled(m, item);
                let is_focus = self.shows_focus() && focused == Some(r);
                let changed = c.value(m, item) != item.default;
                let label = div()
                    .w(px(LABEL_W))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_sm()
                    .text_color(if enabled {
                        theme::text()
                    } else {
                        theme::faint()
                    })
                    .child(div().w(px(LABEL_W - 16.)).child(item.label.clone()))
                    .when(changed, |d| {
                        d.child(div().text_color(theme::accent()).child("•"))
                    });
                let line = div()
                    .id(("csp-line", row_n))
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_1()
                    .px_2()
                    .min_h(px(44.))
                    .rounded(px(theme::RADIUS_SM))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        let next = if *hovered {
                            Some(r)
                        } else if this.hovered == Some(r) {
                            None
                        } else {
                            this.hovered
                        };
                        if next != this.hovered {
                            this.hovered = next;
                            cx.notify();
                        }
                    }))
                    .child(label)
                    .child(div().flex_1().min_w_0().child(self.control(
                        r,
                        row_n * 64,
                        enabled,
                        cx,
                    )));
                out.push(
                    Self::part(("csp-row", row_n), col_w, Part::Middle)
                        .child(focus_ring(
                            line.when(is_focus, |d| d.bg(theme::panel_hover())),
                            is_focus,
                        ))
                        .into_any_element(),
                );
                row_n += 1;
            }
            out.push(
                Self::part(("csp-end", si), col_w, Part::Bottom)
                    .when_some(s.hint_bottom.clone(), |d, h| {
                        d.pt_1().child(
                            div()
                                .w(px(inner_w))
                                .text_xs()
                                .text_color(theme::faint())
                                .child(h),
                        )
                    })
                    .into_any_element(),
            );
        }
        out
    }

    fn footer(&self, col_w: f32, cx: &mut Context<Self>) -> Div {
        let Some(c) = self.cfg.as_ref() else {
            return div();
        };
        let dirty = c.dirty_files();
        // The hovered (mouse) or focused (pad) option's explanation.
        let info_row = self.hovered.or_else(|| {
            self.shows_focus()
                .then(|| self.rows().get(self.focus).copied())
                .flatten()
        });
        let info = info_row.and_then(|r| self.item(r)).and_then(|i| {
            let mut t = i.tooltip.clone()?;
            if i.new {
                t = format!("New: {t}");
            }
            Some(t)
        });
        let status: SharedString = match (&self.message, dirty.is_empty()) {
            (Some((m, _)), _) => m.clone().into(),
            (None, true) => format!("Saved in {}", c.user_dir.display()).into(),
            (None, false) => format!("Unsaved: {}", dirty.join(", ")).into(),
        };
        let status_err = self.message.as_ref().is_some_and(|(_, e)| *e);
        div()
            .w(px(col_w))
            .mx_auto()
            .flex()
            .flex_col()
            .gap_1()
            .py_2()
            .when(self.open.is_some(), |d| {
                d.child(
                    div()
                        .w(px(col_w))
                        .h(px(32.))
                        .overflow_hidden()
                        .text_xs()
                        .text_color(theme::muted())
                        .line_clamp(2)
                        .child(info.unwrap_or_else(|| {
                            "Hover or focus an option to see what it does.".into()
                        })),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .text_color(if status_err {
                                theme::error()
                            } else {
                                theme::muted()
                            })
                            .child(if status_err {
                                format!("⚠ {status}")
                            } else {
                                status.to_string()
                            }),
                    )
                    .when(!dirty.is_empty(), |d| {
                        d.child(
                            button_pad("csp-revert", "Revert", self.pad_label(NavAction::Secondary))
                                .on_click(cx.listener(|this, _, _, cx| this.revert(cx))),
                        )
                    })
                    .child(
                        primary_button_pad("csp-save", "Save", self.pad_label(NavAction::Primary))
                            .when(dirty.is_empty(), |d| d.opacity(0.5))
                            .when(!dirty.is_empty(), |d| {
                                d.on_click(cx.listener(|this, _, _, cx| this.save(cx)))
                            }),
                    ),
            )
    }

    fn empty_state(&self, col_w: f32) -> Div {
        let (title, body) = match &self.error {
            Some(e) => ("Couldn't read CSP settings".to_owned(), format!("⚠ {e}")),
            None => (
                "Custom Shaders Patch isn't installed".to_owned(),
                format!(
                    "Extract a CSP release from acstuff.club/patch into {} so extension/ and dwrite.dll sit next to \
                     acs.exe, then press Reload. Races started from here load it automatically.",
                    self.root.display()
                ),
            ),
        };
        panel()
            .w(px(col_w))
            .mt(px(GAP))
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::BOLD).child(title))
            .child(
                div()
                    .w(px(col_w - 26.))
                    .text_sm()
                    .text_color(if self.error.is_some() {
                        theme::error()
                    } else {
                        theme::muted()
                    })
                    .child(body),
            )
    }
}

impl Render for CspView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vp_w = f32::from(window.viewport_size().width);
        let col_w = (vp_w - 32.).clamp(480., MAX_W);
        self.cols = (((col_w + GAP) / (CARD_MIN_W + GAP)).floor() as usize).clamp(1, 4);
        let rows = self.rows();
        if self.open.is_some() && !rows.is_empty() && self.focus >= rows.len() {
            self.focus = rows.len() - 1;
        }

        let mut children: Vec<AnyElement> = vec![self.header(col_w, cx).into_any_element()];
        match (&self.cfg, self.open) {
            (None, _) => children.push(self.empty_state(col_w).into_any_element()),
            (Some(_), None) => children.extend(self.grid(col_w, cx)),
            (Some(_), Some(_)) => children.extend(self.module_children(col_w, cx)),
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .id("csp-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .items_center()
                    .pt_4()
                    .pb_4()
                    .children(children),
            )
            .when(self.cfg.is_some(), |d| {
                d.child(
                    div()
                        .border_t_1()
                        .border_color(theme::border())
                        .bg(theme::sidebar())
                        .child(self.footer(col_w, cx)),
                )
            })
    }
}
