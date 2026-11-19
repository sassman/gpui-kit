use gpui_kit::component::{
    ActiveTheme,
    conflict_resolv::{BlockResolution, ConflictResolv, ConflictResolvState},
    v_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div,
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

pub struct ConflictResolvStory {
    state: Entity<ConflictResolvState>,
    _subscription: Subscription,
}

impl super::Story for ConflictResolvStory {
    fn title() -> &'static str {
        "ConflictResolv"
    }

    fn description() -> &'static str {
        "Three-pane merge conflict resolution: your changes, the result, upstream changes."
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
            Self {
                // Picks notify observers; observing keeps the counter current.
                _subscription: cx.observe(&state, |_, _, cx| cx.notify()),
                state,
            }
        })
    }
}

impl Render for ConflictResolvStory {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let resolved = (0..state.block_count())
            .filter(|ix| state.resolution(*ix) != BlockResolution::Unresolved)
            .count();
        let total = state.block_count();
        let status = if state.all_resolved() {
            format!("All {total} blocks resolved")
        } else {
            format!("Resolved {resolved} of {total} blocks")
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
    }
}
