//! The unchanged lines between and around the hunks of a file, and what the reader revealed of them. A diff shows a few lines
//! of context per hunk; `review.expand` reads the rest from the file. Pure: it works out which lines to ask for and where
//! the answer goes, and reads nothing.

use serde::{Deserialize, Serialize};

use crate::diff::{DiffLine, Hunk, LineKind};

/// How many lines one step reveals.
pub const STEP: u32 = 20;

/// The line numbers a hunk covers on each side, from its `@@ -a,b +c,d @@` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
}

impl Span {
    /// The first line after the hunk on each side. Git numbers an empty range by the line before it.
    fn after(self) -> (u32, u32) {
        let next = |start: u32, count: u32| if count == 0 { start + 1 } else { start + count };
        (next(self.old_start, self.old_count), next(self.new_start, self.new_count))
    }

    /// The first new-side line that is not before the hunk.
    fn new_first(self) -> u32 {
        if self.new_count == 0 { self.new_start + 1 } else { self.new_start }
    }
}

/// `@@ -12,5 +12,7 @@ ctx`, or `None` when the header is not one.
pub fn span(hunk: &Hunk) -> Option<Span> {
    let (old, rest) = hunk.header.strip_prefix("@@ -")?.split_once(' ')?;
    let (new, _) = rest.strip_prefix('+')?.split_once(' ')?;
    let range = |text: &str| -> Option<(u32, u32)> {
        let (start, count) = text.split_once(',').unwrap_or((text, "1"));
        Some((start.parse().ok()?, count.parse().ok()?))
    };
    let ((old_start, old_count), (new_start, new_count)) = (range(old)?, range(new)?);
    Some(Span { old_start, old_count, new_start, new_count })
}

/// The unchanged lines before the first hunk, between two hunks, or after the last one, as the diff leaves them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// 0 is above the first hunk, `n` is below hunk `n - 1`.
    pub index: usize,
    /// The first new-side line of the gap.
    pub first: u32,
    /// The last new-side line, or `None` below the last hunk, where the file's length is not in the diff.
    pub last: Option<u32>,
    /// New-side number minus old-side number inside this gap.
    pub delta: i64,
}

impl Gap {
    /// How many lines the diff leaves out here, when that is known.
    pub fn hidden(self) -> Option<u32> {
        self.last.map(|last| if last >= self.first { last - self.first + 1 } else { 0 })
    }
}

/// Every gap of a file with these hunks: one more than there are hunks. A hunk with an unreadable header ends the list.
pub fn gaps(hunks: &[Hunk]) -> Vec<Gap> {
    let spans: Vec<Span> = hunks.iter().map_while(span).collect();
    let mut out = Vec::with_capacity(spans.len() + 1);
    let mut first = 1;
    let mut delta = 0;
    for (index, hunk) in spans.iter().enumerate() {
        out.push(Gap { index, first, last: Some(hunk.new_first().saturating_sub(1)), delta });
        let (old_after, new_after) = hunk.after();
        first = new_after;
        delta = i64::from(new_after) - i64::from(old_after);
    }
    out.push(Gap { index: spans.len(), first, last: None, delta });
    out
}

/// A line of the file as `review.expand` read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedLine {
    pub number: u32,
    pub text: String,
}

/// What the reader revealed in one gap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reveal {
    /// Lines next to the hunk above, in order.
    pub top: Vec<DiffLine>,
    /// Lines next to the hunk below, in order.
    pub bottom: Vec<DiffLine>,
    /// The file ends at this line, learned from a read; only a gap below the last hunk can learn it.
    pub end: Option<u32>,
}

/// Where the lines still hidden in a gap are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remaining {
    /// Nothing is hidden.
    None,
    /// These new-side lines are hidden.
    Lines { first: u32, last: u32 },
    /// Lines below the last hunk, and the file's length is not known yet: the first hidden line.
    Unknown { first: u32 },
}

pub fn remaining(gap: Gap, reveal: &Reveal) -> Remaining {
    let first = gap.first + reveal.top.len() as u32;
    match gap.last.or(reveal.end) {
        Some(last) => {
            let last = last.saturating_sub(reveal.bottom.len() as u32);
            if last >= first { Remaining::Lines { first, last } } else { Remaining::None }
        }
        None => Remaining::Unknown { first },
    }
}

/// Which lines of a gap to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// [`STEP`] more lines next to the hunk below.
    Above,
    /// [`STEP`] more lines next to the hunk above.
    Below,
    /// Every hidden line.
    All,
}

/// The new-side lines `from..=to` to read, or `None` when this gap has none to show that way.
pub fn plan(gap: Gap, reveal: &Reveal, kind: Kind) -> Option<(u32, u32)> {
    let (first, last) = match remaining(gap, reveal) {
        Remaining::None => return None,
        Remaining::Lines { first, last } => (first, Some(last)),
        Remaining::Unknown { first } => (first, None),
    };
    match (kind, last) {
        (Kind::All, last) => Some((first, last.unwrap_or(u32::MAX))),
        (Kind::Below, last) => Some((first, last.map_or(first + STEP - 1, |l| l.min(first + STEP - 1)))),
        // Lines above a hunk need a hunk below, so the gap's end is known.
        (Kind::Above, Some(last)) => Some((last.saturating_sub(STEP - 1).max(first), last)),
        (Kind::Above, None) => None,
    }
}

/// Puts `lines`, read for [`plan`]'s `kind`, into `reveal`. `total` is the file's length when the read said so.
pub fn apply(gap: Gap, reveal: &mut Reveal, kind: Kind, lines: Vec<ExpandedLine>, total: Option<u32>) {
    let context = |line: ExpandedLine| {
        let old = i64::from(line.number) - gap.delta;
        DiffLine { kind: LineKind::Context, old: u32::try_from(old).ok(), new: Some(line.number), text: line.text }
    };
    let read: Vec<DiffLine> = lines.into_iter().map(context).collect();
    match kind {
        Kind::Above => {
            let mut earlier = read;
            earlier.append(&mut reveal.bottom);
            reveal.bottom = earlier;
        }
        Kind::Below | Kind::All => reveal.top.extend(read),
    }
    if gap.last.is_none() {
        reveal.end = total.or(reveal.end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(header: &str) -> Hunk {
        Hunk { header: header.into(), lines: Vec::new() }
    }

    #[test]
    fn gaps_surround_every_hunk_and_carry_the_shift_between_the_two_numberings() {
        // The first hunk adds three lines (+3), the second removes one.
        let hunks = [hunk("@@ -10,4 +10,7 @@ f"), hunk("@@ -40,5 +43,4 @@ g")];
        let all = gaps(&hunks);
        assert_eq!(all.len(), 3);
        assert_eq!((all[0].first, all[0].last, all[0].delta), (1, Some(9), 0));
        // Between: new lines 17..=42, which are old lines 14..=39.
        assert_eq!((all[1].first, all[1].last, all[1].delta), (17, Some(42), 3));
        assert_eq!((all[2].first, all[2].last, all[2].delta), (47, None, 2));
        assert_eq!(all[1].hidden(), Some(26));
    }

    #[test]
    fn an_empty_side_is_numbered_by_the_line_before_it() {
        // Pure deletion of two lines after new line 4, and a pure addition after old line 20.
        let hunks = [hunk("@@ -5,2 +4,0 @@"), hunk("@@ -20,0 +19,3 @@")];
        let all = gaps(&hunks);
        assert_eq!(all[0].last, Some(4), "the gap above ends with the line before the deletion");
        assert_eq!(all[1].first, 5, "the next gap starts at the line after it");
        assert_eq!(all[1].last, Some(18), "old line 20 is new line 18 after the deletion, and the added range starts at 19");
        assert_eq!(all[2].first, 22);
    }

    #[test]
    fn a_file_with_no_hunks_has_one_gap_that_is_all_of_it() {
        let all = gaps(&[]);
        assert_eq!(all, [Gap { index: 0, first: 1, last: None, delta: 0 }]);
    }

    #[test]
    fn showing_above_reveals_the_lines_next_to_the_hunk_below_and_each_step_goes_further_up() {
        let hunks = [hunk("@@ -50,3 +50,3 @@")];
        let gap = gaps(&hunks)[0];
        let mut reveal = Reveal::default();
        let (from, to) = plan(gap, &reveal, Kind::Above).expect("lines hidden");
        assert_eq!((to + 1, to - from + 1), (50, STEP), "a step of lines ending where the hunk starts");
        let read: Vec<_> = (from..=to).map(|number| ExpandedLine { number, text: format!("l{number}") }).collect();
        apply(gap, &mut reveal, Kind::Above, read, None);
        assert_eq!(remaining(gap, &reveal), Remaining::Lines { first: 1, last: 29 });
        assert_eq!(plan(gap, &reveal, Kind::Above).map(|p| p.1), Some(from - 1), "the next step ends where the last one began");
        assert_eq!(reveal.bottom.first().and_then(|l| l.new), Some(30));
        assert_eq!(reveal.bottom.last().and_then(|l| l.old), Some(49), "the numbers on both sides are given");
    }

    #[test]
    fn expanding_everything_leaves_nothing_hidden_and_nothing_to_plan() {
        let hunks = [hunk("@@ -5,2 +5,2 @@"), hunk("@@ -12,2 +12,2 @@")];
        let gap = gaps(&hunks)[1];
        let mut reveal = Reveal::default();
        let (from, to) = plan(gap, &reveal, Kind::All).expect("lines hidden");
        assert_eq!((from, to), (7, 11));
        let read = (from..=to).map(|number| ExpandedLine { number, text: String::new() }).collect();
        apply(gap, &mut reveal, Kind::All, read, None);
        assert_eq!(remaining(gap, &reveal), Remaining::None);
        assert_eq!(plan(gap, &reveal, Kind::All), None);
    }

    #[test]
    fn below_the_last_hunk_the_file_length_is_learned_from_the_read() {
        let hunks = [hunk("@@ -1,2 +1,2 @@")];
        let gap = gaps(&hunks)[1];
        let mut reveal = Reveal::default();
        assert_eq!(remaining(gap, &reveal), Remaining::Unknown { first: 3 });
        assert_eq!(plan(gap, &reveal, Kind::Above), None, "no hunk below to attach lines to");
        let (from, to) = plan(gap, &reveal, Kind::Below).expect("lines may follow");
        assert_eq!((from, to - from + 1), (3, STEP), "below the last hunk it asks for a step of lines");
        // The file has only five lines.
        let read = (3..=5).map(|number| ExpandedLine { number, text: String::new() }).collect();
        apply(gap, &mut reveal, Kind::Below, read, Some(5));
        assert_eq!(remaining(gap, &reveal), Remaining::None, "the end was reached");
    }
}
