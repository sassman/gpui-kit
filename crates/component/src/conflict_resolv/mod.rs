//! JetBrains-style three-pane conflict resolution: your changes left, the
//! derived result middle, upstream changes right. Lives beside [`crate::diff`]
//! and shares its visual idiom; the diff surface itself stays read-only.
//! The middle pane is read-only derived text in this iteration; a later one
//! embeds an editor over the same single result buffer.
mod model;
mod state;

pub use model::BlockResolution;
pub use state::{ConflictResolvEvent, ConflictResolvState};

use model::{ConflictCell, ConflictRow, ConflictRowKind};

use gpui::{
    AnyElement, App, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, RenderOnce, SharedString, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use rust_i18n::t;

use crate::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};

/// A themed three-pane resolver surface driven by [`ConflictResolvState`].
///
/// All rows are laid out at full height: the consumer scrolls the whole
/// surface, while each pane scrolls long lines horizontally on its own.
/// Pane labels come from the application, which knows the operation's refs,
/// for example "Your changes · feature". Per-block controls sit in the
/// gutters between the panes; block backgrounds tint by resolution state.
#[derive(IntoElement)]
pub struct ConflictResolv {
    state: Entity<ConflictResolvState>,
    style: StyleRefinement,
    yours_label: Option<SharedString>,
    result_label: Option<SharedString>,
    upstream_label: Option<SharedString>,
    tints: [Option<Hsla>; 5],
}

/// One override slot per [`BlockResolution`] variant.
fn tint_slot(resolution: BlockResolution) -> usize {
    match resolution {
        BlockResolution::Unresolved => 0,
        BlockResolution::TakenYours => 1,
        BlockResolution::TakenUpstream => 2,
        BlockResolution::Both => 3,
        BlockResolution::HandEdited => 4,
    }
}

impl ConflictResolv {
    pub fn new(state: &Entity<ConflictResolvState>) -> Self {
        Self {
            state: state.clone(),
            style: StyleRefinement::default(),
            yours_label: None,
            result_label: None,
            upstream_label: None,
            tints: [None; 5],
        }
    }
    /// Labels the left pane; default is a localized "Your changes".
    pub fn yours_label(mut self, label: impl Into<SharedString>) -> Self {
        self.yours_label = Some(label.into());
        self
    }
    /// Labels the middle pane; default is a localized "Result".
    pub fn result_label(mut self, label: impl Into<SharedString>) -> Self {
        self.result_label = Some(label.into());
        self
    }
    /// Labels the right pane; default is a localized "Upstream changes".
    pub fn upstream_label(mut self, label: impl Into<SharedString>) -> Self {
        self.upstream_label = Some(label.into());
        self
    }
    /// Overrides the tint behind blocks in `resolution` state. The color
    /// renders verbatim, so include any transparency in it; only the
    /// theme-following defaults carry their own wash, with unresolved
    /// clearly set apart.
    pub fn block_tint(mut self, resolution: BlockResolution, color: impl Into<Hsla>) -> Self {
        self.tints[tint_slot(resolution)] = Some(color.into());
        self
    }

    fn tint(&self, resolution: BlockResolution, cx: &App) -> Hsla {
        if let Some(color) = self.tints[tint_slot(resolution)] {
            return color;
        }
        // Unresolved stands out; a resolved tint reads as a quiet wash.
        match resolution {
            BlockResolution::Unresolved => cx.theme().warning.opacity(0.2),
            BlockResolution::TakenYours => cx.theme().success.opacity(0.12),
            BlockResolution::TakenUpstream => cx.theme().info.opacity(0.12),
            BlockResolution::Both => cx.theme().primary.opacity(0.12),
            BlockResolution::HandEdited => cx.theme().muted_foreground.opacity(0.12),
        }
    }
}

impl Styled for ConflictResolv {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// Shared metrics and colors for the panes and gutters of one render pass.
struct PaneStyle {
    row_height: Pixels,
    header_height: Pixels,
    font_size: Pixels,
    font_family: SharedString,
    border: Hsla,
    header_bg: Hsla,
    header_fg: Hsla,
}

fn render_pane(
    id: &'static str,
    label: SharedString,
    rows: &[ConflictRow],
    cell: impl Fn(&ConflictRow) -> ConflictCell,
    lines: &[SharedString],
    backgrounds: &[Option<Hsla>],
    style: &PaneStyle,
) -> AnyElement {
    let mut body = v_flex().min_w_full();
    for (row_ix, row) in rows.iter().enumerate() {
        let content = match cell(row) {
            ConflictCell::Line(ix) => Some(lines[ix].clone()),
            ConflictCell::Spacer => None,
        };
        body = body.child(
            h_flex()
                .h(style.row_height)
                .px_2()
                .whitespace_nowrap()
                .line_height(style.row_height)
                .when_some(backgrounds[row_ix], |this, bg| this.bg(bg))
                .when_some(content, |this, line| this.child(line)),
        );
    }
    v_flex()
        .flex_1()
        .min_w_0()
        .child(
            h_flex()
                .h(style.header_height)
                .px_2()
                .text_xs()
                .text_color(style.header_fg)
                .bg(style.header_bg)
                .border_b_1()
                .border_color(style.border)
                .child(label),
        )
        .child(
            div()
                .w_full()
                .font_family(style.font_family.clone())
                .text_size(style.font_size)
                .overflow_x_scrollbar()
                .id(id)
                .child(body),
        )
        .into_any_element()
}

/// The controls on a block's first row: picks while unresolved, undo after.
fn gutter_controls(
    left: bool,
    block: usize,
    resolution: BlockResolution,
    state: &Entity<ConflictResolvState>,
) -> Vec<AnyElement> {
    let button = |id: &'static str,
                  icon: IconName,
                  label: String,
                  apply: fn(
        &mut ConflictResolvState,
        usize,
        &mut gpui::Context<ConflictResolvState>,
    )| {
        let state = state.clone();
        Button::new((id, block))
            .ghost()
            .xsmall()
            .icon(icon)
            .tooltip(label)
            .on_click(move |_, _, cx| state.update(cx, |state, cx| apply(state, block, cx)))
            .into_any_element()
    };
    match (left, resolution == BlockResolution::Unresolved) {
        (true, true) => vec![
            button(
                "take-yours",
                IconName::ArrowRight,
                t!("ConflictResolv.TakeYours").to_string(),
                |state, block, cx| state.take_yours(block, cx),
            ),
            button(
                "take-both",
                IconName::ChevronsRightLeft,
                t!("ConflictResolv.TakeBoth").to_string(),
                |state, block, cx| state.take_both(block, cx),
            ),
        ],
        (true, false) => vec![button(
            "undo-block",
            IconName::Undo,
            t!("ConflictResolv.UndoBlock").to_string(),
            |state, block, cx| state.undo_block(block, cx),
        )],
        (false, true) => vec![button(
            "take-upstream",
            IconName::ArrowLeft,
            t!("ConflictResolv.TakeUpstream").to_string(),
            |state, block, cx| state.take_upstream(block, cx),
        )],
        (false, false) => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_gutter(
    left: bool,
    width: Pixels,
    rows: &[ConflictRow],
    first_block_rows: &[Option<usize>],
    resolutions: &[BlockResolution],
    backgrounds: &[Option<Hsla>],
    state: &Entity<ConflictResolvState>,
    style: &PaneStyle,
) -> AnyElement {
    let mut gutter = v_flex().w(width).flex_none().child(
        div()
            .h(style.header_height)
            .bg(style.header_bg)
            .border_b_1()
            .border_color(style.border),
    );
    for (row_ix, _) in rows.iter().enumerate() {
        let controls = first_block_rows[row_ix].map(|block| (block, resolutions[block]));
        gutter = gutter.child(
            h_flex()
                .h(style.row_height)
                .justify_center()
                .when_some(backgrounds[row_ix], |this, bg| this.bg(bg))
                .when_some(controls, |this, (block, resolution)| {
                    this.children(gutter_controls(left, block, resolution, state))
                }),
        );
    }
    gutter.into_any_element()
}

impl RenderOnce for ConflictResolv {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let rem = window.rem_size();
        let font_size = rem * (f32::from(cx.theme().mono_font_size) / 16.);
        let style = PaneStyle {
            row_height: font_size * 1.6,
            header_height: px(26.),
            font_size,
            font_family: cx.theme().mono_font_family.clone(),
            border: cx.theme().border,
            header_bg: cx.theme().secondary,
            header_fg: cx.theme().muted_foreground,
        };
        let state = self.state.read(cx);
        let rows = state.rows().to_vec();
        let yours: Vec<SharedString> = state.yours_lines().to_vec();
        let middle: Vec<SharedString> = state.middle_lines().to_vec();
        let upstream: Vec<SharedString> = state.upstream_lines().to_vec();
        let resolutions: Vec<BlockResolution> = (0..state.block_count())
            .map(|ix| state.resolution(ix))
            .collect();
        let backgrounds: Vec<Option<Hsla>> = rows
            .iter()
            .map(|row| match row.kind {
                ConflictRowKind::Context => None,
                ConflictRowKind::Block(ix) => Some(self.tint(resolutions[ix], cx)),
            })
            .collect();
        let mut seen = None;
        let first_block_rows: Vec<Option<usize>> = rows
            .iter()
            .map(|row| match row.kind {
                ConflictRowKind::Block(ix) if seen != Some(ix) => {
                    seen = Some(ix);
                    Some(ix)
                }
                _ => None,
            })
            .collect();
        let yours_label = self
            .yours_label
            .clone()
            .unwrap_or_else(|| t!("ConflictResolv.YourChanges").to_string().into());
        let result_label = self
            .result_label
            .clone()
            .unwrap_or_else(|| t!("ConflictResolv.Result").to_string().into());
        let upstream_label = self
            .upstream_label
            .clone()
            .unwrap_or_else(|| t!("ConflictResolv.UpstreamChanges").to_string().into());
        let entity = self.state.clone();
        h_flex()
            .id(("conflict-resolv", entity.entity_id()))
            .w_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .border_1()
            .border_color(cx.theme().border)
            .child(render_pane(
                "conflict-yours",
                yours_label,
                &rows,
                |row| row.left,
                &yours,
                &backgrounds,
                &style,
            ))
            .child(render_gutter(
                true,
                px(46.),
                &rows,
                &first_block_rows,
                &resolutions,
                &backgrounds,
                &entity,
                &style,
            ))
            .child(render_pane(
                "conflict-result",
                result_label,
                &rows,
                |row| row.middle,
                &middle,
                &backgrounds,
                &style,
            ))
            .child(render_gutter(
                false,
                px(24.),
                &rows,
                &first_block_rows,
                &resolutions,
                &backgrounds,
                &entity,
                &style,
            ))
            .child(render_pane(
                "conflict-upstream",
                upstream_label,
                &rows,
                |row| row.right,
                &upstream,
                &backgrounds,
                &style,
            ))
            .refine_style(&self.style)
    }
}
