//! What changed in the markup of an SVG (Figma frame 79): elements matched by id, else by tag and place under their
//! parent; each changed attribute, each added or removed element and each changed text is one row, with the source line
//! a comment on the row posts to. No I/O.

use std::collections::{BTreeMap, HashMap};

use craft::xml::{Event, Options};

use crate::diff::{FileDiff, LineKind};
use crate::review::LineRef;

/// The largest SVG source read, per side.
pub const MAX_BYTES: usize = 4 << 20;
/// The most elements read, per side.
pub const MAX_ELEMENTS: usize = 20_000;
/// The most rows kept; the rest are counted.
pub const MAX_ROWS: usize = 500;
const MAX_DEPTH: usize = 64;
/// Characters of an attribute value or text shown in a row.
const VALUE_CHARS: usize = 60;

/// What one row says changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgChange {
    /// Shown for context: the root's viewBox when it did not change.
    Same(String),
    Changed { old: String, new: String },
    Added(String),
    Removed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgRow {
    /// `<path#arrow>`, `<path[2]>`, `<title>`.
    pub element: String,
    /// The attribute, `text` for the element's text, `None` for the element as a whole.
    pub attr: Option<String>,
    pub change: SvgChange,
    /// The diff line a comment on this row posts on; `None` when the diff shows no line of the element.
    pub anchor: Option<LineRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SvgStructure {
    pub rows: Vec<SvgRow>,
    /// Rows past [`MAX_ROWS`], not kept.
    pub left_out: usize,
}

struct Element {
    key: String,
    tag: String,
    id: Option<String>,
    attrs: Vec<(String, String)>,
    text: String,
    parent: Option<usize>,
    /// 1-based place among siblings of the same tag, and how many such siblings there are.
    place: usize,
    siblings: usize,
    /// First and last line of the start tag, and of the whole element up to its end tag.
    lines: (u32, u32),
    whole: (u32, u32),
}

impl Element {
    fn name(&self) -> String {
        match &self.id {
            Some(id) => format!("<{}#{id}>", self.tag),
            None if self.siblings > 1 => format!("<{}[{}]>", self.tag, self.place),
            None => format!("<{}>", self.tag),
        }
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// The elements of `text` in document order.
fn read(text: &str, side: &str) -> Result<Vec<Element>, String> {
    if text.len() > MAX_BYTES {
        return Err(format!("the {side} SVG is larger than {} MB", MAX_BYTES >> 20));
    }
    let newlines: Vec<usize> = text.match_indices('\n').map(|(at, _)| at).collect();
    let line = |at: usize| u32::try_from(newlines.partition_point(|n| *n < at) + 1).unwrap_or(u32::MAX);
    let options = Options::default();
    let mut events = options.events(text.as_bytes());
    let mut elements: Vec<Element> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut counts: HashMap<(Option<usize>, String), usize> = HashMap::new();
    let mut ids: HashMap<String, usize> = HashMap::new();
    while let Some(event) = events.next().map_err(|e| format!("the {side} SVG is not readable XML: {e}"))? {
        match event {
            Event::Start { name, attrs } => {
                if open.len() >= MAX_DEPTH {
                    return Err(format!("the {side} SVG nests elements deeper than {MAX_DEPTH} levels"));
                }
                if elements.len() >= MAX_ELEMENTS {
                    return Err(format!("the {side} SVG has more than {MAX_ELEMENTS} elements"));
                }
                let parent = open.last().copied();
                let count = counts.entry((parent, name.clone())).or_default();
                *count += 1;
                let place = *count;
                let id = attrs.iter().find(|(k, _)| k == "id").map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
                // An id names an element wherever it moves; a repeated id falls back to its place.
                let key = match &id {
                    Some(id) if *ids.entry(id.clone()).and_modify(|n| *n += 1).or_insert(1) == 1 => format!("#{id}"),
                    _ => format!("{}/{name}[{place}]", parent.map_or("", |p| elements[p].key.as_str())),
                };
                let span = events.span();
                let lines = (line(span.start), line(span.end.saturating_sub(1).max(span.start)));
                elements.push(Element { key, tag: name, id, attrs, text: String::new(), parent, place, siblings: 0, lines, whole: lines });
                open.push(elements.len() - 1);
            }
            Event::End { .. } => {
                if let Some(at) = open.pop() {
                    elements[at].whole.1 = line(events.span().end.saturating_sub(1)).max(elements[at].lines.1);
                }
            }
            Event::Text(text) | Event::CData(text) => {
                if let Some(&at) = open.last() {
                    elements[at].text.push_str(&text);
                }
            }
            Event::Pi { .. } | Event::Comment(_) => {}
        }
    }
    for element in &mut elements {
        element.siblings = counts.get(&(element.parent, element.tag.clone())).copied().unwrap_or(1);
        element.text = squash(&element.text);
    }
    Ok(elements)
}

/// The diff lines of a file, by line number on each side, and whether the diff changed them.
#[derive(Default)]
struct Lines {
    new: BTreeMap<u32, (LineRef, bool)>,
    old: BTreeMap<u32, (LineRef, bool)>,
}

impl Lines {
    fn of(file: &FileDiff) -> Self {
        let mut lines = Lines::default();
        for line in file.hunks.iter().flat_map(|h| &h.lines) {
            let at = LineRef { old: line.old, new: line.new };
            if let Some(n) = line.new {
                lines.new.insert(n, (at, line.kind == LineKind::Added));
            }
            if let Some(n) = line.old {
                lines.old.insert(n, (at, line.kind == LineKind::Removed));
            }
        }
        lines
    }

    /// The last changed line of the element on the new side, else on the old side, else the last line the diff shows.
    fn anchor(&self, new: Option<(u32, u32)>, old: Option<(u32, u32)>) -> Option<LineRef> {
        let last = |map: &BTreeMap<u32, (LineRef, bool)>, span: Option<(u32, u32)>, changed: bool| {
            let (first, last) = span?;
            map.range(first..=last).rev().find(|(_, (_, c))| *c || !changed).map(|(_, (at, _))| *at)
        };
        last(&self.new, new, true).or_else(|| last(&self.old, old, true)).or_else(|| last(&self.new, new, false)).or_else(|| last(&self.old, old, false))
    }
}

/// The width and height of the root's viewBox, when it has one.
pub fn view_box_size(text: &str) -> Option<(f32, f32)> {
    let options = Options::default();
    let mut events = options.events(text.as_bytes());
    while let Ok(Some(event)) = events.next() {
        if let Event::Start { attrs, .. } = event {
            let value = attrs.into_iter().find(|(k, _)| k == "viewBox")?.1;
            let numbers: Vec<f32> = value.split(|c: char| c.is_whitespace() || c == ',').filter_map(|n| n.parse().ok()).collect();
            return match numbers[..] {
                [_, _, w, h] if w > 0.0 && h > 0.0 => Some((w, h)),
                _ => None,
            };
        }
    }
    None
}

/// The rows of what changed between `old` and `new` (either absent when the change adds or deletes the file): the unchanged
/// viewBox for scale, then changed attributes and text, then added elements, then removed ones, each in document order.
pub fn structure(old: Option<&str>, new: Option<&str>, file: &FileDiff) -> Result<SvgStructure, String> {
    let before = old.map(|t| read(t, "old")).transpose()?.unwrap_or_default();
    let after = new.map(|t| read(t, "new")).transpose()?.unwrap_or_default();
    let lines = Lines::of(file);
    let by_key: HashMap<&str, usize> = before.iter().enumerate().map(|(i, e)| (e.key.as_str(), i)).collect();
    let matched: Vec<Option<usize>> = after.iter().map(|e| by_key.get(e.key.as_str()).copied()).collect();
    let mut taken = vec![false; before.len()];
    for o in matched.iter().flatten() {
        taken[*o] = true;
    }
    let mut rows = Rows::default();
    if let (Some(a), Some(b)) = (before.first(), after.first())
        && let (Some(was), Some(is)) = (a.attr("viewBox"), b.attr("viewBox"))
        && was == is
    {
        rows.push(SvgRow { element: b.name(), attr: Some("viewBox".into()), change: SvgChange::Same(cut(is)), anchor: lines.anchor(Some(b.lines), None) });
    }
    for (n, element) in after.iter().enumerate() {
        if let Some(o) = matched[n] {
            changed_rows(&before[o], element, &lines, &mut rows);
        }
    }
    // An element inside a new or removed element is told in that element's row.
    for (n, element) in after.iter().enumerate().filter(|(n, e)| matched[*n].is_none() && !e.parent.is_some_and(|p| matched[p].is_none())) {
        let change = SvgChange::Added(summary(element, inside(&after, n)));
        rows.push(SvgRow { element: element.name(), attr: None, change, anchor: lines.anchor(Some(element.whole), None) });
    }
    for (o, element) in before.iter().enumerate().filter(|(o, e)| !taken[*o] && !e.parent.is_some_and(|p| !taken[p])) {
        let change = SvgChange::Removed(summary(element, inside(&before, o)));
        rows.push(SvgRow { element: element.name(), attr: None, change, anchor: lines.anchor(None, Some(element.whole)) });
    }
    Ok(SvgStructure { rows: rows.rows, left_out: rows.left_out })
}

/// Rows up to [`MAX_ROWS`]; the rest are only counted, so a huge file never holds them.
#[derive(Default)]
struct Rows {
    rows: Vec<SvgRow>,
    left_out: usize,
}

impl Rows {
    fn push(&mut self, row: SvgRow) {
        if self.rows.len() < MAX_ROWS {
            self.rows.push(row);
        } else {
            self.left_out += 1;
        }
    }
}

/// One row per attribute added, removed or changed, and one for changed text, which posts on a changed line inside the element.
fn changed_rows(old: &Element, new: &Element, lines: &Lines, rows: &mut Rows) {
    let anchor = lines.anchor(Some(new.lines), Some(old.lines));
    let row = |attr: &str, change, anchor| SvgRow { element: new.name(), attr: Some(attr.to_string()), change, anchor };
    let (was, is): (HashMap<&str, &str>, HashMap<&str, &str>) = (
        old.attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(),
        new.attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(),
    );
    for (name, value) in &new.attrs {
        match was.get(name.as_str()) {
            Some(before) if *before == value.as_str() => {}
            Some(before) => rows.push(row(name, SvgChange::Changed { old: shown_value(name, before), new: shown_value(name, value) }, anchor)),
            None => rows.push(row(name, SvgChange::Added(shown_value(name, value)), anchor)),
        }
    }
    for (name, value) in old.attrs.iter().filter(|(name, _)| !is.contains_key(name.as_str())) {
        rows.push(row(name, SvgChange::Removed(shown_value(name, value)), anchor));
    }
    if old.text != new.text {
        let change = match (old.text.is_empty(), new.text.is_empty()) {
            (true, _) => SvgChange::Added(quoted(&new.text)),
            (_, true) => SvgChange::Removed(quoted(&old.text)),
            _ => SvgChange::Changed { old: quoted(&old.text), new: quoted(&new.text) },
        };
        rows.push(row("text", change, lines.anchor(Some(new.whole), Some(old.whole))));
    }
}

/// How many elements sit inside element `at`. Its subtree follows it in document order, and the first element after the
/// subtree has a parent before `at`.
fn inside(elements: &[Element], at: usize) -> usize {
    elements.iter().skip(at + 1).take_while(|e| e.parent.is_some_and(|p| p >= at)).count()
}

/// An added or removed element in words: its layer name, how many points it draws, its text, what it holds.
fn summary(element: &Element, inside: usize) -> String {
    let mut parts = Vec::new();
    if let Some(label) = ["aria-label", "data-name", "inkscape:label"].iter().find_map(|k| element.attr(k)).filter(|l| !l.trim().is_empty()) {
        parts.push(cut(label.trim()));
    }
    if let Some(points) = element.attr("d").map(|d| path_points(d).0).or_else(|| element.attr("points").map(|p| numbers(p) / 2)) {
        parts.push(points_word(points));
    }
    if !element.text.is_empty() {
        parts.push(quoted(&element.text));
    }
    if inside > 0 {
        parts.push(format!("{inside} {} inside", if inside == 1 { "element" } else { "elements" }));
    }
    parts.join(", ")
}

fn points_word(n: usize) -> String {
    format!("{n} {}", if n == 1 { "point" } else { "points" })
}

/// Path data as its first two segments and how many points it draws; other values cut to fit a row.
fn shown_value(attr: &str, value: &str) -> String {
    match attr {
        "d" => {
            let (points, start) = path_points(value);
            format!("{} ({})", cut(&squash(start)), points_word(points))
        }
        _ => cut(&squash(value)),
    }
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", cut(text))
}

fn cut(text: &str) -> String {
    match text.char_indices().nth(VALUE_CHARS) {
        Some((at, _)) => format!("{}…", text.get(..at).unwrap_or(text)),
        None => text.to_string(),
    }
}

/// How many numbers `text` holds.
fn numbers(text: &str) -> usize {
    let mut reader = Numbers { bytes: text.as_bytes(), at: 0 };
    std::iter::from_fn(|| reader.number()).count()
}

struct Numbers<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Numbers<'_> {
    fn skip_separators(&mut self) {
        while self.bytes.get(self.at).is_some_and(|b| b.is_ascii_whitespace() || *b == b',') {
            self.at += 1;
        }
    }

    /// The next number: a sign, digits with at most one point, an exponent. `None` when no number starts here.
    fn number(&mut self) -> Option<()> {
        self.skip_separators();
        let start = self.at;
        let digit = |b: Option<&u8>| b.is_some_and(u8::is_ascii_digit);
        if matches!(self.bytes.get(self.at), Some(b'+' | b'-')) {
            self.at += 1;
        }
        let mut digits = 0;
        while digit(self.bytes.get(self.at)) {
            self.at += 1;
            digits += 1;
        }
        if self.bytes.get(self.at) == Some(&b'.') {
            self.at += 1;
            while digit(self.bytes.get(self.at)) {
                self.at += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            self.at = start;
            return None;
        }
        if matches!(self.bytes.get(self.at), Some(b'e' | b'E')) {
            let mark = self.at;
            self.at += 1;
            if matches!(self.bytes.get(self.at), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if digit(self.bytes.get(self.at)) {
                while digit(self.bytes.get(self.at)) {
                    self.at += 1;
                }
            } else {
                self.at = mark;
            }
        }
        Some(())
    }

    /// An arc flag: one `0` or `1`, which may touch the next number (`a1 1 0 1012 12`).
    fn flag(&mut self) -> Option<()> {
        self.skip_separators();
        match self.bytes.get(self.at) {
            Some(b'0' | b'1') => {
                self.at += 1;
                Some(())
            }
            _ => None,
        }
    }
}

/// How many points path data `d` draws (one per segment end; a close draws none), and its first two segments as written.
fn path_points(d: &str) -> (usize, &str) {
    let mut reader = Numbers { bytes: d.as_bytes(), at: 0 };
    let (mut points, mut segments, mut second_end) = (0, 0, d.len());
    let mut command = None;
    loop {
        reader.skip_separators();
        let Some(&byte) = reader.bytes.get(reader.at) else { break };
        if byte.is_ascii_alphabetic() {
            reader.at += 1;
            command = Some(byte.to_ascii_lowercase());
            if command == Some(b'z') {
                segments += 1;
                if segments == 2 {
                    second_end = reader.at;
                }
                continue;
            }
        }
        let arity = match command {
            Some(b'm' | b'l' | b't') => 2,
            Some(b'h' | b'v') => 1,
            Some(b's' | b'q') => 4,
            Some(b'c') => 6,
            Some(b'a') => 7,
            _ => break,
        };
        let read = (0..arity).all(|i| if command == Some(b'a') && (i == 3 || i == 4) { reader.flag().is_some() } else { reader.number().is_some() });
        if !read {
            break;
        }
        points += 1;
        segments += 1;
        if segments == 2 {
            second_end = reader.at;
        }
    }
    (points, d.get(..second_end).unwrap_or(d).trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified;

    const OLD: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\">\n  <path id=\"arrow\" d=\"M4 12a8 8 0 1 0 2.3-5.7L4 4\" stroke-width=\"1.5\"/>\n  <circle r=\"1\"/>\n  <circle r=\"2\"/>\n</svg>\n";
    const NEW: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\">\n  <title>Retry</title>\n  <path id=\"arrow\" d=\"M3 11a9 9 0 0 1 15-5.3L20 3l1 1\" stroke-width=\"2\"/>\n  <path id=\"tail\" data-name=\"second arrow\" d=\"M21 13a9 9 0 0 1-15 5.3\"/>\n  <circle r=\"1\"/>\n</svg>\n";
    const PATCH: &str = "diff --git a/i.svg b/i.svg\n--- a/i.svg\n+++ b/i.svg\n@@ -1,5 +1,6 @@\n <svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\">\n-  <path id=\"arrow\" d=\"M4 12a8 8 0 1 0 2.3-5.7L4 4\" stroke-width=\"1.5\"/>\n+  <title>Retry</title>\n+  <path id=\"arrow\" d=\"M3 11a9 9 0 0 1 15-5.3L20 3l1 1\" stroke-width=\"2\"/>\n+  <path id=\"tail\" data-name=\"second arrow\" d=\"M21 13a9 9 0 0 1-15 5.3\"/>\n   <circle r=\"1\"/>\n-  <circle r=\"2\"/>\n </svg>\n";

    fn rows() -> Vec<SvgRow> {
        let file = parse_unified(PATCH).into_iter().next().expect("one file");
        structure(Some(OLD), Some(NEW), &file).expect("both sides read").rows
    }

    fn find<'a>(rows: &'a [SvgRow], element: &str, attr: Option<&str>) -> &'a SvgRow {
        rows.iter().find(|r| r.element == element && r.attr.as_deref() == attr).unwrap_or_else(|| panic!("no row {element} {attr:?} in {rows:#?}"))
    }

    fn no_diff() -> FileDiff {
        parse_unified("diff --git a/i.svg b/i.svg\n--- a/i.svg\n+++ b/i.svg\n").into_iter().next().expect("one file")
    }

    #[test]
    fn each_changed_attribute_added_element_and_removed_element_is_one_row() {
        let rows = rows();
        assert_eq!(rows[0].change, SvgChange::Same("0 0 24 24".into()), "the unchanged viewBox leads, for scale");
        assert_eq!(
            find(&rows, "<path#arrow>", Some("d")).change,
            SvgChange::Changed { old: "M4 12a8 8 0 1 0 2.3-5.7 (3 points)".into(), new: "M3 11a9 9 0 0 1 15-5.3 (4 points)".into() }
        );
        assert_eq!(find(&rows, "<path#arrow>", Some("stroke-width")).change, SvgChange::Changed { old: "1.5".into(), new: "2".into() });
        assert_eq!(find(&rows, "<path#tail>", None).change, SvgChange::Added("second arrow, 2 points".into()));
        assert_eq!(find(&rows, "<title>", None).change, SvgChange::Added("\"Retry\"".into()));
        assert!(matches!(find(&rows, "<circle[2]>", None).change, SvgChange::Removed(_)), "an element without an id is matched by its place");
        assert!(rows.iter().all(|r| r.element != "<circle[1]>" && r.element != "<circle>"), "the unchanged circle has no row");
    }

    #[test]
    fn a_row_posts_on_the_line_the_diff_changed() {
        let rows = rows();
        assert_eq!(find(&rows, "<path#arrow>", Some("d")).anchor, Some(LineRef { old: None, new: Some(3) }));
        assert_eq!(find(&rows, "<circle[2]>", None).anchor, Some(LineRef { old: Some(4), new: None }), "a removed element posts on its old line");
        assert_eq!(rows[0].anchor, Some(LineRef { old: Some(1), new: Some(1) }), "an unchanged line the diff shows still takes a comment");
    }

    #[test]
    fn changes_to_kept_elements_come_before_added_ones_and_removed_ones_last() {
        let rows = rows();
        let at = |element: &str| rows.iter().position(|r| r.element == element).unwrap_or(usize::MAX);
        assert!(at("<path#arrow>") < at("<title>"), "the title comes first in the file but is new");
        assert!(at("<title>") < at("<circle[2]>"));
    }

    #[test]
    fn path_data_counts_segment_ends_and_reads_packed_numbers_and_arc_flags() {
        assert_eq!(path_points("M4 12a8 8 0 1 0 2.3-5.7L4 4z").0, 3);
        assert_eq!(path_points("M0,0a1 1 0 1012 12").0, 2, "flags 1 and 0 touch the x that follows");
        assert_eq!(path_points("M.5.5l1e2-3 4 4h1v1"), (5, "M.5.5l1e2-3"), "an implicit repeat is a segment of its own");
        assert_eq!(path_points("M0 0 L").0, 1, "a cut-off command is not counted");
    }

    #[test]
    fn changed_text_posts_on_its_changed_line_even_when_the_start_tag_is_outside_the_hunk() {
        let old = "<svg>\n<style>\na{}\nb{}\nc{}\nd{}\ne{fill:red}\n</style>\n</svg>\n";
        let new = old.replace("red", "blue");
        let patch = "diff --git a/i.svg b/i.svg\n--- a/i.svg\n+++ b/i.svg\n@@ -4,5 +4,5 @@\n b{}\n c{}\n d{}\n-e{fill:red}\n+e{fill:blue}\n </style>\n";
        let file = parse_unified(patch).into_iter().next().expect("one file");
        let rows = structure(Some(old), Some(&new), &file).expect("reads").rows;
        assert_eq!(find(&rows, "<style>", Some("text")).anchor, Some(LineRef { old: None, new: Some(7) }));
    }

    #[test]
    fn rows_past_the_limit_are_counted_not_kept() {
        let new = format!("<svg>{}</svg>", (0..MAX_ROWS + 100).map(|i| format!("<g id=\"g{i}\"/>")).collect::<String>());
        let shown = structure(Some("<svg/>"), Some(&new), &no_diff()).expect("reads");
        assert_eq!((shown.rows.len(), shown.left_out), (MAX_ROWS, 100));
    }

    #[test]
    fn an_added_file_lists_its_elements_and_hostile_input_is_refused() {
        let added = structure(None, Some(NEW), &no_diff()).expect("reads");
        assert!(matches!(added.rows[0].change, SvgChange::Added(_)));
        assert_eq!(added.rows.len(), 1, "what the new root holds is told in its row");
        let deep = format!("{}{}", "<g>".repeat(MAX_DEPTH + 1), "</g>".repeat(MAX_DEPTH + 1));
        assert!(structure(None, Some(&deep), &no_diff()).is_err());
        let wide = format!("<svg>{}</svg>", "<g/>".repeat(MAX_ELEMENTS));
        assert!(structure(None, Some(&wide), &no_diff()).is_err());
    }
}
