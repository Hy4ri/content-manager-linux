//! Small styled building blocks.

use std::path::PathBuf;
use std::sync::Arc;

use ac_core::ai_line::RacingLine;

use gpui::{
    AnyElement, Bounds, BoxShadow, Div, ElementId, FontWeight, Hsla, ObjectFit, PathBuilder, ScrollHandle,
    SharedString, Stateful, canvas, div, fill, img, linear_color_stop, linear_gradient, point, prelude::*, px,
    size, transparent_black,
};

use crate::theme;

/// A fixed-height frame that always shows the whole image, centered.
///
/// GPUI's `img` derives its size from the image aspect ratio, so a
/// `size_full` image in a wide box grows faster than the box and the bottom
/// is clipped. Pinning the image height and capping its width avoids that;
/// `Contain` letterboxes when the width cap kicks in. Callers set the width.
pub fn picture(path: Option<PathBuf>, height: f32) -> Div {
    div()
        .h(px(height))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .rounded(px(theme::RADIUS_SM))
        // Previews carry their own dark backdrop; only empty frames get a fill.
        .when(path.is_none(), |d| d.bg(theme::bg()))
        .when_some(path, |d, p| {
            d.child(
                img(p)
                    .h(px(height))
                    .max_w_full()
                    .rounded(px(theme::RADIUS_SM))
                    .object_fit(ObjectFit::Contain),
            )
        })
}

/// Square track map: the AI racing line drawn as a vector, or `outline.png` when unavailable.
pub fn track_map(line: Option<Arc<RacingLine>>, outline: Option<PathBuf>, side: f32) -> Div {
    let Some(line) = line else { return picture(outline, side).w(px(side)); };
    div().size(px(side)).flex_shrink_0().child(canvas(|_, _, _| {}, move |bounds, _, window, _| {
        let pad = 6.;
        let span = f32::from(bounds.size.width.min(bounds.size.height)) - 2. * pad;
        let at = |p: &[f32; 2]| point(bounds.origin.x + px(pad + p[0] * span), bounds.origin.y + px(pad + p[1] * span));
        let mut path = PathBuilder::stroke(px(2.5));
        path.move_to(at(&line.points[0]));
        for p in &line.points[1..] { path.line_to(at(p)); }
        if line.closed { path.close(); }
        if let Ok(path) = path.build() { window.paint_path(path, theme::text()); }
        let start = at(&line.points[0]); let d = px(8.);
        window.paint_quad(fill(Bounds::new(point(start.x - d / 2., start.y - d / 2.), size(d, d)), theme::accent()).corner_radii(d / 2.));
    }).size_full())
}

/// Label column + control, so every setting lines up on the same grid.
pub fn setting_row(label: impl Into<SharedString>, control: impl IntoElement) -> Div {
    div().flex().items_center().gap_2().min_h(px(28.)).child(div().w(px(120.)).flex_shrink_0().text_sm().text_color(theme::muted()).child(label.into())).child(control)
}

/// Two or more mutually exclusive chips drawn as one control.
pub fn segmented(items: Vec<Stateful<Div>>) -> Div {
    div()
        .flex()
        .gap_1()
        .children(items.into_iter().map(|i| i.into_any_element()).collect::<Vec<AnyElement>>())
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .min_h(px(theme::MIN_TARGET_H))
        .px_3()
        .py_1()
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(theme::border())
        .bg(theme::panel())
        .text_sm()
        .cursor_pointer()
        .hover(|s| s.bg(theme::panel_hover()))
        .child(label.into())
}

pub fn primary_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .min_h(px(theme::MIN_TARGET_H))
        .px_6()
        .py_2()
        .rounded(px(theme::RADIUS_SM))
        .bg(theme::accent())
        .text_color(theme::on_accent())
        .font_weight(FontWeight::BOLD)
        .cursor_pointer()
        .hover(|s| s.bg(theme::accent_hover()))
        .child(label.into())
}

pub fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .min_h(px(theme::MIN_TARGET_H))
        .px_3()
        .py_1()
        .rounded(px(theme::RADIUS_SM))
        .text_sm()
        .cursor_pointer()
        .border_1()
        .when(active, |d| {
            d.bg(theme::accent())
                .text_color(theme::on_accent())
                .border_color(theme::accent())
        })
        .when(!active, |d| {
            d.border_color(theme::border()).hover(|s| s.bg(theme::panel_hover()))
        })
        .child(label.into())
}

pub fn section_title(text: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .text_color(theme::muted())
        .font_weight(FontWeight::BOLD)
        .mt_2()
        .child(text.into().to_uppercase())
}

pub fn field(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .gap_2()
        .text_sm()
        .child(
            div()
                .w(px(110.))
                .flex_shrink_0()
                .text_color(theme::muted())
                .child(label.into()),
        )
        .child(div().child(value.into()))
}

pub fn panel() -> Div {
    div()
        .bg(theme::panel())
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .p_3()
}

/// Applies a focus ring to a stateful div.
///
/// When focused: 2px accent border + box_shadow glow.
/// When not: transparent 2px border so layout does not shift.
pub fn focus_ring(el: Stateful<Div>, focused: bool) -> Stateful<Div> {
    if focused {
        el.border_2()
            .border_color(theme::accent())
            .shadow(vec![BoxShadow {
                color: theme::focus_glow().into(),
                offset: point(px(0.), px(0.)),
                blur_radius: px(6.),
                spread_radius: px(2.),
            }])
    } else {
        el.border_2().border_color(transparent_black())
    }
}

/// Renders a small rounded pill button label (e.g. "A") with associated action text (e.g. "Select").
pub fn hint(button_label: &str, action: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .px_2()
                .py_0p5()
                .rounded(px(theme::RADIUS_SM))
                .bg(theme::raised())
                .border_1()
                .border_color(theme::border())
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(theme::text())
                .child(button_label.to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::muted())
                .child(action.to_string()),
        )
}

/// Bottom/footer bar presenting gamepad/keyboard hints and an optional primary action element.
pub fn hint_bar(hints: Vec<Div>, primary: Option<AnyElement>) -> Div {
    let mut left = div().flex().items_center().gap_4();
    for hint_el in hints {
        left = left.child(hint_el);
    }

    let mut bar = div()
        .h(px(56.))
        .px_6()
        .bg(theme::surface())
        .border_t_1()
        .border_color(theme::border())
        .flex()
        .items_center()
        .justify_between()
        .child(left);

    if let Some(pri) = primary {
        bar = bar.child(pri);
    }

    bar
}

/// Width of the fade + chevron that marks a scrollable edge of `h_scroller`.
const SCROLL_EDGE_W: f32 = 40.;

/// Wraps a horizontally scrolling row so each edge with hidden content fades out and
/// shows a clickable chevron that pages the row one view-width in that direction.
///
/// `bg` is the color behind the row so the fade blends into it.
pub fn h_scroller(
    id: &'static str,
    handle: &ScrollHandle,
    bg: impl Into<Hsla>,
    strip: Stateful<Div>,
) -> Div {
    let bg: Hsla = bg.into();
    let edges = |h: &ScrollHandle| {
        let x = -f32::from(h.offset().x);
        let max = f32::from(h.max_offset().width);
        (x > 1., x < max - 1.)
    };
    let shown = edges(handle);
    let (left, right) = shown;
    let check = handle.clone();
    let edge = |angle: f32, at_left: bool| {
        let h = handle.clone();
        div()
            .id((id, at_left as usize))
            .absolute()
            .top_0()
            .bottom_0()
            .when(at_left, |d| d.left_0())
            .when(!at_left, |d| d.right_0())
            .w(px(SCROLL_EDGE_W))
            .flex()
            .items_center()
            .when(at_left, |d| d.justify_start())
            .when(!at_left, |d| d.justify_end())
            .bg(linear_gradient(
                angle,
                linear_color_stop(bg, 0.35),
                linear_color_stop(bg.opacity(0.), 1.),
            ))
            .cursor_pointer()
            .block_mouse_except_scroll()
            .on_click(move |_, window, _| {
                let view = f32::from(h.bounds().size.width);
                let max = f32::from(h.max_offset().width);
                let step = (view - 2. * SCROLL_EDGE_W).max(view * 0.5);
                let x = -f32::from(h.offset().x) + if at_left { -step } else { step };
                h.set_offset(point(px(-x.clamp(0., max)), h.offset().y));
                window.refresh();
            })
            .child(
                div()
                    .size(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(theme::RADIUS_SM))
                    .bg(theme::overlay())
                    .border_1()
                    .border_color(theme::border())
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .child(if at_left { "‹" } else { "›" }),
            )
    };
    div()
        .relative()
        .flex()
        .flex_col()
        .min_w_0()
        .child(strip.overflow_x_scroll().track_scroll(handle))
        .child(
            // The strip updates its scroll bounds during prepaint, after render read them.
            canvas(
                move |_, window, _| {
                    if edges(&check) != shown {
                        window.request_animation_frame();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_0(),
        )
        .when(left, |d| d.child(edge(90., true)))
        .when(right, |d| d.child(edge(270., false)))
}
