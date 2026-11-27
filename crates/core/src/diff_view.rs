//! How the review draws a hunk: the layout options a reader saves per repository, whitespace-only changes folded into
//! unchanged lines, split rows, and the words that changed inside a line. Pure: it reads parsed lines and draws nothing.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::diff::{DiffLine, Hunk, LineKind};

/// Split view needs a window at least this wide (px): two code columns and the side panes must all fit.
pub const MIN_SPLIT_WINDOW: f32 = 1280.0;

/// Whether a window `width` px wide has room for split view.
pub fn split_allowed(width: f32) -> bool {
    width >= MIN_SPLIT_WINDOW
}

/// The diff options of `review.set_layout`, saved per repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutOptions {
    /// Two halves, old on the left and new on the right, instead of one column.
    pub split: bool,
    /// A removed and an added line that differ only in whitespace count as unchanged.
    pub hide_whitespace: bool,
    /// Mark the words that changed inside a removed and an added line.
    pub word_highlight: bool,
    /// Long lines wrap instead of scrolling sideways.
    pub wrap: bool,
}

/// A line to draw, with its place in [`Hunk::lines`]: selections and comments name lines by that index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub index: usize,
    pub line: DiffLine,
}

/// The lines of a hunk as drawn, and how many pairs of lines whitespace-only changes folded away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visible {
    pub lines: Vec<Shown>,
    pub hidden: u32,
}

/// The text with every run of whitespace as one space and none at the ends: two lines that differ only in how much space
/// separates their words are the same line, but `else if` and `elseif` are not.
fn squeezed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The lines of `hunk` to draw. With `hide_whitespace`, a removed line and an added line of one change block that differ
/// only in whitespace become one unchanged line (the new text, both line numbers), counted in [`Visible::hidden`]. The
/// `pinned` line (its place in the hunk: the one an open comment box sits under) is never folded.
pub fn visible(hunk: &Hunk, hide_whitespace: bool, pinned: Option<usize>) -> Visible {
    let lines = &hunk.lines;
    let mut out = Vec::with_capacity(lines.len());
    let mut hidden = 0;
    let mut at = 0;
    while let Some(line) = lines.get(at) {
        if !hide_whitespace || line.kind == LineKind::Context {
            out.push(Shown { index: at, line: line.clone() });
            at += 1;
            continue;
        }
        let removed = run_of(lines, at, LineKind::Removed);
        let added = run_of(lines, removed.end, LineKind::Added);
        at = added.end.max(removed.end).max(at + 1);
        hidden += fold_group(lines, removed, added, pinned, &mut out);
    }
    Visible { lines: out, hidden }
}

/// The lines from `from` on that all have `kind`.
fn run_of(lines: &[DiffLine], from: usize, kind: LineKind) -> Range<usize> {
    let len = lines.iter().skip(from).take_while(|l| l.kind == kind).count();
    from..from + len
}

/// Draws one removed run and the added run after it, folding the pairs that differ only in whitespace. Returns how many.
// shortcut: pairs are matched greedily in order, not by longest common subsequence; a run where an early line has a
// whitespace twin only far below hides fewer pairs than it could.
fn fold_group(lines: &[DiffLine], removed: Range<usize>, added: Range<usize>, pinned: Option<usize>, out: &mut Vec<Shown>) -> u32 {
    let mut pairs = Vec::new();
    let mut next = added.start;
    for r in removed.clone() {
        let Some(old) = lines.get(r) else { break };
        if pinned == Some(r) {
            continue;
        }
        let key = squeezed(&old.text);
        if let Some(a) = (next..added.end).filter(|&a| pinned != Some(a)).find(|&a| lines.get(a).is_some_and(|new| squeezed(&new.text) == key)) {
            pairs.push((r, a));
            next = a + 1;
        }
    }
    let plain =
        |range: Range<usize>, out: &mut Vec<Shown>| out.extend(range.filter_map(|i| lines.get(i).map(|l| Shown { index: i, line: l.clone() })));
    let (mut r_at, mut a_at) = (removed.start, added.start);
    for &(r, a) in &pairs {
        plain(r_at..r, out);
        plain(a_at..a, out);
        if let (Some(old), Some(new)) = (lines.get(r), lines.get(a)) {
            out.push(Shown { index: a, line: DiffLine { kind: LineKind::Context, old: old.old, new: new.new, text: new.text.clone() } });
        }
        (r_at, a_at) = (r + 1, a + 1);
    }
    plain(r_at..removed.end, out);
    plain(a_at..added.end, out);
    pairs.len() as u32
}

/// One row of split view: positions into the slice [`split_rows`] was given. An unchanged line is on both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitRow {
    pub left: Option<usize>,
    pub right: Option<usize>,
}

/// Rows for split view: unchanged lines on both sides; a removed run and the added run after it side by side, line by line,
/// the shorter side padded with empty cells.
pub fn split_rows(lines: &[DiffLine]) -> Vec<SplitRow> {
    let mut rows = Vec::with_capacity(lines.len());
    let mut at = 0;
    while let Some(line) = lines.get(at) {
        if line.kind == LineKind::Context {
            rows.push(SplitRow { left: Some(at), right: Some(at) });
            at += 1;
            continue;
        }
        let removed = run_of(lines, at, LineKind::Removed);
        let added = run_of(lines, removed.end, LineKind::Added);
        for k in 0..removed.len().max(added.len()) {
            rows.push(SplitRow { left: (k < removed.len()).then_some(removed.start + k), right: (k < added.len()).then_some(added.start + k) });
        }
        at = added.end.max(removed.end).max(at + 1);
    }
    rows
}

/// For each line, the byte ranges of its text that changed against the line it pairs with in [`split_rows`]. Empty for
/// unchanged lines, lines with no partner, and pairs too different for marks to help.
pub fn changed_words(lines: &[DiffLine]) -> Vec<Vec<Range<usize>>> {
    let mut marks = vec![Vec::new(); lines.len()];
    for row in split_rows(lines) {
        let (Some(l), Some(r)) = (row.left, row.right) else { continue };
        if l == r {
            continue;
        }
        if let (Some(old), Some(new)) = (lines.get(l), lines.get(r)) {
            let (a, b) = word_ranges(&old.text, &new.text);
            if let Some(slot) = marks.get_mut(l) {
                *slot = a;
            }
            if let Some(slot) = marks.get_mut(r) {
                *slot = b;
            }
        }
    }
    marks
}

/// The tokens of `text`: a run of word characters, a run of whitespace, or one other character.
fn tokens(text: &str) -> Vec<Range<usize>> {
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last = None;
    for (i, c) in text.char_indices() {
        let kind = class(c);
        match out.last_mut() {
            Some(prev) if last == Some(kind) && kind != 2 => prev.end = i + c.len_utf8(),
            _ => out.push(i..i + c.len_utf8()),
        }
        last = Some(kind);
    }
    out
}

/// The longest tokens-of-`a`-equal-to-tokens-of-`b` pairing, as the positions of the tokens it keeps in each.
fn common(a: &[&str], b: &[&str]) -> (Vec<bool>, Vec<bool>) {
    let width = b.len() + 1;
    let mut table = vec![0u16; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            let hit = a.get(i) == b.get(j);
            let best = if hit {
                table.get((i + 1) * width + j + 1).copied().unwrap_or(0).saturating_add(1)
            } else {
                let down = table.get((i + 1) * width + j).copied().unwrap_or(0);
                down.max(table.get(i * width + j + 1).copied().unwrap_or(0))
            };
            if let Some(cell) = table.get_mut(i * width + j) {
                *cell = best;
            }
        }
    }
    let (mut keep_a, mut keep_b) = (vec![false; a.len()], vec![false; b.len()]);
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let here = table.get(i * width + j).copied().unwrap_or(0);
        if a.get(i) == b.get(j) {
            keep_a[i] = true;
            keep_b[j] = true;
            (i, j) = (i + 1, j + 1);
        } else if table.get((i + 1) * width + j).copied().unwrap_or(0) == here {
            i += 1;
        } else {
            j += 1;
        }
    }
    (keep_a, keep_b)
}

/// The most token pairs compared: a longer line gets no marks rather than a slow draw.
const MAX_WORD_CELLS: usize = 40_000;

/// The byte ranges of `old` and of `new` that are not shared words. Both empty when almost nothing is shared (the lines
/// are a rewrite, and marking everything says nothing) or when a line is too long to compare.
pub fn word_ranges(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let (ta, tb) = (tokens(old), tokens(new));
    if ta.len().saturating_mul(tb.len()) > MAX_WORD_CELLS {
        return (Vec::new(), Vec::new());
    }
    let text = |t: &Range<usize>, whole: &str| whole.get(t.clone()).unwrap_or_default().to_string();
    let (sa, sb): (Vec<String>, Vec<String>) = (ta.iter().map(|t| text(t, old)).collect(), tb.iter().map(|t| text(t, new)).collect());
    let (ra, rb): (Vec<&str>, Vec<&str>) = (sa.iter().map(String::as_str).collect(), sb.iter().map(String::as_str).collect());
    let (keep_a, keep_b) = common(&ra, &rb);
    let marks = |tokens: &[Range<usize>], keep: &[bool]| {
        let mut out: Vec<Range<usize>> = Vec::new();
        for (t, _) in tokens.iter().zip(keep).filter(|(_, kept)| !**kept) {
            match out.last_mut() {
                Some(prev) if prev.end == t.start => prev.end = t.end,
                _ => out.push(t.clone()),
            }
        }
        out
    };
    let (ma, mb) = (marks(&ta, &keep_a), marks(&tb, &keep_b));
    let covered = |marks: &[Range<usize>]| marks.iter().map(Range::len).sum::<usize>();
    // Marks over most of the line are noise: the line was rewritten.
    if covered(&mb) * 10 >= new.len() * 7 || covered(&ma) * 10 >= old.len() * 7 {
        return (Vec::new(), Vec::new());
    }
    (ma, mb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
        DiffLine { kind, old, new, text: text.into() }
    }

    fn hunk(lines: Vec<DiffLine>) -> Hunk {
        Hunk { header: "@@ -1,4 +1,4 @@".into(), lines }
    }

    #[test]
    fn a_removed_run_and_the_added_run_after_it_sit_side_by_side_and_the_shorter_side_is_padded() {
        let lines = vec![
            line(LineKind::Context, Some(1), Some(1), "a"),
            line(LineKind::Removed, Some(2), None, "b"),
            line(LineKind::Removed, Some(3), None, "c"),
            line(LineKind::Added, None, Some(2), "B"),
            line(LineKind::Added, None, Some(3), "C"),
            line(LineKind::Added, None, Some(4), "D"),
            line(LineKind::Context, Some(4), Some(5), "e"),
        ];
        let rows: Vec<_> = split_rows(&lines).iter().map(|r| (r.left, r.right)).collect();
        assert_eq!(rows, [(Some(0), Some(0)), (Some(1), Some(3)), (Some(2), Some(4)), (None, Some(5)), (Some(6), Some(6))]);
    }

    #[test]
    fn split_rows_show_every_line_exactly_once_per_side() {
        let lines = vec![
            line(LineKind::Added, None, Some(1), "x"),
            line(LineKind::Removed, Some(1), None, "y"),
            line(LineKind::Added, None, Some(2), "z"),
            line(LineKind::Context, Some(2), Some(3), "w"),
        ];
        let rows = split_rows(&lines);
        let left: Vec<_> = rows.iter().filter_map(|r| r.left).collect();
        let right: Vec<_> = rows.iter().filter_map(|r| r.right).collect();
        assert_eq!(left, [1, 3], "removed and unchanged lines, in order");
        assert_eq!(right, [0, 2, 3], "added and unchanged lines, in order");
    }

    #[test]
    fn lines_that_differ_only_in_whitespace_become_unchanged_and_are_counted() {
        let h = hunk(vec![
            line(LineKind::Context, Some(1), Some(1), "func f() {"),
            line(LineKind::Removed, Some(2), None, "\tx := 1"),
            line(LineKind::Removed, Some(3), None, "\treturn x"),
            line(LineKind::Added, None, Some(2), "    x := 1"),
            line(LineKind::Added, None, Some(3), "    return x + 1"),
        ]);
        let shown = visible(&h, true, None);
        assert_eq!(shown.hidden, 1);
        let kinds: Vec<_> = shown.lines.iter().map(|s| (s.line.kind, s.line.old, s.line.new)).collect();
        assert_eq!(
            kinds,
            [
                (LineKind::Context, Some(1), Some(1)),
                (LineKind::Context, Some(2), Some(2)),
                (LineKind::Removed, Some(3), None),
                (LineKind::Added, None, Some(3)),
            ]
        );
        assert_eq!(shown.lines[1].line.text, "    x := 1", "the new text is drawn");
        assert_eq!(shown.lines[1].index, 3, "selecting it names the added line, so a comment anchors where it did");
        assert_eq!(visible(&h, false, None).hidden, 0);
        assert_eq!(visible(&h, false, None).lines.len(), h.lines.len(), "without the option nothing changes");
    }

    #[test]
    fn the_line_an_open_comment_box_sits_under_is_never_folded_away() {
        let h = hunk(vec![
            line(LineKind::Removed, Some(1), None, "a  b"),
            line(LineKind::Added, None, Some(1), "a b"),
            line(LineKind::Removed, Some(2), None, "c  d"),
            line(LineKind::Added, None, Some(2), "c d"),
        ]);
        assert_eq!(visible(&h, true, None).hidden, 2);
        for pin in 0..4 {
            let shown = visible(&h, true, Some(pin));
            assert!(shown.lines.iter().any(|s| s.index == pin), "line {pin} is still drawn, so its comment box has a row");
            assert!(shown.hidden < 2, "pinning keeps that pair as a change");
        }
    }

    #[test]
    fn joining_or_splitting_words_is_a_change_but_resizing_the_gap_between_them_is_not() {
        let one = |old: &str, new: &str| {
            let h = hunk(vec![line(LineKind::Removed, Some(1), None, old), line(LineKind::Added, None, Some(1), new)]);
            visible(&h, true, None).hidden
        };
        assert_eq!(one("} else if x {", "}   else  if x {"), 1);
        assert_eq!(one("} else if x {", "} elseif x {"), 0, "a joined keyword is another token");
        assert_eq!(one("f(a, b)", "f(a,b)"), 0, "a space between punctuation is not always nothing");
    }

    #[test]
    fn a_real_change_is_never_hidden_even_when_whitespace_differs_too() {
        let h = hunk(vec![line(LineKind::Removed, Some(1), None, "a  b"), line(LineKind::Added, None, Some(1), "a c")]);
        let shown = visible(&h, true, None);
        assert_eq!((shown.hidden, shown.lines.len()), (0, 2));
        assert!(shown.lines.iter().all(|s| s.line.kind != LineKind::Context));
    }

    #[test]
    fn a_line_added_in_the_middle_does_not_stop_the_whitespace_twins_around_it_from_folding() {
        let h = hunk(vec![
            line(LineKind::Removed, Some(1), None, "a"),
            line(LineKind::Removed, Some(2), None, "b"),
            line(LineKind::Added, None, Some(1), " a"),
            line(LineKind::Added, None, Some(2), "new"),
            line(LineKind::Added, None, Some(3), " b"),
        ]);
        let shown = visible(&h, true, None);
        assert_eq!(shown.hidden, 2);
        let numbers: Vec<_> = shown.lines.iter().map(|s| s.line.new).collect();
        assert_eq!(numbers, [Some(1), Some(2), Some(3)], "new-side numbers stay in order");
    }

    #[test]
    fn only_the_words_that_changed_are_marked() {
        let (old, new) = ("retry := n > 3", "retry := n >= max");
        let (a, b) = word_ranges(old, new);
        let cut = |text: &str, ranges: &[Range<usize>]| ranges.iter().map(|r| text[r.clone()].to_string()).collect::<Vec<_>>();
        assert_eq!(cut(old, &a), ["3"]);
        assert_eq!(cut(new, &b), ["=", "max"]);
    }

    #[test]
    fn a_rewritten_line_gets_no_marks() {
        let (a, b) = word_ranges("alpha beta gamma", "one two three");
        assert!(a.is_empty() && b.is_empty());
    }

    #[test]
    fn changed_words_pair_lines_the_way_split_view_does() {
        let lines = vec![
            line(LineKind::Context, Some(1), Some(1), "keep"),
            line(LineKind::Removed, Some(2), None, "return err"),
            line(LineKind::Added, None, Some(2), "return fmt.Errorf(err)"),
            line(LineKind::Added, None, Some(3), "extra"),
        ];
        let marks = changed_words(&lines);
        assert!(marks[0].is_empty() && marks[3].is_empty(), "context and unpaired lines carry none");
        assert!(marks[1].is_empty(), "nothing of the old line changed: the new one only wrapped it");
        assert!(!marks[2].is_empty());
    }
}
