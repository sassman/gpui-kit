//! Pure three-column model behind [`super::ConflictResolv`], derived from one
//! marker-laden working file. No GPUI dependency beyond strings, so every
//! transition is testable without a window.
use std::ops::Range;

use gpui::SharedString;

use crate::diff::{DiffFile, DiffParseError};

/// How one conflict block of the middle result stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BlockResolution {
    /// The middle still shows the block's base lines.
    Unresolved,
    /// The middle shows the left pane's lines.
    TakenYours,
    /// The middle shows the right pane's lines.
    TakenUpstream,
    /// The middle shows yours followed by upstream.
    Both,
    /// Free typing changed the block; reserved for the embedded editor.
    HandEdited,
}

/// A pick the user makes on a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    Yours,
    Upstream,
    Both,
}

/// What a visual row shows in one pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConflictCell {
    /// An index into that pane's column of lines.
    Line(usize),
    /// Padding that keeps a block's rows aligned across the panes.
    Spacer,
}

/// Which part of the file a visual row belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConflictRowKind {
    /// Shared lines outside every conflict; the three cells align 1:1.
    Context,
    /// A conflict block, by the index [`super::ConflictResolvState`] queries accept.
    Block(usize),
}

/// One visual row across the three panes: yours, result, upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConflictRow {
    pub kind: ConflictRowKind,
    pub left: ConflictCell,
    pub middle: ConflictCell,
    pub right: ConflictCell,
}

/// One conflict region. Column ranges on the sides are fixed; the middle
/// range is derived by `refresh` from the segments before the block.
struct Block {
    yours: Range<usize>,
    upstream: Range<usize>,
    middle: Range<usize>,
    /// The block's current slice of the result: base lines at first, a
    /// pick's lines after, free text once the embedded editor ran. Every
    /// line newline-terminated, or empty.
    middle_text: String,
    /// Raw side texts kept for picking, every line newline-terminated
    /// because a marker line always follows a side's last line.
    yours_text: String,
    upstream_text: String,
    base_text: String,
    yours_label: SharedString,
    upstream_label: SharedString,
    resolution: BlockResolution,
}

/// One span of the middle column, in file order.
enum MiddleSegment {
    /// Shared lines outside every conflict, raw and newline-terminated.
    /// Strictly read-only.
    Context(String),
    /// The block whose `middle_text` fills this span; editable through
    /// [`ConflictModel::edit`] once the block is resolved.
    Block(usize),
}

/// Three derived columns, the conflict blocks, and the visual row table.
///
/// The middle column is a sequence of segments so each block owns its slice
/// of the result: an embedded editor can replace one block's text without
/// splicing a shared buffer, and undo is a plain reset to the base lines.
/// The cached lines only serve display.
pub(crate) struct ConflictModel {
    path: SharedString,
    yours: Vec<SharedString>,
    upstream: Vec<SharedString>,
    segments: Vec<MiddleSegment>,
    middle_lines: Vec<SharedString>,
    blocks: Vec<Block>,
    rows: Vec<ConflictRow>,
}

/// A line for display, without its line ending.
fn display(line: &str) -> SharedString {
    line.trim_end_matches(['\n', '\r']).to_owned().into()
}

fn display_lines(text: &str) -> Vec<SharedString> {
    text.split_inclusive('\n').map(display).collect()
}

fn line_count(text: &str) -> usize {
    text.split_inclusive('\n').count()
}

impl ConflictModel {
    /// Builds the model from a working file with Git conflict markers,
    /// including `diff3` base sections. Markers reach no column: the sides
    /// keep their part's lines, the middle starts on the base lines, empty
    /// for two-way markers.
    pub(crate) fn parse(path: impl Into<SharedString>, text: &str) -> Result<Self, DiffParseError> {
        let path = path.into();
        let file = DiffFile::parse_conflicts(path.clone(), text)?;
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        // Working-file line ranges are one-based with an exclusive end.
        let slice = |range: Range<usize>| lines[range.start - 1..range.end - 1].concat();
        let mut model = Self {
            path,
            yours: Vec::new(),
            upstream: Vec::new(),
            segments: Vec::new(),
            middle_lines: Vec::new(),
            blocks: Vec::new(),
            rows: Vec::new(),
        };
        let mut cursor = 1;
        for conflict in file.conflicts() {
            model.push_context(&lines[cursor - 1..conflict.lines().start - 1]);
            cursor = conflict.lines().end;
            let yours_text = slice(conflict.current_lines());
            let upstream_text = slice(conflict.incoming_lines());
            let base_text = conflict.base_lines().map(slice).unwrap_or_default();
            let yours = model.yours.len()..model.yours.len() + line_count(&yours_text);
            model.yours.extend(display_lines(&yours_text));
            let upstream = model.upstream.len()..model.upstream.len() + line_count(&upstream_text);
            model.upstream.extend(display_lines(&upstream_text));
            model
                .segments
                .push(MiddleSegment::Block(model.blocks.len()));
            model.blocks.push(Block {
                yours,
                upstream,
                // Derived by refresh below, like after every later change.
                middle: 0..0,
                middle_text: base_text.clone(),
                yours_text,
                upstream_text,
                base_text,
                yours_label: conflict.current_label().clone(),
                upstream_label: conflict.incoming_label().clone(),
                resolution: BlockResolution::Unresolved,
            });
        }
        model.push_context(&lines[cursor - 1..]);
        model.refresh();
        Ok(model)
    }

    /// Shared lines outside every conflict reach all three columns.
    fn push_context(&mut self, lines: &[&str]) {
        if lines.is_empty() {
            return;
        }
        for line in lines {
            self.yours.push(display(line));
            self.upstream.push(display(line));
        }
        self.segments.push(MiddleSegment::Context(lines.concat()));
    }

    pub(crate) fn path(&self) -> &SharedString {
        &self.path
    }
    pub(crate) fn rows(&self) -> &[ConflictRow] {
        &self.rows
    }
    pub(crate) fn yours_lines(&self) -> &[SharedString] {
        &self.yours
    }
    pub(crate) fn middle_lines(&self) -> &[SharedString] {
        &self.middle_lines
    }
    pub(crate) fn upstream_lines(&self) -> &[SharedString] {
        &self.upstream
    }
    pub(crate) fn block_count(&self) -> usize {
        self.blocks.len()
    }
    pub(crate) fn resolution(&self, ix: usize) -> BlockResolution {
        self.blocks[ix].resolution
    }
    pub(crate) fn all_resolved(&self) -> bool {
        self.blocks
            .iter()
            .all(|block| block.resolution != BlockResolution::Unresolved)
    }
    /// The middle column's text: the merge result once every block is
    /// resolved, reflecting every pick and hand edit as it stands.
    pub(crate) fn result_text(&self) -> String {
        self.segments
            .iter()
            .map(|segment| match segment {
                MiddleSegment::Context(text) => text.as_str(),
                MiddleSegment::Block(ix) => self.blocks[*ix].middle_text.as_str(),
            })
            .collect()
    }
    pub(crate) fn yours_label(&self, ix: usize) -> &SharedString {
        &self.blocks[ix].yours_label
    }
    pub(crate) fn upstream_label(&self, ix: usize) -> &SharedString {
        &self.blocks[ix].upstream_label
    }

    pub(crate) fn pick(&mut self, ix: usize, pick: Pick) {
        let block = &mut self.blocks[ix];
        let (text, resolution) = match pick {
            Pick::Yours => (block.yours_text.clone(), BlockResolution::TakenYours),
            Pick::Upstream => (block.upstream_text.clone(), BlockResolution::TakenUpstream),
            Pick::Both => (
                format!("{}{}", block.yours_text, block.upstream_text),
                BlockResolution::Both,
            ),
        };
        block.middle_text = text;
        block.resolution = resolution;
        self.refresh();
    }

    /// Restores the block's base lines and the unresolved state.
    pub(crate) fn undo(&mut self, ix: usize) {
        let block = &mut self.blocks[ix];
        block.middle_text = block.base_text.clone();
        block.resolution = BlockResolution::Unresolved;
        self.refresh();
    }

    /// Marks a block the embedded editor changed; the text already moved
    /// through [`Self::edit`], so only the state follows.
    pub(crate) fn mark_hand_edited(&mut self, ix: usize) {
        self.blocks[ix].resolution = BlockResolution::HandEdited;
    }

    /// Applies the embedded editor's value to a block and reports whether
    /// the text changed. The editor shows lines without the final newline
    /// the stored texts carry, so the value gains one back; an unresolved
    /// block is read-only until a pick and refuses the edit.
    pub(crate) fn edit(&mut self, ix: usize, value: &str) -> bool {
        if self.blocks[ix].resolution == BlockResolution::Unresolved {
            return false;
        }
        let text = if value.is_empty() {
            String::new()
        } else {
            format!("{value}\n")
        };
        if self.blocks[ix].middle_text == text {
            return false;
        }
        self.blocks[ix].middle_text = text;
        self.refresh();
        true
    }

    /// The block's result text as the embedded editor shows it: without
    /// the final newline, so Enter at the end grows the block by a real
    /// line. [`Self::edit`] takes the same shape back.
    pub(crate) fn block_editor_text(&self, ix: usize) -> String {
        let text = &self.blocks[ix].middle_text;
        text.strip_suffix('\n').unwrap_or(text).to_owned()
    }

    /// Reassembles the middle lines from the segments, re-derives each
    /// block's middle range, and rebuilds the row table.
    fn refresh(&mut self) {
        self.middle_lines.clear();
        for i in 0..self.segments.len() {
            match &self.segments[i] {
                MiddleSegment::Context(text) => self.middle_lines.extend(display_lines(text)),
                &MiddleSegment::Block(ix) => {
                    let start = self.middle_lines.len();
                    self.middle_lines
                        .extend(display_lines(&self.blocks[ix].middle_text));
                    self.blocks[ix].middle = start..self.middle_lines.len();
                }
            }
        }
        self.recompute_rows();
    }

    /// Rebuilds the visual rows: context aligns 1:1 across the panes, each
    /// block pads its shorter sides with spacers to its tallest column.
    fn recompute_rows(&mut self) {
        fn context(rows: &mut Vec<ConflictRow>, cursors: &mut [usize; 3], count: usize) {
            let [left, middle, right] = *cursors;
            for k in 0..count {
                rows.push(ConflictRow {
                    kind: ConflictRowKind::Context,
                    left: ConflictCell::Line(left + k),
                    middle: ConflictCell::Line(middle + k),
                    right: ConflictCell::Line(right + k),
                });
            }
            for cursor in cursors {
                *cursor += count;
            }
        }
        self.rows.clear();
        let mut cursors = [0, 0, 0];
        for (ix, block) in self.blocks.iter().enumerate() {
            let run = block.yours.start - cursors[0];
            context(&mut self.rows, &mut cursors, run);
            debug_assert!(cursors[1] == block.middle.start && cursors[2] == block.upstream.start);
            let cell = |range: &Range<usize>, k: usize| {
                if k < range.len() {
                    ConflictCell::Line(range.start + k)
                } else {
                    ConflictCell::Spacer
                }
            };
            // An all-empty block would otherwise span zero rows, hiding
            // the gutter controls that resolve it; keep one spacer row.
            let height = block
                .yours
                .len()
                .max(block.middle.len())
                .max(block.upstream.len())
                .max(1);
            for k in 0..height {
                self.rows.push(ConflictRow {
                    kind: ConflictRowKind::Block(ix),
                    left: cell(&block.yours, k),
                    middle: cell(&block.middle, k),
                    right: cell(&block.upstream, k),
                });
            }
            cursors = [block.yours.end, block.middle.end, block.upstream.end];
        }
        let tail = self.yours.len() - cursors[0];
        context(&mut self.rows, &mut cursors, tail);
        debug_assert!(cursors[1] == self.middle_lines.len() && cursors[2] == self.upstream.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_WAY: &str =
        "fn retry() {\n<<<<<<< yours\n    3\n=======\n    5\n>>>>>>> upstream\n}\n";
    const DIFF3: &str = "head\n<<<<<<< yours\nleft one\nleft two\n||||||| base\nold\n=======\nright\n>>>>>>> upstream\ntail\n";
    const TWO_CONFLICTS: &str = "a\n<<<<<<< yours\nl1\nl2\nl3\n=======\nr1\n>>>>>>> upstream\nb\n<<<<<<< yours\nl4\n||||||| base\nbase4\n=======\nr4\nr5\n>>>>>>> upstream\nc\n";

    fn column(
        model: &ConflictModel,
        pick: fn(&ConflictRow) -> ConflictCell,
        side: u8,
    ) -> Vec<String> {
        model
            .rows()
            .iter()
            .map(|row| match pick(row) {
                ConflictCell::Spacer => "·".to_owned(),
                ConflictCell::Line(ix) => match side {
                    0 => model.yours_lines()[ix].to_string(),
                    1 => model.middle_lines()[ix].to_string(),
                    _ => model.upstream_lines()[ix].to_string(),
                },
            })
            .collect()
    }

    fn left(model: &ConflictModel) -> Vec<String> {
        column(model, |row| row.left, 0)
    }
    fn middle(model: &ConflictModel) -> Vec<String> {
        column(model, |row| row.middle, 1)
    }
    fn right(model: &ConflictModel) -> Vec<String> {
        column(model, |row| row.right, 2)
    }

    #[test]
    fn two_way_columns_hold_sides_and_empty_base() {
        let model = ConflictModel::parse("retry.rs", TWO_WAY).unwrap();
        assert_eq!(model.block_count(), 1);
        assert_eq!(left(&model), ["fn retry() {", "    3", "}"]);
        assert_eq!(right(&model), ["fn retry() {", "    5", "}"]);
        // No base lines in a two-way conflict: the middle shows a spacer.
        assert_eq!(middle(&model), ["fn retry() {", "·", "}"]);
        assert_eq!(model.result_text(), "fn retry() {\n}\n");
        assert_eq!(model.yours_label(0).as_str(), "yours");
        assert_eq!(model.upstream_label(0).as_str(), "upstream");
    }

    #[test]
    fn diff3_base_fills_the_middle_initially() {
        let model = ConflictModel::parse("a.txt", DIFF3).unwrap();
        assert_eq!(left(&model), ["head", "left one", "left two", "tail"]);
        assert_eq!(middle(&model), ["head", "old", "·", "tail"]);
        assert_eq!(right(&model), ["head", "right", "·", "tail"]);
        assert_eq!(model.result_text(), "head\nold\ntail\n");
    }

    #[test]
    fn markers_reach_no_column() {
        for text in [TWO_WAY, DIFF3, TWO_CONFLICTS] {
            let model = ConflictModel::parse("a.txt", text).unwrap();
            for column in [left(&model), middle(&model), right(&model)] {
                for line in column {
                    for marker in ["<<<<<<<", "|||||||", "=======", ">>>>>>>"] {
                        assert!(!line.contains(marker), "marker leaked: {line}");
                    }
                }
            }
            assert!(!model.result_text().contains("<<<<<<<"));
        }
    }

    #[test]
    fn row_table_tags_and_aligns_blocks() {
        let model = ConflictModel::parse("a.txt", TWO_CONFLICTS).unwrap();
        let kinds: Vec<ConflictRowKind> = model.rows().iter().map(|row| row.kind).collect();
        use ConflictRowKind::*;
        assert_eq!(
            kinds,
            [
                Context,
                Block(0),
                Block(0),
                Block(0),
                Context,
                Block(1),
                Block(1),
                Context
            ]
        );
        // First block: three left lines against one right line and no base.
        assert_eq!(left(&model), ["a", "l1", "l2", "l3", "b", "l4", "·", "c"]);
        assert_eq!(middle(&model), ["a", "·", "·", "·", "b", "base4", "·", "c"]);
        assert_eq!(right(&model), ["a", "r1", "·", "·", "b", "r4", "r5", "c"]);
    }

    #[test]
    fn picks_splice_and_shift_later_blocks() {
        let mut model = ConflictModel::parse("a.txt", TWO_CONFLICTS).unwrap();
        model.pick(0, Pick::Yours);
        assert_eq!(model.resolution(0), BlockResolution::TakenYours);
        assert_eq!(model.result_text(), "a\nl1\nl2\nl3\nb\nbase4\nc\n");
        // The later block still splices in place after the first one grew.
        model.pick(1, Pick::Upstream);
        assert_eq!(model.resolution(1), BlockResolution::TakenUpstream);
        assert_eq!(model.result_text(), "a\nl1\nl2\nl3\nb\nr4\nr5\nc\n");
        assert!(model.all_resolved());
        // Rows follow the splice: the first block now fills its middle cells.
        assert_eq!(
            middle(&model),
            ["a", "l1", "l2", "l3", "b", "r4", "r5", "c"]
        );
    }

    #[test]
    fn both_keeps_yours_before_upstream() {
        let mut model = ConflictModel::parse("retry.rs", TWO_WAY).unwrap();
        model.pick(0, Pick::Both);
        assert_eq!(model.resolution(0), BlockResolution::Both);
        assert_eq!(model.result_text(), "fn retry() {\n    3\n    5\n}\n");
    }

    #[test]
    fn undo_restores_base_lines_and_unresolved() {
        let mut model = ConflictModel::parse("a.txt", TWO_CONFLICTS).unwrap();
        model.pick(1, Pick::Both);
        model.pick(0, Pick::Upstream);
        assert!(model.all_resolved());
        model.undo(1);
        assert_eq!(model.resolution(1), BlockResolution::Unresolved);
        assert!(!model.all_resolved());
        assert_eq!(model.result_text(), "a\nr1\nb\nbase4\nc\n");
        // A re-pick after undo works against the restored range.
        model.pick(1, Pick::Yours);
        assert_eq!(model.result_text(), "a\nr1\nb\nl4\nc\n");
    }

    #[test]
    fn deletion_versus_change_keeps_an_empty_side() {
        let text = "keep\n<<<<<<< yours\n=======\nchanged\n>>>>>>> upstream\nend\n";
        let mut model = ConflictModel::parse("a.txt", text).unwrap();
        assert_eq!(left(&model), ["keep", "·", "end"]);
        assert_eq!(right(&model), ["keep", "changed", "end"]);
        model.pick(0, Pick::Yours);
        assert_eq!(model.result_text(), "keep\nend\n");
        model.undo(0);
        model.pick(0, Pick::Upstream);
        assert_eq!(model.result_text(), "keep\nchanged\nend\n");
    }

    #[test]
    fn hand_edit_counts_as_resolved_until_undone() {
        let mut model = ConflictModel::parse("retry.rs", TWO_WAY).unwrap();
        model.mark_hand_edited(0);
        assert_eq!(model.resolution(0), BlockResolution::HandEdited);
        assert!(model.all_resolved());
        model.undo(0);
        assert_eq!(model.resolution(0), BlockResolution::Unresolved);
    }

    #[test]
    fn an_all_empty_block_still_gets_a_row() {
        let text = "<<<<<<< ours\n||||||| base\n=======\n>>>>>>> theirs\n";
        let mut model = ConflictModel::parse("a.txt", text).unwrap();
        assert_eq!(model.block_count(), 1);
        // The spacer row keeps the gutter controls visible.
        assert_eq!(model.rows().len(), 1);
        assert_eq!(model.rows()[0].kind, ConflictRowKind::Block(0));
        assert_eq!(left(&model), ["·"]);
        assert_eq!(middle(&model), ["·"]);
        assert_eq!(right(&model), ["·"]);
        model.pick(0, Pick::Yours);
        assert_eq!(model.resolution(0), BlockResolution::TakenYours);
        assert!(model.all_resolved());
        assert_eq!(model.result_text(), "");
        // Resolving keeps the row, so undo stays reachable too.
        assert_eq!(model.rows().len(), 1);
        model.undo(0);
        assert_eq!(model.resolution(0), BlockResolution::Unresolved);
    }

    #[test]
    fn edits_grow_and_shrink_the_block_rows() {
        let mut model = ConflictModel::parse("a.txt", TWO_CONFLICTS).unwrap();
        model.pick(0, Pick::Upstream);
        // One middle line against three on the left: the block holds three rows.
        assert_eq!(model.rows().len(), 8);
        // Growing past both sides stretches the block; left and right pad out.
        assert!(model.edit(0, "r1\nr1b\nr1c\nr1d\nr1e"));
        assert_eq!(
            model.result_text(),
            "a\nr1\nr1b\nr1c\nr1d\nr1e\nb\nbase4\nc\n"
        );
        assert_eq!(
            middle(&model),
            [
                "a", "r1", "r1b", "r1c", "r1d", "r1e", "b", "base4", "·", "c"
            ]
        );
        assert_eq!(
            left(&model),
            ["a", "l1", "l2", "l3", "·", "·", "b", "l4", "·", "c"]
        );
        assert_eq!(
            right(&model),
            ["a", "r1", "·", "·", "·", "·", "b", "r4", "r5", "c"]
        );
        // Shrinking narrows the middle back below the left side's height.
        assert!(model.edit(0, "only"));
        assert_eq!(model.result_text(), "a\nonly\nb\nbase4\nc\n");
        assert_eq!(
            middle(&model),
            ["a", "only", "·", "·", "b", "base4", "·", "c"]
        );
        // The later block's queries still address the shifted range.
        model.pick(1, Pick::Yours);
        assert_eq!(model.result_text(), "a\nonly\nb\nl4\nc\n");
    }

    #[test]
    fn enter_above_and_below_grow_the_region() {
        let mut model = ConflictModel::parse("retry.rs", TWO_WAY).unwrap();
        model.pick(0, Pick::Yours);
        assert_eq!(model.rows().len(), 3);
        // Enter at the end of the editor region: the value keeps a trailing
        // newline, so the block gains an empty last line.
        assert!(model.edit(0, "    3\n"));
        assert_eq!(model.result_text(), "fn retry() {\n    3\n\n}\n");
        assert_eq!(middle(&model), ["fn retry() {", "    3", "", "}"]);
        assert_eq!(left(&model), ["fn retry() {", "    3", "·", "}"]);
        // Enter at the start: an empty first line inside the block.
        assert!(model.edit(0, "\n    3"));
        assert_eq!(model.result_text(), "fn retry() {\n\n    3\n}\n");
        assert_eq!(middle(&model), ["fn retry() {", "", "    3", "}"]);
    }

    #[test]
    fn editor_text_round_trips_and_equal_edits_are_refused() {
        let mut model = ConflictModel::parse("a.txt", TWO_CONFLICTS).unwrap();
        model.pick(0, Pick::Yours);
        let value = model.block_editor_text(0);
        assert_eq!(value, "l1\nl2\nl3");
        // Feeding the editor's own text back is no change.
        assert!(!model.edit(0, &value));
        // Emptying the block keeps its row for the gutter controls.
        assert!(model.edit(0, ""));
        assert_eq!(model.block_editor_text(0), "");
        assert_eq!(model.result_text(), "a\nb\nbase4\nc\n");
        assert_eq!(middle(&model), ["a", "·", "·", "·", "b", "base4", "·", "c"]);
    }

    #[test]
    fn unresolved_blocks_refuse_edits() {
        let mut model = ConflictModel::parse("retry.rs", TWO_WAY).unwrap();
        assert!(!model.edit(0, "typed"));
        assert_eq!(model.result_text(), "fn retry() {\n}\n");
        assert_eq!(model.resolution(0), BlockResolution::Unresolved);
    }

    #[test]
    fn undo_after_a_hand_edit_restores_base_and_height() {
        let mut model = ConflictModel::parse("a.txt", DIFF3).unwrap();
        let rows_before = model.rows().len();
        model.pick(0, Pick::Both);
        assert!(model.edit(0, "left one\nleft two\nright\nextra"));
        model.mark_hand_edited(0);
        assert_eq!(model.resolution(0), BlockResolution::HandEdited);
        assert_eq!(model.rows().len(), rows_before + 2);
        model.undo(0);
        assert_eq!(model.resolution(0), BlockResolution::Unresolved);
        assert_eq!(model.result_text(), "head\nold\ntail\n");
        assert_eq!(model.rows().len(), rows_before);
        assert_eq!(middle(&model), ["head", "old", "·", "tail"]);
    }

    #[test]
    fn a_file_without_conflicts_is_resolved_as_is() {
        let model = ConflictModel::parse("a.txt", "one\ntwo\n").unwrap();
        assert_eq!(model.block_count(), 0);
        assert!(model.all_resolved());
        assert_eq!(model.result_text(), "one\ntwo\n");
        assert_eq!(left(&model), ["one", "two"]);
    }

    #[test]
    fn parse_errors_surface_from_the_marker_parser() {
        assert!(ConflictModel::parse("a.txt", "<<<<<<< yours\nx\n").is_err());
    }
}
