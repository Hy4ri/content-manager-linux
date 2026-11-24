//! Installed mods manager under the Mods tab: list, search, filter, enable/disable, open folder, and delete.

use std::path::PathBuf;

use ac_core::content::natural_cmp;
use ac_core::install::Kind;
use ac_core::mods::{self, InstalledMod, ModOrigin};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FontWeight, Render, ScrollStrategy, SharedString,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};

use crate::nav::InputDevice;
use crate::text_input::TextInput;
use crate::theme;
use crate::widgets::{button, chip, focus_ring, panel, picture, primary_button, segmented};

const MAX_W: f32 = 1120.;
const ROW_H: f32 = 88.;
const ROW_GAP: f32 = 8.;

/// Emitted whenever a mod is enabled, disabled, or deleted so `CmApp` refreshes Quick Drive/Garage/Tracks.
pub struct ModsChanged;
impl EventEmitter<ModsChanged> for ModsView {}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ModsTarget {
    Browser,
    Install,
}

/// Emitted from empty-state CTAs to jump to Get mods or Install.
pub struct ModsNavigate(pub ModsTarget);
impl EventEmitter<ModsNavigate> for ModsView {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModFilter {
    All,
    Cars,
    Tracks,
    Disabled,
}

impl ModFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Cars => "Cars",
            Self::Tracks => "Tracks",
            Self::Disabled => "Disabled",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModSort {
    Name,
    Newest,
    Size,
}

impl ModSort {
    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Newest => "Newest",
            Self::Size => "Size",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ModKey {
    kind: Kind,
    id: String,
    enabled: bool,
}

impl ModKey {
    fn of(m: &InstalledMod) -> Self {
        Self {
            kind: m.kind,
            id: m.id.clone(),
            enabled: m.enabled,
        }
    }
}

pub struct ModsView {
    pub root: PathBuf,
    items: Vec<InstalledMod>,
    visible: Vec<usize>,
    filter: ModFilter,
    sort: ModSort,
    show_stock: bool,
    search: Entity<TextInput>,
    selected: usize,
    scroll: UniformListScrollHandle,
    confirm_delete: Option<ModKey>,
    loading: bool,
    working: bool,
    scan_gen: u64,
    input_device: InputDevice,
    warning: Option<String>,
    message: Option<(String, bool)>,
}

impl ModsView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new(cx, "Search mods by name, folder, author…", ""));
        cx.observe(&search, |this, _, cx| {
            this.confirm_delete = None;
            this.refilter(cx);
        })
        .detach();
        let mut view = Self {
            root,
            items: Vec::new(),
            visible: Vec::new(),
            filter: ModFilter::All,
            sort: ModSort::Name,
            show_stock: false,
            search,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            confirm_delete: None,
            loading: false,
            working: false,
            scan_gen: 0,
            input_device: InputDevice::Mouse,
            warning: None,
            message: None,
        };
        view.refresh(cx);
        view
    }

    pub fn set_input_device(&mut self, device: InputDevice, cx: &mut Context<Self>) {
        if self.input_device != device {
            self.input_device = device;
            cx.notify();
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.scan_gen = self.scan_gen.wrapping_add(1);
        let generation = self.scan_gen;
        let root = self.root.clone();
        self.loading = true;
        self.confirm_delete = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { mods::scan(&root) })
                .await;
            this.update(cx, |this, cx| {
                if this.scan_gen != generation {
                    return;
                }
                this.loading = false;
                this.items = result.items;
                this.warning = result.warning;
                this.refilter(cx);
            })
            .ok();
        })
        .detach();
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let q = self.search.read(cx).text().trim().to_lowercase();
        let mut visible: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                if !self.show_stock && m.origin == ModOrigin::Stock && m.enabled {
                    return false;
                }
                let filter_ok = match self.filter {
                    ModFilter::All => true,
                    ModFilter::Cars => m.kind == Kind::Car,
                    ModFilter::Tracks => m.kind == Kind::Track,
                    ModFilter::Disabled => !m.enabled,
                };
                if !filter_ok {
                    return false;
                }
                q.is_empty() || m.search_text.contains(&q)
            })
            .map(|(i, _)| i)
            .collect();

        visible.sort_by(|&a, &b| {
            let ma = &self.items[a];
            let mb = &self.items[b];
            match self.sort {
                ModSort::Name => natural_cmp(&ma.name, &mb.name)
                    .then_with(|| ma.kind.cmp(&mb.kind))
                    .then_with(|| ma.id.cmp(&mb.id)),
                ModSort::Newest => mb
                    .installed_at
                    .cmp(&ma.installed_at)
                    .then_with(|| natural_cmp(&ma.name, &mb.name)),
                ModSort::Size => mb
                    .size_bytes
                    .cmp(&ma.size_bytes)
                    .then_with(|| natural_cmp(&ma.name, &mb.name)),
            }
        });

        self.visible = visible;
        if self.visible.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.visible.len() {
            self.selected = self.visible.len() - 1;
        }
        cx.notify();
    }

    fn toggle_enabled(&mut self, idx: usize, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        let Some(item) = self.items.get(idx).cloned() else {
            return;
        };
        self.working = true;
        self.confirm_delete = None;
        let root = self.root.clone();
        let enable = !item.enabled;
        let kind = item.kind;
        let id = item.id.clone();
        let name = item.name.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    mods::set_enabled(&root, kind, &id, enable)?;
                    Ok::<_, anyhow::Error>(mods::scan(&root))
                })
                .await;
            this.update(cx, |this, cx| {
                this.working = false;
                match result {
                    Ok(scan) => {
                        this.items = scan.items;
                        this.warning = scan.warning;
                        let verb = if enable { "Enabled" } else { "Disabled" };
                        this.message = Some((format!("{verb} {name}."), false));
                        this.refilter(cx);
                        cx.emit(ModsChanged);
                    }
                    Err(e) => {
                        this.message = Some((format!("Couldn't update {name}: {e:#}"), true));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn delete_item(&mut self, idx: usize, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        let Some(item) = self.items.get(idx).cloned() else {
            return;
        };
        let key = ModKey::of(&item);
        if self.confirm_delete.as_ref() != Some(&key) {
            self.confirm_delete = Some(key);
            cx.notify();
            return;
        }
        self.confirm_delete = None;
        self.working = true;
        let root = self.root.clone();
        let kind = item.kind;
        let id = item.id.clone();
        let enabled = item.enabled;
        let name = item.name.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    mods::delete_mod(&root, kind, &id, enabled)?;
                    Ok::<_, anyhow::Error>(mods::scan(&root))
                })
                .await;
            this.update(cx, |this, cx| {
                this.working = false;
                match result {
                    Ok(scan) => {
                        this.items = scan.items;
                        this.warning = scan.warning;
                        this.message = Some((format!("Deleted {name}."), false));
                        this.refilter(cx);
                        cx.emit(ModsChanged);
                    }
                    Err(e) => {
                        this.message = Some((format!("Couldn't delete {name}: {e:#}"), true));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_item_folder(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(idx) else {
            return;
        };
        match mods::open_folder(&item.dir) {
            Ok(()) => {
                self.message = Some((format!("Opened folder for {}.", item.name), false));
            }
            Err(e) => {
                self.message = Some((format!("Couldn't open folder: {e:#}"), true));
            }
        }
        cx.notify();
    }

    /// Handles gamepad/keyboard navigation inside the Installed Mods view.
    /// Returns `true` if consumed, `false` to let `CmApp` handle fallback (e.g. Back to Drive).
    pub fn pad(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        match name {
            "DPAD_UP" => {
                if !self.visible.is_empty() && self.selected > 0 {
                    self.selected -= 1;
                    self.confirm_delete = None;
                    self.scroll.scroll_to_item(self.selected, ScrollStrategy::Center);
                    cx.notify();
                }
                true
            }
            "DPAD_DOWN" => {
                if !self.visible.is_empty() && self.selected + 1 < self.visible.len() {
                    self.selected += 1;
                    self.confirm_delete = None;
                    self.scroll.scroll_to_item(self.selected, ScrollStrategy::Center);
                    cx.notify();
                }
                true
            }
            "A" => {
                if let Some(&idx) = self.visible.get(self.selected) {
                    let is_confirming = self
                        .items
                        .get(idx)
                        .is_some_and(|m| self.confirm_delete.as_ref() == Some(&ModKey::of(m)));
                    if is_confirming {
                        self.delete_item(idx, cx);
                    } else {
                        self.toggle_enabled(idx, cx);
                    }
                    return true;
                }
                false
            }
            "X" => {
                if let Some(&idx) = self.visible.get(self.selected) {
                    self.open_item_folder(idx, cx);
                    return true;
                }
                false
            }
            "B" => {
                if self.confirm_delete.is_some() {
                    self.confirm_delete = None;
                    cx.notify();
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn summary_counts(&self) -> (usize, usize, usize, u64) {
        let mut mod_count = 0usize;
        let mut disabled_count = 0usize;
        let mut stock_count = 0usize;
        let mut mod_bytes = 0u64;
        for m in &self.items {
            if !m.enabled {
                disabled_count += 1;
            }
            if m.origin == ModOrigin::Stock {
                stock_count += 1;
            } else {
                mod_count += 1;
                mod_bytes = mod_bytes.saturating_add(m.size_bytes);
            }
        }
        (mod_count, disabled_count, stock_count, mod_bytes)
    }

    fn render_row(
        &self,
        vis_idx: usize,
        item_idx: usize,
        text_max_w: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let m = &self.items[item_idx];
        let focused = self.input_device.shows_focus() && self.selected == vis_idx;
        let key = ModKey::of(m);
        let confirming = self.confirm_delete.as_ref() == Some(&key);
        let is_stock = m.origin == ModOrigin::Stock;

        let kind_label = match m.kind {
            Kind::Car => "Car",
            Kind::Track => "Track",
        };
        let detail_unit = match m.kind {
            Kind::Car => {
                if m.detail_count == 1 {
                    "1 skin".to_string()
                } else {
                    format!("{} skins", m.detail_count)
                }
            }
            Kind::Track => {
                if m.detail_count == 1 {
                    "1 layout".to_string()
                } else {
                    format!("{} layouts", m.detail_count)
                }
            }
        };

        let mut meta_parts = vec![m.id.clone()];
        if let Some(bc) = &m.brand_or_country {
            meta_parts.push(bc.clone());
        }
        if let Some(author) = &m.author {
            meta_parts.push(format!("by {author}"));
        }
        meta_parts.push(detail_unit);
        meta_parts.push(mods::format_size(m.size_bytes));
        if let Some(archive) = &m.archive_name {
            meta_parts.push(archive.clone());
        }
        let meta_line: SharedString = meta_parts.join(" · ").into();

        let badge = |label: &'static str, bg: gpui::Rgba, fg: gpui::Rgba| {
            div()
                .flex_shrink_0()
                .px_2()
                .py(px(2.))
                .rounded(px(theme::RADIUS_SM))
                .bg(bg)
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(fg)
                .child(label)
        };

        let mut actions = div().flex().items_center().gap_2().flex_shrink_0();
        if confirming {
            actions = actions
                .child(
                    div()
                        .id(("mod-confirm-del", item_idx))
                        .flex()
                        .items_center()
                        .min_h(px(theme::MIN_TARGET_H))
                        .px_3()
                        .py_1()
                        .rounded(px(theme::RADIUS_SM))
                        .bg(theme::error())
                        .text_color(theme::on_accent())
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .cursor_pointer()
                        .when(self.working, |d| d.opacity(0.6))
                        .when(!self.working, |d| {
                            d.on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_item(item_idx, cx);
                            }))
                        })
                        .child("Confirm delete"),
                )
                .child(
                    button(("mod-cancel-del", item_idx), "Cancel").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.confirm_delete = None;
                            cx.notify();
                        },
                    )),
                );
        } else {
            let toggle_label = if m.enabled { "Disable" } else { "Enable" };
            actions = actions
                .child(
                    button(("mod-toggle", item_idx), toggle_label)
                        .when(self.working, |d| d.opacity(0.6))
                        .when(!self.working, |d| {
                            d.on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_enabled(item_idx, cx);
                            }))
                        }),
                )
                .child(
                    button(("mod-folder", item_idx), "Folder").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.open_item_folder(item_idx, cx);
                        },
                    )),
                );
            if !is_stock {
                actions = actions.child(
                    div()
                        .id(("mod-delete", item_idx))
                        .flex()
                        .items_center()
                        .min_h(px(theme::MIN_TARGET_H))
                        .px_3()
                        .py_1()
                        .rounded(px(theme::RADIUS_SM))
                        .border_1()
                        .border_color(theme::error())
                        .bg(theme::panel())
                        .text_sm()
                        .text_color(theme::error())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::panel_hover()))
                        .when(self.working, |d| d.opacity(0.6))
                        .when(!self.working, |d| {
                            d.on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_item(item_idx, cx);
                            }))
                        })
                        .child("Delete"),
                );
            }
        }

        let title_w = (text_max_w - 140.).max(140.);
        let row = div()
            .id(("mod-row", item_idx))
            .h(px(ROW_H))
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .rounded(px(theme::RADIUS_MD))
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .when(!m.enabled, |d| d.opacity(0.72))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = vis_idx;
                cx.notify();
            }))
            .child(
                div()
                    .w(px(106.))
                    .h(px(62.))
                    .flex_shrink_0()
                    .rounded(px(theme::RADIUS_SM))
                    .bg(theme::bg())
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(picture(m.preview.clone(), 62.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .max_w(px(title_w))
                                    .text_sm()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme::text())
                                    .truncate()
                                    .child(m.name.clone()),
                            )
                            .child(badge(kind_label, theme::overlay(), theme::text()))
                            .when(!m.enabled, |d| {
                                d.child(badge("Disabled", theme::error(), theme::on_accent()))
                            })
                            .when(is_stock, |d| {
                                d.child(badge("Kunos", theme::raised(), theme::muted()))
                            }),
                    )
                    .child(
                        div()
                            .w(px(text_max_w))
                            .text_xs()
                            .text_color(theme::muted())
                            .truncate()
                            .child(meta_line),
                    ),
            )
            .child(actions);

        div()
            .pb(px(ROW_GAP))
            .w_full()
            .child(focus_ring(row, focused))
            .into_any_element()
    }
}

impl Render for ModsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vp_w = f32::from(window.viewport_size().width);
        let col_w = (vp_w - 32.).clamp(520., MAX_W);
        // Row inner width minus preview (106), gaps (24), padding (24), and action buttons (~260).
        let text_max_w = (col_w - 430.).max(180.);

        let (mod_count, disabled_count, stock_count, mod_bytes) = self.summary_counts();

        let summary: SharedString = if self.loading {
            "Scanning content folder…".into()
        } else if mod_count == 0 && disabled_count == 0 {
            format!("0 mods installed · {stock_count} Kunos stock items").into()
        } else {
            let mut parts = vec![if mod_count == 1 {
                "1 mod".to_string()
            } else {
                format!("{mod_count} mods")
            }];
            if disabled_count > 0 {
                parts.push(format!("{disabled_count} disabled"));
            }
            if mod_bytes > 0 {
                parts.push(mods::format_size(mod_bytes));
            }
            if self.show_stock {
                parts.push(format!("+{stock_count} stock"));
            }
            parts.join(" · ").into()
        };

        let header = panel()
            .flex()
            .flex_col()
            .gap_3()
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
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("Installed mods"),
                            )
                            .child(div().text_sm().text_color(theme::muted()).child(summary)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                chip(
                                    "mods-show-stock",
                                    format!("Show stock ({stock_count})"),
                                    self.show_stock,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_stock = !this.show_stock;
                                    this.confirm_delete = None;
                                    this.refilter(cx);
                                })),
                            )
                            .child(
                                button("mods-refresh", "Refresh")
                                    .when(self.loading || self.working, |d| d.opacity(0.6))
                                    .when(!self.loading && !self.working, |d| {
                                        d.on_click(cx.listener(|this, _, _, cx| {
                                            this.refresh(cx);
                                        }))
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .flex_wrap()
                    .child(segmented(
                        [
                            ModFilter::All,
                            ModFilter::Cars,
                            ModFilter::Tracks,
                            ModFilter::Disabled,
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(i, f)| {
                            chip(("mod-filter", i), f.label(), self.filter == f).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.filter = f;
                                    this.confirm_delete = None;
                                    this.refilter(cx);
                                }),
                            )
                        })
                        .collect(),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(260.))
                                    .child(self.search.clone()),
                            )
                            .child(segmented(
                                [ModSort::Name, ModSort::Newest, ModSort::Size]
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, s)| {
                                        chip(("mod-sort", i), s.label(), self.sort == s).on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.sort = s;
                                                this.refilter(cx);
                                            }),
                                        )
                                    })
                                    .collect(),
                            )),
                    ),
            );

        let banner = self.warning.as_ref().map(|w| (w.clone(), true)).or_else(|| self.message.clone());

        let body: AnyElement = if self.loading && self.items.is_empty() {
            panel()
                .p_6()
                .text_sm()
                .text_color(theme::muted())
                .child("Scanning installed cars and tracks…")
                .into_any_element()
        } else if self.visible.is_empty() {
            let has_filters = self.filter != ModFilter::All
                || !self.search.read(cx).text().trim().is_empty();
            if has_filters {
                panel()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_6()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::BOLD)
                            .child("No matching items"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::muted())
                            .child("Try clearing the search query or switching the filter chips."),
                    )
                    .child(
                        div().flex().gap_2().child(
                            button("mods-clear-filters", "Clear filters").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.filter = ModFilter::All;
                                    this.search.update(cx, |s, cx| s.set_text("", cx));
                                    this.refilter(cx);
                                },
                            )),
                        ),
                    )
                    .into_any_element()
            } else {
                panel()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_6()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::BOLD)
                            .child("No installed mods yet"),
                    )
                    .child(
                        div()
                            .w(px((col_w - 64.).max(240.)))
                            .text_sm()
                            .text_color(theme::muted())
                            .child(
                                "Cars and tracks installed from .zip archives or added to your Assetto Corsa content folder appear here. You can disable, open, or delete them individually.",
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                primary_button("mods-go-browser", "Get mods").on_click(
                                    cx.listener(|_, _, _, cx| {
                                        cx.emit(ModsNavigate(ModsTarget::Browser));
                                    }),
                                ),
                            )
                            .child(
                                button("mods-go-install", "Install archive…").on_click(
                                    cx.listener(|_, _, _, cx| {
                                        cx.emit(ModsNavigate(ModsTarget::Install));
                                    }),
                                ),
                            )
                            .when(stock_count > 0 && !self.show_stock, |d| {
                                d.child(
                                    button(
                                        "mods-empty-show-stock",
                                        format!("Show {stock_count} stock items"),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_stock = true;
                                        this.refilter(cx);
                                    })),
                                )
                            }),
                    )
                    .into_any_element()
            }
        } else {
            let count = self.visible.len();
            uniform_list(
                "installed-mods-list",
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .map(|vis_idx| {
                            let item_idx = this.visible[vis_idx];
                            this.render_row(vis_idx, item_idx, text_max_w, cx)
                        })
                        .collect()
                }),
            )
            .track_scroll(self.scroll.clone())
            .flex_1()
            .size_full()
            .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .p_4()
            .child(
                div()
                    .w(px(col_w))
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(header)
                    .when_some(banner, |d, (text, is_err)| {
                        d.child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded(px(theme::RADIUS_SM))
                                .border_1()
                                .border_color(if is_err { theme::error() } else { theme::ok() })
                                .bg(theme::surface())
                                .text_sm()
                                .text_color(if is_err { theme::error() } else { theme::ok() })
                                .child(text),
                        )
                    })
                    .child(body),
            )
    }
}
