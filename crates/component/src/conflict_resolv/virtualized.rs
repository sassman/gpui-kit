//! Virtualized rival of [`super::ConflictResolv`], built on the Diff
//! component's techniques: rows materialize through a [`ListState`] so only
//! the visible slice renders, syntax highlighting is prepared on the
//! background executor and cached behind a generation guard, and each pane
//! scrolls long lines horizontally through its own shared [`ScrollHandle`].
//! It drives the same [`ConflictResolvState`], so picks, events and the
//! embedded result editors behave identically; only the render layer differs.
use std::{cell::RefCell, collections::HashMap, ops::Range, rc::Rc, sync::Arc};

use gpui::{
    AnyElement, App, AppContext as _, Axis, Entity, EntityId, Font, Global, HighlightStyle, Hsla,
    InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState, ParentElement as _,
    Pixels, RenderOnce, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled, StyledText, Subscription, Task, TextRun, Window, div, list,
    prelude::FluentBuilder as _, px, relative,
};
use gpui_base::input::HighlightStyleResolver;
use rust_i18n::t;

use super::{
    BlockResolution, ConflictResolvState, default_tint,
    model::{ConflictCell, ConflictRow, ConflictRowKind},
    state::DisplayRow,
    tint_slot,
};
use crate::{
    ActiveTheme as _, Rope, StyledExt as _, h_flex,
    highlighter::{HighlightTheme, SyntaxHighlighter},
    scroll::{ScrollableMask, Scrollbar},
    v_flex,
};

/// The width of each pick-control gutter on the middle pane's edges.
const GUTTER_WIDTH: f32 = 24.;
/// Lines longer than this render without syntax spans; shaping and painting
/// pathological lines costs more than the emphasis is worth (the Diff's cap).
const MAX_SYNTAX_LINE: usize = 1000;

/// The three line columns, indexed as `[yours, result, upstream]`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Yours = 0,
    Result = 1,
    Upstream = 2,
}

/// A virtualized three-pane resolver surface driven by [`ConflictResolvState`].
///
/// The builder surface matches [`super::ConflictResolv`], so swapping the two
/// is a type change at the call site. Unlike the full-height original, this
/// component owns its vertical scrolling: give it a bounded height.
#[derive(IntoElement)]
pub struct ConflictResolvVirtual {
    state: Entity<ConflictResolvState>,
    style: StyleRefinement,
    yours_label: Option<SharedString>,
    result_label: Option<SharedString>,
    upstream_label: Option<SharedString>,
    tints: [Option<Hsla>; 5],
}

impl ConflictResolvVirtual {
    /// The pane-label band's height. With it, a host can size the view to
    /// its content: this plus [`ConflictResolvState::display_len`] rows of
    /// the mono font size times 1.6, the row height used here.
    pub const HEADER_HEIGHT: Pixels = px(26.);

    /// The uniform row height render derives from the window's mono font;
    /// published so a host sizing to content cannot drift from it.
    pub fn row_height(window: &Window, cx: &App) -> Pixels {
        let rem = window.rem_size();
        rem * (f32::from(cx.theme().mono_font_size) / 16.) * 1.6
    }

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
    /// Overrides the tint behind blocks in `resolution` state, as
    /// [`super::ConflictResolv::block_tint`] does.
    pub fn block_tint(mut self, resolution: BlockResolution, color: impl Into<Hsla>) -> Self {
        self.tints[tint_slot(resolution)] = Some(color.into());
        self
    }
}

impl Styled for ConflictResolvVirtual {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// One list item: the virtualization granularity. A resolved block's embedded
/// editor must be one element, so its rows collapse into one taller item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RowItem {
    /// One visual row, by display-row index.
    Row(usize),
    /// A fold band standing in for `hidden` unchanged rows; `ix` keys the
    /// reveal.
    Fold { ix: usize, hidden: usize },
    /// A resolved block's embedded editor with the display rows it covers;
    /// the side panes still show those rows' lines beside it.
    Editor { block: usize, rows: Range<usize> },
}

/// Maps the folded display rows onto list items. `spans[block]` is the number
/// of rows the block's mounted editor covers, `None` while the block has no
/// editor; the covered rows fold into one [`RowItem::Editor`].
pub(super) fn derive_items(
    display: &[DisplayRow],
    rows: &[ConflictRow],
    spans: &[Option<usize>],
) -> Vec<RowItem> {
    let mut items = Vec::with_capacity(display.len());
    let mut editor_seen = None;
    let mut d = 0;
    while d < display.len() {
        match &display[d] {
            DisplayRow::Fold { ix, hidden } => {
                items.push(RowItem::Fold {
                    ix: *ix,
                    hidden: hidden.len(),
                });
                d += 1;
            }
            DisplayRow::Row(rix) => {
                let kind = rows[*rix].kind;
                if let ConflictRowKind::Block(block) = kind
                    && editor_seen != Some(block)
                    && let Some(Some(span)) = spans.get(block)
                {
                    editor_seen = Some(block);
                    // Clamp to the rows the block occupies, so a span can
                    // never swallow a neighbouring block's rows.
                    let available = display[d..]
                        .iter()
                        .take_while(
                            |entry| matches!(entry, DisplayRow::Row(r) if rows[*r].kind == kind),
                        )
                        .count();
                    let span = (*span).clamp(1, available.max(1));
                    items.push(RowItem::Editor {
                        block,
                        rows: d..d + span,
                    });
                    d += span;
                } else {
                    items.push(RowItem::Row(d));
                    d += 1;
                }
            }
        }
    }
    items
}

/// The narrowest replacement that turns `old` into `new`, for
/// [`ListState::splice`]; `None` when nothing changed. Replacing only the
/// middle keeps the list's scroll anchor and row measurements around it.
pub(super) fn splice_bounds(old: &[RowItem], new: &[RowItem]) -> Option<(Range<usize>, usize)> {
    if old == new {
        return None;
    }
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(old.len().min(new.len()) - prefix)
        .take_while(|(a, b)| a == b)
        .count();
    Some((prefix..old.len() - suffix, new.len() - prefix - suffix))
}

/// Retained render state for one [`ConflictResolvState`] entity. It cannot
/// live in the state itself: list and scroll handles are not `Send`, and the
/// state is parsed on background threads, so the component keeps its render
/// side in an app global keyed by the entity.
struct Retained {
    list: ListState,
    scroll: [ScrollHandle; 3],
    items: Rc<Vec<RowItem>>,
    row_height: Option<Pixels>,
    /// The list width the row-height hint was last applied at; a list that
    /// lays out at a new width forgets every height, hints included.
    hinted_width: Option<Pixels>,
    /// Each pane's widest shaped line, clamping its horizontal scroll.
    widths: [Pixels; 3],
    widths_key: Option<(u64, Pixels, SharedString)>,
    presentation: Option<Arc<ConflictPresentation>>,
    /// Where the list stood before the wheel turn being dispatched; the
    /// root's wheel listener runs after the list's own, so comparing
    /// against this tells whether the list consumed the turn.
    last_wheel_top: ListOffset,
    /// The row-table revision `presentation` was prepared from.
    presented: Option<u64>,
    preparing: Option<u64>,
    task: Task<()>,
    _release: Subscription,
}

#[derive(Default)]
struct VirtualRegistry(HashMap<EntityId, Rc<RefCell<Retained>>>);
impl Global for VirtualRegistry {}

fn retained_entry(state: &Entity<ConflictResolvState>, cx: &mut App) -> Rc<RefCell<Retained>> {
    let id = state.entity_id();
    if let Some(found) = cx.default_global::<VirtualRegistry>().0.get(&id) {
        return found.clone();
    }
    // Dropping the state entity drops its render side with it.
    let release = cx.observe_release(state, move |_, cx| {
        cx.default_global::<VirtualRegistry>().0.remove(&id);
    });
    let entry = Rc::new(RefCell::new(Retained {
        list: ListState::new(0, ListAlignment::Top, px(0.)),
        scroll: [
            ScrollHandle::new(),
            ScrollHandle::new(),
            ScrollHandle::new(),
        ],
        items: Rc::default(),
        row_height: None,
        hinted_width: None,
        widths: [px(0.); 3],
        widths_key: None,
        presentation: None,
        last_wheel_top: ListOffset::default(),
        presented: None,
        preparing: None,
        task: Task::ready(()),
        _release: release,
    }));
    cx.default_global::<VirtualRegistry>()
        .0
        .insert(id, entry.clone());
    entry
}

/// Everything an item render needs beyond the state entity, captured once per
/// render pass; lines and rows are read from the entity when the item paints.
struct ItemContext {
    state: Entity<ConflictResolvState>,
    items: Rc<Vec<RowItem>>,
    scroll: [ScrollHandle; 3],
    widths: [Pixels; 3],
    number_widths: [Pixels; 3],
    presentation: Option<Arc<ConflictPresentation>>,
    highlight_theme: Arc<HighlightTheme>,
    /// Indexed by [`tint_slot`].
    tints: [Hsla; 5],
    rem: Pixels,
    row_height: Pixels,
    font_size: Pixels,
    number_size: Pixels,
    font_family: SharedString,
    border: Hsla,
    band_bg: Hsla,
    band_fg: Hsla,
    number_fg: Hsla,
}

impl ItemContext {
    fn pane_scroll(&self, pane: Pane) -> &ScrollHandle {
        &self.scroll[pane as usize]
    }
    fn pane_width(&self, pane: Pane) -> Pixels {
        self.widths[pane as usize]
    }
    fn number_width(&self, pane: Pane) -> Pixels {
        self.number_widths[pane as usize]
    }
}

impl RenderOnce for ConflictResolvVirtual {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let entity = self.state.clone();
        let retained = retained_entry(&entity, cx);
        // Mount or drop the per-block result editors before reading: render
        // is the only place a window exists to create input states in. The
        // folded view re-derives here too, on the same revision check.
        entity.update(cx, |state, cx| {
            state.sync_editors(window, cx);
            state.sync_folds();
        });
        let rem = window.rem_size();
        let font_size = rem * (f32::from(cx.theme().mono_font_size) / 16.);
        let row_height = Self::row_height(window, cx);
        let number_size = font_size * 0.8;
        let font_family = cx.theme().mono_font_family.clone();
        let number_widths;
        {
            let state = entity.read(cx);
            let spans: Vec<Option<usize>> = (0..state.block_count())
                .map(|block| {
                    state
                        .editor(block)
                        .map(|_| state.middle_line_count(block).max(1))
                })
                .collect();
            let items = derive_items(state.display_rows(), state.rows(), &spans);
            let number_width = |lines: usize| {
                let digits = lines.max(1).to_string().len();
                number_size * 0.7 * digits as f32 + rem
            };
            number_widths = [
                number_width(state.yours_lines().len()),
                number_width(state.middle_lines().len()),
                number_width(state.upstream_lines().len()),
            ];
            let mut retained = retained.borrow_mut();
            if retained.row_height != Some(row_height) {
                retained.row_height = Some(row_height);
                retained.apply_row_height_hint();
            }
            if *retained.items != items {
                if let Some((range, count)) = splice_bounds(&retained.items, &items) {
                    retained.list.splice(range, count);
                }
                retained.items = Rc::new(items);
                retained.apply_row_height_hint();
            }
            let widths_key = (state.rows_revision(), font_size, font_family.clone());
            if retained.widths_key.as_ref() != Some(&widths_key) {
                retained.widths = measure_column_widths(
                    [
                        state.yours_lines(),
                        state.middle_lines(),
                        state.upstream_lines(),
                    ],
                    font_size,
                    &font_family,
                    window,
                    cx,
                );
                retained.widths_key = Some(widths_key);
            }
        }
        ensure_presentation(&entity, &retained, cx);
        let tints = [
            BlockResolution::Unresolved,
            BlockResolution::TakenYours,
            BlockResolution::TakenUpstream,
            BlockResolution::Both,
            BlockResolution::HandEdited,
        ]
        .map(|resolution| {
            self.tints[tint_slot(resolution)].unwrap_or_else(|| default_tint(resolution, cx))
        });
        let (list_state, item_context) = {
            let retained = retained.borrow();
            (
                retained.list.clone(),
                Rc::new(ItemContext {
                    state: entity.clone(),
                    items: retained.items.clone(),
                    scroll: retained.scroll.clone(),
                    widths: retained.widths,
                    number_widths,
                    presentation: retained.presentation.clone(),
                    highlight_theme: cx.theme().highlight_theme.clone(),
                    tints,
                    rem,
                    row_height,
                    font_size,
                    number_size,
                    font_family,
                    border: cx.theme().border,
                    band_bg: cx.theme().secondary,
                    band_fg: cx.theme().muted_foreground,
                    number_fg: cx.theme().muted_foreground,
                }),
            )
        };
        // Re-anchor the wheel comparison each frame; the list notifies on
        // every consumed turn, so a fresh render always lands in between.
        retained.borrow_mut().last_wheel_top = list_state.logical_scroll_top();
        let has_rows = !item_context.items.is_empty();
        let header = pane_headers(
            self.yours_label
                .unwrap_or_else(|| t!("ConflictResolv.YourChanges").to_string().into()),
            self.result_label
                .unwrap_or_else(|| t!("ConflictResolv.Result").to_string().into()),
            self.upstream_label
                .unwrap_or_else(|| t!("ConflictResolv.UpstreamChanges").to_string().into()),
            &item_context,
        );
        let items = item_context.clone();
        let scroll = item_context.scroll.clone();
        // A pane-third overlay per handle, so a wheel's horizontal delta goes
        // to the pane under the pointer, as the Diff's split panels route it.
        let third = |slot: usize| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .w(relative(1. / 3.))
                .map(|this| match slot {
                    0 => this.left_0(),
                    1 => this.left(relative(1. / 3.)),
                    _ => this.right_0(),
                })
        };
        let wheel_ids = ["crv-wheel-yours", "crv-wheel-result", "crv-wheel-upstream"];
        let bar_ids = ["crv-bar-yours", "crv-bar-result", "crv-bar-upstream"];
        let body = div()
            .id("crv-body")
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                list(list_state.clone(), move |ix, _, cx| {
                    render_item(ix, &items, cx)
                })
                .w_full()
                .h_full(),
            )
            .when(has_rows, |this| {
                let mut this = this;
                for (slot, id) in wheel_ids.into_iter().enumerate() {
                    this = this.child(
                        third(slot)
                            .child(ScrollableMask::new(Axis::Horizontal, &scroll[slot]).id(id)),
                    );
                }
                this = this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .child(Scrollbar::vertical(&list_state).viewport_from_layout()),
                );
                for (slot, id) in bar_ids.into_iter().enumerate() {
                    this = this.child(
                        third(slot).child(
                            Scrollbar::horizontal(&scroll[slot])
                                .id(id)
                                .viewport_from_layout(),
                        ),
                    );
                }
                this
            })
            .child(
                // Runs after the list's prepaint, which drops every row
                // height when its width changes; the hint must come back
                // before the next wheel event or scrolling stops early.
                gpui::canvas(
                    {
                        let retained = retained.clone();
                        let entity = entity.clone();
                        move |_, _, cx| {
                            let mut retained = retained.borrow_mut();
                            let width = retained.list.viewport_bounds().size.width;
                            if retained.row_height.is_some() && retained.hinted_width != Some(width)
                            {
                                retained.hinted_width = Some(width);
                                retained.apply_row_height_hint();
                                drop(retained);
                                entity.update(cx, |_, cx| cx.notify());
                            }
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        v_flex()
            .id(("conflict-resolv-virtual", entity.entity_id()))
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .border_1()
            .border_color(cx.theme().border)
            // A host may stack this view inside its own scroll column. The
            // list's listener has already run by bubble time here: when it
            // consumed the turn the host must not scroll too, and only a
            // list sitting at its edge (or fitting its frame) lets the
            // turn through so the host can carry on past this file.
            .on_scroll({
                let retained = retained.clone();
                move |event, window, cx| {
                    if event.delta.pixel_delta(window.line_height()).y == px(0.) {
                        return;
                    }
                    let mut retained = retained.borrow_mut();
                    let top = retained.list.logical_scroll_top();
                    if std::mem::replace(&mut retained.last_wheel_top, top) != top {
                        cx.stop_propagation();
                    }
                }
            })
            .child(header)
            .child(body)
            .refine_style(&self.style)
    }
}

impl Retained {
    /// Gives unmeasured rows a code-row height so the scrollbar is sized for
    /// the whole file before every row has been rendered.
    fn apply_row_height_hint(&self) {
        if let Some(height) = self.row_height {
            let _ = self.list.clone().with_uniform_item_height(height);
        }
    }
}

/// Prepares highlighting for the current row table once, on the background
/// executor; a result from an older table is discarded, never installed.
fn ensure_presentation(
    entity: &Entity<ConflictResolvState>,
    retained: &Rc<RefCell<Retained>>,
    cx: &mut App,
) {
    let retained = retained.clone();
    entity.update(cx, |state, cx| {
        let revision = state.rows_revision();
        let mut guard = retained.borrow_mut();
        if guard.presented == Some(revision) || guard.preparing == Some(revision) {
            return;
        }
        guard.preparing = Some(revision);
        let path = state.path().clone();
        let columns = [
            state.yours_lines().to_vec(),
            state.middle_lines().to_vec(),
            state.upstream_lines().to_vec(),
        ];
        let id = cx.entity().entity_id();
        guard.task = cx.spawn(async move |this, cx| {
            let presentation = cx
                .background_spawn(async move {
                    Arc::new(ConflictPresentation::prepare(&path, &columns))
                })
                .await;
            let _ = this.update(cx, |_, cx| {
                let Some(entry) = cx.default_global::<VirtualRegistry>().0.get(&id).cloned() else {
                    return;
                };
                let mut retained = entry.borrow_mut();
                // A newer table started preparing meanwhile: this result
                // indexes lines that no longer exist, so it dies here.
                if retained.preparing != Some(revision) {
                    return;
                }
                retained.preparing = None;
                retained.presented = Some(revision);
                retained.presentation = Some(presentation);
                drop(retained);
                cx.notify();
            });
        });
    });
}

/// Shapes each column's widest line candidates with the code font; the
/// result clamps that pane's horizontal scroll. Candidates are picked by
/// display cells first because shaping every line would defeat the point.
fn measure_column_widths(
    columns: [&[SharedString]; 3],
    font_size: Pixels,
    family: &SharedString,
    window: &mut Window,
    cx: &App,
) -> [Pixels; 3] {
    // Glyph widths vary within a font, so shape several candidates rather
    // than trusting the single widest by cell count.
    const CANDIDATES: usize = 8;
    let font = Font {
        family: family.clone(),
        ..Default::default()
    };
    columns.map(|lines| {
        widest_candidates(lines, CANDIDATES)
            .into_iter()
            .map(|text| {
                window
                    .text_system()
                    .shape_line(
                        text.clone(),
                        font_size,
                        &[TextRun {
                            len: text.len(),
                            font: font.clone(),
                            color: cx.theme().foreground,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        None,
                    )
                    .width
            })
            .fold(px(0.), Pixels::max)
    })
}

/// The column's lines widest in display cells, as shaping candidates.
fn widest_candidates(lines: &[SharedString], per: usize) -> Vec<&SharedString> {
    let mut candidates: Vec<(usize, &SharedString)> = Vec::with_capacity(per + 1);
    for line in lines {
        let cells = unicode_width::UnicodeWidthStr::width(line.as_str());
        if candidates.len() == per
            && candidates
                .last()
                .is_some_and(|(widest, _)| *widest >= cells)
        {
            continue;
        }
        let ix = candidates.partition_point(|(widest, _)| *widest >= cells);
        candidates.insert(ix, (cells, line));
        candidates.truncate(per);
    }
    candidates.into_iter().map(|(_, line)| line).collect()
}

fn pane_headers(
    yours: SharedString,
    result: SharedString,
    upstream: SharedString,
    ctx: &ItemContext,
) -> AnyElement {
    let header_height = ConflictResolvVirtual::HEADER_HEIGHT;
    let label = |text: SharedString| {
        h_flex()
            .flex_1()
            .min_w_0()
            .h(header_height)
            .px_2()
            .text_xs()
            .text_color(ctx.band_fg)
            .bg(ctx.band_bg)
            .border_b_1()
            .border_color(ctx.border)
            .child(text)
    };
    let gutter = |left: bool| {
        div()
            .w(px(GUTTER_WIDTH))
            .flex_none()
            .h(header_height)
            .bg(ctx.band_bg)
            .border_b_1()
            .when(left, |this| this.border_l_1())
            .when(!left, |this| this.border_r_1())
            .border_color(ctx.border)
    };
    h_flex()
        .w_full()
        .child(label(yours))
        .child(gutter(true))
        .child(label(result))
        .child(gutter(false))
        .child(label(upstream))
        .into_any_element()
}

fn render_item(ix: usize, ctx: &Rc<ItemContext>, cx: &mut App) -> AnyElement {
    let Some(item) = ctx.items.get(ix).cloned() else {
        return div().into_any_element();
    };
    match item {
        RowItem::Fold { ix: fold, hidden } => {
            let state = ctx.state.clone();
            h_flex()
                .id(("crv-fold", fold))
                .h(ctx.row_height)
                .w_full()
                .justify_center()
                .whitespace_nowrap()
                .line_height(ctx.row_height)
                .text_size(ctx.number_size)
                .text_color(ctx.band_fg)
                .bg(ctx.band_bg)
                .cursor_pointer()
                .on_click(move |_, _, cx| state.update(cx, |state, cx| state.reveal_fold(fold, cx)))
                .child(t!("ConflictResolv.UnchangedLines", count = hidden).to_string())
                .into_any_element()
        }
        RowItem::Row(d) => render_line_row(d, ctx, cx),
        RowItem::Editor { block, rows } => render_editor_item(block, rows, ctx, cx),
    }
}

/// One visual row: yours | gutter | result | gutter | upstream, each pane
/// with a fixed number lane and a horizontally scrolled code cell.
fn render_line_row(d: usize, ctx: &Rc<ItemContext>, cx: &App) -> AnyElement {
    let state = ctx.state.read(cx);
    let display = state.display_rows();
    let rows = state.rows();
    let Some(DisplayRow::Row(rix)) = display.get(d) else {
        return div().h(ctx.row_height).into_any_element();
    };
    let row = rows[*rix];
    let (tint, anchor) = match row.kind {
        ConflictRowKind::Context => (None, None),
        ConflictRowKind::Block(block) => {
            // The block's first visual row anchors its controls, as in the
            // original; folds never interrupt a block, so looking one row
            // back suffices.
            let first = !matches!(
                d.checked_sub(1).and_then(|p| display.get(p)),
                Some(DisplayRow::Row(prev)) if rows[*prev].kind == row.kind
            );
            (
                Some(ctx.tints[tint_slot(state.resolution(block))]),
                first.then_some(block),
            )
        }
    };
    let controls = anchor.map(|block| (block, state.resolution(block)));
    let take_both = controls
        .filter(|(_, resolution)| *resolution == BlockResolution::Unresolved)
        .map(|(block, _)| block);
    h_flex()
        .h(ctx.row_height)
        .w_full()
        .whitespace_nowrap()
        .font_family(ctx.font_family.clone())
        .text_size(ctx.font_size)
        .line_height(ctx.row_height)
        .when_some(tint, |this, tint| this.bg(tint))
        .child(pane_cell(
            Pane::Yours,
            d,
            row.left,
            state.yours_lines(),
            None,
            ctx,
        ))
        .child(gutter_cell(true, controls, ctx))
        .child(pane_cell(
            Pane::Result,
            d,
            row.middle,
            state.middle_lines(),
            take_both,
            ctx,
        ))
        .child(gutter_cell(false, controls, ctx))
        .child(pane_cell(
            Pane::Upstream,
            d,
            row.right,
            state.upstream_lines(),
            None,
            ctx,
        ))
        .into_any_element()
}

/// A resolved block's rows as one item: the side panes stack the covered
/// rows, the middle hosts the embedded editor at the same height, and the
/// right gutter carries Undo on the first row.
fn render_editor_item(
    block: usize,
    span: Range<usize>,
    ctx: &Rc<ItemContext>,
    cx: &App,
) -> AnyElement {
    let state = ctx.state.read(cx);
    let Some(editor) = state.editor(block).cloned() else {
        // The editor dropped since the items were derived (an undo between
        // notify and paint); the next render replaces this item.
        return div()
            .h(ctx.row_height * span.len() as f32)
            .into_any_element();
    };
    let display = state.display_rows();
    let rows = state.rows();
    let resolution = state.resolution(block);
    let tint = ctx.tints[tint_slot(resolution)];
    let cells = |pick: fn(&ConflictRow) -> ConflictCell| -> Vec<ConflictCell> {
        span.clone()
            .map(|d| match display.get(d) {
                Some(DisplayRow::Row(rix)) => pick(&rows[*rix]),
                _ => ConflictCell::Spacer,
            })
            .collect()
    };
    let controls = Some((block, resolution));
    h_flex()
        .h(ctx.row_height * span.len() as f32)
        .w_full()
        .items_start()
        .whitespace_nowrap()
        .font_family(ctx.font_family.clone())
        .text_size(ctx.font_size)
        .line_height(ctx.row_height)
        .bg(tint)
        .child(pane_column(
            Pane::Yours,
            block,
            cells(|row| row.left),
            state.yours_lines(),
            ctx,
        ))
        .child(gutter_column(true, controls, ctx))
        .child(editor_column(block, cells(|row| row.middle), editor, ctx))
        .child(gutter_column(false, controls, ctx))
        .child(pane_column(
            Pane::Upstream,
            block,
            cells(|row| row.right),
            state.upstream_lines(),
            ctx,
        ))
        .into_any_element()
}

fn number_cell(cell: ConflictCell, ctx: &ItemContext) -> AnyElement {
    h_flex()
        .h(ctx.row_height)
        .px_1()
        .justify_end()
        .text_size(ctx.number_size)
        .text_color(ctx.number_fg)
        .when_some(
            match cell {
                ConflictCell::Line(ix) => Some((ix + 1).to_string()),
                ConflictCell::Spacer => None,
            },
            |this, number| this.child(number),
        )
        .into_any_element()
}

fn line_text(
    pane: Pane,
    ix: usize,
    lines: &[SharedString],
    ctx: &ItemContext,
) -> Option<AnyElement> {
    let text = lines.get(ix)?.clone();
    let highlights = ctx
        .presentation
        .as_ref()
        .map(|presentation| {
            presentation.highlights(pane as usize, ix, &text, ctx.highlight_theme.as_ref())
        })
        .unwrap_or_default();
    Some(if highlights.is_empty() {
        text.into_any_element()
    } else {
        StyledText::new(text)
            .with_highlights(highlights)
            .into_any_element()
    })
}

fn pane_code_id(pane: Pane, item: usize) -> (&'static str, usize) {
    (
        match pane {
            Pane::Yours => "crv-yours",
            Pane::Result => "crv-result",
            Pane::Upstream => "crv-upstream",
        },
        item,
    )
}

/// A pane's share of one row: fixed number lane, then the code cell shifted
/// by the pane's shared horizontal offset. The take-both control floats over
/// the whole pane, centered, so it never shifts the row's text.
fn pane_cell(
    pane: Pane,
    d: usize,
    cell: ConflictCell,
    lines: &[SharedString],
    take_both: Option<usize>,
    ctx: &Rc<ItemContext>,
) -> AnyElement {
    let line = match cell {
        ConflictCell::Line(ix) => line_text(pane, ix, lines, ctx),
        ConflictCell::Spacer => None,
    };
    h_flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .relative()
        .child(
            div()
                .w(ctx.number_width(pane))
                .flex_none()
                .h_full()
                .child(number_cell(cell, ctx)),
        )
        .child(
            div()
                .id(pane_code_id(pane, d))
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_x_hidden()
                .track_scroll(ctx.pane_scroll(pane))
                .child(
                    div()
                        .px_2()
                        .h_full()
                        // Every row carries the pane's measured width, so the
                        // shared offset clamps the same in each one; the rem
                        // keeps the longest line clear of the edge.
                        .min_w(ctx.pane_width(pane) + ctx.rem)
                        .children(line),
                ),
        )
        .when_some(take_both, |this, block| {
            this.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(super::take_both_button(block, &ctx.state)),
            )
        })
        .into_any_element()
}

/// A pane's share of an editor item: the covered rows stacked, numbers fixed
/// and the code scrolled as one group on the pane's shared offset.
fn pane_column(
    pane: Pane,
    block: usize,
    cells: Vec<ConflictCell>,
    lines: &[SharedString],
    ctx: &Rc<ItemContext>,
) -> AnyElement {
    h_flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .items_start()
        .child(
            v_flex()
                .w(ctx.number_width(pane))
                .flex_none()
                .children(cells.iter().map(|cell| number_cell(*cell, ctx))),
        )
        .child(
            div()
                .id(pane_code_id(pane, block))
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_x_hidden()
                .track_scroll(ctx.pane_scroll(pane))
                .child(
                    v_flex()
                        .px_2()
                        .min_w(ctx.pane_width(pane) + ctx.rem)
                        .children(cells.iter().map(|cell| {
                            div().h(ctx.row_height).children(match cell {
                                ConflictCell::Line(ix) => line_text(pane, *ix, lines, ctx),
                                ConflictCell::Spacer => None,
                            })
                        })),
                ),
        )
        .into_any_element()
}

/// The middle pane of an editor item: numbers beside the embedded editor,
/// which replaces the block's line cells wholesale.
fn editor_column(
    block: usize,
    cells: Vec<ConflictCell>,
    editor: Entity<crate::input::TextareaState>,
    ctx: &Rc<ItemContext>,
) -> AnyElement {
    let _ = block;
    h_flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .items_start()
        .child(
            v_flex()
                .w(ctx.number_width(Pane::Result))
                .flex_none()
                .children(cells.iter().map(|cell| number_cell(*cell, ctx))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .px_2()
                .line_height(ctx.row_height)
                .child(editor),
        )
        .into_any_element()
}

/// One gutter cell on the middle pane's edge; its outer border is the pane
/// divider, and a block's anchor row carries the pick or undo control.
fn gutter_cell(
    left: bool,
    controls: Option<(usize, BlockResolution)>,
    ctx: &Rc<ItemContext>,
) -> AnyElement {
    h_flex()
        .w(px(GUTTER_WIDTH))
        .flex_none()
        .h_full()
        .justify_center()
        .when(left, |this| this.border_l_1())
        .when(!left, |this| this.border_r_1())
        .border_color(ctx.border)
        .when_some(controls, |this, (block, resolution)| {
            this.children(super::gutter_controls(left, block, resolution, &ctx.state))
        })
        .into_any_element()
}

/// A gutter spanning an editor item, controls on its first row.
fn gutter_column(
    left: bool,
    controls: Option<(usize, BlockResolution)>,
    ctx: &Rc<ItemContext>,
) -> AnyElement {
    v_flex()
        .w(px(GUTTER_WIDTH))
        .flex_none()
        .h_full()
        .when(left, |this| this.border_l_1())
        .when(!left, |this| this.border_r_1())
        .border_color(ctx.border)
        .child(h_flex().h(ctx.row_height).justify_center().when_some(
            controls,
            |this, (block, resolution)| {
                this.children(super::gutter_controls(left, block, resolution, &ctx.state))
            },
        ))
        .into_any_element()
}

/// Syntax spans for the three columns, prepared off the UI thread and cached
/// behind the row-table revision; names resolve against the theme only when
/// a line paints, so a theme switch needs no re-preparation.
pub(super) struct ConflictPresentation {
    names: Vec<SharedString>,
    columns: [ColumnSpans; 3],
}

/// Non-overlapping byte ranges relative to each line's start, with the index
/// of the capture name that paints them.
#[derive(Default)]
struct ColumnSpans {
    /// `starts[ix]..starts[ix + 1]` index the spans of line `ix`.
    starts: Vec<u32>,
    spans: Vec<(Range<u32>, u16)>,
}

impl ColumnSpans {
    fn from_lines(per_line: Vec<Vec<(Range<u32>, u16)>>) -> Self {
        let mut columns = Self::default();
        columns.starts.reserve(per_line.len() + 1);
        columns.starts.push(0);
        for line in per_line {
            columns.spans.extend(line);
            columns.starts.push(columns.spans.len() as u32);
        }
        columns
    }

    fn line(&self, ix: usize) -> &[(Range<u32>, u16)] {
        match (self.starts.get(ix), self.starts.get(ix + 1)) {
            (Some(start), Some(end)) => &self.spans[*start as usize..*end as usize],
            _ => &[],
        }
    }
}

impl Default for ConflictPresentation {
    fn default() -> Self {
        Self {
            names: Vec::new(),
            columns: [
                ColumnSpans::default(),
                ColumnSpans::default(),
                ColumnSpans::default(),
            ],
        }
    }
}

impl ConflictPresentation {
    /// Blocking; run it on a background thread. Each column joins into one
    /// plausible file variant and parses as a whole, unlike the Diff's
    /// per-hunk fragments: the resolver always has the full text.
    pub(super) fn prepare(path: &str, columns: &[Vec<SharedString>; 3]) -> Self {
        let language = detected_language(path);
        let mut presentation = Self::default();
        if language.as_str() == "text" {
            return presentation;
        }
        let mut highlighter = SyntaxHighlighter::new(&language);
        let mut name_ixs = HashMap::<SharedString, u16>::new();
        for (slot, lines) in columns.iter().enumerate() {
            if lines.is_empty() {
                continue;
            }
            let mut text = String::with_capacity(lines.iter().map(|line| line.len() + 1).sum());
            for line in lines {
                text.push_str(line);
                text.push('\n');
            }
            let captures: Vec<(Range<usize>, u16)> = highlighter
                .capture_names(&Rope::from_str(&text))
                .into_iter()
                .filter_map(|(range, name)| {
                    // The sentinel marks unpainted bytes, so the last slot
                    // stays unused; a file needing 65535 names has other
                    // problems.
                    let next = name_ixs.len() as u16;
                    if next == u16::MAX && !name_ixs.contains_key(&name) {
                        return None;
                    }
                    let ix = *name_ixs.entry(name.clone()).or_insert_with(|| {
                        presentation.names.push(name.clone());
                        next
                    });
                    Some((range, ix))
                })
                .collect();
            presentation.columns[slot] = column_spans(lines, &captures, MAX_SYNTAX_LINE);
        }
        presentation
    }

    /// Theme styles for one line, clamped against the text it will paint:
    /// a stale presentation may briefly index a replaced table, and a wrong
    /// span must degrade to plain text, never to a panic.
    pub(super) fn highlights(
        &self,
        column: usize,
        ix: usize,
        line: &str,
        theme: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        self.columns[column]
            .line(ix)
            .iter()
            .filter_map(|(range, name)| {
                let start = range.start as usize;
                let end = (range.end as usize).min(line.len());
                if start >= end || !line.is_char_boundary(start) || !line.is_char_boundary(end) {
                    return None;
                }
                let style = theme.style(&self.names[*name as usize])?;
                Some((start..end, style))
            })
            .collect()
    }
}

/// Distributes whole-column captures onto lines as non-overlapping spans in
/// paint order: a later capture overrides an earlier one, which is the
/// highlighter's precedence contract.
fn column_spans(
    lines: &[SharedString],
    captures: &[(Range<usize>, u16)],
    max_line_length: usize,
) -> ColumnSpans {
    let mut offsets = Vec::with_capacity(lines.len() + 1);
    let mut total = 0;
    for line in lines {
        offsets.push(total);
        total += line.len() + 1;
    }
    offsets.push(total);
    let mut raw: Vec<Vec<(Range<u32>, u16)>> = vec![Vec::new(); lines.len()];
    for (capture, name) in captures {
        let first = offsets
            .partition_point(|&start| start <= capture.start)
            .saturating_sub(1);
        for (line_ix, line) in lines.iter().enumerate().skip(first) {
            let line_start = offsets[line_ix];
            if line_start >= capture.end {
                break;
            }
            if line.len() > max_line_length {
                continue;
            }
            let start = capture.start.max(line_start);
            let end = capture.end.min(line_start + line.len());
            if start < end {
                raw[line_ix].push((
                    (start - line_start) as u32..(end - line_start) as u32,
                    *name,
                ));
            }
        }
    }
    let per_line = raw
        .into_iter()
        .zip(lines)
        .map(|(spans, line)| resolve_overlaps(line.len(), &spans))
        .collect();
    ColumnSpans::from_lines(per_line)
}

/// Flattens possibly overlapping spans into sorted, disjoint runs, later
/// spans winning. Byte stamping is linear in the line and runs once per
/// preparation, where clarity beats an interval tree.
fn resolve_overlaps(len: usize, spans: &[(Range<u32>, u16)]) -> Vec<(Range<u32>, u16)> {
    if spans.is_empty() {
        return Vec::new();
    }
    const BARE: u16 = u16::MAX;
    let mut paint = vec![BARE; len];
    for (range, name) in spans {
        let start = (range.start as usize).min(len);
        let end = (range.end as usize).min(len);
        paint[start..end].fill(*name);
    }
    let mut resolved = Vec::new();
    let mut ix = 0;
    while ix < len {
        let name = paint[ix];
        let start = ix;
        while ix < len && paint[ix] == name {
            ix += 1;
        }
        if name != BARE {
            resolved.push((start as u32..ix as u32, name));
        }
    }
    resolved
}

/// Copied from the Diff's private presentation module; keep the two in step.
/// File paths may come from another platform. Only the final component
/// determines the language; dots in a directory are not extensions.
fn detected_language(path: &str) -> SharedString {
    let name = path.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
    let language = match name.as_str() {
        "makefile" | "gnumakefile" => "make",
        "cmakelists.txt" => "cmake",
        _ => match name.rsplit_once('.').map(|(_, extension)| extension) {
            Some("jsx" | "mjs" | "cjs") => "javascript",
            Some("mts" | "cts") => "typescript",
            Some("cc" | "cxx" | "hpp" | "hxx") => "cpp",
            Some("h") => "c",
            Some("htm") => "html",
            Some("gql") => "graphql",
            Some("exs") => "elixir",
            Some(
                extension @ ("astro" | "sh" | "bash" | "c" | "cmake" | "cs" | "cpp" | "css"
                | "scss" | "diff" | "ejs" | "ex" | "erb" | "go" | "graphql" | "html"
                | "java" | "js" | "json" | "jsonc" | "kt" | "kts" | "lua" | "md"
                | "mdx" | "php" | "php3" | "php4" | "php5" | "phtml" | "proto" | "py"
                | "pyi" | "rb" | "rs" | "scala" | "sql" | "svelte" | "swift" | "toml"
                | "tsx" | "ts" | "yaml" | "yml" | "zig"),
            ) => extension,
            _ => "text",
        },
    };
    crate::highlighter::language_name(language)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "a\n<<<<<<< yours\nl\n=======\nr\n>>>>>>> upstream\nb\n<<<<<<< yours\nl2\n=======\nr2\n>>>>>>> upstream\nc\n";

    fn parsed(text: &str) -> ConflictResolvState {
        let mut state = ConflictResolvState::parse("a.rs", text).unwrap();
        state.sync_folds();
        state
    }

    #[test]
    fn items_map_one_to_one_without_editors() {
        let state = parsed(TEXT);
        let spans = vec![None; state.block_count()];
        let items = derive_items(state.display_rows(), state.rows(), &spans);
        assert_eq!(items.len(), state.display_rows().len());
        assert!(
            items
                .iter()
                .enumerate()
                .all(|(ix, item)| *item == RowItem::Row(ix))
        );
    }

    #[test]
    fn a_mounted_editor_collapses_its_rows_into_one_item() {
        // Three left lines against one right: the block spans three rows,
        // the middle one line, so the editor covers one row and the block's
        // remaining rows stay plain.
        let text = "a\n<<<<<<< yours\nl1\nl2\nl3\n=======\nr1\n>>>>>>> upstream\nb\n";
        let mut state = parsed(text);
        // Picks run through the model directly; no window is needed to pin
        // the item mapping.
        let display_len = state.display_rows().len();
        let spans = vec![Some(state.middle_line_count(0).max(1))];
        let items = derive_items(state.display_rows(), state.rows(), &spans);
        assert_eq!(
            items,
            [
                RowItem::Row(0),
                RowItem::Editor {
                    block: 0,
                    rows: 1..2
                },
                RowItem::Row(2),
                RowItem::Row(3),
                RowItem::Row(4),
            ]
        );
        // A span past the block clamps to the block's rows.
        let items = derive_items(state.display_rows(), state.rows(), &[Some(99)]);
        assert_eq!(
            items,
            [
                RowItem::Row(0),
                RowItem::Editor {
                    block: 0,
                    rows: 1..4
                },
                RowItem::Row(4),
            ]
        );
        assert_eq!(display_len, 5);
        let _ = &mut state;
    }

    #[test]
    fn folds_become_fold_items() {
        let run = |name: &str| (0..10).map(|i| format!("{name} {i}\n")).collect::<String>();
        let conflict = "<<<<<<< yours\nx\n=======\ny\n>>>>>>> upstream\n";
        let state = parsed(&format!(
            "{}{conflict}{}{conflict}{}",
            run("head"),
            run("mid"),
            run("tail")
        ));
        let spans = vec![None; state.block_count()];
        let items = derive_items(state.display_rows(), state.rows(), &spans);
        let folds: Vec<usize> = items
            .iter()
            .filter_map(|item| match item {
                RowItem::Fold { hidden, .. } => Some(*hidden),
                _ => None,
            })
            .collect();
        assert_eq!(folds, [7, 4, 7]);
    }

    #[test]
    fn splice_bounds_replace_only_the_changed_middle() {
        let old = vec![RowItem::Row(0), RowItem::Row(1), RowItem::Row(2)];
        assert_eq!(splice_bounds(&old, &old), None);
        let new = vec![
            RowItem::Row(0),
            RowItem::Editor {
                block: 0,
                rows: 1..2,
            },
            RowItem::Row(2),
        ];
        assert_eq!(splice_bounds(&old, &new), Some((1..2, 1)));
        // Growth in the middle keeps both anchors.
        let grown = vec![
            RowItem::Row(0),
            RowItem::Row(9),
            RowItem::Row(8),
            RowItem::Row(2),
        ];
        assert_eq!(splice_bounds(&old, &grown), Some((1..2, 2)));
        assert_eq!(splice_bounds(&[], &old), Some((0..0, 3)));
    }

    #[test]
    fn column_spans_split_captures_per_line_and_later_captures_win() {
        let lines: Vec<SharedString> = ["fn a()", "fn b()"].into_iter().map(Into::into).collect();
        // "fn a()\nfn b()" - a capture across the newline lands on both
        // lines, and the later capture overrides the earlier where they
        // overlap.
        let captures = vec![(0..13, 0), (0..2, 1), (7..9, 2)];
        let columns = column_spans(&lines, &captures, MAX_SYNTAX_LINE);
        assert_eq!(columns.line(0), [(0..2, 1), (2..6, 0)]);
        assert_eq!(columns.line(1), [(0..2, 2), (2..6, 0)]);
        assert_eq!(columns.line(2), []);
    }

    #[test]
    fn highlights_clamp_against_the_painted_line() {
        struct Bold;
        impl HighlightStyleResolver for Bold {
            fn style(&self, _: &str) -> Option<HighlightStyle> {
                Some(HighlightStyle {
                    font_weight: Some(gpui::FontWeight::BOLD),
                    ..Default::default()
                })
            }
        }
        let lines: Vec<SharedString> = vec!["let value".into()];
        let presentation = ConflictPresentation {
            names: vec!["keyword".into()],
            columns: [
                ColumnSpans::from_lines(vec![vec![(0..3, 0), (4..20, 0)]]),
                ColumnSpans::default(),
                ColumnSpans::default(),
            ],
        };
        // A stale span past the line's end clamps; one inside a shorter
        // replacement line survives as far as it fits.
        let spans = presentation.highlights(0, 0, &lines[0], &Bold);
        let ranges: Vec<Range<usize>> = spans.into_iter().map(|(range, _)| range).collect();
        assert_eq!(ranges, [0..3, 4..9]);
        // A clamp landing inside a multi-byte character degrades that span
        // to plain text instead of panicking in the text system.
        let torn = ConflictPresentation {
            names: vec!["keyword".into()],
            columns: [
                ColumnSpans::from_lines(vec![vec![(0..1, 0), (4..9, 0)]]),
                ColumnSpans::default(),
                ColumnSpans::default(),
            ],
        };
        assert!(torn.highlights(0, 0, "é", &Bold).is_empty());
    }

    #[test]
    fn preparation_follows_the_file_path() {
        let columns = [
            vec![SharedString::from("fn main() {}")],
            vec![SharedString::from("fn main() {}")],
            vec![SharedString::from("fn main() {}")],
        ];
        let plain = ConflictPresentation::prepare("notes.unknown", &columns);
        assert!(plain.names.is_empty());
        assert!(plain.columns.iter().all(|column| column.line(0).is_empty()));
    }

    struct Host(Entity<ConflictResolvState>);

    impl gpui::Render for Host {
        fn render(
            &mut self,
            _: &mut Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
                .w(px(900.))
                .h(px(300.))
                .child(ConflictResolvVirtual::new(&self.0).size_full())
        }
    }

    fn draw(cx: &mut gpui::VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn the_window_mounts_items_editors_and_prepared_highlighting(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        cx.update(crate::init);
        let conflict =
            "<<<<<<< yours\nlet picked = 1;\n=======\nlet other = 2;\n>>>>>>> upstream\n";
        // One long run to fold, then runs short enough to stay visible, so
        // the surface holds far more rows than a 300 px viewport shows.
        let mut text = (0..40)
            .map(|i| format!("let head_{i} = {i};\n"))
            .collect::<String>();
        for block in 0..30 {
            text.push_str(conflict);
            for i in 0..7 {
                text.push_str(&format!("let keep_{block}_{i} = {i};\n"));
            }
        }
        let state = cx.new(|_| ConflictResolvState::parse("large.rs", &text).unwrap());
        let (_, cx) = cx.add_window_view({
            let state = state.clone();
            move |_, _| Host(state)
        });
        for _ in 0..3 {
            draw(cx);
        }
        let entry = cx.update(|_, cx| {
            cx.default_global::<VirtualRegistry>()
                .0
                .get(&state.entity_id())
                .expect("render retains its state")
                .clone()
        });
        {
            let retained = entry.borrow();
            // Long context runs fold, both blocks start unresolved as rows.
            assert!(
                retained
                    .items
                    .iter()
                    .any(|item| matches!(item, RowItem::Fold { .. }))
            );
            assert!(
                !retained
                    .items
                    .iter()
                    .any(|item| matches!(item, RowItem::Editor { .. }))
            );
            // Highlighting prepared in the background and installed for the
            // current table; column widths were shaped.
            assert_eq!(
                retained.presented,
                state.read_with(cx, |state, _| { Some(state.rows_revision()) })
            );
            assert!(retained.presentation.is_some());
            assert!(retained.widths.iter().all(|width| *width > px(0.)));
            // The hint sizes the scrollbar past the rendered slice.
            let list = retained.list.clone();
            assert!(
                list.max_offset_for_scrollbar().y > list.viewport_bounds().size.height,
                "the fixture outruns a 300 px viewport"
            );
        }
        // A pick resolves the block; the next render mounts its editor and
        // the item mapping folds the block's rows into one editor item.
        state.update(cx, |state, cx| state.take_yours(0, cx));
        for _ in 0..3 {
            draw(cx);
        }
        state.read_with(cx, |state, _| assert!(state.editor(0).is_some()));
        assert!(
            entry
                .borrow()
                .items
                .iter()
                .any(|item| matches!(item, RowItem::Editor { block: 0, .. }))
        );
        // Undo drops the editor again; the item mapping follows.
        state.update(cx, |state, cx| state.undo_block(0, cx));
        for _ in 0..3 {
            draw(cx);
        }
        assert!(
            !entry
                .borrow()
                .items
                .iter()
                .any(|item| matches!(item, RowItem::Editor { .. }))
        );
    }
}
