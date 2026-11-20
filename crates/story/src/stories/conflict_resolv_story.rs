use gpui_kit::component::{
    ActiveTheme, Sizable as _,
    button::{Button, ButtonVariants as _},
    conflict_resolv::{
        BlockResolution, ConflictResolv, ConflictResolvState, ConflictResolvVirtual,
    },
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div, px,
};

/// A rebase of local retry work onto upstream: three conflicts, one with a
/// diff3 base, one two-way, one deletion against a change.
const CONFLICTED: &str = "use std::time::Duration;\n\
\n\
pub struct RetryPolicy {\n\
<<<<<<< feature/retry-backoff\n\
    pub max_attempts: u32,\n\
    pub base_delay: Duration,\n\
||||||| merged common ancestors\n\
    pub max_attempts: u32,\n\
=======\n\
    pub attempts: u32,\n\
>>>>>>> main\n\
}\n\
\n\
impl RetryPolicy {\n\
    pub fn delay(&self, attempt: u32) -> Duration {\n\
<<<<<<< feature/retry-backoff\n\
        self.base_delay * 2_u32.pow(attempt.min(6))\n\
=======\n\
        Duration::from_millis(100) * attempt\n\
>>>>>>> main\n\
    }\n\
\n\
<<<<<<< feature/retry-backoff\n\
=======\n\
    pub fn describe(&self) -> String {\n\
        format!(\"{} attempts\", self.attempts)\n\
    }\n\
>>>>>>> main\n\
}\n";

/// A generated service module of a few thousand realistic Rust lines with
/// conflicts spread through it, for feeling scroll, typing and highlighting
/// in both renderers rather than judging them on a toy file.
fn large_conflicted() -> String {
    let mut text = String::new();
    text.push_str("use std::time::Duration;\n\n");
    for section in 0..25 {
        for i in 0..18 {
            text.push_str(&format!(
                "/// Shapes request {i} of section {section} before dispatch.\n\
                 fn prepare_{section}_{i}(payload: &str, attempt: u32) -> String {{\n\
                 \x20   let budget = Duration::from_millis(50 + attempt as u64 * {i});\n\
                 \x20   let label = format!(\"s{section}-{i}-{{}}\", payload.len());\n\
                 \x20   format!(\"{{label}}:{{}}ms\", budget.as_millis())\n\
                 }}\n\n"
            ));
        }
        text.push_str(&format!(
            "<<<<<<< feature/retry-backoff\n\
             fn retry_delay_{section}(attempt: u32) -> Duration {{\n\
             \x20   Duration::from_millis(50) * 2_u32.pow(attempt.min(6))\n\
             }}\n\
             =======\n\
             fn retry_delay_{section}(attempt: u32) -> Duration {{\n\
             \x20   Duration::from_millis(100) * attempt\n\
             }}\n\
             >>>>>>> main\n\n"
        ));
    }
    text
}

pub struct ConflictResolvStory {
    state: Entity<ConflictResolvState>,
    large: Entity<ConflictResolvState>,
    /// Which renderer shows the large fixture; both drive the same state,
    /// so flipping keeps every pick and edit.
    virtualized: bool,
    _subscriptions: Vec<Subscription>,
}

impl super::Story for ConflictResolvStory {
    fn title() -> &'static str {
        "ConflictResolv"
    }

    fn description() -> &'static str {
        "Three-pane merge conflict resolution: your changes, the editable result, upstream changes."
    }

    fn new_view(window: &mut Window, cx: &mut App) -> Entity<impl Render> {
        Self::view(window, cx)
    }
}

impl ConflictResolvStory {
    pub fn view(_window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let state = cx.new(|_| {
                ConflictResolvState::parse("src/retry.rs", CONFLICTED)
                    .expect("Story conflict is valid")
            });
            // One block arrives picked and adjusted by hand, so the gallery
            // shows the embedded result editor and the hand-edited tint.
            state.update(cx, |state, cx| {
                state.take_upstream(1, cx);
                state.edit_block(1, "        Duration::from_millis(250) * attempt", cx);
            });
            let large = cx.new(|_| {
                ConflictResolvState::parse("src/service.rs", &large_conflicted())
                    .expect("Generated conflict is valid")
            });
            Self {
                // Picks notify observers; observing keeps the counters current.
                _subscriptions: vec![
                    cx.observe(&state, |_, _, cx| cx.notify()),
                    cx.observe(&large, |_, _, cx| cx.notify()),
                ],
                state,
                large,
                virtualized: true,
            }
        })
    }
}

fn status_line(state: &ConflictResolvState) -> String {
    let total = state.block_count();
    let resolved = (0..total)
        .filter(|ix| state.resolution(*ix) != BlockResolution::Unresolved)
        .count();
    if state.all_resolved() {
        format!("All {total} blocks resolved")
    } else {
        format!("Resolved {resolved} of {total} blocks")
    }
}

impl Render for ConflictResolvStory {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = status_line(self.state.read(cx));
        let large_status = status_line(self.large.read(cx));
        let virtualized = self.virtualized;
        let renderer = |label: &'static str, selects: bool| {
            let button = Button::new(("large-renderer", selects as usize))
                .xsmall()
                .label(label);
            if selects == virtualized {
                button.primary()
            } else {
                button.outline()
            }
        };
        // The same state behind both renderers: flip while scrolled into the
        // middle and compare how each one feels on an identical file.
        let large: gpui_kit::AnyElement = if virtualized {
            ConflictResolvVirtual::new(&self.large)
                .size_full()
                .yours_label("Your changes · feature/retry-backoff")
                .upstream_label("Upstream changes · main")
                .into_any_element()
        } else {
            div()
                .size_full()
                .overflow_y_scrollbar()
                .id("large-classic")
                .child(
                    ConflictResolv::new(&self.large)
                        .w_full()
                        .yours_label("Your changes · feature/retry-backoff")
                        .upstream_label("Upstream changes · main"),
                )
                .into_any_element()
        };
        v_flex()
            .w_full()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(status),
            )
            .child(
                ConflictResolv::new(&self.state)
                    .w_full()
                    .yours_label("Your changes · feature/retry-backoff")
                    .upstream_label("Upstream changes · main"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        renderer("Classic", false).on_click(cx.listener(|this, _, _, cx| {
                            this.virtualized = false;
                            cx.notify();
                        })),
                    )
                    .child(
                        renderer("Virtualized", true).on_click(cx.listener(|this, _, _, cx| {
                            this.virtualized = true;
                            cx.notify();
                        })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Large fixture · {large_status} · {}",
                                if virtualized {
                                    "virtualized rows, background highlighting"
                                } else {
                                    "classic full-height rows"
                                }
                            )),
                    ),
            )
            .child(div().w_full().h(px(560.)).child(large))
    }
}
