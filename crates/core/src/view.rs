//! File views: which views a path offers, and how a rendered view maps back to source lines, so a comment made on
//! a rendered block posts on a line of the file unchanged. No I/O.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::diff::{FileDiff, LineKind};
use crate::review::LineRef;

/// One way to show a changed file. [`FileView::Source`] is the unified diff every file has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileView {
    Source,
    Rendered,
    /// What changed in an SVG's markup, element by element.
    Structure,
    ImageTwoUp,
    ImageSwipe,
    ImageOnion,
    ImageDifference,
    Notebook,
}

impl FileView {
    /// The words on the view switcher.
    pub fn label(self) -> &'static str {
        match self {
            FileView::Source => "Source",
            FileView::Rendered => "Rendered",
            FileView::Structure => "Structure",
            FileView::ImageTwoUp => "2-up",
            FileView::ImageSwipe => "Swipe",
            FileView::ImageOnion => "Onion",
            FileView::ImageDifference => "Difference",
            FileView::Notebook => "Cells",
        }
    }
}

/// The lowercase extension of `path` without its dot; empty when the name has none. The key a view choice is remembered under.
pub fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default()
}

/// The views `path` offers, the default first. Source is always last.
pub fn views_for(path: &str) -> &'static [FileView] {
    use FileView::{ImageDifference, ImageOnion, ImageSwipe, ImageTwoUp, Notebook, Rendered, Source, Structure};
    match extension(path).as_str() {
        "md" | "markdown" => &[Rendered, Source],
        "svg" => &[Rendered, Structure, Source],
        "png" | "jpg" | "jpeg" | "gif" | "webp" => &[ImageTwoUp, ImageSwipe, ImageOnion, ImageDifference, Source],
        "ipynb" => &[Notebook, Source],
        _ => &[Source],
    }
}

/// The remembered view when `path` still offers it, else the first view it offers.
pub fn default_view(path: &str, remembered: Option<FileView>) -> FileView {
    let offered = views_for(path);
    remembered.filter(|view| offered.contains(view)).or_else(|| offered.first().copied()).unwrap_or(FileView::Source)
}

/// What the rendered/source key switches to from `current`: an SVG steps Rendered, Structure, Source; others toggle.
pub fn toggled(path: &str, current: FileView) -> FileView {
    let offered = views_for(path);
    match current {
        FileView::Source => offered.first().copied().unwrap_or(FileView::Source),
        FileView::Rendered if offered.contains(&FileView::Structure) => FileView::Structure,
        _ => FileView::Source,
    }
}

/// What one Markdown block is, as far as laying it out goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdKind {
    Heading(u8),
    Paragraph,
    List,
    Quote,
    Code,
    Table,
    Rule,
}

/// A run of source lines that renders as one block: `first` and `last` are 1-based line numbers in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdBlock {
    pub kind: MdKind,
    pub first: u32,
    pub last: u32,
    /// The lines as written.
    pub lines: Vec<String>,
}

/// What the block splitter hands back: a block, or a run of blocks it was told to leave out.
enum Piece {
    Block(MdBlock),
    /// `blocks` blocks ending at line `last`, not kept.
    Gap {
        blocks: usize,
        last: u32,
    },
}

impl Piece {
    fn last(&self) -> u32 {
        match self {
            Piece::Block(block) => block.last,
            Piece::Gap { last, .. } => *last,
        }
    }
}

/// Decides whether a closed block is kept, given its index, first and last line, and the last line of the block before.
type Keep<'a> = &'a mut dyn FnMut(usize, u32, u32, u32) -> bool;

/// Collects closed blocks; one `keep` says no to is dropped at once and counted in a [`Piece::Gap`], so a file of
/// millions of blocks never holds them all.
struct Sink<'a> {
    pieces: Vec<Piece>,
    index: usize,
    prev_last: u32,
    keep: Keep<'a>,
}

impl Sink<'_> {
    fn take(&mut self, block: Option<MdBlock>) {
        let Some(block) = block else { return };
        let keep = (self.keep)(self.index, block.first, block.last, self.prev_last);
        self.index += 1;
        self.prev_last = block.last;
        if keep {
            self.pieces.push(Piece::Block(block));
        } else if let Some(Piece::Gap { blocks, last }) = self.pieces.last_mut() {
            *blocks += 1;
            *last = block.last;
        } else {
            self.pieces.push(Piece::Gap { blocks: 1, last: block.last });
        }
    }
}

/// Splits Markdown into blocks: blank lines separate them, a heading is its own block, a fenced code block runs to its
/// closing fence blank lines included. Bullets, quotes and tables keep their wrapped lines.
// shortcut: no setext headings, indented code or HTML blocks, and a loose list splits at its blank lines; use craft-markdown spans when it offers them.
pub fn md_blocks(text: &str) -> Vec<MdBlock> {
    split_blocks(text, &mut |_, _, _, _| true)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Block(block) => Some(block),
            Piece::Gap { .. } => None,
        })
        .collect()
}

fn split_blocks(text: &str, keep: Keep<'_>) -> Vec<Piece> {
    let mut sink = Sink { pieces: Vec::new(), index: 0, prev_last: 0, keep };
    let mut open: Option<MdBlock> = None;
    let mut fence: Option<(char, usize)> = None;
    for (index, line) in text.lines().enumerate() {
        let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let trimmed = line.trim();
        if let Some((marker, len)) = fence {
            let closes = trimmed.chars().all(|c| c == marker) && trimmed.len() >= len;
            extend(&mut open, line, number);
            if closes {
                fence = None;
                sink.take(open.take());
            }
            continue;
        }
        if trimmed.is_empty() {
            sink.take(open.take());
        } else if let Some(opened) = fence_start(trimmed) {
            sink.take(open.take());
            open = Some(MdBlock { kind: MdKind::Code, first: number, last: number, lines: vec![line.to_string()] });
            fence = Some(opened);
        } else if let Some(level) = heading_level(trimmed) {
            sink.take(open.take());
            sink.take(Some(MdBlock { kind: MdKind::Heading(level), first: number, last: number, lines: vec![line.to_string()] }));
        } else {
            let kind = line_kind(trimmed);
            match &open {
                Some(block) if continues(block.kind, kind) => extend(&mut open, line, number),
                _ => {
                    sink.take(open.take());
                    open = Some(MdBlock { kind, first: number, last: number, lines: vec![line.to_string()] });
                }
            }
        }
    }
    sink.take(open);
    sink.pieces
}

fn extend(open: &mut Option<MdBlock>, line: &str, number: u32) {
    if let Some(block) = open {
        block.lines.push(line.to_string());
        block.last = number;
    }
}

/// The marker and length of a code fence opening this line.
fn fence_start(trimmed: &str) -> Option<(char, usize)> {
    let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = trimmed.chars().take_while(|c| *c == marker).count();
    (len >= 3).then_some((marker, len))
}

fn heading_level(trimmed: &str) -> Option<u8> {
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    let rest = &trimmed[hashes..];
    ((1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with(' '))).then(|| u8::try_from(hashes).unwrap_or(6))
}

fn line_kind(trimmed: &str) -> MdKind {
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    let is_rule = compact.len() >= 3 && ['-', '*', '_'].iter().any(|r| compact.chars().all(|c| c == *r));
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    let ordered = digits > 0 && trimmed[digits..].starts_with(['.', ')']) && trimmed[digits + 1..].starts_with(' ');
    if trimmed.starts_with('>') {
        MdKind::Quote
    } else if trimmed.starts_with('|') {
        MdKind::Table
    } else if is_rule {
        MdKind::Rule
    } else if ordered || ["- ", "* ", "+ "].iter().any(|bullet| trimmed.starts_with(bullet)) {
        MdKind::List
    } else {
        MdKind::Paragraph
    }
}

/// Whether a line of kind `next` belongs to the open block of kind `open` rather than starting a block.
fn continues(open: MdKind, next: MdKind) -> bool {
    match (open, next) {
        (a, b) if a == b => !matches!(a, MdKind::Rule),
        // Wrapped lines of a bullet or quote, and `---` under a paragraph.
        (MdKind::List | MdKind::Quote, MdKind::Paragraph) | (MdKind::Paragraph, MdKind::Rule) => true,
        _ => false,
    }
}

/// How a rendered block relates to the diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Unchanged,
    /// Every line of the block is new.
    Added,
    Changed,
}

impl Mark {
    /// The word that carries the mark where colour does not.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Mark::Unchanged => None,
            Mark::Added => Some("Added"),
            Mark::Changed => Some("Changed"),
        }
    }
}

/// A block of the head version and what the diff says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdBlockView {
    pub block: MdBlock,
    pub mark: Mark,
    /// The text the diff removed in the same place, for word marks; empty when nothing was removed there.
    pub old: String,
    /// The diff line a comment on this block lands on: the block's last added line, else its last line the diff shows.
    /// `None` when the diff shows none of its lines, so the forge would refuse a comment there.
    pub anchor: Option<LineRef>,
}

/// One entry of a rendered Markdown file, in reading order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdItem {
    Block(MdBlockView),
    /// Text the diff removed, shown where it was.
    Removed(String),
    /// What the file has that this list leaves out: whole blocks, and removed paragraphs.
    Omitted {
        blocks: usize,
        removed: usize,
    },
}

/// What a file's hunks say, by line number of the head version.
#[derive(Default)]
struct HunkLines {
    added: BTreeSet<u32>,
    shown: BTreeMap<u32, LineRef>,
    /// Each run of removed lines with the head line that follows it.
    removed: Vec<(u32, String)>,
}

impl HunkLines {
    fn of(file: &FileDiff) -> Self {
        let mut lines = HunkLines::default();
        for hunk in &file.hunks {
            let (mut pending, mut last) = (Vec::new(), 0);
            for line in &hunk.lines {
                let Some(number) = line.new else {
                    pending.push(line.text.as_str());
                    continue;
                };
                if !pending.is_empty() {
                    lines.removed.push((number, pending.join("\n")));
                    pending.clear();
                }
                last = number;
                lines.shown.insert(number, LineRef { old: line.old, new: line.new });
                if line.kind == LineKind::Added {
                    lines.added.insert(number);
                }
            }
            if !pending.is_empty() {
                lines.removed.push((last.saturating_add(1), pending.join("\n")));
            }
        }
        lines
    }
}

/// Blocks of a file's head kept whatever the diff says, from the top.
pub const MD_HEAD_BLOCKS: usize = 1000;
/// Blocks the diff touched kept beyond the first [`MD_HEAD_BLOCKS`].
pub const MD_CHANGED_BLOCKS: usize = 1000;
/// Removed paragraphs kept.
pub const MD_REMOVED_ITEMS: usize = 1000;

impl HunkLines {
    /// Whether the diff touched a block over lines `first..=last` that follows a block ending at `prev_last`: a new
    /// line in it, or removed text in front of it or inside it.
    fn touches(&self, prev_last: u32, first: u32, last: u32) -> bool {
        let removed = self.removed.partition_point(|(before, _)| *before <= prev_last);
        self.added.range(first..=last).next().is_some() || self.removed.get(removed).is_some_and(|(before, _)| *before <= last)
    }
}

/// The head version of a Markdown file as blocks, each marked by what the diff changed in it, with the removed text
/// put where it was. Every block says which source line a comment on it posts to. A file of very many blocks keeps
/// the first [`MD_HEAD_BLOCKS`] and up to [`MD_CHANGED_BLOCKS`] more that the diff touched, and says how many it left out.
pub fn md_items(head: &str, file: &FileDiff) -> Vec<MdItem> {
    let lines = HunkLines::of(file);
    let mut changed_kept = 0;
    let pieces = split_blocks(head, &mut |index, first, last, prev_last| {
        if index < MD_HEAD_BLOCKS {
            return true;
        }
        let keep = changed_kept < MD_CHANGED_BLOCKS && lines.touches(prev_last, first, last);
        changed_kept += usize::from(keep);
        keep
    });
    let count = pieces.len();
    // Removed text goes in front of the kept block it belongs to; text that belonged to a left-out block is counted, not shown.
    let mut by_slot: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    let (mut shown, mut left_out) = (0, 0);
    for (before, text) in &lines.removed {
        let slot = pieces.partition_point(|piece| piece.last() < *before);
        if shown < MD_REMOVED_ITEMS && !matches!(pieces.get(slot), Some(Piece::Gap { .. })) {
            by_slot.entry(slot).or_default().push(text.as_str());
            shown += 1;
        } else {
            left_out += 1;
        }
    }
    let mut items = Vec::new();
    for (at, piece) in pieces.iter().enumerate() {
        let old = by_slot.get(&at).map_or(&[][..], Vec::as_slice);
        match piece {
            Piece::Gap { blocks, .. } => items.push(MdItem::Omitted { blocks: *blocks, removed: 0 }),
            Piece::Block(block) => {
                items.extend(old.iter().map(|text| MdItem::Removed((*text).to_string())));
                items.push(MdItem::Block(block_view(block, &lines, old)));
            }
        }
    }
    items.extend(by_slot.get(&count).into_iter().flatten().map(|text| MdItem::Removed((*text).to_string())));
    if left_out > 0 {
        items.push(MdItem::Omitted { blocks: 0, removed: left_out });
    }
    items
}

fn block_view(block: &MdBlock, lines: &HunkLines, removed_here: &[&str]) -> MdBlockView {
    let range = block.first..=block.last;
    let added = range.clone().filter(|n| lines.added.contains(n)).count();
    let nonblank = block.lines.iter().filter(|l| !l.trim().is_empty()).count();
    // A removal inside the block (a deleted bullet) changes it even though no line of it is new.
    let after_first = lines.removed.partition_point(|(before, _)| *before <= block.first);
    let inside = lines.removed.get(after_first).is_some_and(|(before, _)| *before <= block.last);
    let mark = match (added, inside) {
        (0, false) => Mark::Unchanged,
        (n, false) if n >= nonblank && removed_here.is_empty() => Mark::Added,
        _ => Mark::Changed,
    };
    let old = if added > 0 { removed_here.join("\n") } else { String::new() };
    let anchor = range
        .clone()
        .rev()
        .find(|n| lines.added.contains(n))
        .or_else(|| range.rev().find(|n| lines.shown.contains_key(n)))
        .and_then(|n| lines.shown.get(&n).copied());
    MdBlockView { block: block.clone(), mark, old, anchor }
}

/// Splits `text` into words with their spaces and tells which are new against `old`: a word is new when `old` has no
/// word with the same letters, whatever its punctuation. Pieces with no letter are never new.
pub fn mark_words(old: &str, text: &str) -> Vec<(String, bool)> {
    let core = |word: &str| word.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    let known: HashSet<String> = old.split_whitespace().map(core).collect();
    text.split_inclusive(char::is_whitespace)
        .map(|piece| {
            let word = core(piece);
            let new = !word.is_empty() && !known.contains(&word);
            (piece.to_string(), new)
        })
        .collect()
}

/// What a notebook cell holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    Markdown,
    Code,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub id: Option<String>,
    pub kind: CellKind,
    /// `In [n]` of a code cell that ran.
    pub count: Option<u64>,
    pub source: String,
    /// Each output as text, clipped; one that is not text says what it is and that it is not drawn.
    pub outputs: Vec<String>,
    /// A fingerprint of each output's whole text, so a change past the clip still shows as a change.
    pub text_marks: Vec<u64>,
    /// A fingerprint of each output's non-text parts (images, HTML, JSON) by type and content; 0 for none.
    pub media_marks: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notebook {
    pub cells: Vec<Cell>,
}

/// The most lines of one output a notebook view keeps.
const OUTPUT_LINES: usize = 40;

/// Reads an nbformat 4 notebook. Anything else is an error saying why, never an empty notebook.
pub fn parse_notebook(text: &str) -> Result<Notebook, String> {
    let json: Value = serde_json::from_str(text).map_err(|e| format!("not a notebook: {e}"))?;
    let version = json.get("nbformat").and_then(Value::as_u64).ok_or("not a notebook: it has no nbformat")?;
    let cells = json
        .get("cells")
        .and_then(Value::as_array)
        .filter(|_| version >= 4)
        .ok_or_else(|| format!("this notebook is nbformat {version}; only version 4 shows as cells"))?;
    Ok(Notebook { cells: cells.iter().map(cell_of).collect() })
}

fn cell_of(cell: &Value) -> Cell {
    let kind = match cell.get("cell_type").and_then(Value::as_str) {
        Some("markdown") => CellKind::Markdown,
        Some("code") => CellKind::Code,
        _ => CellKind::Raw,
    };
    let mut outputs = Vec::new();
    let (mut text_marks, mut media_marks) = (Vec::new(), Vec::new());
    for output in cell.get("outputs").and_then(Value::as_array).into_iter().flatten().filter_map(output_of) {
        outputs.push(output.shown);
        text_marks.push(output.text_mark);
        media_marks.push(output.media_mark);
    }
    Cell {
        id: cell.get("id").and_then(Value::as_str).map(str::to_string),
        kind,
        count: cell.get("execution_count").and_then(Value::as_u64),
        source: joined(cell.get("source")),
        outputs,
        text_marks,
        media_marks,
    }
}

/// A notebook text field is a string or a list of lines.
fn joined(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

struct Output {
    shown: String,
    text_mark: u64,
    media_mark: u64,
}

/// FNV-1a over `parts`, with a separator between them, so a fingerprint does not change with the Rust version.
fn fingerprint(parts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in parts.iter().flat_map(|part| part.bytes().chain(std::iter::once(0))) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn output_of(output: &Value) -> Option<Output> {
    let mut media_mark = 0;
    let text = match output.get("output_type").and_then(Value::as_str)? {
        "stream" => joined(output.get("text")),
        "error" => {
            let field = |name| output.get(name).and_then(Value::as_str).unwrap_or_default();
            format!("{}: {}", field("ename"), field("evalue"))
        }
        _ => {
            let data = output.get("data")?.as_object()?;
            let mut parts: Vec<String> = data.get("text/plain").map(|t| joined(Some(t))).into_iter().collect();
            let others: Vec<(&String, String)> = data
                .iter()
                .filter(|(mime, _)| *mime != "text/plain")
                .map(|(mime, value)| (mime, if value.is_array() || value.is_string() { joined(Some(value)) } else { value.to_string() }))
                .collect();
            parts.extend(others.iter().map(|(mime, _)| format!("[{mime} output, not drawn here]")));
            if !others.is_empty() {
                let all: Vec<&str> = others.iter().flat_map(|(mime, content)| [mime.as_str(), content.as_str()]).collect();
                media_mark = fingerprint(&all);
            }
            parts.join("\n")
        }
    };
    Some(Output { text_mark: fingerprint(&[&text]), media_mark, shown: clip_lines(&text, OUTPUT_LINES) })
}

/// The most characters of one output line a notebook view keeps.
const OUTPUT_LINE_CHARS: usize = 2000;

/// The first `max` lines of `text`, each cut to [`OUTPUT_LINE_CHARS`], with a closing line saying what was left out.
fn clip_lines(text: &str, max: usize) -> String {
    let mut cut = 0;
    let kept: Vec<&str> = text
        .lines()
        .take(max)
        .map(|line| match line.char_indices().nth(OUTPUT_LINE_CHARS) {
            Some((at, _)) => {
                cut += line.get(at..).map_or(0, |rest| rest.chars().count());
                line.get(..at).unwrap_or(line)
            }
            None => line,
        })
        .collect();
    let more = text.lines().count().saturating_sub(max);
    let mut out = kept.join("\n").trim_end().to_string();
    match (more, cut) {
        (0, 0) => {}
        (lines, 0) => out.push_str(&format!("\n… {lines} more lines, in the source view")),
        (0, chars) => out.push_str(&format!("\n… {chars} more characters, in the source view")),
        (lines, chars) => out.push_str(&format!("\n… {lines} more lines and {chars} more characters, in the source view")),
    }
    out
}

/// What changed in a cell of the head notebook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellChange {
    Unchanged,
    /// The source changed.
    Modified,
    Added,
    /// The source is the same and the text outputs changed.
    OutputChanged,
    /// The source and text are the same and an image or other non-text output changed. It cannot be drawn here.
    MediaChanged,
}

/// Each head cell against the base notebook, and the base cells the head no longer has. Cells match by id when both
/// have one, else by identical source.
pub fn cell_changes(base: Option<&Notebook>, head: &Notebook) -> (Vec<CellChange>, Vec<Cell>) {
    let Some(base) = base else { return (vec![CellChange::Added; head.cells.len()], Vec::new()) };
    let mut taken = vec![false; base.cells.len()];
    let changes = head
        .cells
        .iter()
        .map(|cell| {
            let found = (0..base.cells.len()).filter(|i| !taken[*i]).find(|i| {
                let old = &base.cells[*i];
                match (&old.id, &cell.id) {
                    (Some(a), Some(b)) => a == b,
                    _ => old.source == cell.source,
                }
            });
            match found {
                Some(i) => {
                    taken[i] = true;
                    let old = &base.cells[i];
                    if old.source != cell.source {
                        CellChange::Modified
                    } else if old.outputs != cell.outputs || old.text_marks != cell.text_marks {
                        CellChange::OutputChanged
                    } else if old.media_marks != cell.media_marks {
                        CellChange::MediaChanged
                    } else {
                        CellChange::Unchanged
                    }
                }
                None => CellChange::Added,
            }
        })
        .collect();
    let removed = base.cells.iter().zip(&taken).filter(|(_, taken)| !**taken).map(|(cell, _)| cell.clone()).collect();
    (changes, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified;

    fn diff(text: &str) -> FileDiff {
        parse_unified(text).into_iter().next().expect("one file")
    }

    #[test]
    fn a_path_offers_its_rendered_views_first_and_source_always() {
        for path in ["docs/a.md", "A.MARKDOWN", "x/icon.svg", "a.png", "b.JPG", "c.gif", "d.webp", "n.ipynb", "Makefile", "lib.rs"] {
            let views = views_for(path);
            assert_eq!(views.last(), Some(&FileView::Source), "{path}");
        }
        assert_eq!(views_for("a.md")[0], FileView::Rendered);
        assert!(views_for("a.png").contains(&FileView::ImageDifference));
        assert_eq!(views_for("lib.rs"), [FileView::Source]);
    }

    #[test]
    fn a_remembered_view_applies_only_where_the_path_offers_it() {
        assert_eq!(default_view("a.md", Some(FileView::Source)), FileView::Source);
        assert_eq!(default_view("a.png", Some(FileView::ImageSwipe)), FileView::ImageSwipe);
        assert_eq!(default_view("a.md", Some(FileView::ImageSwipe)), FileView::Rendered, "an image mode means nothing for Markdown");
        assert_eq!(default_view("a.md", None), FileView::Rendered);
        assert_eq!(toggled("a.md", FileView::Rendered), FileView::Source);
        assert_eq!(toggled("a.md", FileView::Source), FileView::Rendered);
        assert_eq!(toggled("lib.rs", FileView::Source), FileView::Source);
        let svg = [FileView::Rendered, FileView::Structure, FileView::Source, FileView::Rendered];
        assert!(svg.windows(2).all(|w| toggled("icon.svg", w[0]) == w[1]), "an SVG steps through its three views");
    }

    const DOC: &str = "# Webhooks\n\nPR Craft sends a POST.\nEach delivery is signed.\n\n## Retries\n\n- one\n- two\n  wrapped\n\n```yaml\na: 1\n\nb: 2\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> quoted\n\n---\n";

    #[test]
    fn blocks_know_their_source_lines() {
        let blocks = md_blocks(DOC);
        let spans: Vec<_> = blocks.iter().map(|b| (b.kind, b.first, b.last)).collect();
        assert_eq!(
            spans,
            [
                (MdKind::Heading(1), 1, 1),
                (MdKind::Paragraph, 3, 4),
                (MdKind::Heading(2), 6, 6),
                (MdKind::List, 8, 10),
                (MdKind::Code, 12, 16),
                (MdKind::Table, 18, 20),
                (MdKind::Quote, 22, 22),
                (MdKind::Rule, 24, 24),
            ]
        );
        assert_eq!(blocks[4].lines.len(), 5, "the blank line inside the fence stays in the block");
    }

    #[test]
    fn an_unclosed_fence_and_a_hash_without_a_space_do_not_swallow_the_file_or_make_headings() {
        let blocks = md_blocks("#tag\n\n```\ncode\n");
        assert_eq!(blocks[0].kind, MdKind::Paragraph);
        assert_eq!((blocks[1].kind, blocks[1].last), (MdKind::Code, 4));
    }

    const PATCH: &str = "diff --git a/d.md b/d.md\n--- a/d.md\n+++ b/d.md\n@@ -2,6 +2,7 @@\n \n Intro line.\n \n-Failed deliveries are retried 3 times.\n+Failed deliveries are retried up to 6 times.\n+Every attempt is recorded.\n \n Tail.\n";
    const HEAD: &str = "# T\n\nIntro line.\n\nFailed deliveries are retried up to 6 times.\nEvery attempt is recorded.\n\nTail.\n\nFar away.\n";

    #[test]
    fn a_changed_block_anchors_to_its_last_added_line_and_names_what_was_removed() {
        let items = md_items(HEAD, &diff(PATCH));
        let blocks: Vec<&MdBlockView> = items.iter().filter_map(|i| if let MdItem::Block(b) = i { Some(b) } else { None }).collect();
        let changed = blocks.iter().find(|b| b.block.first == 5).expect("the replaced paragraph");
        assert_eq!(changed.mark, Mark::Changed);
        assert_eq!(changed.anchor, Some(LineRef { old: None, new: Some(6) }));
        assert!(changed.old.contains("3 times"));
        let removed_at = items.iter().position(|i| matches!(i, MdItem::Removed(text) if text.contains("3 times"))).expect("removed text");
        let changed_at = items.iter().position(|i| matches!(i, MdItem::Block(b) if b.block.first == 5)).expect("block");
        assert!(removed_at < changed_at, "the removed text sits where it was");
    }

    #[test]
    fn an_untouched_block_anchors_to_its_last_line_in_the_diff_or_to_nothing() {
        let items = md_items(HEAD, &diff(PATCH));
        let block = |first: u32| items.iter().find_map(|i| if let MdItem::Block(b) = i { Some(b).filter(|b| b.block.first == first) } else { None });
        let intro = block(3).expect("intro");
        assert_eq!((intro.mark, intro.anchor), (Mark::Unchanged, Some(LineRef { old: Some(3), new: Some(3) })));
        assert_eq!(block(10).expect("far away").anchor, None, "outside every hunk, so the forge would refuse a comment");
    }

    #[test]
    fn a_block_of_only_new_lines_is_added() {
        let patch = "diff --git a/d.md b/d.md\n--- a/d.md\n+++ b/d.md\n@@ -1,3 +1,5 @@\n # T\n \n+New paragraph.\n+\n Intro.\n";
        let items = md_items("# T\n\nNew paragraph.\n\nIntro.\n", &diff(patch));
        let added = items.iter().find_map(|i| if let MdItem::Block(b) = i { Some(b).filter(|b| b.block.first == 3) } else { None }).expect("block");
        assert_eq!(added.mark, Mark::Added);
        assert!(added.old.is_empty());
    }

    #[test]
    fn words_new_to_the_paragraph_are_marked_whatever_their_punctuation() {
        let marked = mark_words("Failed deliveries are retried 3 times, one minute apart.", "Failed deliveries are retried up to 6 times, sooner.");
        let new: Vec<&str> = marked.iter().filter(|(_, new)| *new).map(|(w, _)| w.trim()).collect();
        assert_eq!(new, ["up", "to", "6", "sooner."]);
        assert_eq!(marked.iter().map(|(w, _)| w.as_str()).collect::<String>(), "Failed deliveries are retried up to 6 times, sooner.");
    }

    fn notebook(cells: &str) -> String {
        format!(r#"{{"nbformat":4,"nbformat_minor":5,"metadata":{{}},"cells":[{cells}]}}"#)
    }

    #[test]
    fn a_notebook_shows_sources_and_text_outputs_and_names_outputs_it_cannot_draw() {
        let text = notebook(
            r##"{"cell_type":"markdown","id":"a","source":["# Retry\n","delays"]},
               {"cell_type":"code","id":"b","execution_count":5,"source":"x = 1","outputs":[
                 {"output_type":"stream","name":"stdout","text":["1\n","2\n"]},
                 {"output_type":"display_data","data":{"image/png":"AAAA","text/plain":["<Figure>"]}},
                 {"output_type":"error","ename":"ValueError","evalue":"bad","traceback":[]}]}"##,
        );
        let nb = parse_notebook(&text).expect("nbformat 4");
        assert_eq!(nb.cells[0].source, "# Retry\ndelays");
        assert_eq!((nb.cells[1].kind, nb.cells[1].count), (CellKind::Code, Some(5)));
        assert_eq!(nb.cells[1].outputs[0], "1\n2");
        assert!(nb.cells[1].outputs[1].contains("<Figure>") && nb.cells[1].outputs[1].contains("image/png output, not drawn"));
        assert_eq!(nb.cells[1].outputs[2], "ValueError: bad");
    }

    #[test]
    fn an_old_or_broken_notebook_is_an_error_not_an_empty_one() {
        assert!(parse_notebook(r#"{"nbformat":3,"worksheets":[]}"#).expect_err("v3").contains("nbformat 3"));
        assert!(parse_notebook("{").is_err());
        assert!(parse_notebook(r#"{"cells":[]}"#).is_err());
    }

    #[test]
    fn a_long_output_is_clipped_and_says_so() {
        let lines: Vec<String> = (0..100).map(|n| format!("\"{n}\\n\"")).collect();
        let text = notebook(&format!(
            r#"{{"cell_type":"code","source":"","outputs":[{{"output_type":"stream","name":"stdout","text":[{}]}}]}}"#,
            lines.join(",")
        ));
        let nb = parse_notebook(&text).expect("parses");
        assert!(nb.cells[0].outputs[0].contains("60 more lines"), "{}", nb.cells[0].outputs[0]);
    }

    #[test]
    fn cells_are_told_apart_by_id_then_by_source() {
        let base = parse_notebook(&notebook(
            r#"{"cell_type":"code","id":"a","source":"x","outputs":[]},{"cell_type":"code","id":"b","source":"y","outputs":[]},{"cell_type":"markdown","source":"gone"}"#,
        ))
        .expect("base");
        let head = parse_notebook(&notebook(
            r#"{"cell_type":"code","id":"a","source":"x","outputs":[]},{"cell_type":"code","id":"b","source":"y2","outputs":[]},{"cell_type":"code","id":"c","source":"new","outputs":[]}"#,
        ))
        .expect("head");
        let (changes, removed) = cell_changes(Some(&base), &head);
        assert_eq!(changes, [CellChange::Unchanged, CellChange::Modified, CellChange::Added]);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].source, "gone");
        assert_eq!(cell_changes(None, &head).0, [CellChange::Added; 3]);
    }

    fn code_cell(output: &str) -> Notebook {
        parse_notebook(&notebook(&format!(r#"{{"cell_type":"code","id":"a","source":"plot()","outputs":[{output}]}}"#))).expect("parses")
    }

    #[test]
    fn an_image_only_output_change_is_not_unchanged_and_names_the_image() {
        let image = |data: &str| format!(r#"{{"output_type":"display_data","data":{{"image/png":"{data}","text/plain":["<Figure>"]}}}}"#);
        let (old, same, new) = (code_cell(&image("AAAA")), code_cell(&image("AAAA")), code_cell(&image("BBBB")));
        assert_eq!(cell_changes(Some(&old), &same).0, [CellChange::Unchanged]);
        assert_eq!(cell_changes(Some(&old), &new).0, [CellChange::MediaChanged], "same text, other pixels");
    }

    #[test]
    fn a_text_output_change_past_the_clip_is_still_a_change() {
        let lines = |last: &str| {
            let mut all: Vec<String> = (0..60).map(|n| format!("\"{n}\\n\"")).collect();
            all.push(format!("\"{last}\""));
            format!(r#"{{"output_type":"stream","name":"stdout","text":[{}]}}"#, all.join(","))
        };
        let (old, new) = (code_cell(&lines("a")), code_cell(&lines("b")));
        assert_eq!(old.cells[0].outputs, new.cells[0].outputs, "the clip hides the difference");
        assert_eq!(cell_changes(Some(&old), &new).0, [CellChange::OutputChanged]);
    }
}

#[cfg(test)]
mod long_line_tests {
    use super::*;

    #[test]
    fn a_one_megabyte_output_line_is_cut_and_the_fingerprint_still_sees_all_of_it() {
        let cell = |tail: &str| {
            let output = format!(r#"{{"output_type":"stream","name":"stdout","text":["{}{tail}"]}}"#, "x".repeat(1 << 20));
            parse_notebook(&format!(r#"{{"nbformat":4,"cells":[{{"cell_type":"code","id":"a","source":"s","outputs":[{output}]}}]}}"#))
                .expect("parses")
        };
        let (old, new) = (cell("a"), cell("b"));
        let shown = &old.cells[0].outputs[0];
        assert!(shown.chars().count() < 2100, "{}", shown.chars().count());
        assert!(shown.contains("more characters"));
        assert_eq!(cell_changes(Some(&old), &new).0, [CellChange::OutputChanged]);
    }
}

#[cfg(test)]
mod many_blocks_tests {
    use super::*;
    use crate::diff::parse_unified;

    #[test]
    fn a_file_of_very_many_blocks_keeps_the_first_ones_and_the_changed_ones_and_counts_the_rest() {
        // 100,000 one-line blocks `a`, each followed by a blank line. Line 2k-1 is block k.
        let blocks = 100_000;
        let head = "a\n\n".repeat(blocks);
        let (added, removed_before) = (179_999, 120_001);
        let patch = format!(
            "diff --git a/m.md b/m.md\n--- a/m.md\n+++ b/m.md\n@@ -{r},2 +{r},1 @@\n \n-gone\n@@ -179998,2 +179998,3 @@\n \n+a\n \n",
            r = removed_before - 1
        );
        let file = parse_unified(&patch).remove(0);
        let items = md_items(&head, &file);
        let (mut kept, mut omitted, mut removed_text) = (0, 0, 0);
        for item in &items {
            match item {
                MdItem::Block(_) => kept += 1,
                MdItem::Omitted { blocks, .. } => omitted += blocks,
                MdItem::Removed(_) => removed_text += 1,
            }
        }
        assert!(kept <= MD_HEAD_BLOCKS + MD_CHANGED_BLOCKS, "kept {kept} of {blocks} blocks");
        assert_eq!(kept + omitted, blocks, "every block is either shown or counted");
        assert!(omitted > 0 && items.iter().any(|i| matches!(i, MdItem::Omitted { blocks, .. } if *blocks > 0)));
        let changed: Vec<u32> = items
            .iter()
            .filter_map(|i| if let MdItem::Block(b) = i { Some(b).filter(|b| b.mark != Mark::Unchanged) } else { None })
            .map(|b| b.block.first)
            .collect();
        assert!(changed.contains(&added), "the changed block far down the file is kept: {changed:?}");
        assert_eq!(removed_text, 1, "so is the removed text in front of the block it belongs to");
    }
}
