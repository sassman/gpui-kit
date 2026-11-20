//! JetBrains-style three-pane conflict resolution: your changes left, the
//! derived result middle, upstream changes right. Lives beside [`crate::diff`]
//! and shares its visual idiom; the diff surface itself stays read-only.
//! Once a block is resolved its middle region becomes an embedded editor,
//! so the reviewer can adjust the picked text by hand; context lines stay
//! strictly read-only.
mod model;
mod state;

pub use model::BlockResolution;
pub use state::{ConflictResolvEvent, ConflictResolvState};

use model::{ConflictCell, ConflictRow, ConflictRowKind};
use state::DisplayRow;

use gpui::{
    AnyElement, App, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled,
    Window, div, prelude::FluentBuilder as _, px,
};
use rust_i18n::t;

use crate::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::TextareaState,
    scroll::ScrollableElement as _,
    v_flex,
};

/// A themed three-pane resolver surface driven by [`ConflictResolvState`].
///
/// All rows are laid out at full height: the consumer scrolls the whole
/// surface, while each pane scrolls long lines horizontally on its own.
/// Each pane carries its own 1-based line numbers. Pane labels come from the
/// application, which knows the operation's refs, for example "Your changes
/// · feature". Per-block pick arrows sit in the middle pane's edge gutters,
/// whose outer borders divide the panes, while take-both floats centered
/// over the result; block backgrounds tint by resolution state.
///
/// A resolved block's middle region is an embedded editor: the reviewer
/// picks a side, then adjusts the result by hand, Enter adding lines inside
/// the block. The first divergence tints the block as hand-edited. Context
/// lines and unresolved blocks stay read-only.
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
    rem: Pixels,
    row_height: Pixels,
    header_height: Pixels,
    font_size: Pixels,
    number_size: Pixels,
    font_family: SharedString,
    border: Hsla,
    header_bg: Hsla,
    header_fg: Hsla,
    number_fg: Hsla,
}

impl PaneStyle {
    /// The gutter width that fits the pane's largest line number, sized
    /// like the Diff component's gutter.
    fn number_width(&self, lines: usize) -> Pixels {
        let digits = lines.max(1).to_string().len();
        self.number_size * 0.7 * digits as f32 + self.rem
    }
}

#[allow(clippy::too_many_arguments)]
fn render_pane(
    id: &'static str,
    label: SharedString,
    display: &[DisplayRow],
    rows: &[ConflictRow],
    cell: impl Fn(&ConflictRow) -> ConflictCell,
    lines: &[SharedString],
    backgrounds: &[Option<Hsla>],
    // Per block, the editor that replaces the block's line cells in this
    // pane with the row span it covers; only the middle pane passes any.
    editors: &[Option<(Entity<TextareaState>, usize)>],
    overlays: Vec<(usize, AnyElement)>,
    state: &Entity<ConflictResolvState>,
    style: &PaneStyle,
) -> AnyElement {
    let mut numbers = v_flex().w(style.number_width(lines.len())).flex_none();
    let mut body = v_flex().min_w_full();
    // Body rows an editor already spans; their numbers still render.
    let mut covered = 0_usize;
    let mut editor_seen = None;
    for (row_ix, entry) in display.iter().enumerate() {
        let row = match entry {
            DisplayRow::Row(rix) => &rows[*rix],
            // The disclosure renders in every pane, so the fold reads as
            // one band across the surface; any pane's click opens it.
            DisplayRow::Fold { ix, hidden } => {
                numbers = numbers.child(div().h(style.row_height));
                let fold = *ix;
                let state = state.clone();
                body = body.child(
                    h_flex()
                        .id((id, fold))
                        .h(style.row_height)
                        .px_2()
                        .justify_center()
                        .whitespace_nowrap()
                        .line_height(style.row_height)
                        .text_size(style.number_size)
                        .text_color(style.number_fg)
                        .bg(style.header_bg)
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            state.update(cx, |state, cx| state.reveal_fold(fold, cx))
                        })
                        .child(
                            t!("ConflictResolv.UnchangedLines", count = hidden.len()).to_string(),
                        ),
                );
                continue;
            }
        };
        let (number, content) = match cell(row) {
            ConflictCell::Line(ix) => (Some((ix + 1).to_string()), Some(lines[ix].clone())),
            // A spacer pads block alignment, so it carries no number.
            ConflictCell::Spacer => (None, None),
        };
        numbers = numbers.child(
            h_flex()
                .h(style.row_height)
                .px_1()
                .justify_end()
                .text_size(style.number_size)
                .text_color(style.number_fg)
                .when_some(backgrounds[row_ix], |this, bg| this.bg(bg))
                .when_some(number, |this, number| this.child(number)),
        );
        if covered > 0 {
            covered -= 1;
            continue;
        }
        if let ConflictRowKind::Block(ix) = row.kind
            && let Some(Some((editor, span))) = editors.get(ix)
            && editor_seen != Some(ix)
        {
            editor_seen = Some(ix);
            // The block's line cells fill its first rows; the editor spans
            // exactly those, precomputed from the block's line count so no
            // row rescan happens per editor.
            let span = *span;
            covered = span - 1;
            body = body.child(
                div()
                    .h(style.row_height * span as f32)
                    .px_2()
                    .line_height(style.row_height)
                    .when_some(backgrounds[row_ix], |this, bg| this.bg(bg))
                    .child(editor.clone()),
            );
            continue;
        }
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
        .relative()
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
            h_flex()
                .w_full()
                .items_start()
                .font_family(style.font_family.clone())
                .text_size(style.font_size)
                .child(numbers)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_x_scrollbar()
                        .id(id)
                        .child(body),
                ),
        )
        // Anchored to the pane, not the scroll content, so a control stays
        // centered in view and never shifts the row's text.
        .children(overlays.into_iter().map(|(row_ix, control)| {
            div()
                .absolute()
                .top(style.header_height + style.row_height * row_ix as f32)
                .left_0()
                .right_0()
                .h(style.row_height)
                .flex()
                .items_center()
                .justify_center()
                .child(control)
        }))
        .into_any_element()
}

/// The controls on a block's anchor row, both gutters on the middle pane's
/// edges: while unresolved the left one picks from yours, the right one
/// picks from upstream; once resolved the right one undoes. Take-both
/// floats over the middle pane instead, see [`take_both_button`].
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
        (true, true) => vec![button(
            "take-yours",
            IconName::ArrowRight,
            t!("ConflictResolv.TakeYours").to_string(),
            |state, block, cx| state.take_yours(block, cx),
        )],
        (true, false) => Vec::new(),
        (false, true) => vec![button(
            "take-upstream",
            IconName::ArrowLeft,
            t!("ConflictResolv.TakeUpstream").to_string(),
            |state, block, cx| state.take_upstream(block, cx),
        )],
        (false, false) => vec![button(
            "undo-block",
            IconName::Undo,
            t!("ConflictResolv.UndoBlock").to_string(),
            |state, block, cx| state.undo_block(block, cx),
        )],
    }
}

/// Take-both writes both sides into the result, so it belongs to neither
/// gutter: it floats horizontally centered over the middle pane's anchor
/// row. Two flows joining into one reads as "both sides"; the tooltip
/// carries the order, yours above upstream.
fn take_both_button(block: usize, state: &Entity<ConflictResolvState>) -> AnyElement {
    let state = state.clone();
    Button::new(("take-both", block))
        .ghost()
        .xsmall()
        // Inward arrows onto the center line: both sides flow into the
        // result, extending the gutter arrows' vocabulary.
        .icon(IconName::FoldHorizontal)
        .tooltip(t!("ConflictResolv.TakeBoth").to_string())
        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.take_both(block, cx)))
        .into_any_element()
}

/// The row a block's controls anchor to, gutter arrows and the floating
/// take-both alike: its first row. Whether they should instead center
/// vertically within the block is undecided, so the choice stays confined
/// to this function.
fn control_anchor_rows(display: &[DisplayRow], rows: &[ConflictRow]) -> Vec<Option<usize>> {
    let mut seen = None;
    display
        .iter()
        .map(|entry| match entry {
            DisplayRow::Row(rix) => match rows[*rix].kind {
                ConflictRowKind::Block(ix) if seen != Some(ix) => {
                    seen = Some(ix);
                    Some(ix)
                }
                _ => None,
            },
            DisplayRow::Fold { .. } => None,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_gutter(
    left: bool,
    width: Pixels,
    display: &[DisplayRow],
    anchor_rows: &[Option<usize>],
    resolutions: &[BlockResolution],
    backgrounds: &[Option<Hsla>],
    state: &Entity<ConflictResolvState>,
    style: &PaneStyle,
) -> AnyElement {
    let mut gutter = v_flex()
        .w(width)
        .flex_none()
        // The gutter's outer edge is the pane divider, full height, so the
        // boundary and its controls read as one.
        .when(left, |this| this.border_l_1())
        .when(!left, |this| this.border_r_1())
        .border_color(style.border)
        .child(
            div()
                .h(style.header_height)
                .bg(style.header_bg)
                .border_b_1()
                .border_color(style.border),
        );
    for (row_ix, entry) in display.iter().enumerate() {
        // A fold row keeps the band's quiet background through the gutter.
        let folded = matches!(entry, DisplayRow::Fold { .. });
        let controls = anchor_rows[row_ix].map(|block| (block, resolutions[block]));
        gutter = gutter.child(
            h_flex()
                .h(style.row_height)
                .justify_center()
                .when(folded, |this| this.bg(style.header_bg))
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
        // Mount or drop the per-block result editors before reading: render
        // is the only place a window exists to create input states in. The
        // folded view re-derives here too, on the same revision check.
        self.state.update(cx, |state, cx| {
            state.sync_editors(window, cx);
            state.sync_folds();
        });
        let rem = window.rem_size();
        let font_size = rem * (f32::from(cx.theme().mono_font_size) / 16.);
        let style = PaneStyle {
            rem,
            row_height: font_size * 1.6,
            header_height: px(26.),
            font_size,
            // Quieter than the code, as in the Diff gutter.
            number_size: font_size * 0.8,
            font_family: cx.theme().mono_font_family.clone(),
            border: cx.theme().border,
            header_bg: cx.theme().secondary,
            header_fg: cx.theme().muted_foreground,
            number_fg: cx.theme().muted_foreground,
        };
        let entity = self.state.clone();
        // Borrowed for the whole pass: the lines and the row table feed
        // the panes as slices, nothing is copied per frame.
        let state = self.state.read(cx);
        let rows = state.rows();
        let display = state.display_rows();
        let yours = state.yours_lines();
        let middle = state.middle_lines();
        let upstream = state.upstream_lines();
        let resolutions: Vec<BlockResolution> = (0..state.block_count())
            .map(|ix| state.resolution(ix))
            .collect();
        // An empty block's editor still spans the one row that keeps the
        // gutter controls visible.
        let editors: Vec<Option<(Entity<TextareaState>, usize)>> = (0..state.block_count())
            .map(|ix| {
                state
                    .editor(ix)
                    .cloned()
                    .map(|editor| (editor, state.middle_line_count(ix).max(1)))
            })
            .collect();
        let backgrounds: Vec<Option<Hsla>> = display
            .iter()
            .map(|entry| match entry {
                DisplayRow::Row(rix) => match rows[*rix].kind {
                    ConflictRowKind::Context => None,
                    ConflictRowKind::Block(ix) => Some(self.tint(resolutions[ix], cx)),
                },
                DisplayRow::Fold { .. } => None,
            })
            .collect();
        let anchor_rows = control_anchor_rows(display, rows);
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
        let take_both_overlays: Vec<(usize, AnyElement)> = anchor_rows
            .iter()
            .enumerate()
            .filter_map(|(row_ix, block)| {
                block
                    .filter(|&block| resolutions[block] == BlockResolution::Unresolved)
                    .map(|block| (row_ix, take_both_button(block, &entity)))
            })
            .collect();
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
                display,
                rows,
                |row| row.left,
                yours,
                &backgrounds,
                &[],
                Vec::new(),
                &entity,
                &style,
            ))
            .child(render_gutter(
                true,
                px(24.),
                display,
                &anchor_rows,
                &resolutions,
                &backgrounds,
                &entity,
                &style,
            ))
            .child(render_pane(
                "conflict-result",
                result_label,
                display,
                rows,
                |row| row.middle,
                middle,
                &backgrounds,
                &editors,
                take_both_overlays,
                &entity,
                &style,
            ))
            .child(render_gutter(
                false,
                px(24.),
                display,
                &anchor_rows,
                &resolutions,
                &backgrounds,
                &entity,
                &style,
            ))
            .child(render_pane(
                "conflict-upstream",
                upstream_label,
                display,
                rows,
                |row| row.right,
                upstream,
                &backgrounds,
                &[],
                Vec::new(),
                &entity,
                &style,
            ))
            .refine_style(&self.style)
    }
}
