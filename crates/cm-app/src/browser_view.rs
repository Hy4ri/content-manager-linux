//! Embedded browser view for GPUI.
//!
//! Provides the UI layer for browsing web content, displaying CEF-rendered
//! frames, handling navigation, forwarding input events, and managing downloads.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ac_core::bookmarks::{self, Bookmark};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Corners, Element, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, GlobalElementId, InspectorElementId, InteractiveElement, IntoElement,
    KeyDownEvent, KeyUpEvent, LayoutId, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, RenderImage, ScrollDelta, ScrollWheelEvent,
    Style, Styled, Task, Window, div, px,
};
use image::{Frame, ImageBuffer};

use crate::browser_engine::BrowserEngine;
use crate::browser_types::{
    BrowserDownload, BrowserKey, BrowserModifiers, BrowserMouseButton, BrowserSnapshot,
    DownloadStatus,
};
use crate::text_input::TextInput;
use crate::theme;
use crate::widgets::{button, chip, primary_button};

/// A local start page, rendered by GPUI rather than a remote mod website.
pub(crate) const DEFAULT_BROWSER_URL: &str = "about:blank";

/// Refresh polling interval for frames and snapshot updates.
const FRAME_POLL_INTERVAL: Duration = Duration::from_millis(16);

/// Event emitted when a user selects a completed download for archive preview.
/// Carries only the path to the downloaded archive; no game file writes.
#[derive(Clone, Debug)]
pub struct PreviewDownload(pub PathBuf);

/// Embedded browser view entity.
pub struct BrowserView {
    focus_handle: FocusHandle,
    address_input: Entity<TextInput>,
    engine: Option<BrowserEngine>,
    snapshot: BrowserSnapshot,
    cached_render_image: Option<Arc<RenderImage>>,
    retired_render_images: Vec<Arc<RenderImage>>,
    current_bounds: Option<Bounds<Pixels>>,
    current_scale: f32,
    error: Option<String>,
    refresh_task: Option<Task<()>>,
    dismissed_download_ids: HashSet<u32>,
    active_mouse_buttons: HashSet<MouseButton>,
    active: bool,
    bookmarks: Vec<Bookmark>,
    show_bookmarks: bool,
    bookmark_error: Option<String>,
}

impl Drop for BrowserView {
    fn drop(&mut self) {
        if let Some(engine) = &self.engine {
            engine.close();
        }
    }
}

impl EventEmitter<PreviewDownload> for BrowserView {}

impl Focusable for BrowserView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BrowserView {
    pub fn new_at(url: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let address = if url == DEFAULT_BROWSER_URL { "" } else { url };
        let address_input = cx.new(|cx| TextInput::new(cx, "Enter URL…", address.to_owned()));

        let mut view = Self {
            focus_handle,
            address_input,
            engine: None,
            snapshot: BrowserSnapshot {
                url: url.to_owned(),
                ..Default::default()
            },
            cached_render_image: None,
            retired_render_images: Vec::new(),
            current_bounds: None,
            current_scale: 1.0,
            error: None,
            refresh_task: None,
            dismissed_download_ids: HashSet::new(),
            active_mouse_buttons: HashSet::new(),
            active: true,
            bookmarks: Vec::new(),
            show_bookmarks: false,
            bookmark_error: None,
        };
        match bookmarks::load() {
            Ok(items) => view.bookmarks = items,
            Err(e) => view.bookmark_error = Some(format!("{e:#}")),
        }

        cx.on_focus(&view.focus_handle, window, |view, _, _| {
            view.forward_focus(true)
        })
        .detach();
        cx.on_blur(&view.focus_handle, window, |view, _, _| {
            view.active_mouse_buttons.clear();
            view.forward_focus(false);
        })
        .detach();
        view.init_engine(url);
        view.start_refresh_timer(cx);
        view
    }

    pub fn release_images(&mut self, window: &mut Window) {
        if let Some(image) = self.cached_render_image.take() {
            let _ = window.drop_image(image);
        }
        for image in self.retired_render_images.drain(..) {
            let _ = window.drop_image(image);
        }
    }

    pub fn session_page(&self) -> Bookmark {
        let snapshot = self
            .engine
            .as_ref()
            .map(|engine| engine.snapshot())
            .unwrap_or_else(|| self.snapshot.clone());
        Bookmark {
            title: if snapshot.url == DEFAULT_BROWSER_URL {
                "Bookmarks".to_owned()
            } else {
                snapshot.title
            },
            url: snapshot.url,
        }
    }

    pub fn take_new_tabs(&self) -> Vec<String> {
        self.engine
            .as_ref()
            .map(|engine| engine.take_new_tabs())
            .unwrap_or_default()
    }

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if let Some(engine) = &self.engine {
            engine.set_hidden(!active);
            if !active {
                engine.focus(false);
            }
        }
        if active {
            match bookmarks::load() {
                Ok(items) => {
                    self.bookmarks = items;
                    self.bookmark_error = None;
                }
                Err(error) => self.bookmark_error = Some(format!("{error:#}")),
            }
            self.tick(cx);
            self.start_refresh_timer(cx);
        } else {
            self.refresh_task = None;
            self.active_mouse_buttons.clear();
        }
    }

    fn init_engine(&mut self, url: &str) {
        match BrowserEngine::new(url) {
            Ok(engine) => {
                self.snapshot = engine.snapshot();
                self.engine = Some(engine);
                self.error = None;
            }
            Err(err) => {
                self.engine = None;
                self.error = Some(format!("Failed to initialize browser engine: {err:#}"));
            }
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        let url = {
            let input_text = self.address_input.read(cx).text();
            if input_text.trim().is_empty() {
                DEFAULT_BROWSER_URL.to_string()
            } else {
                input_text
            }
        };
        self.init_engine(&url);
        self.start_refresh_timer(cx);
        cx.notify();
    }

    fn start_refresh_timer(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME_POLL_INTERVAL).await;
                let updated = this.update(cx, |view, cx| {
                    view.tick(cx);
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        self.refresh_task = Some(task);
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = &self.engine else {
            return;
        };

        let mut needs_notify = false;

        // Fetch latest frame if available
        if let Some(frame) = engine.take_frame() {
            if let Some(render_image) = frame_to_render_image(frame) {
                if let Some(old) = self.cached_render_image.take() {
                    self.retired_render_images.push(old);
                }
                self.cached_render_image = Some(Arc::new(render_image));
                needs_notify = true;
            }
        }

        // Fetch latest snapshot (URL, navigation state, downloads, errors)
        let new_snapshot = engine.snapshot();
        if new_snapshot.url != self.snapshot.url
            || new_snapshot.title != self.snapshot.title
            || new_snapshot.loading != self.snapshot.loading
            || new_snapshot.can_go_back != self.snapshot.can_go_back
            || new_snapshot.can_go_forward != self.snapshot.can_go_forward
            || new_snapshot.error != self.snapshot.error
            || new_snapshot.downloads.len() != self.snapshot.downloads.len()
            || downloads_changed(&new_snapshot.downloads, &self.snapshot.downloads)
        {
            if new_snapshot.url != self.snapshot.url && !new_snapshot.url.is_empty() {
                let url_copy = if new_snapshot.url == DEFAULT_BROWSER_URL {
                    String::new()
                } else {
                    new_snapshot.url.clone()
                };
                self.address_input.update(cx, |input, cx| {
                    input.set_text(url_copy, cx);
                });
            }
            self.snapshot = new_snapshot;
            needs_notify = true;
        }

        if needs_notify {
            cx.notify();
        }
    }

    fn submit_url(&mut self, cx: &mut Context<Self>) {
        let mut target = self.address_input.read(cx).text().trim().to_string();
        if target.is_empty() {
            return;
        }
        if !target.starts_with("http://")
            && !target.starts_with("https://")
            && !target.starts_with("file://")
            && !target.starts_with("about:")
        {
            target = format!("https://{target}");
            let t = target.clone();
            self.address_input.update(cx, |input, cx| {
                input.set_text(t, cx);
            });
        }

        if let Some(engine) = &self.engine {
            if let Err(err) = engine.navigate(&target) {
                self.snapshot.error = Some(err.to_string());
            }
        } else {
            self.init_engine(&target);
        }
        cx.notify();
    }

    fn is_bookmarked(&self) -> bool {
        !self.snapshot.url.is_empty() && self.bookmarks.iter().any(|b| b.url == self.snapshot.url)
    }

    fn persist_bookmarks(&mut self) {
        self.bookmark_error = bookmarks::save(&self.bookmarks)
            .err()
            .map(|e| format!("{e:#}"));
    }

    /// Saves the current page, or removes it if it is already saved.
    fn toggle_bookmark(&mut self, cx: &mut Context<Self>) {
        let url = self.snapshot.url.clone();
        if url.is_empty() || url.starts_with("about:") {
            return;
        }
        if let Some(i) = self.bookmarks.iter().position(|b| b.url == url) {
            self.bookmarks.remove(i);
        } else {
            let title = if self.snapshot.title.trim().is_empty() {
                url.clone()
            } else {
                self.snapshot.title.trim().to_owned()
            };
            self.bookmarks.push(Bookmark { title, url });
        }
        self.persist_bookmarks();
        cx.notify();
    }

    fn remove_bookmark(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.bookmarks.len() {
            self.bookmarks.remove(i);
            self.persist_bookmarks();
            cx.notify();
        }
    }

    fn open_bookmark(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(url) = self.bookmarks.get(i).map(|b| b.url.clone()) else {
            return;
        };
        self.address_input
            .update(cx, |input, cx| input.set_text(url.clone(), cx));
        match &self.engine {
            Some(engine) => {
                if let Err(err) = engine.navigate(&url) {
                    self.snapshot.error = Some(err.to_string());
                }
            }
            None => self.init_engine(&url),
        }
        cx.notify();
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(engine) = &self.engine {
            engine.back();
            cx.notify();
        }
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(engine) = &self.engine {
            engine.forward();
            cx.notify();
        }
    }

    fn reload_or_stop(&mut self, cx: &mut Context<Self>) {
        if let Some(engine) = &self.engine {
            if self.snapshot.loading {
                engine.stop();
            } else {
                engine.reload();
            }
            cx.notify();
        }
    }

    fn cancel_download(&mut self, id: u32, cx: &mut Context<Self>) {
        if let Some(engine) = &self.engine {
            engine.cancel_download(id);
            cx.notify();
        }
    }

    fn dismiss_download(&mut self, id: u32, cx: &mut Context<Self>) {
        self.dismissed_download_ids.insert(id);
        cx.notify();
    }

    fn preview_download(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.emit(PreviewDownload(path));
    }

    fn update_viewport_size(&mut self, bounds: Bounds<Pixels>, scale: f32) {
        let changed = match self.current_bounds {
            Some(prev) => prev != bounds || (self.current_scale - scale).abs() > f32::EPSILON,
            None => true,
        };

        self.current_bounds = Some(bounds);
        self.current_scale = scale;

        if changed {
            if let Some(engine) = &self.engine {
                let w = (f32::from(bounds.size.width) * scale).max(1.0).round() as u32;
                let h = (f32::from(bounds.size.height) * scale).max(1.0).round() as u32;
                engine.resize(w, h, scale);
            }
        }
    }

    fn forward_mouse_move(&self, position: Point<Pixels>, modifiers: Modifiers) {
        let Some(bounds) = self.current_bounds else {
            return;
        };
        let Some(engine) = &self.engine else {
            return;
        };

        let x = (f32::from(position.x - bounds.origin.x) * self.current_scale).round() as i32;
        let y = (f32::from(position.y - bounds.origin.y) * self.current_scale).round() as i32;
        let mods = self.make_modifiers(modifiers);
        engine.mouse_move(x, y, false, mods);
    }

    fn forward_mouse_down(
        &mut self,
        button: MouseButton,
        position: Point<Pixels>,
        modifiers: Modifiers,
        clicks: usize,
    ) {
        self.active_mouse_buttons.insert(button);
        let Some(bounds) = self.current_bounds else {
            return;
        };
        let Some(engine) = &self.engine else {
            return;
        };

        let x = (f32::from(position.x - bounds.origin.x) * self.current_scale).round() as i32;
        let y = (f32::from(position.y - bounds.origin.y) * self.current_scale).round() as i32;
        let btn = map_mouse_button(button);
        let mods = self.make_modifiers(modifiers);
        engine.mouse_button(x, y, btn, false, clicks.max(1) as i32, mods);
    }

    fn forward_mouse_up(
        &mut self,
        button: MouseButton,
        position: Point<Pixels>,
        modifiers: Modifiers,
        clicks: usize,
    ) {
        self.active_mouse_buttons.remove(&button);
        let Some(bounds) = self.current_bounds else {
            return;
        };
        let Some(engine) = &self.engine else {
            return;
        };

        let x = (f32::from(position.x - bounds.origin.x) * self.current_scale).round() as i32;
        let y = (f32::from(position.y - bounds.origin.y) * self.current_scale).round() as i32;
        let btn = map_mouse_button(button);
        let mods = self.make_modifiers(modifiers);
        engine.mouse_button(x, y, btn, true, clicks.max(1) as i32, mods);
    }

    fn forward_mouse_leave(&mut self, window: &mut Window) {
        self.active_mouse_buttons.clear();
        let Some(bounds) = self.current_bounds else {
            return;
        };
        let Some(engine) = &self.engine else {
            return;
        };

        let pos = window.mouse_position();
        let x = (f32::from(pos.x - bounds.origin.x) * self.current_scale).round() as i32;
        let y = (f32::from(pos.y - bounds.origin.y) * self.current_scale).round() as i32;
        let mods = map_modifiers(window.modifiers());
        engine.mouse_move(x, y, true, mods);
    }

    fn forward_scroll(&self, event: &ScrollWheelEvent) {
        let Some(bounds) = self.current_bounds else {
            return;
        };
        let Some(engine) = &self.engine else {
            return;
        };

        let x = (f32::from(event.position.x - bounds.origin.x) * self.current_scale).round() as i32;
        let y = (f32::from(event.position.y - bounds.origin.y) * self.current_scale).round() as i32;

        let (dx, dy) = match event.delta {
            ScrollDelta::Pixels(p) => (
                (f32::from(p.x) * self.current_scale).round() as i32,
                (f32::from(p.y) * self.current_scale).round() as i32,
            ),
            ScrollDelta::Lines(l) => {
                let line_height = 20.0 * self.current_scale;
                (
                    (l.x * line_height).round() as i32,
                    (l.y * line_height).round() as i32,
                )
            }
        };

        let mods = self.make_modifiers(event.modifiers);
        engine.scroll(x, y, dx, dy, mods);
    }

    fn forward_key_down(&self, event: &KeyDownEvent) {
        let Some(engine) = &self.engine else {
            return;
        };

        let key_str = event.keystroke.key.clone();
        let key_char = event.keystroke.key_char.clone();
        let modifiers = self.make_modifiers(event.keystroke.modifiers);

        engine.key(BrowserKey {
            key: key_str,
            text: key_char.clone(),
            down: true,
            modifiers,
        });

        if !event.keystroke.modifiers.control && !event.keystroke.modifiers.platform {
            // Enter often arrives without a usable key_char; CEF needs a CHAR
            // event with '\r' for keypress / form submission.
            if matches!(event.keystroke.key.as_str(), "enter" | "return") {
                engine.text("\r");
            } else if let Some(text) = key_char {
                engine.text(&text);
            }
        }
    }

    fn forward_key_up(&self, event: &KeyUpEvent) {
        let Some(engine) = &self.engine else {
            return;
        };

        let key_str = event.keystroke.key.clone();
        let key_char = event.keystroke.key_char.clone();
        let modifiers = self.make_modifiers(event.keystroke.modifiers);

        engine.key(BrowserKey {
            key: key_str,
            text: key_char,
            down: false,
            modifiers,
        });
    }

    fn forward_focus(&self, focused: bool) {
        if let Some(engine) = &self.engine {
            engine.focus(focused);
        }
    }

    fn make_modifiers(&self, m: Modifiers) -> BrowserModifiers {
        let mut mods = map_modifiers(m);
        mods.left_button = self.active_mouse_buttons.contains(&MouseButton::Left);
        mods.middle_button = self.active_mouse_buttons.contains(&MouseButton::Middle);
        mods.right_button = self.active_mouse_buttons.contains(&MouseButton::Right);
        mods
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_home = self.snapshot.url.is_empty() || self.snapshot.url == DEFAULT_BROWSER_URL;
        if is_home {
            self.release_images(window);
        }
        let is_loading = self.snapshot.loading;
        let can_back = self.snapshot.can_go_back;
        let can_fwd = self.snapshot.can_go_forward;

        let nav_bar = div()
            .flex()
            .items_center()
            .gap_2()
            .p_2()
            .bg(theme::panel())
            .border_b_1()
            .border_color(theme::border())
            .flex_shrink_0()
            // Back button
            .child(
                button("browser-back", "◀")
                    .when(!can_back, |d| d.text_color(theme::muted()))
                    .when(can_back, |d| {
                        d.on_click(cx.listener(|this, _, _, cx| this.go_back(cx)))
                    }),
            )
            // Forward button
            .child(
                button("browser-forward", "▶")
                    .when(!can_fwd, |d| d.text_color(theme::muted()))
                    .when(can_fwd, |d| {
                        d.on_click(cx.listener(|this, _, _, cx| this.go_forward(cx)))
                    }),
            )
            // Reload / Stop button
            .child(
                button("browser-reload", if is_loading { "✕" } else { "⟳" })
                    .on_click(cx.listener(|this, _, _, cx| this.reload_or_stop(cx))),
            )
            .child(
                button("browser-home", "Home").on_click(cx.listener(|this, _, _, cx| {
                    this.address_input
                        .update(cx, |input, cx| input.set_text(DEFAULT_BROWSER_URL, cx));
                    this.submit_url(cx);
                })),
            )
            // Address bar
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                        if event.keystroke.key == "enter" {
                            cx.stop_propagation();
                            this.submit_url(cx);
                        }
                    }))
                    .child(self.address_input.clone()),
            )
            // Go button
            .child(
                button("browser-go", "Go")
                    .on_click(cx.listener(|this, _, _, cx| this.submit_url(cx))),
            )
            // Save / unsave the current page
            .child(
                button("browser-star", if self.is_bookmarked() { "★" } else { "☆" })
                    .when(self.is_bookmarked(), |d| d.text_color(theme::accent()))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_bookmark(cx))),
            )
            // Bookmarks list
            .child(
                chip(
                    "browser-bookmarks",
                    format!("Bookmarks ({})", self.bookmarks.len()),
                    self.show_bookmarks || is_home,
                )
                .when(!is_home, |d| {
                    d.on_click(cx.listener(|this, _, _, cx| {
                        this.show_bookmarks = !this.show_bookmarks;
                        cx.notify();
                    }))
                }),
            );

        let bookmarks_panel = (self.show_bookmarks || is_home).then(|| {
            let text_width = (f32::from(window.viewport_size().width) - 370.).clamp(80., 640.);
            let rows: Vec<AnyElement> = self
                .bookmarks
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id(("bm-open", i))
                                .flex_1()
                                .min_w_0()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::panel_hover()))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_sm()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .when(is_home, |d| d.py_3())
                                .child(
                                    div()
                                        .w(px(text_width))
                                        .text_ellipsis()
                                        .overflow_hidden()
                                        .child(b.title.clone()),
                                )
                                .child(
                                    div()
                                        .w(px(text_width))
                                        .text_ellipsis()
                                        .overflow_hidden()
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .child(b.url.clone()),
                                )
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.open_bookmark(i, cx)),
                                ),
                        )
                        .child(button(("bm-del", i), "Remove").on_click(
                            cx.listener(move |this, _, _, cx| this.remove_bookmark(i, cx)),
                        ))
                        .into_any_element()
                })
                .collect();
            div()
                .id("browser-bookmarks-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .when(!is_home, |d| d.max_h(px(200.)))
                .when(is_home, |d| d.flex_1().min_h_0().p_6())
                .overflow_y_scroll()
                .bg(theme::panel())
                .border_b_1()
                .border_color(theme::border())
                .flex_shrink_0()
                .when(is_home, |d| {
                    d.child(
                        div()
                            .w_full()
                            .max_w(px(760.))
                            .mx_auto()
                            .mb_4()
                            .child(div().text_xl().child("Bookmarks"))
                            .child(
                                div()
                                    .mt_2()
                                    .text_sm()
                                    .text_color(theme::muted())
                                    .child("Your saved pages. Pick one to start browsing."),
                            ),
                    )
                })
                .when_some(self.bookmark_error.clone(), |d, e| {
                    d.child(div().text_sm().text_color(theme::error()).child(e))
                })
                .when(rows.is_empty(), |d| {
                    d.child(
                        div()
                            .text_sm()
                            .text_color(theme::muted())
                            .w_full()
                            .max_w(px(760.))
                            .mx_auto()
                            .child(
                                "No bookmarks yet. Enter a URL above, then press ☆ to save a page.",
                            ),
                    )
                })
                .child(
                    div()
                        .w_full()
                        .max_w(px(760.))
                        .mx_auto()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(rows),
                )
        });

        // Error banner if any snapshot error exists
        let error_banner = self
            .snapshot
            .error
            .as_ref()
            .or(self.error.as_ref())
            .map(|err| {
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .bg(theme::error())
                    .text_color(gpui::white())
                    .text_sm()
                    .flex_shrink_0()
                    .child(div().child(err.clone()))
                    .child(
                        button("browser-retry-btn", "Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                    )
            });

        // Viewport element (canvas drawing + event forwarding)
        let viewport_element = if let Some(fatal_error) = &self.error {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .bg(theme::bg())
                .child(
                    div()
                        .text_lg()
                        .text_color(theme::error())
                        .child("Browser Initialization Error"),
                )
                .child(
                    div()
                        .max_w(px(600.))
                        .text_sm()
                        .text_color(theme::muted())
                        .child(fatal_error.clone()),
                )
                .child(
                    primary_button("browser-retry-main", "Retry")
                        .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                )
                .into_any_element()
        } else {
            let render_img = self.cached_render_image.clone();
            let retired_images = std::mem::take(&mut self.retired_render_images);
            let view_handle = cx.entity().downgrade();

            div()
                .id("browser-viewport")
                .flex_1()
                .min_h_0()
                .bg(theme::bg())
                .track_focus(&self.focus_handle)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, window, _cx| {
                        window.focus(&this.focus_handle);
                        this.forward_mouse_down(
                            MouseButton::Left,
                            event.position,
                            event.modifiers,
                            event.click_count,
                        );
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, event: &MouseDownEvent, window, _cx| {
                        window.focus(&this.focus_handle);
                        this.forward_mouse_down(
                            MouseButton::Right,
                            event.position,
                            event.modifiers,
                            event.click_count,
                        );
                    }),
                )
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(|this, event: &MouseDownEvent, window, _cx| {
                        window.focus(&this.focus_handle);
                        this.forward_mouse_down(
                            MouseButton::Middle,
                            event.position,
                            event.modifiers,
                            event.click_count,
                        );
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                        this.forward_mouse_up(
                            MouseButton::Left,
                            event.position,
                            event.modifiers,
                            event.click_count,
                        );
                    }),
                )
                .on_mouse_up(
                    MouseButton::Right,
                    cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                        this.forward_mouse_up(
                            MouseButton::Right,
                            event.position,
                            event.modifiers,
                            event.click_count,
                        );
                    }),
                )
                .on_mouse_up(
                    MouseButton::Middle,
                    cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                        this.forward_mouse_up(
                            MouseButton::Middle,
                            event.position,
                            event.modifiers,
                            event.click_count,
                        );
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, _cx| {
                    this.forward_mouse_move(event.position, event.modifiers);
                }))
                .on_hover(cx.listener(|this, hovered: &bool, window, _cx| {
                    if !hovered {
                        this.forward_mouse_leave(window);
                    }
                }))
                .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, _cx| {
                    this.forward_scroll(event);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, _cx| {
                    this.forward_key_down(event);
                }))
                .on_key_up(cx.listener(|this, event: &KeyUpEvent, _window, _cx| {
                    this.forward_key_up(event);
                }))
                .child(BrowserViewportElement {
                    image: render_img,
                    retired_images,
                    on_layout: Box::new(move |bounds, scale, cx| {
                        if let Some(view) = view_handle.upgrade() {
                            view.update(cx, |this, _cx| {
                                this.update_viewport_size(bounds, scale);
                            });
                        }
                    }),
                })
                .into_any_element()
        };

        // Downloads bar / list
        let active_downloads: Vec<BrowserDownload> = self
            .snapshot
            .downloads
            .iter()
            .filter(|dl| !self.dismissed_download_ids.contains(&dl.id))
            .cloned()
            .collect();

        let downloads_bar = if active_downloads.is_empty() {
            None
        } else {
            Some(
                div()
                    .id("browser-downloads")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .bg(theme::panel())
                    .border_t_1()
                    .border_color(theme::border())
                    .children(
                        active_downloads
                            .into_iter()
                            .map(|dl| render_download_row(dl, cx)),
                    ),
            )
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(nav_bar)
            .children(bookmarks_panel)
            .children(error_banner)
            .when(!is_home, |d| d.child(viewport_element))
            .children(downloads_bar)
    }
}

/// Helper element to paint CEF BGRA frames and measure viewport bounds
struct BrowserViewportElement {
    image: Option<Arc<RenderImage>>,
    retired_images: Vec<Arc<RenderImage>>,
    on_layout: Box<dyn Fn(Bounds<Pixels>, f32, &mut App)>,
}

impl IntoElement for BrowserViewportElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for BrowserViewportElement {
    type RequestLayoutState = Style;
    type PrepaintState = Bounds<Pixels>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = gpui::Length::Definite(gpui::DefiniteLength::Fraction(1.0));
        style.size.height = gpui::Length::Definite(gpui::DefiniteLength::Fraction(1.0));
        let layout_id = window.request_layout(style.clone(), [], cx);
        (layout_id, style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let scale = window.scale_factor();
        (self.on_layout)(bounds, scale, cx);
        bounds
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        if let Some(image) = &self.image {
            let _ = window.paint_image(bounds, Corners::default(), image.clone(), 0, false);
        }
        // Retire previous atlas entries only after painting their replacement.
        for image in self.retired_images.drain(..) {
            let _ = window.drop_image(image);
        }
    }
}

fn render_download_row(dl: BrowserDownload, cx: &mut Context<BrowserView>) -> AnyElement {
    let id = dl.id;
    let path = dl.path.clone();
    let name = dl.name.clone();
    let status = dl.status.clone();

    let status_str = match &status {
        DownloadStatus::Downloading => {
            if dl.total_bytes > 0 {
                let pct =
                    (dl.received_bytes as f64 / dl.total_bytes as f64 * 100.0).clamp(0.0, 100.0);
                format!("{:.1}% ({})", pct, format_bytes(dl.received_bytes))
            } else {
                format_bytes(dl.received_bytes)
            }
        }
        DownloadStatus::Complete => "Complete".to_string(),
        DownloadStatus::Cancelled => "Cancelled".to_string(),
        DownloadStatus::Interrupted => "Interrupted".to_string(),
    };

    let action_btn = match status {
        DownloadStatus::Downloading => button(("cancel-dl", id as usize), "Cancel")
            .on_click(cx.listener(move |this, _, _, cx| this.cancel_download(id, cx))),
        DownloadStatus::Complete => primary_button(("preview-dl", id as usize), "Preview")
            .on_click(cx.listener(move |this, _, _, cx| this.preview_download(path.clone(), cx))),
        DownloadStatus::Cancelled | DownloadStatus::Interrupted => {
            button(("dismiss-dl", id as usize), "Dismiss")
                .on_click(cx.listener(move |this, _, _, cx| this.dismiss_download(id, cx)))
        }
    };

    div()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .py_1()
        .bg(theme::panel_hover())
        .rounded_sm()
        .text_sm()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().font_weight(gpui::FontWeight::BOLD).child(name))
                .child(div().text_color(theme::muted()).child(status_str)),
        )
        .child(action_btn)
        .into_any_element()
}

fn frame_to_render_image(frame: crate::browser_types::BrowserFrame) -> Option<RenderImage> {
    let width = frame.width;
    let height = frame.height;
    if width == 0 || height == 0 {
        return None;
    }
    let buffer = ImageBuffer::from_raw(width, height, frame.pixels)?;
    let img_frame = Frame::new(buffer);
    Some(RenderImage::new(vec![img_frame]))
}

fn downloads_changed(a: &[BrowserDownload], b: &[BrowserDownload]) -> bool {
    if a.len() != b.len() {
        return true;
    }
    for (x, y) in a.iter().zip(b.iter()) {
        if x.id != y.id
            || x.status != y.status
            || x.received_bytes != y.received_bytes
            || x.total_bytes != y.total_bytes
        {
            return true;
        }
    }
    false
}

pub(crate) fn format_bytes(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn map_mouse_button(b: MouseButton) -> BrowserMouseButton {
    match b {
        MouseButton::Left => BrowserMouseButton::Left,
        MouseButton::Middle => BrowserMouseButton::Middle,
        MouseButton::Right => BrowserMouseButton::Right,
        MouseButton::Navigate(_) => BrowserMouseButton::Left,
    }
}

fn map_modifiers(m: Modifiers) -> BrowserModifiers {
    BrowserModifiers {
        shift: m.shift,
        control: m.control,
        alt: m.alt,
        command: m.platform,
        left_button: false,
        middle_button: false,
        right_button: false,
    }
}
