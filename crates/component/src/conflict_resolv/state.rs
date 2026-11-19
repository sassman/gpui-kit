//! Retained resolver state for one conflicted file, owned by the view that
//! renders [`super::ConflictResolv`].
use gpui::{Context, EventEmitter, SharedString};

use super::model::{BlockResolution, ConflictModel, ConflictRow, Pick};
use crate::diff::DiffParseError;

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
    /// Free typing changed the middle buffer; reserved for the embedded
    /// editor of a later iteration.
    Edited,
}

/// The three derived columns of one conflicted file and each block's
/// resolution, created with [`Self::parse`] inside the owner's entity.
///
/// The public vocabulary is yours and upstream, never Git's ours and theirs:
/// the application maps the operation's sides before parsing.
pub struct ConflictResolvState {
    model: ConflictModel,
}

impl EventEmitter<ConflictResolvEvent> for ConflictResolvState {}

impl ConflictResolvState {
    /// Parses a working file with Git conflict markers, `diff3` bases
    /// included. The middle starts on each block's base lines, empty for
    /// two-way markers; raw markers never appear in any pane.
    pub fn parse(path: impl Into<SharedString>, text: &str) -> Result<Self, DiffParseError> {
        Ok(Self {
            model: ConflictModel::parse(path, text)?,
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
    /// The middle buffer: the merge result once every block is resolved.
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
    /// Restores the block's base lines and the unresolved state.
    pub fn undo_block(&mut self, block: usize, cx: &mut Context<Self>) {
        if block >= self.model.block_count()
            || self.model.resolution(block) == BlockResolution::Unresolved
        {
            return;
        }
        self.model.undo(block);
        cx.emit(ConflictResolvEvent::BlockReopened(block));
        cx.notify();
    }
    /// Marks a block the result editor changed by hand. Crate-private until
    /// the embedded editor lands: until then no edit precedes the call, so
    /// it would resolve an untouched block and silently drop both sides
    /// from the result.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn mark_hand_edited(&mut self, block: usize, cx: &mut Context<Self>) {
        if block >= self.model.block_count() {
            return;
        }
        let was_all = self.model.all_resolved();
        self.model.mark_hand_edited(block);
        cx.emit(ConflictResolvEvent::Edited);
        self.finish(block, was_all, cx);
    }

    fn apply(&mut self, block: usize, pick: Pick, cx: &mut Context<Self>) {
        if block >= self.model.block_count() {
            return;
        }
        let was_all = self.model.all_resolved();
        self.model.pick(block, pick);
        self.finish(block, was_all, cx);
    }

    fn finish(&mut self, block: usize, was_all: bool, cx: &mut Context<Self>) {
        cx.emit(ConflictResolvEvent::BlockResolved(block));
        if !was_all && self.model.all_resolved() {
            cx.emit(ConflictResolvEvent::AllBlocksResolved);
        }
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

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    const TEXT: &str = "a\n<<<<<<< yours\nl\n=======\nr\n>>>>>>> upstream\nb\n<<<<<<< yours\nl2\n=======\nr2\n>>>>>>> upstream\nc\n";

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
            assert_eq!(state.resolution(7), BlockResolution::Unresolved);
            assert_eq!(state.yours_marker(7), SharedString::default());
            assert_eq!(state.upstream_marker(7), SharedString::default());
            assert_eq!(state.result_text(), "x\n");
            assert!(!state.all_resolved());
        });
        assert!(events.borrow().is_empty());
    }

    #[gpui::test]
    fn hand_edits_emit_the_placeholder_event(cx: &mut TestAppContext) {
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
