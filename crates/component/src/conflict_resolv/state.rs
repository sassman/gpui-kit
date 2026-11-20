//! Retained resolver state for one conflicted file, owned by the view that
//! renders [`super::ConflictResolv`].
use std::collections::HashSet;
use std::ops::Range;

use gpui::{AppContext as _, Context, Entity, EventEmitter, SharedString, Window};

use super::model::{BlockResolution, ConflictModel, ConflictRow, ConflictRowKind, Pick};
use crate::diff::DiffParseError;
use crate::input::{InputEvent, TextareaState};

/// Unchanged lines kept visible around each block, the frame the Diff
/// keeps around a change.
const FOLD_CONTEXT: usize = 3;
/// The least hidden rows worth a disclosure; shorter runs stay visible.
const MIN_FOLD: usize = 2;

/// One visual row after folding: a row of the model's table, or a fold
/// standing in for a run of unchanged rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DisplayRow {
    Row(usize),
    /// `rows[hidden]` sit behind one disclosure row; `ix` keys the reveal.
    Fold {
        ix: usize,
        hidden: Range<usize>,
    },
}

/// Context runs collapse behind a fold, keeping the [`FOLD_CONTEXT`]
/// frame toward each neighbouring block; the file's leading and trailing
/// runs keep the frame only on their block side. Pure presentation:
/// picks, edits and the result never see folds.
fn derive_folds(rows: &[ConflictRow], revealed: &HashSet<usize>) -> Vec<DisplayRow> {
    let mut display = Vec::with_capacity(rows.len());
    let mut fold_ix = 0;
    let mut row = 0;
    while row < rows.len() {
        if rows[row].kind != ConflictRowKind::Context {
            display.push(DisplayRow::Row(row));
            row += 1;
            continue;
        }
        let start = row;
        while row < rows.len() && rows[row].kind == ConflictRowKind::Context {
            row += 1;
        }
        let head = if start == 0 { 0 } else { FOLD_CONTEXT };
        let tail = if row == rows.len() { 0 } else { FOLD_CONTEXT };
        if row - start < head + tail + MIN_FOLD {
            display.extend((start..row).map(DisplayRow::Row));
            continue;
        }
        display.extend((start..start + head).map(DisplayRow::Row));
        let hidden = start + head..row - tail;
        if revealed.contains(&fold_ix) {
            display.extend(hidden.clone().map(DisplayRow::Row));
        } else {
            display.push(DisplayRow::Fold {
                ix: fold_ix,
                hidden,
            });
        }
        fold_ix += 1;
        display.extend((row - tail..row).map(DisplayRow::Row));
    }
    display
}

/// Notifications from the three-pane resolver.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConflictResolvEvent {
    /// A pick or hand edit left this block resolved; read the choice with
    /// [`ConflictResolvState::resolution`].
    BlockResolved(usize),
    /// Every block is resolved; [`ConflictResolvState::result_text`] holds
    /// the merge result.
    AllBlocksResolved,
    /// An undo reopened this block, so an earlier [`Self::AllBlocksResolved`]
    /// no longer holds.
    BlockReopened(usize),
    /// Typing in a block's embedded result editor, or
    /// [`ConflictResolvState::edit_block`], changed the result text.
    Edited,
}

/// The three derived columns of one conflicted file and each block's
/// resolution, created with [`Self::parse`] inside the owner's entity.
///
/// The public vocabulary is yours and upstream, never Git's ours and theirs:
/// the application maps the operation's sides before parsing.
pub struct ConflictResolvState {
    model: ConflictModel,
    /// One lazily mounted editor per block, present while the block is
    /// resolved. A block resolved by a pick becomes editable; undo drops
    /// its editor and returns the middle to read-only base lines. Only the
    /// entity is held, never its change subscription: a `Subscription` is
    /// not `Send`, and owners parse this state on background threads.
    editors: Vec<Option<Entity<TextareaState>>>,
    /// Blocks whose mounted editor must be reseeded because the model
    /// changed the text underneath it (a pick over a hand edit).
    stale: Vec<bool>,
    /// The folded view of the row table; see [`derive_folds`].
    display_rows: Vec<DisplayRow>,
    /// The model revision the display rows were derived from.
    folded_revision: Option<u64>,
    /// Folds opened by hand, by ordinal. An edit that changes the table
    /// re-derives the folds; the ordinals usually land on the same runs.
    revealed_folds: HashSet<usize>,
}

// Owners parse off the UI thread and send the state back, so it must stay
// `Send`; a field that loses it (a `Subscription`, say) must fail here, not
// in a downstream build.
const _: () = {
    const fn require_send<T: Send>() {}
    require_send::<ConflictResolvState>()
};

impl EventEmitter<ConflictResolvEvent> for ConflictResolvState {}

impl ConflictResolvState {
    /// Parses a working file with Git conflict markers, `diff3` bases
    /// included. The middle starts on each block's base lines, empty for
    /// two-way markers; raw markers never appear in any pane.
    pub fn parse(path: impl Into<SharedString>, text: &str) -> Result<Self, DiffParseError> {
        let model = ConflictModel::parse(path, text)?;
        let blocks = model.block_count();
        Ok(Self {
            model,
            editors: (0..blocks).map(|_| None).collect(),
            stale: vec![false; blocks],
            display_rows: Vec::new(),
            folded_revision: None,
            revealed_folds: HashSet::new(),
        })
    }

    pub fn path(&self) -> &SharedString {
        self.model.path()
    }
    /// The number of conflict blocks; their indices run in file order.
    pub fn block_count(&self) -> usize {
        self.model.block_count()
    }
    /// The block's state; a stale index reads as unresolved.
    pub fn resolution(&self, block: usize) -> BlockResolution {
        if block >= self.model.block_count() {
            return BlockResolution::Unresolved;
        }
        self.model.resolution(block)
    }
    /// Whether no block is left unresolved. A file without conflicts counts
    /// as resolved.
    pub fn all_resolved(&self) -> bool {
        self.model.all_resolved()
    }
    /// The middle column's text: the merge result once every block is
    /// resolved, reflecting every pick and hand edit as it stands.
    pub fn result_text(&self) -> String {
        self.model.result_text()
    }
    /// The label of the block's opening marker, such as `HEAD`; empty for a
    /// stale index.
    pub fn yours_marker(&self, block: usize) -> SharedString {
        if block >= self.model.block_count() {
            return SharedString::default();
        }
        self.model.yours_label(block).clone()
    }
    /// The label of the block's closing marker, such as a branch name; empty
    /// for a stale index.
    pub fn upstream_marker(&self, block: usize) -> SharedString {
        if block >= self.model.block_count() {
            return SharedString::default();
        }
        self.model.upstream_label(block).clone()
    }

    /// Splices the left pane's lines into the block.
    pub fn take_yours(&mut self, block: usize, cx: &mut Context<Self>) {
        self.apply(block, Pick::Yours, cx);
    }
    /// Splices the right pane's lines into the block.
    pub fn take_upstream(&mut self, block: usize, cx: &mut Context<Self>) {
        self.apply(block, Pick::Upstream, cx);
    }
    /// Splices yours followed by upstream into the block.
    pub fn take_both(&mut self, block: usize, cx: &mut Context<Self>) {
        self.apply(block, Pick::Both, cx);
    }
    /// Restores the block's base lines and the unresolved state. The block
    /// becomes read-only again until the next pick.
    pub fn undo_block(&mut self, block: usize, cx: &mut Context<Self>) {
        if block >= self.model.block_count()
            || self.model.resolution(block) == BlockResolution::Unresolved
        {
            return;
        }
        self.model.undo(block);
        self.stale[block] = true;
        cx.emit(ConflictResolvEvent::BlockReopened(block));
        cx.notify();
    }
    /// Replaces a resolved block's result text, as typing in the embedded
    /// editor would: the first divergence from the picked text flips the
    /// block to [`BlockResolution::HandEdited`]. `text` is the block's
    /// lines without a trailing newline; an empty string empties the
    /// block. An unresolved block is read-only and ignores the call.
    pub fn edit_block(&mut self, block: usize, text: &str, cx: &mut Context<Self>) {
        if block >= self.model.block_count() {
            return;
        }
        if self.apply_edit(block, text, cx) {
            // Unlike an editor-sourced edit, the mounted editor does not
            // hold this text yet and must be reseeded.
            self.stale[block] = true;
        }
    }
    /// Marks a block the result editor changed by hand; the edit itself
    /// precedes the call through [`ConflictModel::edit`].
    pub(crate) fn mark_hand_edited(&mut self, block: usize, cx: &mut Context<Self>) {
        if block >= self.model.block_count() {
            return;
        }
        let was_all = self.model.all_resolved();
        self.model.mark_hand_edited(block);
        cx.emit(ConflictResolvEvent::Edited);
        self.finish(block, was_all, cx);
    }

    /// Syncs one edited text into the block and reports whether it changed
    /// anything. Emits [`ConflictResolvEvent::Edited`] on every change and
    /// flips the block on its first divergence from the picked text.
    fn apply_edit(&mut self, block: usize, text: &str, cx: &mut Context<Self>) -> bool {
        let resolution = self.model.resolution(block);
        if !self.model.edit(block, text) {
            return false;
        }
        if resolution == BlockResolution::HandEdited {
            cx.emit(ConflictResolvEvent::Edited);
            cx.notify();
        } else {
            self.mark_hand_edited(block, cx);
        }
        true
    }

    fn apply(&mut self, block: usize, pick: Pick, cx: &mut Context<Self>) {
        if block >= self.model.block_count() {
            return;
        }
        let was_all = self.model.all_resolved();
        self.model.pick(block, pick);
        // A pick onto a mounted editor (re-pick over a hand edit) changes
        // the text underneath it.
        self.stale[block] = true;
        self.finish(block, was_all, cx);
    }

    fn finish(&mut self, block: usize, was_all: bool, cx: &mut Context<Self>) {
        cx.emit(ConflictResolvEvent::BlockResolved(block));
        if !was_all && self.model.all_resolved() {
            cx.emit(ConflictResolvEvent::AllBlocksResolved);
        }
        cx.notify();
    }

    /// Mounts, reseeds and drops the per-block result editors so they
    /// mirror the model: a resolved block gets an editable middle, an
    /// unresolved one stays read-only derived text. The component calls
    /// this on render, the only place a [`Window`] exists.
    pub(crate) fn sync_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for block in 0..self.model.block_count() {
            if self.model.resolution(block) == BlockResolution::Unresolved {
                // Pick first: until then the middle shows the base lines.
                self.editors[block] = None;
                self.stale[block] = false;
                continue;
            }
            match &self.editors[block] {
                None => {
                    let text = self.model.block_editor_text(block);
                    let input = cx.new(|cx| {
                        // No row cap and no soft wrap: the editor's rows map
                        // 1:1 onto the row table, so the three panes and the
                        // middle line numbers stay aligned.
                        TextareaState::new(window, cx)
                            .auto_grow(1, u16::MAX as usize)
                            .soft_wrap(false)
                            .default_value(text)
                    });
                    // Detached, not held: GPUI releases the subscription with
                    // the editor entity when undo drops it, and holding it
                    // here would cost the state its `Send`.
                    cx.subscribe(&input, move |this, input, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            let value = input.read(cx).value();
                            this.apply_edit(block, &value, cx);
                        }
                    })
                    .detach();
                    self.editors[block] = Some(input);
                    self.stale[block] = false;
                }
                Some(editor) if self.stale[block] => {
                    let text = self.model.block_editor_text(block);
                    let input = editor.clone();
                    // `set_value` emits no change event, and syncing back an
                    // unchanged text is refused anyway, so no feedback loop.
                    input.update(cx, |state, cx| state.set_value(text, window, cx));
                    self.stale[block] = false;
                }
                Some(_) => {}
            }
        }
    }

    /// The block's mounted result editor, present while the block is
    /// resolved and [`Self::sync_editors`] ran since.
    pub(crate) fn editor(&self, block: usize) -> Option<&Entity<TextareaState>> {
        self.editors.get(block)?.as_ref()
    }

    /// The number of middle lines the block currently spans; sizes the
    /// block's embedded editor.
    pub(crate) fn middle_line_count(&self, block: usize) -> usize {
        self.model.middle_line_count(block)
    }

    /// Re-derives the folded view when the row table changed; the
    /// component calls this on render beside [`Self::sync_editors`].
    pub(crate) fn sync_folds(&mut self) {
        if self.folded_revision == Some(self.model.rows_revision()) {
            return;
        }
        self.folded_revision = Some(self.model.rows_revision());
        self.display_rows = derive_folds(self.model.rows(), &self.revealed_folds);
    }

    /// The folded view; valid after [`Self::sync_folds`].
    pub(crate) fn display_rows(&self) -> &[DisplayRow] {
        &self.display_rows
    }

    /// Opens fold `ix`, putting its hidden rows back inline.
    pub(crate) fn reveal_fold(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.revealed_folds.insert(ix);
        self.display_rows = derive_folds(self.model.rows(), &self.revealed_folds);
        cx.notify();
    }

    pub(crate) fn rows(&self) -> &[ConflictRow] {
        self.model.rows()
    }
    pub(crate) fn yours_lines(&self) -> &[SharedString] {
        self.model.yours_lines()
    }
    pub(crate) fn middle_lines(&self) -> &[SharedString] {
        self.model.middle_lines()
    }
    pub(crate) fn upstream_lines(&self) -> &[SharedString] {
        self.model.upstream_lines()
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::TestAppContext;

    use super::*;

    const TEXT: &str = "a\n<<<<<<< yours\nl\n=======\nr\n>>>>>>> upstream\nb\n<<<<<<< yours\nl2\n=======\nr2\n>>>>>>> upstream\nc\n";

    /// Ten context lines, a conflict, ten more, a conflict, ten more.
    fn long_text() -> String {
        let run = |name: &str| (0..10).map(|i| format!("{name} {i}\n")).collect::<String>();
        let conflict = "<<<<<<< yours\nx\n=======\ny\n>>>>>>> upstream\n";
        format!(
            "{}{conflict}{}{conflict}{}",
            run("head"),
            run("mid"),
            run("tail")
        )
    }

    #[test]
    fn long_context_runs_fold_behind_a_disclosure() {
        let mut state = ConflictResolvState::parse("f", &long_text()).unwrap();
        state.sync_folds();
        // The leading run keeps its frame only toward the block (7 hidden),
        // the middle run keeps a frame on both sides (4 hidden), the
        // trailing run again one side (7 hidden).
        let folds: Vec<usize> = state
            .display_rows()
            .iter()
            .filter_map(|row| match row {
                DisplayRow::Fold { hidden, .. } => Some(hidden.len()),
                _ => None,
            })
            .collect();
        assert_eq!(folds, vec![7, 4, 7]);
        // Every block row survives folding, in order.
        let blocks = |rows: &[ConflictRow]| {
            rows.iter()
                .filter(|row| row.kind != ConflictRowKind::Context)
                .count()
        };
        let shown = state
            .display_rows()
            .iter()
            .filter(|row| {
                matches!(row, DisplayRow::Row(ix)
                    if state.rows()[*ix].kind != ConflictRowKind::Context)
            })
            .count();
        assert_eq!(shown, blocks(state.rows()));
    }

    #[test]
    fn short_context_runs_stay_visible() {
        let mut state = ConflictResolvState::parse("f", TEXT).unwrap();
        state.sync_folds();
        assert_eq!(state.display_rows().len(), state.rows().len());
        assert!(
            state
                .display_rows()
                .iter()
                .all(|row| matches!(row, DisplayRow::Row(_)))
        );
    }

    #[test]
    fn a_revealed_fold_puts_its_rows_back() {
        let state = ConflictResolvState::parse("f", &long_text()).unwrap();
        let folded = derive_folds(state.rows(), &HashSet::new());
        let revealed = derive_folds(state.rows(), &HashSet::from([1]));
        // The middle fold's four rows come back inline; the outer folds
        // stay shut, and nothing else moves.
        assert_eq!(revealed.len(), folded.len() + 4 - 1);
        let folds = |display: &[DisplayRow]| {
            display
                .iter()
                .filter(|row| matches!(row, DisplayRow::Fold { .. }))
                .count()
        };
        assert_eq!(folds(&folded), 3);
        assert_eq!(folds(&revealed), 2);
    }

    #[gpui::test]
    fn picks_emit_block_and_completion_events(cx: &mut TestAppContext) {
        let state = cx.new(|_| ConflictResolvState::parse("a.txt", TEXT).unwrap());
        let events = Rc::new(RefCell::new(Vec::new()));
        let log = events.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&state, move |_, event: &ConflictResolvEvent, _| {
                log.borrow_mut().push(event.clone());
            })
        });
        state.update(cx, |state, cx| state.take_yours(0, cx));
        state.update(cx, |state, cx| state.take_upstream(1, cx));
        // Undoing reopens the file, so finishing again completes once more.
        state.update(cx, |state, cx| state.undo_block(1, cx));
        // A second undo on an already unresolved block stays silent.
        state.update(cx, |state, cx| state.undo_block(1, cx));
        state.update(cx, |state, cx| state.take_both(1, cx));
        assert_eq!(
            *events.borrow(),
            [
                ConflictResolvEvent::BlockResolved(0),
                ConflictResolvEvent::BlockResolved(1),
                ConflictResolvEvent::AllBlocksResolved,
                ConflictResolvEvent::BlockReopened(1),
                ConflictResolvEvent::BlockResolved(1),
                ConflictResolvEvent::AllBlocksResolved,
            ]
        );
        state.update(cx, |state, _| {
            assert_eq!(state.result_text(), "a\nl\nb\nl2\nr2\nc\n");
            assert!(state.all_resolved());
        });
    }

    #[gpui::test]
    fn stale_block_indices_are_ignored(cx: &mut TestAppContext) {
        let state = cx.new(|_| {
            ConflictResolvState::parse("a.txt", "x\n<<<<<<< a\n1\n=======\n2\n>>>>>>> b\n").unwrap()
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let log = events.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&state, move |_, event: &ConflictResolvEvent, _| {
                log.borrow_mut().push(event.clone());
            })
        });
        state.update(cx, |state, cx| {
            state.take_yours(7, cx);
            state.take_both(7, cx);
            state.undo_block(7, cx);
            state.mark_hand_edited(7, cx);
            state.edit_block(7, "typed", cx);
            assert_eq!(state.resolution(7), BlockResolution::Unresolved);
            assert_eq!(state.yours_marker(7), SharedString::default());
            assert_eq!(state.upstream_marker(7), SharedString::default());
            assert_eq!(state.result_text(), "x\n");
            assert!(!state.all_resolved());
        });
        assert!(events.borrow().is_empty());
    }

    #[gpui::test]
    fn edits_flip_to_hand_edited_once_then_only_report_changes(cx: &mut TestAppContext) {
        let state = cx.new(|_| ConflictResolvState::parse("a.txt", TEXT).unwrap());
        let events = Rc::new(RefCell::new(Vec::new()));
        let log = events.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&state, move |_, event: &ConflictResolvEvent, _| {
                log.borrow_mut().push(event.clone());
            })
        });
        state.update(cx, |state, cx| state.take_yours(0, cx));
        state.update(cx, |state, cx| state.edit_block(0, "changed", cx));
        // Feeding the same text back changes nothing and stays silent.
        state.update(cx, |state, cx| state.edit_block(0, "changed", cx));
        state.update(cx, |state, cx| state.edit_block(0, "changed\nmore", cx));
        // An unresolved block is read-only: pick first.
        state.update(cx, |state, cx| state.edit_block(1, "nope", cx));
        assert_eq!(
            *events.borrow(),
            [
                ConflictResolvEvent::BlockResolved(0),
                ConflictResolvEvent::Edited,
                ConflictResolvEvent::BlockResolved(0),
                ConflictResolvEvent::Edited,
            ]
        );
        state.update(cx, |state, _| {
            assert_eq!(state.resolution(0), BlockResolution::HandEdited);
            assert_eq!(state.resolution(1), BlockResolution::Unresolved);
            assert_eq!(state.result_text(), "a\nchanged\nmore\nb\nc\n");
            assert!(!state.all_resolved());
        });
    }

    #[gpui::test]
    fn undo_after_an_edit_restores_base_and_read_only(cx: &mut TestAppContext) {
        let state = cx.new(|_| ConflictResolvState::parse("a.txt", TEXT).unwrap());
        state.update(cx, |state, cx| {
            state.take_upstream(0, cx);
            state.edit_block(0, "hand\nwork", cx);
            state.undo_block(0, cx);
            assert_eq!(state.resolution(0), BlockResolution::Unresolved);
            assert_eq!(state.result_text(), "a\nb\nc\n");
            // Read-only again: the edit is refused until the next pick.
            state.edit_block(0, "still typing", cx);
            assert_eq!(state.result_text(), "a\nb\nc\n");
        });
    }

    #[gpui::test]
    fn an_emptied_editor_keeps_the_blank_line(cx: &mut TestAppContext) {
        let text = "keep\n<<<<<<< yours\n\n=======\nx\n>>>>>>> upstream\n";
        let state = cx.new(|_| ConflictResolvState::parse("a.txt", text).unwrap());
        state.update(cx, |state, cx| {
            state.take_yours(0, cx);
            // Typing, then deleting everything, delivers an empty value;
            // the picked blank line must survive the round trip.
            state.edit_block(0, "typed", cx);
            state.edit_block(0, "", cx);
            assert_eq!(state.result_text(), "keep\n\n");
            assert_eq!(state.middle_line_count(0), 1);
        });
    }

    #[gpui::test]
    fn editors_mount_seed_sync_and_drop_in_a_window(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler as _;

        cx.update(crate::init);
        let mut slot = None;
        let (_probe, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(|_| ConflictResolvState::parse("a.txt", TEXT).unwrap());
            state.update(cx, |state, cx| {
                // Unresolved blocks mount no editor: pick first.
                state.sync_editors(window, cx);
                assert!(state.editor(0).is_none() && state.editor(1).is_none());
                state.take_yours(0, cx);
                state.sync_editors(window, cx);
                assert!(state.editor(1).is_none());
            });
            slot = Some(state.clone());
            Probe(state)
        });
        let state = slot.unwrap();
        let editor = state.read_with(cx, |state, _| state.editor(0).unwrap().clone());
        // The editor seeds from the picked text, without the final newline.
        editor.read_with(cx, |editor, _| assert_eq!(editor.value(), "l"));
        // A genuine editor change syncs into the model and flips the block.
        gpui::VisualTestContext::update(cx, |window, cx| {
            editor.update(cx, |editor, cx| {
                editor.replace_text_in_range(Some(0..0), "typed ", window, cx);
            });
        });
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_eq!(state.resolution(0), BlockResolution::HandEdited);
            assert_eq!(state.result_text(), "a\ntyped l\nb\nc\n");
        });
        // A re-pick reseeds the mounted editor with the picked text.
        gpui::VisualTestContext::update(cx, |window, cx| {
            state.update(cx, |state, cx| {
                state.take_upstream(0, cx);
                state.sync_editors(window, cx);
            });
        });
        cx.run_until_parked();
        editor.read_with(cx, |editor, _| assert_eq!(editor.value(), "r"));
        state.read_with(cx, |state, _| {
            assert_eq!(state.resolution(0), BlockResolution::TakenUpstream);
        });
        // Undo drops the editor; the middle is read-only base lines again.
        gpui::VisualTestContext::update(cx, |window, cx| {
            state.update(cx, |state, cx| {
                state.undo_block(0, cx);
                state.sync_editors(window, cx);
            });
        });
        state.read_with(cx, |state, _| assert!(state.editor(0).is_none()));
    }

    struct Probe(
        #[expect(dead_code, reason = "held so the entity outlives the window")]
        Entity<ConflictResolvState>,
    );

    impl gpui::Render for Probe {
        fn render(
            &mut self,
            _: &mut Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::Empty
        }
    }

    #[gpui::test]
    fn mark_hand_edited_reports_the_edit_and_the_resolution(cx: &mut TestAppContext) {
        let state = cx.new(|_| {
            ConflictResolvState::parse("a.txt", "x\n<<<<<<< a\n1\n=======\n2\n>>>>>>> b\n").unwrap()
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let log = events.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&state, move |_, event: &ConflictResolvEvent, _| {
                log.borrow_mut().push(event.clone());
            })
        });
        state.update(cx, |state, cx| state.mark_hand_edited(0, cx));
        assert_eq!(
            *events.borrow(),
            [
                ConflictResolvEvent::Edited,
                ConflictResolvEvent::BlockResolved(0),
                ConflictResolvEvent::AllBlocksResolved,
            ]
        );
    }
}
