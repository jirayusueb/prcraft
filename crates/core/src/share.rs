//! Share images (PRODUCT.md "Infographic"): what the image says, and its SVG. Pure text; the engine fills the model
//! from a change and `pr-craft-share-render` turns the SVG into a PNG.
//!
//! The image carries titles, numbers and people, never code or comment text: the model has no field for either, and
//! every string that reaches the SVG passes [`clean`], [`fit`] and escaping.

use std::fmt::Write;

use serde::{Deserialize, Serialize};

/// What the image is about (Figma frame 19's Template).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareKind {
    ReviewSummary,
    ReleaseNote,
    StackProgress,
}

impl ShareKind {
    pub const ALL: [ShareKind; 3] = [ShareKind::ReviewSummary, ShareKind::ReleaseNote, ShareKind::StackProgress];

    pub fn word(self) -> &'static str {
        match self {
            ShareKind::ReviewSummary => "Review summary",
            ShareKind::ReleaseNote => "Release note",
            ShareKind::StackProgress => "Stack progress",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            ShareKind::ReviewSummary => "What changed and how it was reviewed",
            ShareKind::ReleaseNote => "For the channel, after merge",
            ShareKind::StackProgress => "The layers of a stack and where each stands",
        }
    }

    /// For file names.
    pub fn slug(self) -> &'static str {
        match self {
            ShareKind::ReviewSummary => "review-summary",
            ShareKind::ReleaseNote => "release-note",
            ShareKind::StackProgress => "stack-progress",
        }
    }
}

/// The image sizes of Figma frame 19.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareSize {
    /// The link-card size: 1200×630.
    Wide1200x630,
    /// 1080×1080.
    Square1080,
    /// 1280×720, 16:9. The frame names no pixel size for Slack; Slack shows an inline image scaled to its message
    /// column, so any size reads well and a 16:9 frame fills it without cropping.
    Slack,
}

impl ShareSize {
    pub const ALL: [ShareSize; 3] = [ShareSize::Wide1200x630, ShareSize::Square1080, ShareSize::Slack];

    pub fn pixels(self) -> (u32, u32) {
        match self {
            ShareSize::Wide1200x630 => (1200, 630),
            ShareSize::Square1080 => (1080, 1080),
            ShareSize::Slack => (1280, 720),
        }
    }

    /// `1200×630`.
    pub fn dimensions(self) -> String {
        let (w, h) = self.pixels();
        format!("{w}×{h}")
    }

    pub fn word(self) -> &'static str {
        match self {
            ShareSize::Wide1200x630 => "1200×630",
            ShareSize::Square1080 => "Square",
            ShareSize::Slack => "Slack",
        }
    }

    /// For file names.
    pub fn slug(self) -> &'static str {
        match self {
            ShareSize::Wide1200x630 => "1200x630",
            ShareSize::Square1080 => "square",
            ShareSize::Slack => "slack",
        }
    }
}

/// Which optional parts the image carries (Figma frame 19's Include list). Checks are not here: PR Craft does not read
/// them yet, so there is nothing to include.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Include {
    pub diff_stats: bool,
    pub people: bool,
    /// Off by default: a private repository's name is not for a channel the repository's members do not share.
    pub repo_name: bool,
}

impl Default for Include {
    fn default() -> Self {
        Include { diff_stats: true, people: true, repo_name: false }
    }
}

/// One number of the stats row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stat {
    pub label: String,
    pub value: String,
    /// A line under the value; empty for none.
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerState {
    Draft,
    InReview,
}

impl LayerState {
    pub fn word(self) -> &'static str {
        match self {
            LayerState::Draft => "Draft",
            LayerState::InReview => "In review",
        }
    }
}

/// One layer of a stack, bottom first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Layer {
    pub title: String,
    /// `#482`; the repository is not repeated on every row.
    pub number: String,
    pub state: LayerState,
    /// The change this image was made for.
    pub current: bool,
}

/// The Ember dot-matrix number and what it counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Big {
    pub value: u64,
    pub caption: String,
}

/// Everything an image says. Built from titles, counts and names only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShareModel {
    pub kind: ShareKind,
    /// `Review summary · Nov 14`.
    pub eyebrow: String,
    pub headline: String,
    /// `acme/api #482`, or `#482` when the repository name is left out.
    pub reference: String,
    /// One line of the sharer's own words; the image shows it under the headline.
    pub summary: Option<String>,
    pub big: Big,
    /// The stats row; empty for a stack.
    pub stats: Vec<Stat>,
    pub people: Vec<String>,
    /// Stack progress only.
    pub layers: Vec<Layer>,
    /// What the image does not show and why, so no caller presents it as complete (AGENTS.md rule 15).
    pub left_out: Vec<String>,
}

impl ShareModel {
    /// The image as words, for its accessible name and the SVG's title.
    pub fn alt(&self) -> String {
        let mut out = format!(
            "{}. {}. {}. {} {}.",
            short(&self.eyebrow),
            short(&self.headline),
            short(&self.reference),
            self.big.value,
            short(&self.big.caption)
        );
        if let Some(summary) = &self.summary {
            let _ = write!(out, " {}.", short(summary));
        }
        for stat in &self.stats {
            let note = if stat.note.is_empty() { String::new() } else { format!(", {}", short(&stat.note)) };
            let _ = write!(out, " {}: {}{note}.", short(&stat.label), short(&stat.value));
        }
        for layer in self.layers.iter().take(ALT_LAYERS) {
            let _ =
                write!(out, " {} {}, {}{}.", layer.number, short(&layer.title), layer.state.word(), if layer.current { ", this change" } else { "" });
        }
        if !self.people.is_empty() {
            let names: Vec<String> = self.people.iter().take(ALT_LAYERS).map(|p| short(p)).collect();
            let _ = write!(out, " People: {}.", names.join(", "));
        }
        clean(&out)
    }
}

/// Layers and people the alt text names; the image itself shows fewer.
const ALT_LAYERS: usize = 12;

/// `text` cleaned and cut to 120 characters, so a field of any length stays a sentence.
fn short(text: &str) -> String {
    let text = clean(text);
    if text.chars().count() <= 120 { text } else { text.chars().take(119).collect::<String>() + "…" }
}

// ---- text ----

/// `text` as one line that is safe to lay out: control characters (newlines and tabs too), the bidirectional override
/// controls and the two characters XML forbids are gone, and runs of whitespace are one space.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            gap = true;
            continue;
        }
        if matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FFFE}' | '\u{FFFF}') {
            continue;
        }
        if gap && !out.is_empty() {
            out.push(' ');
        }
        gap = false;
        out.push(c);
    }
    out
}

/// How wide `c` sets, in em. Deliberately a little wide, so text in a real font never outgrows a box measured here.
fn advance(c: char) -> f32 {
    match c {
        ' ' | 'i' | 'j' | 'l' | '.' | ',' | ':' | ';' | '\'' | '|' | '!' | '…' => 0.30,
        'm' | 'w' | 'M' | 'W' => 0.90,
        c if c.is_ascii_uppercase() || c.is_ascii_digit() => 0.66,
        c if c.is_ascii() => 0.56,
        c if is_wide(c) => 1.05,
        _ => 0.72,
    }
}

fn is_wide(c: char) -> bool {
    matches!(u32::from(c),
        0x1100..=0x11FF | 0x2600..=0x27BF | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F000..=0x1FAFF | 0x20000..=0x3FFFD)
}

/// A monospaced face sets every narrow character as wide as an `m`.
fn advance_mono(c: char) -> f32 {
    if is_wide(c) { 1.2 } else { 0.62 }
}

fn width_of(chars: &[char], em: f32, adv: fn(char) -> f32) -> f32 {
    chars.iter().map(|&c| adv(c)).sum::<f32>() * em
}

/// `text` cleaned and broken into at most `max_lines` lines that each fit `width` at font size `em`. Breaks fall at
/// spaces where there is one, inside a word where there is not. Text that does not fit ends its last line in `…`.
pub fn fit(text: &str, em: f32, width: f32, max_lines: usize) -> Vec<String> {
    fit_by(text, em, width, max_lines, advance)
}

/// [`fit`] for text set in the monospaced face.
fn fit_mono(text: &str, em: f32, width: f32, max_lines: usize) -> Vec<String> {
    fit_by(text, em, width, max_lines, advance_mono)
}

fn fit_by(text: &str, em: f32, width: f32, max_lines: usize, adv: fn(char) -> f32) -> Vec<String> {
    let chars: Vec<char> = clean(text).chars().collect();
    let mut lines: Vec<String> = Vec::new();
    let mut start = 0;
    while start < chars.len() && lines.len() < max_lines {
        let (mut used, mut end, mut last_space) = (0.0, start, None);
        while end < chars.len() {
            let next = adv(chars[end]) * em;
            if used + next > width && end > start {
                break;
            }
            used += next;
            if chars[end] == ' ' {
                last_space = Some(end);
            }
            end += 1;
        }
        let cut = match last_space {
            Some(space) if end < chars.len() && space > start => space,
            _ => end,
        };
        lines.push(chars[start..cut].iter().collect::<String>().trim_end().to_string());
        start = cut;
        while chars.get(start) == Some(&' ') {
            start += 1;
        }
    }
    if start < chars.len()
        && let Some(last) = lines.last_mut()
    {
        let mut kept: Vec<char> = last.chars().collect();
        while !kept.is_empty() && width_of(&kept, em, adv) + adv('…') * em > width {
            kept.pop();
        }
        while kept.last() == Some(&' ') {
            kept.pop();
        }
        kept.push('…');
        *last = kept.into_iter().collect();
    }
    lines
}

/// The largest size up to `em` at which `text` fits one line of `width`, never below 60% of `em`.
fn fit_em(text: &str, em: f32, width: f32) -> f32 {
    let chars: Vec<char> = clean(text).chars().collect();
    let natural = width_of(&chars, em, advance);
    if natural <= width || natural <= 0.0 { em } else { (em * width / natural).max(em * 0.6) }
}

fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

// ---- the SVG ----

/// Brand tokens of DESIGN.md's Ember-night share image. The image is not a themed screen: it looks the same in light
/// and dark, so its colours live here and not in the UI theme.
const NIGHT: &str = "#111114";
const PANEL: &str = "#1B1B20";
const WHITE: &str = "#FFFFFF";
const SOFT: &str = "#E9E7EE";
/// Secondary text on the night field; 7.8:1 against it.
const DIM: &str = "#A9A6B5";
const EMBER: &str = "#F0603A";
const HAZE: &str = "#C2BCEF";
const MOSS: &str = "#31A667";
const DOT_OFF: &str = "#2A2A31";
/// Geist and Geist Mono are named first and are used when installed; the rest are system fonts present on a Mac.
const SANS: &str = "Geist, 'Helvetica Neue', Helvetica, Arial, sans-serif";
const MONO: &str = "'Geist Mono', Menlo, Monaco, 'Courier New', monospace";

/// A 5×7 dot-matrix glyph per digit, one row per byte, the left dot in bit 4.
const DIGITS: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

#[derive(Clone, Copy, PartialEq)]
enum Anchor {
    Start,
    Middle,
    End,
}

#[derive(Clone, Copy)]
struct Style {
    em: f32,
    fill: &'static str,
    weight: u16,
    mono: bool,
    anchor: Anchor,
}

impl Style {
    fn new(em: f32, fill: &'static str) -> Self {
        Style { em, fill, weight: 400, mono: false, anchor: Anchor::Start }
    }

    fn weight(self, weight: u16) -> Self {
        Style { weight, ..self }
    }

    fn mono(self) -> Self {
        Style { mono: true, ..self }
    }

    fn anchor(self, anchor: Anchor) -> Self {
        Style { anchor, ..self }
    }
}

/// A box in the image.
#[derive(Clone, Copy)]
struct Area {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// The SVG of `model` at `size`: the Ember-night field, white type, one dot-matrix number and a stats row (DESIGN.md
/// "Share image"). Every text box is clipped, so a title the metrics got wrong cannot run into its neighbour.
pub fn svg(model: &ShareModel, size: ShareSize) -> String {
    let (w, h) = size.pixels();
    let mut canvas = Canvas { model, w: w as f32, h: h as f32, out: String::new(), clips: 0 };
    let _ = write!(
        canvas.out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><title>{}</title><rect width="{w}" height="{h}" fill="{NIGHT}"/>"#,
        esc(&model.alt())
    );
    canvas.paint(size != ShareSize::Square1080);
    canvas.out.push_str("</svg>");
    canvas.out
}

struct Canvas<'a> {
    model: &'a ShareModel,
    w: f32,
    h: f32,
    out: String,
    clips: usize,
}

impl Canvas<'_> {
    fn text(&mut self, x: f32, y: f32, style: Style, content: &str) {
        let family = if style.mono { MONO } else { SANS };
        let anchor = match style.anchor {
            Anchor::Start => "",
            Anchor::Middle => r#" text-anchor="middle""#,
            Anchor::End => r#" text-anchor="end""#,
        };
        let _ = write!(
            self.out,
            r#"<text x="{x:.1}" y="{y:.1}" font-family="{family}" font-size="{:.1}" font-weight="{}" fill="{}"{anchor}>{}</text>"#,
            style.em,
            style.weight,
            style.fill,
            esc(&clean(content))
        );
    }

    /// `lines` of text from `(x, first_baseline)`, `step` apart, clipped to `area`. Returns the last baseline.
    fn lines(&mut self, x: f32, first: f32, step: f32, style: Style, area: Area, lines: &[String]) -> f32 {
        self.clips += 1;
        let _ = write!(
            self.out,
            r#"<clipPath id="c{0}"><rect x="{1:.1}" y="{2:.1}" width="{3:.1}" height="{4:.1}"/></clipPath><g clip-path="url(#c{0})">"#,
            self.clips, area.x, area.y, area.w, area.h
        );
        let mut y = first;
        for line in lines {
            self.text(x, y, style, line);
            y += step;
        }
        self.out.push_str("</g>");
        first + lines.len().saturating_sub(1) as f32 * step
    }

    fn rect(&mut self, area: Area, radius: f32, fill: &str, stroke: Option<&str>) {
        let stroke = stroke.map(|s| format!(r#" stroke="{s}" stroke-width="2""#)).unwrap_or_default();
        let _ = write!(
            self.out,
            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="{radius:.1}" fill="{fill}"{stroke}/>"#,
            area.x, area.y, area.w, area.h
        );
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, fill: &str) {
        let _ = write!(self.out, r#"<circle cx="{cx:.1}" cy="{cy:.1}" r="{r:.1}" fill="{fill}"/>"#);
    }

    fn paint(&mut self, landscape: bool) {
        let (w, h) = (self.w, self.h);
        let pad = if landscape { (w * 0.05).round() } else { (w * 0.067).round() };
        let (head_em, ref_em, sum_em) =
            if landscape { ((h * 0.083).round(), (h * 0.036).round(), (h * 0.041).round()) } else { ((w * 0.063).round(), 26.0, 30.0) };
        let column = if landscape { (w * 0.60).round() } else { w - 2.0 * pad };
        let top = self.top_bar(pad, column);

        let step = (head_em * 1.12).round();
        let base = top + 16.0 + head_em;
        let headline = fit(&self.model.headline, head_em, column, 3);
        let last = self.lines(
            pad,
            base,
            step,
            Style::new(head_em, WHITE).weight(500),
            Area { x: pad, y: base - head_em, w: column, h: headline.len() as f32 * step + head_em * 0.3 },
            &headline,
        );
        let ref_y = last + ref_em * 2.0;
        let reference = fit_mono(&self.reference_line(), ref_em, column, 1);
        self.lines(pad, ref_y, 0.0, Style::new(ref_em, DIM).mono(), Area { x: pad, y: ref_y - ref_em, w: column, h: ref_em * 1.4 }, &reference);

        let stack = self.model.kind == ShareKind::StackProgress;
        let big_area = if landscape {
            let x = pad + column + (pad * 0.5).round();
            Area { x, y: top + 40.0, w: w - pad - x, h: h * 0.27 }
        } else {
            Area { x: pad, y: ref_y + if stack { 36.0 } else { 150.0 }, w: w - 2.0 * pad, h: 112.0 }
        };
        let big_bottom = self.big_number(big_area, landscape);

        if stack {
            let start = if landscape { ref_y + 34.0 } else { big_bottom + 48.0 };
            let width = if landscape { column } else { w - 2.0 * pad };
            self.layers(Area { x: pad, y: start, w: width, h: h - pad - start }, (h * 0.08).round());
            return;
        }
        let summary = self.model.summary.as_deref().map(|s| fit(s, sum_em, column, 2)).unwrap_or_default();
        let sum_y = ref_y + sum_em * 1.9;
        let sum_step = (sum_em * 1.3).round();
        self.lines(
            pad,
            sum_y,
            sum_step,
            Style::new(sum_em, SOFT),
            Area { x: pad, y: sum_y - sum_em, w: column, h: summary.len() as f32 * sum_step + sum_em * 0.3 },
            &summary,
        );
        self.stats(pad, landscape);
    }

    /// The brand mark, the eyebrow and the people. Returns where the headline may start.
    fn top_bar(&mut self, pad: f32, column: f32) -> f32 {
        let w = self.w;
        let cy = pad + 12.0;
        self.circle(pad + 12.0, cy, 12.0, EMBER);
        self.text(pad + 36.0, cy + 8.0, Style::new(22.0, WHITE).weight(500), "PR Craft");
        let brand = pad + 36.0 + width_of(&"PR Craft".chars().collect::<Vec<_>>(), 22.0, advance) + 20.0;
        let initials: Vec<String> =
            self.model.people.iter().filter_map(|p| p.chars().find(|c| c.is_alphanumeric())).map(|c| c.to_uppercase().collect()).collect();
        let shown = initials.len().min(4);
        let hidden = initials.len() - shown;
        let chips = shown + usize::from(hidden > 0);
        let chips_width = if chips == 0 { 0.0 } else { chips as f32 * 44.0 };
        let room = (w - pad - chips_width - 24.0 - brand).min(column + (w - 2.0 * pad - column) - 240.0).max(80.0);
        let eyebrow = fit(&self.model.eyebrow, 20.0, room, 1);
        self.lines(brand, cy + 7.0, 0.0, Style::new(20.0, DIM), Area { x: brand, y: cy - 16.0, w: room, h: 36.0 }, &eyebrow);
        let chip = Style::new(17.0, NIGHT).weight(600).anchor(Anchor::Middle);
        for (i, initial) in initials.iter().take(shown).enumerate() {
            let cx = w - pad - 18.0 - i as f32 * 44.0;
            self.circle(cx, cy, 18.0, HAZE);
            self.text(cx, cy + 6.0, chip, initial);
        }
        if hidden > 0 {
            let cx = w - pad - 18.0 - shown as f32 * 44.0;
            self.circle(cx, cy, 18.0, PANEL);
            self.text(cx, cy + 6.0, Style::new(15.0, SOFT).weight(600).anchor(Anchor::Middle), &format!("+{hidden}"));
        }
        pad + 40.0
    }

    /// `acme/api #482 · by ada, grace`.
    fn reference_line(&self) -> String {
        let mut line = self.model.reference.clone();
        if !self.model.people.is_empty() {
            let _ = write!(line, "  ·  by {}", self.model.people.join(", "));
        }
        line
    }

    /// The dot-matrix number with its caption under it (landscape) or beside it (square). Returns the bottom.
    fn big_number(&mut self, area: Area, landscape: bool) -> f32 {
        let digits = self.model.big.value.to_string();
        let count = digits.len() as f32;
        let (max_w, max_h) = if landscape { (area.w, area.h) } else { (area.w * 0.4, area.h) };
        let u = (max_w / (count * 6.0 - 1.0)).min(max_h / 7.0).max(2.0);
        for (i, digit) in digits.bytes().enumerate() {
            let glyph = DIGITS.get(usize::from(digit.wrapping_sub(b'0'))).copied().unwrap_or_default();
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..5 {
                    let on = bits >> (4 - col) & 1 == 1;
                    let (cx, cy) = (area.x + ((i * 6 + col) as f32 + 0.5) * u, area.y + (row as f32 + 0.5) * u);
                    self.circle(cx, cy, u * 0.4, if on { EMBER } else { DOT_OFF });
                }
            }
        }
        let number_w = (count * 6.0 - 1.0) * u;
        let em = if landscape { (self.h * 0.036).round() } else { 26.0 };
        let (x, y, width) = if landscape {
            (area.x, area.y + 7.0 * u + em * 1.6, area.w)
        } else {
            (area.x + number_w + 40.0, area.y + 7.0 * u * 0.5, area.w - number_w - 40.0)
        };
        let caption = fit(&self.model.big.caption, em, width, 2);
        let step = (em * 1.3).round();
        self.lines(x, y, step, Style::new(em, SOFT), Area { x, y: y - em, w: width, h: caption.len() as f32 * step + em * 0.3 }, &caption);
        area.y + 7.0 * u + if landscape { em * 1.6 + step } else { 0.0 }
    }

    /// The stats row along the bottom: one row in landscape, two columns in the square.
    fn stats(&mut self, pad: f32, landscape: bool) {
        let stats = &self.model.stats;
        if stats.is_empty() {
            return;
        }
        let n = stats.len().min(4);
        let gap = 16.0;
        let (columns, cell_h) = if landscape { (n, (self.h * 0.19).round()) } else { (2.min(n), 128.0) };
        let rows = n.div_ceil(columns);
        let cell_w = (self.w - 2.0 * pad - gap * (columns as f32 - 1.0)) / columns as f32;
        let top = self.h - pad - rows as f32 * cell_h - (rows as f32 - 1.0) * gap;
        let stats: Vec<Stat> = stats.iter().take(n).cloned().collect();
        for (i, stat) in stats.iter().enumerate() {
            let (col, row) = (i % columns, i / columns);
            let (x, y) = (pad + col as f32 * (cell_w + gap), top + row as f32 * (cell_h + gap));
            self.rect(Area { x, y, w: cell_w, h: cell_h }, 16.0, PANEL, None);
            let inner = cell_w - 40.0;
            let small = (cell_h * 0.15).round().max(14.0);
            let area = |at: f32, em: f32| Area { x: x + 20.0, y: at - em, w: inner, h: em * 1.4 };
            let label = fit(&stat.label, small, inner, 1);
            self.lines(x + 20.0, y + cell_h * 0.27, 0.0, Style::new(small, DIM), area(y + cell_h * 0.27, small), &label);
            let em = fit_em(&stat.value, (cell_h * 0.34).round(), inner);
            let value = fit(&stat.value, em, inner, 1);
            self.lines(x + 20.0, y + cell_h * 0.64, 0.0, Style::new(em, WHITE).weight(500), area(y + cell_h * 0.64, em), &value);
            if !stat.note.is_empty() {
                let note = fit(&stat.note, small, inner, 1);
                self.lines(x + 20.0, y + cell_h * 0.89, 0.0, Style::new(small, DIM), area(y + cell_h * 0.89, small), &note);
            }
        }
    }

    /// Stack progress: one row per layer that fits `area`, then a count of the layers left out.
    fn layers(&mut self, area: Area, row_h: f32) {
        let layers = self.model.layers.clone();
        let rows = ((area.h / row_h).floor() as usize).max(1);
        let (shown, hidden) = if layers.len() > rows { (rows - 1, layers.len() - rows + 1) } else { (layers.len(), 0) };
        let em = (row_h * 0.36).round();
        let state_w = (area.w * 0.28).max(150.0);
        for (i, layer) in layers.iter().take(shown).enumerate() {
            let y = area.y + i as f32 * row_h;
            let row = Area { x: area.x, y, w: area.w, h: row_h - 10.0 };
            self.rect(row, 14.0, PANEL, layer.current.then_some(EMBER));
            let mid = y + (row_h - 10.0) / 2.0;
            let dot = if layer.state == LayerState::InReview { MOSS } else { HAZE };
            self.circle(area.x + 24.0, mid, 8.0, dot);
            let state = if layer.current { format!("{} · this one", layer.state.word()) } else { layer.state.word().to_string() };
            let state_lines = fit(&state, em * 0.9, state_w - 44.0, 1);
            self.lines(
                area.x + 44.0,
                mid + em * 0.3,
                0.0,
                Style::new(em * 0.9, DIM),
                Area { x: area.x + 44.0, y, w: state_w - 44.0, h: row_h },
                &state_lines,
            );
            let number_w = 100.0;
            self.text(area.x + area.w - 20.0, mid + em * 0.3, Style::new(em * 0.9, DIM).mono().anchor(Anchor::End), &layer.number);
            let title_x = area.x + state_w + 8.0;
            let title_w = (area.w - state_w - 8.0 - number_w - 20.0).max(40.0);
            let title = fit(&layer.title, em, title_w, 1);
            let weight = if layer.current { 600 } else { 400 };
            self.lines(title_x, mid + em * 0.33, 0.0, Style::new(em, WHITE).weight(weight), Area { x: title_x, y, w: title_w, h: row_h }, &title);
        }
        if hidden > 0 {
            let y = area.y + shown as f32 * row_h + row_h * 0.5;
            let more = format!("+{hidden} more layers");
            self.text(area.x + 24.0, y, Style::new(em * 0.9, DIM), &more);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(headline: &str) -> ShareModel {
        ShareModel {
            kind: ShareKind::ReviewSummary,
            eyebrow: "Review summary · Nov 14".into(),
            headline: headline.into(),
            reference: "acme/api #482".into(),
            summary: Some(headline.into()),
            big: Big { value: 6, caption: "files changed".into() },
            stats: vec![Stat { label: "Diff".into(), value: "+184 −37".into(), note: "6 files".into() }],
            people: vec![headline.into(), "grace".into()],
            layers: Vec::new(),
            left_out: Vec::new(),
        }
    }

    const HOSTILE: [&str; 6] = [
        "<script>alert(1)</script> & \"quotes\" 'single'",
        "]]></text><circle r=\"999\"/><text>",
        "nul\u{0} and \u{202E}override\u{202C} and\nnewline\ttab",
        "مرحبا بالعالم، هذا عنوان طويل جدا يتجاوز العرض المتاح للصورة كلها",
        "🔥🚀 emoji ünïcödé 日本語のとても長いタイトルがここに入ります",
        "A_VERY_LONG_UNBROKEN_TITLE_WITHOUT_SPACES_",
    ];

    #[test]
    fn hostile_titles_are_escaped_and_cannot_add_markup() {
        for title in HOSTILE {
            let svg = svg(&model(&title.repeat(40)), ShareSize::Wide1200x630);
            assert!(!svg.contains("<script"), "{title}");
            assert!(!svg.contains("<circle r=\"999\""), "{title}");
            assert!(!svg.chars().any(|c| c.is_control() && c != '\n') && !svg.contains(['\u{202E}', '\u{FFFF}']), "{title}");
            assert_eq!(svg.matches("<svg").count(), 1);
        }
        let escaped = svg(&model("a < b & c"), ShareSize::Slack);
        assert!(escaped.contains("a &lt; b &amp; c"));
    }

    #[test]
    fn a_very_long_title_is_clipped_not_grown() {
        let short = svg(&model("Short title"), ShareSize::Wide1200x630).len();
        let long = svg(&model(&"word ".repeat(5000)), ShareSize::Wide1200x630).len();
        assert!(long < short * 3, "{long} vs {short}");
        let lines = fit(&"word ".repeat(5000), 50.0, 700.0, 3);
        assert_eq!(lines.len(), 3);
        assert!(lines[2].ends_with('…'));
    }

    #[test]
    fn fitted_lines_stay_inside_their_width() {
        for title in HOSTILE {
            for width in [200.0, 700.0] {
                for line in fit(&title.repeat(10), 40.0, width, 3) {
                    let chars: Vec<char> = line.chars().collect();
                    assert!(width_of(&chars, 40.0, advance) <= width + 1.1 * 40.0, "{line:?} is wider than {width}");
                }
            }
        }
    }
}
