//! Tab ownership and crash-safe session restoration for the mod browser.

use std::time::Duration;

use ac_core::bookmarks::Bookmark;
use ac_core::browser_session::BrowserSession;
use gpui::{Context, Entity, EventEmitter, Render, ScrollHandle, Task, Window, div, prelude::*, px};

use crate::browser_view::{BrowserView, DEFAULT_BROWSER_URL, PreviewDownload};
use crate::{
    theme,
    widgets::{button, h_scroller},
};

struct Tab {
    id: usize,
    page: Entity<BrowserView>,
}

pub struct BrowserTabs {
    tabs: Vec<Tab>,
    selected: usize,
    next_id: usize,
    active: bool,
    saved: BrowserSession,
    session_error: Option<String>,
    strip: ScrollHandle,
    _refresh: Task<()>,
}

impl EventEmitter<PreviewDownload> for BrowserTabs {}

impl BrowserTabs {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (saved, session_error) = match BrowserSession::load() {
            Ok(session) => (session, None),
            Err(error) => (
                BrowserSession::default(),
                Some(format!("Cannot restore tabs: {error:#}")),
            ),
        };
        let refresh = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| this.refresh(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut this = Self {
            tabs: Vec::new(),
            selected: 0,
            next_id: 0,
            active: true,
            saved: saved.clone(),
            session_error,
            strip: ScrollHandle::new(),
            _refresh: refresh,
        };
        for tab in &saved.tabs {
            this.push_tab(&tab.url, window, cx);
        }
        if this.tabs.is_empty() {
            this.push_tab(DEFAULT_BROWSER_URL, window, cx);
        }
        this.selected = saved.active_tab.min(this.tabs.len() - 1);
        this.update_visibility(cx);
        this
    }

    fn push_tab(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let page = cx.new(|cx| BrowserView::new_at(url, window, cx));
        cx.subscribe(&page, |_, _, event: &PreviewDownload, cx| {
            cx.emit(event.clone())
        })
        .detach();
        self.tabs.push(Tab {
            id: self.next_id,
            page,
        });
        self.next_id += 1;
    }

    fn update_visibility(&self, cx: &mut Context<Self>) {
        for (i, tab) in self.tabs.iter().enumerate() {
            tab.page.update(cx, |page, cx| {
                page.set_active(self.active && i == self.selected, cx)
            });
        }
    }

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.active = active;
        self.update_visibility(cx);
    }

    fn session(&self, cx: &Context<Self>) -> BrowserSession {
        BrowserSession {
            tabs: self
                .tabs
                .iter()
                .map(|tab| tab.page.read(cx).session_page())
                .collect(),
            active_tab: self.selected,
        }
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        let session = self.session(cx);
        if session != self.saved {
            // Remember the attempted state too: don't retry a failed disk write every frame.
            self.saved = session.clone();
            self.session_error = session
                .save()
                .err()
                .map(|error| format!("Cannot save tabs: {error:#}"));
            cx.notify();
        }
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut requests = Vec::new();
        for tab in &self.tabs {
            requests.extend(tab.page.read(cx).take_new_tabs());
        }
        for url in requests {
            self.add_tab(&url, window, cx);
        }
        self.persist(cx);
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || index == self.selected {
            return;
        }
        self.selected = index;
        self.update_visibility(cx);
        self.persist(cx);
        cx.notify();
    }

    fn add_tab(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.push_tab(url, window, cx);
        self.selected = self.tabs.len() - 1;
        self.update_visibility(cx);
        self.persist(cx);
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs[index]
            .page
            .update(cx, |page, _| page.release_images(window));
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.push_tab(DEFAULT_BROWSER_URL, window, cx);
            self.selected = 0;
        } else if index < self.selected {
            self.selected -= 1;
        } else {
            self.selected = self.selected.min(self.tabs.len() - 1);
        }
        self.update_visibility(cx);
        self.persist(cx);
        cx.notify();
    }
}

impl Render for BrowserTabs {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let page: Bookmark = tab.page.read(cx).session_page();
                let title = if page.title.trim().is_empty() {
                    page.url
                } else {
                    page.title
                };
                div()
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .rounded_sm()
                    .bg(if i == self.selected {
                        theme::panel_hover()
                    } else {
                        theme::panel()
                    })
                    .border_1()
                    .border_color(if i == self.selected {
                        theme::accent()
                    } else {
                        theme::border()
                    })
                    .child(
                        div()
                            .id(("browser-tab", tab.id))
                            .w(px(160.))
                            .px_2()
                            .py_1()
                            .text_sm()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .cursor_pointer()
                            .child(title)
                            .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx))),
                    )
                    // Sibling controls: closing a tab must not also select it.
                    .child(button(("browser-tab-close", tab.id), "×").on_click(
                        cx.listener(move |this, _, window, cx| this.close_tab(i, window, cx)),
                    ))
            })
            .collect::<Vec<_>>();

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .flex_shrink_0()
                    .bg(theme::panel())
                    .child(
                        h_scroller(
                            "browser-tabs-edge",
                            &self.strip,
                            theme::panel(),
                            div()
                                .id("browser-tabs")
                                .flex()
                                .gap_2()
                                .children(tabs),
                        )
                        .flex_1(),
                    )
                    .child(button("browser-new-tab", "+").on_click(cx.listener(
                        |this, _, window, cx| this.add_tab(DEFAULT_BROWSER_URL, window, cx),
                    ))),
            )
            .when_some(self.session_error.clone(), |d, error| {
                d.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .text_color(theme::error())
                        .child(error),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(self.tabs[self.selected].page.clone()),
            )
    }
}
