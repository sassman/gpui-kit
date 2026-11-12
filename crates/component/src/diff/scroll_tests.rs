use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, ScrollDelta,
    ScrollEvent, Styled as _, TestAppContext, VisualTestContext, Window, div, point, px,
};

use super::{Diff, DiffState, document::fixture};

/// Every line changed, so the unified patch is two rows per line.
const LINES: usize = 300;

struct Host {
    state: Entity<DiffState>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(400.))
            .h(px(200.))
            .child(Diff::new(&self.state).size_full())
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
}

fn long_patch(cx: &mut TestAppContext) -> (Entity<DiffState>, &mut VisualTestContext) {
    cx.update(crate::init);
    let original = (0..LINES)
        .map(|ix| format!("old {ix}\n"))
        .collect::<String>();
    let modified = (0..LINES)
        .map(|ix| format!("new {ix}\n"))
        .collect::<String>();
    let state =
        cx.new(|cx| DiffState::new([fixture::modified("long.txt", &original, &modified)], cx));
    let (_, cx) = cx.add_window_view({
        let state = state.clone();
        move |_, _| Host { state }
    });
    // The body records its size on the first paint and lays out again.
    for _ in 0..3 {
        draw(cx);
    }
    (state, cx)
}

#[gpui::test]
fn the_whole_patch_keeps_its_height_after_the_first_layout(cx: &mut TestAppContext) {
    let (state, cx) = long_patch(cx);
    let (max, viewport) = cx.update(|_, cx| {
        let list = state.read(cx).list();
        (
            list.max_offset_for_scrollbar().y,
            list.viewport_bounds().size.height,
        )
    });
    // Rows past the viewport have not been rendered; without their hinted
    // height the patch would read as barely taller than one screen.
    assert!(
        max > viewport * 10.,
        "max offset {max:?} for a {viewport:?} viewport"
    );
}

#[gpui::test]
fn the_wheel_scrolls_past_the_rows_rendered_so_far(cx: &mut TestAppContext) {
    let (state, cx) = long_patch(cx);
    for _ in 0..50 {
        cx.simulate_event(ScrollEvent {
            position: point(px(100.), px(120.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(-40.))),
            ..Default::default()
        });
        draw(cx);
    }
    let top = cx.update(|_, cx| state.read(cx).list().logical_scroll_top().item_ix);
    // 2000 px of wheel travel crosses far more than the first screen of rows.
    assert!(top > 40, "scrolled to row {top}");
}

/// Wraps its lines, so rows are taller than the height the list estimates.
struct WrappedHost {
    state: Entity<DiffState>,
}

impl Render for WrappedHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(400.))
            .h(px(200.))
            .child(Diff::new(&self.state).soft_wrap(true).size_full())
    }
}

#[gpui::test]
fn a_mode_switch_at_the_top_keeps_the_file_header_in_view(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let line = |prefix: &str, ix: usize| {
        format!(
            "{prefix} {ix} {}\n",
            "a long line that wraps in the column ".repeat(3)
        )
    };
    let original = (0..40).map(|ix| line("old", ix)).collect::<String>();
    let modified = (0..40).map(|ix| line("new", ix)).collect::<String>();
    let state =
        cx.new(|cx| DiffState::new([fixture::modified("long.txt", &original, &modified)], cx));
    let (_, cx) = cx.add_window_view({
        let state = state.clone();
        move |_, _| WrappedHost { state }
    });
    for _ in 0..3 {
        draw(cx);
    }
    let top =
        |cx: &mut VisualTestContext| cx.update(|_, cx| state.read(cx).list().logical_scroll_top());
    assert_eq!(top(cx).item_ix, 0, "starts at the file header");

    state.update(cx, |state, cx| state.set_mode(super::DiffMode::Split, cx));
    for _ in 0..3 {
        draw(cx);
    }
    let after = top(cx);
    assert_eq!(
        (after.item_ix, after.offset_in_item),
        (0, px(0.)),
        "the file header stays in view"
    );
}

/// A split patch whose modified side is far wider than the original's.
fn split_patch(cx: &mut TestAppContext) -> (Entity<DiffState>, &mut VisualTestContext) {
    cx.update(crate::init);
    let original = (0..40).map(|ix| format!("o {ix}\n")).collect::<String>();
    let modified = (0..40)
        .map(|ix| format!("modified {ix} {}\n", "x".repeat(300)))
        .collect::<String>();
    let state =
        cx.new(|cx| DiffState::new([fixture::modified("wide.txt", &original, &modified)], cx));
    state.update(cx, |state, cx| {
        state.set_mode(super::DiffMode::Split, cx);
    });
    let (_, cx) = cx.add_window_view({
        let state = state.clone();
        move |_, _| Host { state }
    });
    for _ in 0..3 {
        draw(cx);
    }
    (state, cx)
}

fn wheel_at(cx: &mut VisualTestContext, x: f32, delta_x: f32) {
    cx.simulate_event(ScrollEvent {
        position: point(px(x), px(100.)),
        delta: ScrollDelta::Pixels(point(px(delta_x), px(0.))),
        ..Default::default()
    });
    draw(cx);
}

#[gpui::test]
fn split_panels_scroll_separately(cx: &mut TestAppContext) {
    let (state, cx) = split_patch(cx);

    // A wheel over the right half moves only the modified panel.
    wheel_at(cx, 300., -60.);
    let offsets = cx.update(|_, cx| {
        let split = state.read(cx).split_scroll().clone();
        [split[0].offset().x, split[1].offset().x]
    });
    assert_eq!(offsets[0], px(0.));
    assert!(offsets[1] < px(-50.), "right panel at {:?}", offsets[1]);
    // The shared strip offset never moves in split mode.
    let strip = cx.update(|_, cx| state.read(cx).horizontal_scroll().offset().x);
    assert_eq!(strip, px(0.));

    // The original side has nothing wider than its panel, so a wheel over
    // the left half scrolls neither panel: the event bubbles to the list.
    wheel_at(cx, 100., -60.);
    let offsets = cx.update(|_, cx| {
        let split = state.read(cx).split_scroll().clone();
        [split[0].offset().x, split[1].offset().x]
    });
    assert_eq!(offsets[0], px(0.));
    assert!(offsets[1] < px(-50.), "right panel kept {:?}", offsets[1]);
}

#[gpui::test]
fn leaving_split_mode_rests_both_panels(cx: &mut TestAppContext) {
    let (state, cx) = split_patch(cx);
    wheel_at(cx, 300., -60.);
    state.update(cx, |state, cx| {
        state.set_mode(super::DiffMode::Unified, cx);
    });
    let offsets = cx.update(|_, cx| {
        let split = state.read(cx).split_scroll().clone();
        [split[0].offset().x, split[1].offset().x]
    });
    assert_eq!(offsets, [px(0.); 2]);
}

#[gpui::test]
fn width_candidates_stay_on_their_side(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let original = "short\n";
    let modified = format!("{}\n", "y".repeat(400));
    let files = [fixture::modified("sides.txt", original, &modified)];
    let [original_side, modified_side] = super::width_candidates(&files, 8);
    assert!(
        original_side
            .iter()
            .all(|line| line.len() < 40 && !line.contains('y')),
        "the deleted side must not carry the modified side's lines"
    );
    assert!(modified_side.iter().any(|line| line.len() >= 400));
}
