//! The shared components of the Figma page `DS · Components` (DESIGN.md → Components): pills, tags, key caps, rows, cards,
//! banners and the title and status bars. Each one paints with the [`Palette`] and text sizes of the current look and names
//! itself to the accessibility tree with a role, a label that starts with its visible words, and its state (AGENTS.md rule 21).

use std::ops::Range;
use std::sync::Arc;

use craft::egui::text::{LayoutJob, TextWrapping};
use craft::egui::{
    self, Align, Color32, CornerRadius, FontId, Galley, Layout, Margin, Pos2, Rect, Response, RichText, Role, Sense, Stroke, StrokeKind, TextFormat,
    Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use pr_craft_core::Forge;
use pr_craft_core::diff::{DiffLine, LineKind};

use crate::theme::{self, Palette};

/// Text sizes of the design, in px at the default interface size; they follow the reader's interface size.
fn sans(ui: &Ui, px: f32) -> FontId {
    FontId::proportional(px * theme::sz(ui).body / 13.5)
}

fn mono(ui: &Ui, px: f32) -> FontId {
    FontId::monospace(px * theme::sz(ui).code / 12.5)
}

fn radius(px: f32) -> CornerRadius {
    CornerRadius::same(px.round().clamp(0.0, 255.0) as u8)
}

fn galley(ui: &Ui, text: &str, font: FontId, colour: Color32) -> Arc<Galley> {
    ui.painter().layout_no_wrap(text.to_string(), font, colour)
}

/// `text` on one line, cut with an ellipsis at `width`.
fn fit(ui: &Ui, text: &str, font: FontId, colour: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_string(), TextFormat::simple(font, colour));
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    ui.painter().layout_job(job)
}

/// Paints `galley` with its left edge at `left`, centred on `centre_y`; returns its right edge.
fn put(ui: &Ui, galley: Arc<Galley>, left: f32, centre_y: f32) -> f32 {
    let size = galley.size();
    ui.painter().galley(pos2(left, centre_y - size.y / 2.0), galley, Color32::PLACEHOLDER);
    left + size.x
}

fn put_right(ui: &Ui, galley: Arc<Galley>, right: f32, centre_y: f32) -> f32 {
    let left = right - galley.size().x;
    put(ui, galley, left, centre_y);
    left
}

fn hairline(ui: &Ui) -> Stroke {
    Stroke::new(theme::sz(ui).hairline, theme::pal(ui).border_strong)
}

/// The focus ring: 2px with a 2px gap outside `rect`, drawn for keyboard focus only (or always, with Keep focus rings visible).
pub(crate) fn ring(ui: &Ui, response: &Response, rect: Rect, corner: f32) {
    if response.has_focus() && theme::ring_visible(ui) {
        let width = if theme::look(ui).focus_rings { 3.0 } else { 2.0 };
        ui.painter().rect_stroke(rect.expand(2.0), radius(corner + 2.0), Stroke::new(width, theme::pal(ui).focus_ring), StrokeKind::Outside);
    }
}

/// [`ring`] for a row that fills its pane: drawn inside `rect`, so the rows around it do not cover it.
pub(crate) fn ring_in(ui: &Ui, response: &Response, rect: Rect, corner: f32) {
    if response.has_focus() && theme::ring_visible(ui) {
        let width = if theme::look(ui).focus_rings { 3.0 } else { 2.0 };
        ui.painter().rect_stroke(rect, radius(corner), Stroke::new(width, theme::pal(ui).focus_ring), StrokeKind::Inside);
    }
}

/// [`ring_in`] for a row or button filled with `inverse`, in the ring colour that shows on it.
pub(crate) fn ring_on_inverse(ui: &Ui, response: &Response, rect: Rect, corner: f32) {
    if response.has_focus() && theme::ring_visible(ui) {
        let width = if theme::look(ui).focus_rings { 3.0 } else { 2.0 };
        ui.painter().rect_stroke(rect, radius(corner), Stroke::new(width, theme::pal(ui).focus_ring_on_inverse), StrokeKind::Inside);
    }
}

/// The weak text colour on a row that may be under the pointer: tertiary at rest, secondary while the row has its hover or pressed
/// fill, where tertiary falls below 4.5:1 (AGENTS.md rule 21).
pub(crate) fn faint(ui: &Ui, response: &Response) -> Color32 {
    let p = theme::pal(ui);
    if quiet_fill(ui, response, true).is_some() { p.text_secondary } else { p.text_tertiary }
}

/// The fill a quiet control takes under the pointer (`bg/hover`) and while pressed (Stone/150): one step up the ramp each, from
/// the ground the control sits on. A ground that already is `bg/hover` (the side panes, the title bar) starts one step higher.
/// `None` at rest, and always for a disabled control.
pub(crate) fn quiet_fill(ui: &Ui, response: &Response, enabled: bool) -> Option<Color32> {
    let p = theme::pal(ui);
    let (hover, pressed) = if ui.visuals().panel_fill == p.hover { (p.pressed, p.border_strong) } else { (p.hover, p.pressed) };
    match enabled {
        false => None,
        true if response.is_pointer_button_down_on() => Some(pressed),
        true if response.hovered() => Some(hover),
        true => None,
    }
}

pub(crate) fn label(ui: &mut Ui, text: impl Into<String>, px: f32, colour: Color32) -> Response {
    ui.label(RichText::new(text.into()).font(sans(ui, px)).color(colour))
}

pub(crate) fn mono_label(ui: &mut Ui, text: impl Into<String>, px: f32, colour: Color32) -> Response {
    ui.label(RichText::new(text.into()).font(mono(ui, px)).color(colour))
}

/// [`mono_label`] cut with an ellipsis at `max` px; its accessible name keeps the whole text.
pub(crate) fn mono_label_cut(ui: &mut Ui, text: impl Into<String>, px: f32, colour: Color32, max: f32) -> Response {
    let text = text.into();
    let (rect, response) = ui.allocate_exact_size(vec2(mono_width(ui, &text, px).min(max.max(16.0)), 18.0), Sense::hover());
    put(ui, fit(ui, &text, mono(ui, px), colour, rect.width()), rect.left(), rect.center().y);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text.clone()));
    response
}

/// The width `text` takes in the code font at `px`.
pub(crate) fn mono_width(ui: &Ui, text: &str, px: f32) -> f32 {
    galley(ui, text, mono(ui, px), Color32::PLACEHOLDER).size().x
}

/// A wrapping line or paragraph of secondary text at the caption size.
pub(crate) fn note(ui: &mut Ui, text: impl Into<String>, colour: Color32) -> Response {
    note_sized(ui, text, 11.5, colour)
}

pub(crate) fn note_sized(ui: &mut Ui, text: impl Into<String>, px: f32, colour: Color32) -> Response {
    ui.add(egui::Label::new(RichText::new(text.into()).font(sans(ui, px)).color(colour)).wrap())
}

/// `add`, inside `pad`.
pub(crate) fn padded<R>(ui: &mut Ui, pad: Margin, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new().inner_margin(pad).show(ui, add).inner
}

/// A pane of the body: `fill` over `rect`, and `add` drawn inside `pad` and clipped to the pane. `id` keeps the panes' widget
/// ids apart.
pub(crate) fn pane_at<R>(ui: &mut Ui, id: &str, rect: Rect, fill: Color32, pad: Margin, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.painter().rect_filled(rect, 0.0, fill);
    let inner = Rect::from_min_max(
        pos2(rect.left() + f32::from(pad.left), rect.top() + f32::from(pad.top)),
        pos2(rect.right() - f32::from(pad.right), rect.bottom() - f32::from(pad.bottom)),
    );
    let mut child = ui.new_child(UiBuilder::new().id_salt(id).max_rect(inner).layout(Layout::top_down(Align::Min)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    // The ground the pane's controls sit on: quiet controls step up from it ([`quiet_fill`]).
    child.visuals_mut().panel_fill = fill;
    add(&mut child)
}

/// A 1 px line across the width of `ui`.
pub(crate) fn rule(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::pal(ui).border);
}

/// A draft in the Your review list: the first line of its text and where it is, 43 px. Pressing it opens the draft's editor.
pub(crate) fn draft_row(ui: &mut Ui, text: &str, location: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(rect, radius(8.0), fill);
    }
    put(ui, fit(ui, text, sans(ui, 12.5), p.graphite, rect.width() - 16.0), rect.left() + 8.0, rect.top() + 12.0);
    put(ui, fit(ui, location, mono(ui, 11.0), faint(ui, &response), rect.width() - 16.0), rect.left() + 8.0, rect.top() + 28.0);
    ring(ui, &response, rect, 8.0);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("{text}, {location}")));
    response
}

/// A 1 px line across `rect`'s top edge.
pub(crate) fn seam_top(ui: &Ui, rect: Rect) {
    let p = theme::pal(ui);
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(theme::sz(ui).hairline, p.border));
}

pub(crate) fn seam_bottom(ui: &Ui, rect: Rect) {
    let p = theme::pal(ui);
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(theme::sz(ui).hairline, p.border));
}

pub(crate) fn seam_right(ui: &Ui, rect: Rect) {
    let p = theme::pal(ui);
    ui.painter().vline(rect.right() - 0.5, rect.y_range(), Stroke::new(theme::sz(ui).hairline, p.border));
}

// ---- key cap ----

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum CapTone {
    /// A bordered raised box.
    Quiet,
    /// On a graphite pill: a faint light fill.
    Inverse,
}

fn cap_width(ui: &Ui, label: &str) -> f32 {
    (galley(ui, label, mono(ui, 11.0), Color32::PLACEHOLDER).size().x + 12.0).max(18.0)
}

/// Paints a key cap with its left edge at `left`, centred on `centre_y`; returns its right edge.
fn paint_cap(ui: &Ui, left: f32, centre_y: f32, label: &str, tone: CapTone) -> f32 {
    let p = theme::pal(ui);
    let (width, height) = (cap_width(ui, label), if tone == CapTone::Quiet { 18.0 } else { 16.0 });
    let rect = Rect::from_min_size(pos2(left, centre_y - height / 2.0), vec2(width, height));
    let ink = match tone {
        CapTone::Quiet => {
            ui.painter().rect(rect, radius(4.0), p.raised, hairline(ui), StrokeKind::Inside);
            p.text_secondary
        }
        CapTone::Inverse => {
            ui.painter().rect_filled(rect, radius(4.0), p.on_inverse.gamma_multiply(0.15));
            p.on_inverse
        }
    };
    put(ui, galley(ui, label, mono(ui, 11.0), ink), rect.center().x - (width - 12.0) / 2.0, centre_y);
    rect.right()
}

/// A key cap on its own: `⌘K`, `J`.
pub(crate) fn key_cap(ui: &mut Ui, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(cap_width(ui, label), 18.0), Sense::hover());
    paint_cap(ui, rect.left(), rect.center().y, label, CapTone::Quiet);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, label));
    response
}

// ---- pill buttons ----

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum PillKind {
    Primary,
    Secondary,
    Quiet,
    Danger,
    /// Needs you: the label in Ember's text colour.
    Warn,
}

/// `button-primary` and `button-secondary`: a 28 px pill (26 px inside a comment) with the key cap on its right.
pub(crate) struct Pill<'a> {
    kind: PillKind,
    label: &'a str,
    key: Option<String>,
    height: f32,
    enabled: bool,
    block: bool,
    max_width: Option<f32>,
}

impl<'a> Pill<'a> {
    pub(crate) fn new(kind: PillKind, label: &'a str) -> Self {
        Pill { kind, label, key: None, height: 28.0, enabled: true, block: false, max_width: None }
    }

    /// As wide as the room there is, 32 px tall, the label and cap in the middle.
    pub(crate) fn block(mut self) -> Self {
        self.block = true;
        self.height = 32.0;
        self
    }

    /// The cap shows the binding of the action `id`.
    pub(crate) fn key(mut self, ui: &Ui, id: &str) -> Self {
        self.key = Some(crate::keymap::label(ui, id)).filter(|k| !k.is_empty());
        self
    }

    /// The pill is never wider than `width`; a longer label is cut with an ellipsis (its accessible name stays whole).
    pub(crate) fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }

    /// The cap shows `key`, for a shortcut that is not an action of the registry (⌘C copies in any text view).
    pub(crate) fn key_text(mut self, key: &str) -> Self {
        self.key = Some(key.to_string());
        self
    }

    /// The width this pill takes, for centring a row of them.
    pub(crate) fn width(&self, ui: &Ui) -> f32 {
        let cap = self.key.as_deref().map(|key| cap_width(ui, key));
        let text = galley(ui, self.label, sans(ui, 13.0), Color32::PLACEHOLDER).size().x;
        12.0 + text + cap.map_or(0.0, |c| 6.0 + c) + if cap.is_some() { 8.0 } else { 12.0 }
    }

    pub(crate) fn small(mut self) -> Self {
        self.height = 26.0;
        self
    }

    pub(crate) fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    pub(crate) fn show(self, ui: &mut Ui) -> Response {
        let p = theme::pal(ui);
        let enabled = self.enabled && ui.is_enabled();
        let ink = match self.kind {
            PillKind::Primary => p.on_inverse,
            PillKind::Secondary => p.graphite,
            PillKind::Quiet => p.text_secondary,
            PillKind::Danger => p.rose_text,
            PillKind::Warn => p.ember_text,
        };
        let cap = self.key.as_deref().map(|key| cap_width(ui, key));
        // The label is cut with an ellipsis when the pill may not be as wide as the whole label.
        let label_room = self.max_width.map(|max| (max - 12.0 - cap.map_or(12.0, |c| 6.0 + c + 8.0)).max(8.0));
        let label_galley = |ui: &Ui, colour: Color32| match label_room {
            Some(room) => fit(ui, self.label, sans(ui, 13.0), colour, room),
            None => galley(ui, self.label, sans(ui, 13.0), colour),
        };
        let text = label_galley(ui, ink);
        let content = text.size().x + cap.map_or(0.0, |c| 6.0 + c);
        let width = if self.block { ui.available_width() } else { 12.0 + content + if cap.is_some() { 8.0 } else { 12.0 } };
        let height = self.height.max(text.size().y + 8.0);
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), if enabled { Sense::click() } else { Sense::hover() });
        if ui.is_rect_visible(rect) {
            let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.5) };
            let (hovered, down) = (enabled && response.hovered(), enabled && response.is_pointer_button_down_on());
            let (fill, stroke) = match self.kind {
                PillKind::Primary => (
                    if down {
                        p.inverse_pressed
                    } else if hovered {
                        p.inverse_hover
                    } else {
                        p.inverse
                    },
                    Stroke::NONE,
                ),
                PillKind::Secondary | PillKind::Danger | PillKind::Warn => (
                    if down {
                        p.pressed
                    } else if hovered {
                        p.hover
                    } else {
                        p.raised
                    },
                    hairline(ui),
                ),
                PillKind::Quiet => (quiet_fill(ui, &response, enabled).unwrap_or(Color32::TRANSPARENT), Stroke::NONE),
            };
            ui.painter().rect(rect, radius(height / 2.0), dim(fill), Stroke::new(stroke.width, dim(stroke.color)), StrokeKind::Inside);
            let centre = rect.center().y;
            let start = if self.block { rect.center().x - content / 2.0 } else { rect.left() + 12.0 };
            let right = put(ui, label_galley(ui, dim(ink)), start, centre);
            if let Some(key) = &self.key {
                let tone = if self.kind == PillKind::Primary { CapTone::Inverse } else { CapTone::Quiet };
                paint_cap(ui, right + 6.0, centre, key, tone);
            }
            ring(ui, &response, rect, height / 2.0);
        }
        let name = match &self.key {
            Some(key) => format!("{}  {key}", self.label),
            None => self.label.to_string(),
        };
        response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, name.clone()));
        response
    }
}

// ---- tags, chips, dots ----

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TagKind {
    /// `tag-your-turn`.
    YourTurn,
    /// `tag-on-the-way`: work on the way, AI drafts, queued reviews.
    OnTheWay,
    /// A quiet count or status on the hover tone.
    Neutral,
}

/// The colours of a tag: fill and text.
fn tag_colours(p: Palette, kind: TagKind) -> (Color32, Color32) {
    match kind {
        TagKind::YourTurn => (p.ember_subtle, p.ember_text),
        TagKind::OnTheWay => (p.haze_subtle, p.haze_text),
        TagKind::Neutral => (p.hover, p.text_secondary),
    }
}

/// A 20 px pill with a word in it.
pub(crate) fn tag(ui: &mut Ui, kind: TagKind, text: &str) -> Response {
    let (fill, ink) = tag_colours(theme::pal(ui), kind);
    let text_galley = galley(ui, text, sans(ui, 11.5), ink);
    let (rect, response) = ui.allocate_exact_size(vec2(text_galley.size().x + 16.0, 20.0_f32.max(text_galley.size().y + 4.0)), Sense::hover());
    ui.painter().rect_filled(rect, radius(rect.height() / 2.0), fill);
    put(ui, text_galley, rect.left() + 8.0, rect.center().y);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    response
}

/// A round chip with the first letters of a name.
pub(crate) fn avatar(ui: &mut Ui, name: &str, diameter: f32) -> Response {
    let p = theme::pal(ui);
    let take = if diameter < 22.0 { 1 } else { 2 };
    let initials: String = name.chars().filter(|c| c.is_alphanumeric()).take(take).flat_map(char::to_uppercase).collect();
    let (rect, response) = ui.allocate_exact_size(vec2(diameter, diameter), Sense::hover());
    ui.painter().circle_filled(rect.center(), diameter / 2.0, p.haze_subtle);
    let initials_galley = galley(ui, &initials, sans(ui, 11.0), p.haze_text);
    put(ui, Arc::clone(&initials_galley), rect.center().x - initials_galley.size().x / 2.0, rect.center().y);
    response.widget_info(|| WidgetInfo::labeled(Role::Image, true, name));
    response
}

/// The PR Craft mark: two pills, the head dropped by a radius.
pub(crate) fn logomark(ui: &mut Ui) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(14.0, 16.0), Sense::hover());
    let ink = theme::pal(ui).graphite;
    for (dx, dy) in [(0.0, 0.0), (8.0, 3.0)] {
        ui.painter().rect_filled(Rect::from_min_size(rect.min + vec2(dx, dy), vec2(6.0, 13.0)), radius(3.2), ink);
    }
    response.widget_info(|| WidgetInfo::labeled(Role::Image, true, "PR Craft"));
    response
}

/// A round mark for a forge. The official marks are not shipped; the initials stand in.
fn forge_disc(ui: &Ui, centre: Pos2, diameter: f32, forge: Forge) {
    let p = theme::pal(ui);
    ui.painter().circle(centre, diameter / 2.0, p.sunken, hairline(ui));
    let initials = match forge {
        Forge::GitHub => "GH",
        Forge::GitLab => "GL",
    };
    let text = galley(ui, initials, sans(ui, 7.5), p.text_secondary);
    put(ui, Arc::clone(&text), centre.x - text.size().x / 2.0, centre.y);
}

pub(crate) fn forge_mark(ui: &mut Ui, forge: Forge) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    forge_disc(ui, rect.center(), 18.0, forge);
    response.widget_info(|| WidgetInfo::labeled(Role::Image, true, forge.name()));
    response
}

// ---- segmented control ----

/// `Unified / Split`, `All / GitHub 7 / GitLab 5`: one chip raised, the rest quiet. Returns the index clicked.
pub(crate) fn segmented(ui: &mut Ui, id: &str, group: &str, options: &[String], selected: usize) -> Option<usize> {
    let p = theme::pal(ui);
    let widths: Vec<f32> = options.iter().map(|o| (galley(ui, o, sans(ui, 12.5), Color32::PLACEHOLDER).size().x + 22.0).max(44.0)).collect();
    let total = widths.iter().sum::<f32>() + 4.0 + 2.0 * options.len().saturating_sub(1) as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(total, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, radius(12.0), p.hover);
    let mut clicked = None;
    let mut x = rect.left() + 2.0;
    for (i, (option, width)) in options.iter().zip(&widths).enumerate() {
        let chip = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(*width, 20.0));
        let response = ui.interact(chip, ui.scope_id().with((id, i)), Sense::click());
        let on = i == selected;
        if on {
            // A 3:1 edge as well as the raised fill, so the choice is not only a tint.
            ui.painter().rect(chip, radius(10.0), p.raised, Stroke::new(1.0, p.control), StrokeKind::Inside);
        } else if response.is_pointer_button_down_on() {
            ui.painter().rect_filled(chip, radius(10.0), p.pressed);
        } else if response.hovered() {
            ui.painter().rect_filled(chip, radius(10.0), p.sunken);
        }
        let ink = if on { p.graphite } else { p.text_secondary };
        let text = galley(ui, option, sans(ui, 12.5), ink);
        put(ui, Arc::clone(&text), chip.center().x - text.size().x / 2.0, chip.center().y);
        ring(ui, &response, chip, 10.0);
        // The name starts with what is drawn and ends with what the choice is for.
        let named = format!("{option}, {group}");
        response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, on, named.clone()));
        if response.clicked() && !on {
            clicked = Some(i);
        }
        x += width + 2.0;
    }
    clicked
}

// ---- navigation ----

/// `nav-item-selected` and its quiet twin: 28 px, radius 8, the count on the right.
pub(crate) fn nav_item(ui: &mut Ui, selected: bool, text: &str, count: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, radius(8.0), p.inverse);
    } else if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(rect, radius(8.0), fill);
    }
    let (ink, count_ink) = if selected { (p.on_inverse, p.on_inverse.gamma_multiply(0.7)) } else { (p.graphite, faint(ui, &response)) };
    let centre = rect.center().y;
    let right = rect.right() - 8.0;
    let left = if count.is_empty() { right } else { put_right(ui, galley(ui, count, sans(ui, 12.0), count_ink), right, centre) };
    put(ui, fit(ui, text, sans(ui, 13.0), ink, left - rect.left() - 20.0), rect.left() + 8.0, centre);
    ring(ui, &response, rect, 8.0);
    let name = if count.is_empty() { text.to_string() } else { format!("{text}  {count}") };
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, name.clone()));
    response
}

/// A repository of the sidebar with how many of its changes wait; a filled mark for GitHub, a ring for GitLab.
pub(crate) fn repo_row(ui: &mut Ui, forge: Forge, repo: &str, count: usize) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let mark = pos2(rect.left() + 8.0, rect.center().y);
    match forge {
        Forge::GitHub => ui.painter().circle_filled(mark, 4.0, p.graphite),
        Forge::GitLab => ui.painter().circle_stroke(mark, 3.5, Stroke::new(1.5, p.graphite)),
    };
    let right = put_right(ui, galley(ui, &count.to_string(), sans(ui, 12.0), p.text_tertiary), rect.right() - 4.0, rect.center().y);
    put(ui, fit(ui, repo, mono(ui, 12.0), p.graphite, right - rect.left() - 36.0), rect.left() + 20.0, rect.center().y);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{repo}  {count}")));
    response
}

/// A small caption over a group: `Queue`, `Sources`.
pub(crate) fn section_title(ui: &mut Ui, text: &str) {
    ui.add_space(theme::SPACE_3);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        label(ui, text, 11.0, theme::pal(ui).text_tertiary);
    });
    ui.add_space(4.0);
}

// ---- inbox rows ----

pub(crate) struct InboxRow<'a> {
    pub forge: Forge,
    pub title: &'a str,
    /// `repo #n · author · age`, in the code font.
    pub meta: String,
    pub draft: bool,
    pub unread: bool,
    pub selected: bool,
}

/// A clickable block of `size`, keyed by `id` so focus follows the row when the list re-sorts.
fn keyed(ui: &mut Ui, id: &str, size: egui::Vec2) -> (Rect, Response) {
    let rect = ui.allocate_space(size).1;
    let response = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    (rect, response)
}

fn row_height(ui: &Ui, base: f32) -> f32 {
    base + 2.0 * theme::sz(ui).row
}

fn row_ground(ui: &Ui, rect: Rect, selected: bool, response: &Response) {
    let p = theme::pal(ui);
    if selected {
        ui.painter().rect_filled(rect, 0.0, p.raised);
        // A rail as well as the raised ground, so the selection is not only a tint.
        ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(2.0, rect.height())), 0.0, p.graphite);
    } else if let Some(fill) = quiet_fill(ui, response, true) {
        ui.painter().rect_filled(rect, 0.0, fill);
    }
}

/// A queue row: forge mark, title, mono meta line, and the draft tag on the right. 66 px.
pub(crate) fn inbox_row(ui: &mut Ui, id: &str, row: &InboxRow) -> Response {
    let p = theme::pal(ui);
    let height = row_height(ui, 66.0);
    let (rect, response) = keyed(ui, id, vec2(ui.available_width(), height));
    if ui.is_rect_visible(rect) {
        row_ground(ui, rect, row.selected, &response);
        let (top, right) = (rect.top() + 12.0 + theme::sz(ui).row, rect.right() - 20.0);
        if row.unread {
            ui.painter().circle_filled(pos2(rect.left() + 11.0, top + 9.0), 3.0, p.ember);
        }
        forge_disc(ui, pos2(rect.left() + 31.0, top + 9.0), 18.0, row.forge);
        let text_left = rect.left() + 48.0;
        let mut title_right = right;
        if row.draft {
            let tag_galley = galley(ui, "Draft", sans(ui, 11.5), p.haze_text);
            let tag_rect = Rect::from_min_size(pos2(right - tag_galley.size().x - 16.0, top - 1.0), vec2(tag_galley.size().x + 16.0, 20.0));
            ui.painter().rect_filled(tag_rect, radius(10.0), p.haze_subtle);
            put(ui, tag_galley, tag_rect.left() + 8.0, tag_rect.center().y);
            title_right = tag_rect.left() - 12.0;
        }
        put(ui, fit(ui, row.title, sans(ui, 13.0), p.graphite, title_right - text_left), text_left, top + 8.5);
        put(ui, fit(ui, &row.meta, mono(ui, 11.5), faint(ui, &response), right - text_left), text_left, top + 30.0);
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(theme::sz(ui).hairline, p.border));
        ring_in(ui, &response, rect, 8.0);
    }
    let name = format!("{}, {}{}", row.title, row.meta, if row.draft { ", draft" } else { "" });
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, row.selected, name.clone()));
    response
}

/// An own change of the queue "Your changes": forge mark, title with its size (+added −removed) on the right, the mono meta line, and
/// the status on the right of it. An ember dot marks a reviewer who waits. 66 px.
pub(crate) struct MineRow<'a> {
    pub forge: Forge,
    pub title: &'a str,
    pub meta: String,
    pub size: Option<(u64, u64)>,
    pub status: &'a str,
    pub tone: pr_craft_core::mine::Tone,
    pub waiting: bool,
    pub selected: bool,
}

pub(crate) fn mine_row(ui: &mut Ui, id: &str, row: &MineRow) -> Response {
    use pr_craft_core::mine::Tone;
    let p = theme::pal(ui);
    let height = row_height(ui, 66.0);
    let (rect, response) = keyed(ui, id, vec2(ui.available_width(), height));
    if ui.is_rect_visible(rect) {
        row_ground(ui, rect, row.selected, &response);
        let (top, right) = (rect.top() + 12.0 + theme::sz(ui).row, rect.right() - 20.0);
        if row.waiting {
            ui.painter().circle_filled(pos2(rect.left() + 11.0, top + 9.0), 3.0, p.ember);
        }
        forge_disc(ui, pos2(rect.left() + 31.0, top + 9.0), 18.0, row.forge);
        let text_left = rect.left() + 48.0;
        let mut title_right = right;
        if let Some((added, removed)) = row.size {
            let minus = put_right(ui, galley(ui, &format!("−{removed}"), mono(ui, 11.5), p.rose_text), right, top + 9.0);
            title_right = put_right(ui, galley(ui, &format!("+{added}"), mono(ui, 11.5), p.moss_text), minus - 8.0, top + 9.0) - 12.0;
        }
        put(ui, fit(ui, row.title, sans(ui, 13.0), p.graphite, title_right - text_left), text_left, top + 9.5);
        let (dot, ink) = match row.tone {
            Tone::Bad => (p.rose, p.rose_text),
            Tone::Warn => (p.ember, p.ember_text),
            Tone::Good => (p.moss, p.moss_text),
            Tone::Quiet => (p.haze, p.text_secondary),
        };
        let status = fit(ui, row.status, sans(ui, 11.5), ink, (rect.width() * 0.55).min(right - text_left - 90.0));
        let status_left = put_right(ui, status, right, top + 31.0);
        ui.painter().circle_filled(pos2(status_left - 10.0, top + 31.0), 3.5, dot);
        put(ui, fit(ui, &row.meta, mono(ui, 11.5), faint(ui, &response), status_left - 24.0 - text_left), text_left, top + 31.0);
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(theme::sz(ui).hairline, p.border));
        ring_in(ui, &response, rect, 8.0);
    }
    let size = row.size.map(|(a, r)| format!(", plus {a} minus {r}")).unwrap_or_default();
    let name = format!("{}, {}, {}{size}{}", row.title, row.meta, row.status, if row.waiting { ", a reviewer is waiting on you" } else { "" });
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, row.selected, name.clone()));
    response
}

/// One layer of a stack row.
pub(crate) struct Layer<'a> {
    pub title: &'a str,
    pub reference: String,
    pub draft: bool,
}

pub(crate) struct StackRow<'a> {
    pub forge: Forge,
    pub repo: &'a str,
    /// Layers of the whole stack, and those of them that are in the queue.
    pub size: usize,
    pub layers: Vec<Layer<'a>>,
    pub meta: String,
    pub selected: bool,
    pub unread: bool,
}

/// `Your turn on 2 of 4`: only layers that wait on the reviewer count, not drafts.
fn yours_text(row: &StackRow) -> String {
    format!("Your turn on {} of {}", row.layers.iter().filter(|l| !l.draft).count(), row.size)
}

/// A stack in the inbox: header with layer bars and `Your turn on n of m`, then one rail line per layer.
pub(crate) fn stack_row(ui: &mut Ui, id: &str, row: &StackRow) -> Response {
    let p = theme::pal(ui);
    let head = row_height(ui, 58.0);
    let height = head + 34.0 * row.layers.len() as f32 + 6.0;
    let (rect, response) = keyed(ui, id, vec2(ui.available_width(), height));
    if ui.is_rect_visible(rect) {
        row_ground(ui, rect, row.selected, &response);
        let (top, right) = (rect.top() + 11.0 + theme::sz(ui).row, rect.right() - 20.0);
        if row.unread {
            ui.painter().circle_filled(pos2(rect.left() + 11.0, top + 9.0), 3.0, p.ember);
        }
        forge_disc(ui, pos2(rect.left() + 31.0, top + 9.0), 18.0, row.forge);
        let text_left = rect.left() + 48.0;
        let yours = row.layers.iter().filter(|l| !l.draft).count();
        let state = yours_text(row);
        let state_left = put_right(ui, galley(ui, &state, sans(ui, 11.5), p.text_secondary), right, top + 19.5);
        ui.painter().circle_filled(pos2(state_left - 9.0, top + 19.5), 3.5, p.ember);
        let bars_left = right - (row.size as f32 * 21.0 - 3.0);
        for i in 0..row.size {
            let colour = if i < yours { p.ember } else { p.border_strong };
            let bar = Rect::from_min_size(pos2(bars_left + i as f32 * 21.0, top), vec2(18.0, 4.0));
            ui.painter().rect_filled(bar, radius(2.0), colour);
        }
        let title_galley = galley(ui, &format!("Stack · {}", row.repo), sans(ui, 13.0), p.graphite);
        let after = put(ui, title_galley, text_left, top + 9.5) + 8.0;
        let chip = format!("{} merge requests", row.size);
        let chip_galley = galley(ui, &chip, sans(ui, 11.5), p.text_secondary);
        let chip_rect = Rect::from_min_size(pos2(after, top - 1.0), vec2(chip_galley.size().x + 16.0, 20.0));
        if chip_rect.right() < bars_left - 8.0 {
            ui.painter().rect_filled(chip_rect, radius(10.0), p.hover);
            put(ui, chip_galley, chip_rect.left() + 8.0, chip_rect.center().y);
        }
        put(ui, fit(ui, &row.meta, mono(ui, 11.5), faint(ui, &response), state_left - 20.0 - text_left), text_left, top + 33.5);
        let mut y = rect.top() + head;
        for (n, layer) in row.layers.iter().enumerate() {
            let centre = y + 17.0;
            let rail = rect.left() + 45.0;
            let line = Stroke::new(2.0, p.border_strong);
            if n > 0 {
                ui.painter().vline(rail, (y)..=(centre - 5.0), line);
            }
            ui.painter().vline(
                rail,
                (centre + 5.0)..=(y + 34.0),
                Stroke::new(2.0, if n + 1 < row.layers.len() { p.border_strong } else { Color32::TRANSPARENT }),
            );
            let node = pos2(rail, centre);
            if layer.draft {
                ui.painter().circle(node, 5.0, p.raised, Stroke::new(1.0, faint(ui, &response)));
            } else {
                ui.painter().circle_filled(node, 5.0, p.ember);
            }
            let state_word = if layer.draft { "Draft" } else { "Your turn" };
            let state_ink = if layer.draft { faint(ui, &response) } else { p.ember_text };
            let words_left = put_right(ui, galley(ui, state_word, sans(ui, 11.5), state_ink), right, centre);
            let ref_left = put_right(ui, galley(ui, &layer.reference, mono(ui, 11.5), faint(ui, &response)), words_left - 8.0, centre);
            put(ui, fit(ui, layer.title, sans(ui, 12.5), p.graphite, ref_left - 12.0 - rect.left() - 62.0), rect.left() + 62.0, centre);
            y += 34.0;
        }
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(theme::sz(ui).hairline, p.border));
        ring_in(ui, &response, rect, 8.0);
    }
    let states: Vec<String> = row.layers.iter().map(|l| format!("{} {}", l.reference, if l.draft { "draft" } else { "your turn" })).collect();
    let opens = row.layers.last().map_or(String::new(), |l| format!(", Enter opens {}", l.reference));
    let name = format!("Stack · {}, {} merge requests, {}, {}: {}{opens}", row.repo, row.size, yours_text(row), row.meta, states.join(", "));
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, row.selected, name.clone()));
    response
}

// ---- files ----

/// The mono path and a proportion bar: moss for added lines, rose for removed, on a hover track. 26 px.
pub(crate) fn files_changed_row(ui: &mut Ui, path: &str, adds: u32, dels: u32, most: u32) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let bar = (rect.width() * 0.4).clamp(40.0, 180.0);
    let track = Rect::from_center_size(pos2(rect.right() - bar / 2.0, rect.center().y), vec2(bar, 5.0));
    ui.painter().rect_filled(track, radius(2.5), p.hover);
    let scale = if most == 0 { 0.0 } else { bar / most as f32 };
    let (add_w, del_w) = (adds as f32 * scale, dels as f32 * scale);
    if adds > 0 {
        ui.painter().rect_filled(Rect::from_min_size(track.min, vec2(add_w.max(3.0), 5.0)), radius(2.5), p.moss);
    }
    if dels > 0 {
        ui.painter().rect_filled(
            Rect::from_min_size(track.min + vec2(add_w.max(if adds > 0 { 3.0 } else { 0.0 }) + 1.0, 0.0), vec2(del_w.max(3.0), 5.0)),
            radius(2.5),
            p.rose,
        );
    }
    put(ui, fit(ui, path, mono(ui, 12.0), p.graphite, track.left() - rect.left() - 12.0), rect.left(), rect.center().y);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{path}  +{adds} −{dels}")));
    response
}

/// A file of the Changes list: viewed mark, mono name, `+a −d`. 30 px, radius 8, graphite when current.
pub(crate) fn review_file_row(ui: &mut Ui, name: &str, current: bool, viewed: bool, adds: u32, dels: u32) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), row_height(ui, 30.0)), Sense::click());
    if current {
        ui.painter().rect_filled(rect, radius(8.0), p.inverse);
    } else if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(rect, radius(8.0), fill);
    }
    let (ink, quiet) = if current { (p.on_inverse, p.on_inverse.gamma_multiply(0.7)) } else { (p.graphite, faint(ui, &response)) };
    let centre = rect.center().y;
    let mark = pos2(rect.left() + 15.0, centre);
    if viewed {
        ui.painter().circle_filled(mark, 5.0, ink);
    } else {
        ui.painter().circle_stroke(mark, 4.5, Stroke::new(1.0, quiet));
    }
    let stat = format!("+{adds} −{dels}");
    let stat_left = put_right(ui, galley(ui, &stat, mono(ui, 11.0), quiet), rect.right() - 8.0, centre);
    put(ui, fit(ui, name, mono(ui, 12.0), ink, stat_left - rect.left() - 36.0), rect.left() + 28.0, centre);
    ring(ui, &response, rect, 8.0);
    let described = format!("{name}, {}, +{adds} −{dels}", if viewed { "viewed" } else { "not viewed" });
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, current, described.clone()));
    response
}

// ---- diff ----

/// Row tint, gutter tint and the ink of the numbers and sign of a diff line. A selected line is Haze with graphite ink.
pub(crate) fn diff_colours(p: Palette, kind: LineKind, selected: bool) -> (Color32, Color32, Color32) {
    if selected {
        return (p.haze_subtle, p.selection, p.graphite);
    }
    match kind {
        LineKind::Context => (Color32::TRANSPARENT, Color32::TRANSPARENT, p.text_tertiary),
        LineKind::Added => (p.moss_bg, p.moss_gutter, p.moss_text),
        LineKind::Removed => (p.rose_bg, p.rose_gutter, p.rose_text),
    }
}

/// How the code of a diff row is drawn: the changed words to mark, and whether a long line wraps instead of running on.
#[derive(Clone, Copy, Default)]
pub(crate) struct CodeStyle<'a> {
    /// Byte ranges of the line's text that changed against its partner line.
    pub marks: &'a [Range<usize>],
    pub wrap: bool,
}

/// What a long line does at the edge of its column.
#[derive(Clone, Copy)]
enum CodeFit {
    /// Runs on; the pane scrolls sideways.
    Free,
    /// Wraps at this width.
    Wrap(f32),
    /// Ends in an ellipsis at this width.
    Cut(f32),
}

/// The code of a line: tabs as four spaces, with `marks` on a stronger tint of the line's own colour.
fn code_galley(ui: &Ui, kind: LineKind, text: &str, marks: &[Range<usize>], fit: CodeFit) -> Arc<Galley> {
    let p = theme::pal(ui);
    let fill = match kind {
        LineKind::Added => p.moss_mark,
        LineKind::Removed => p.rose_mark,
        LineKind::Context => p.hover,
    };
    let mut job = LayoutJob::default();
    let piece = |job: &mut LayoutJob, text: &str, marked: bool| {
        let mut format = TextFormat::simple(mono(ui, 12.5), p.graphite);
        if marked {
            format.background = fill;
        }
        job.append(&text.replace('\t', "    "), 0.0, format);
    };
    let mut at = 0;
    for mark in marks {
        if mark.start < at || mark.start >= mark.end {
            continue;
        }
        piece(&mut job, text.get(at..mark.start).unwrap_or_default(), false);
        piece(&mut job, text.get(mark.clone()).unwrap_or_default(), true);
        at = mark.end;
    }
    piece(&mut job, text.get(at..).unwrap_or_default(), false);
    match fit {
        CodeFit::Free => {}
        CodeFit::Wrap(width) => {
            job.wrap.max_width = width.max(1.0);
            job.wrap.break_anywhere = true;
        }
        CodeFit::Cut(width) => job.wrap = TextWrapping::truncate_at_width(width.max(1.0)),
    }
    ui.painter().layout_job(job)
}

/// The one-character sign of a line, for the eye; the accessible name carries it too.
fn sign_of(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Context => "",
        LineKind::Added => "+",
        LineKind::Removed => "−",
    }
}

/// One line of a hunk: gutter with the old and new numbers, the sign, then the code on a full-width tint. Selected lines turn
/// Haze with a rail, so the selection is not only a colour.
pub(crate) fn diff_row(ui: &mut Ui, min_width: f32, line: &DiffLine, selected: bool, interactive: bool, style: CodeStyle<'_>) -> Response {
    let (p, s) = (theme::pal(ui), theme::sz(ui));
    let fit = if style.wrap { CodeFit::Wrap((min_width - 126.0).max(120.0)) } else { CodeFit::Free };
    let code = code_galley(ui, line.kind, &line.text, style.marks, fit);
    let unit = galley(ui, "M", mono(ui, 12.5), p.graphite).size().y;
    let base = s.line.unwrap_or(20.0_f32.max(unit));
    let height = base.max(code.size().y);
    let width = if style.wrap { min_width } else { min_width.max(110.0 + code.size().x + 24.0) };
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), if interactive { Sense::click() } else { Sense::hover() });
    // The pointer over the row, even when the gutter button sits between it and the row.
    let over = interactive && ui.rect_contains_pointer(rect);
    let shown = line.new.or(line.old).unwrap_or(0);
    if ui.is_rect_visible(rect) {
        let gutter = Rect::from_min_size(rect.min, vec2(86.0, height));
        let (row_fill, gutter_fill, number_ink) = diff_colours(p, line.kind, selected);
        ui.painter().rect_filled(rect, 0.0, row_fill);
        // Under the pointer the code side darkens to the gutter tint; the gutter is painted once, so its numbers keep their contrast.
        let hover = over && !selected;
        if hover && line.kind == LineKind::Context {
            ui.painter().rect_filled(rect, 0.0, p.hover.gamma_multiply(0.6));
        }
        if hover && line.kind != LineKind::Context {
            ui.painter().rect_filled(Rect::from_min_max(pos2(gutter.right(), rect.top()), rect.right_bottom()), 0.0, gutter_fill);
        }
        ui.painter().rect_filled(gutter, 0.0, gutter_fill);
        if selected {
            ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(2.0, height)), 0.0, p.focus_ring);
        }
        let centre = rect.top() + base / 2.0;
        let number = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
        for (n, right) in [(line.old, 40.0), (line.new, 76.0)] {
            put_right(ui, galley(ui, &number(n), mono(ui, 11.5), number_ink), rect.left() + right, centre);
        }
        put(ui, galley(ui, sign_of(line.kind), mono(ui, 12.5), number_ink), rect.left() + 92.0, centre);
        ui.painter().galley(pos2(rect.left() + 110.0, centre - unit / 2.0), code, Color32::PLACEHOLDER);
        ring_in(ui, &response, rect, 0.0);
        if over {
            gutter_button(ui, &response, pos2(rect.left() + 14.0, rect.top() + base / 2.0), shown);
        }
    }
    let sign = match line.kind {
        LineKind::Context => ' ',
        LineKind::Added => '+',
        LineKind::Removed => '−',
    };
    let name = format!("{shown} {sign} {}", line.text);
    let role = if interactive { Role::Button } else { Role::Label };
    response.widget_info(|| WidgetInfo::selected(role, true, selected, name.clone()));
    response
}

/// What the 18px comment button of a hovered diff line was pressed for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Gutter {
    /// Clicked: comment on this line.
    Click,
    /// Dragged from: the lines under the pointer become the selection.
    DragStart,
    /// Right-clicked: the 18 px button sits over the row, so the row's menu opens from here.
    Menu,
}

fn gutter_key() -> egui::Id {
    egui::Id::NULL.with("gutter-press")
}

/// The Haze comment button in the gutter of a hovered diff line. A press is left for [`gutter_press`] to read.
fn gutter_button(ui: &Ui, row: &Response, centre: Pos2, shown: u32) {
    let p = theme::pal(ui);
    let area = Rect::from_center_size(centre, vec2(18.0, 18.0));
    let button = ui.interact(area, row.id.with("comment"), Sense::click_and_drag());
    let lit = button.hovered() || button.is_pointer_button_down_on();
    ui.painter().circle_filled(centre, 9.0, if lit { p.haze_text } else { p.focus_ring });
    let stroke = Stroke::new(1.6, p.on_inverse);
    ui.painter().hline((centre.x - 4.0)..=(centre.x + 4.0), centre.y, stroke);
    ui.painter().vline(centre.x, (centre.y - 4.0)..=(centre.y + 4.0), stroke);
    ring(ui, &button, area, 9.0);
    let name = format!("Comment on line {shown}");
    button.widget_info(|| WidgetInfo::labeled(Role::Button, true, name.clone()));
    let tip = format!("{name} · {} · drag for a range", crate::keymap::label(ui, "review.comment"));
    let button = button.on_hover_text(tip);
    let press = if button.clicked() {
        Some(Gutter::Click)
    } else if button.drag_started() {
        Some(Gutter::DragStart)
    } else if button.secondary_clicked() {
        Some(Gutter::Menu)
    } else {
        None
    };
    if let Some(press) = press {
        ui.ctx().data_mut(|d| d.insert_temp(gutter_key(), Some((row.id, press))));
    }
}

/// What the gutter button of `row` (just drawn by [`diff_row`]) was pressed for this frame.
pub(crate) fn gutter_press(ui: &Ui, row: &Response) -> Option<Gutter> {
    let found = ui.ctx().data_mut(|d| d.remove_temp::<Option<(egui::Id, Gutter)>>(gutter_key())).flatten();
    found.filter(|(id, _)| *id == row.id).map(|(_, press)| press)
}

/// The pointer is over `row`, even while another widget is being dragged.
pub(crate) fn pointer_over(row: &Response) -> bool {
    row.contains_pointer()
}
/// One side of a split row: the line, whether it is selected, and the words to mark in it.
pub(crate) struct Cell<'a> {
    pub line: &'a DiffLine,
    pub selected: bool,
    pub marks: &'a [Range<usize>],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Half {
    Left,
    Right,
}

/// Width of the gutter of one half, and of the seam between the halves (frame 91).
const SPLIT_GUTTER: f32 = 44.0;
const SPLIT_SEAM: f32 = 2.0;

/// What a split row reports: the half pressed, the gutter button pressed on a half, the half under the pointer, and a request
/// for the menu of a half (where, and the control that gets focus back).
#[derive(Default)]
pub(crate) struct SplitOut {
    pub clicked: Option<Half>,
    pub gutter: Option<(Half, Gutter)>,
    pub over: Option<Half>,
    pub menu: Option<(Half, Pos2, egui::Id)>,
}

/// One row of split view: the old line on the left, the new on the right, each in its half with its own gutter. A side with
/// no line is a sunken blank. Returns the half that was pressed. A long line wraps with `wrap`, else ends in an ellipsis
/// (its whole text is in the accessible name).
pub(crate) fn split_row(ui: &mut Ui, width: f32, cells: [Option<Cell<'_>>; 2], wrap: bool, interactive: bool) -> SplitOut {
    let (p, s) = (theme::pal(ui), theme::sz(ui));
    let half = ((width - SPLIT_SEAM) / 2.0).floor();
    let code_width = half - SPLIT_GUTTER - 30.0;
    let fit = if wrap { CodeFit::Wrap(code_width) } else { CodeFit::Cut(code_width) };
    let codes = cells.each_ref().map(|c| c.as_ref().map(|c| code_galley(ui, c.line.kind, &c.line.text, c.marks, fit)));
    let unit = galley(ui, "M", mono(ui, 12.5), p.graphite).size().y;
    let base = s.line.unwrap_or(20.0_f32.max(unit));
    let height = codes.iter().flatten().map(|g| g.size().y).fold(base, f32::max);
    let (rect, row) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let mut out = SplitOut::default();
    for (side, cell, code) in [(Half::Left, &cells[0], &codes[0]), (Half::Right, &cells[1], &codes[1])] {
        let left = if side == Half::Left { rect.left() } else { rect.left() + half + SPLIT_SEAM };
        let right = if side == Half::Left { rect.left() + half } else { rect.right() };
        let area = Rect::from_min_max(pos2(left, rect.top()), pos2(right, rect.bottom()));
        let (Some(cell), Some(code)) = (cell, code) else {
            ui.painter().rect_filled(area, 0.0, p.sunken);
            continue;
        };
        let response = ui.interact(area, row.id.with(side as u8), if interactive { Sense::click() } else { Sense::hover() });
        if ui.is_rect_visible(area) {
            let (row_fill, gutter_fill, ink) = diff_colours(p, cell.line.kind, cell.selected);
            ui.painter().rect_filled(area, 0.0, row_fill);
            let gutter = Rect::from_min_size(area.min, vec2(SPLIT_GUTTER, height));
            if interactive && !cell.selected && response.hovered() {
                if cell.line.kind == LineKind::Context {
                    ui.painter().rect_filled(area, 0.0, p.hover.gamma_multiply(0.6));
                } else {
                    ui.painter().rect_filled(Rect::from_min_max(pos2(gutter.right(), area.top()), area.right_bottom()), 0.0, gutter_fill);
                }
            }
            ui.painter().rect_filled(gutter, 0.0, gutter_fill);
            if cell.selected {
                ui.painter().rect_filled(Rect::from_min_size(area.min, vec2(2.0, height)), 0.0, p.focus_ring);
            }
            let centre = rect.top() + base / 2.0;
            let number = if side == Half::Left { cell.line.old } else { cell.line.new };
            let number = number.map(|n| n.to_string()).unwrap_or_default();
            put_right(ui, galley(ui, &number, mono(ui, 11.5), ink), area.left() + SPLIT_GUTTER - 8.0, centre);
            put(ui, galley(ui, sign_of(cell.line.kind), mono(ui, 12.5), ink), area.left() + SPLIT_GUTTER + 6.0, centre);
            let at = pos2(area.left() + SPLIT_GUTTER + 22.0, centre - unit / 2.0);
            ui.painter().with_clip_rect(area).galley(at, Arc::clone(code), Color32::PLACEHOLDER);
            ring_in(ui, &response, area, 0.0);
            if interactive && ui.rect_contains_pointer(area) {
                gutter_button(ui, &response, pos2(area.left() + 13.0, rect.top() + base / 2.0), cell.line.new.or(cell.line.old).unwrap_or(0));
            }
        }
        let sign = match cell.line.kind {
            LineKind::Context => ' ',
            LineKind::Added => '+',
            LineKind::Removed => '−',
        };
        let shown = cell.line.new.or(cell.line.old).unwrap_or(0);
        let name = format!("{shown} {sign} {}, {} side", cell.line.text, if side == Half::Left { "old" } else { "new" });
        let role = if interactive { Role::Button } else { Role::Label };
        response.widget_info(|| WidgetInfo::selected(role, true, cell.selected, name.clone()));
        if response.clicked() {
            out.clicked = Some(side);
        }
        if interactive {
            if let Some(press) = gutter_press(ui, &response) {
                out.gutter = Some((side, press));
            }
            if response.contains_pointer() {
                out.over = Some(side);
            }
            let by_key = response.has_focus() && crate::keymap::pressed(ui, "app.context_menu");
            if response.secondary_clicked() || by_key {
                let at = if by_key { crate::menu::below(area) } else { ui.input(|i| i.pointer.interact_pos()).unwrap_or_else(|| area.center()) };
                out.menu = Some((side, at, response.id));
            }
        }
    }
    let seam = Rect::from_min_size(pos2(rect.left() + half, rect.top()), vec2(SPLIT_SEAM, height));
    ui.painter().rect_filled(seam, 0.0, p.border);
    out
}

/// `@@ -36,11 +36,16 @@ func …`: a 26 px band in the code font.
pub(crate) fn hunk_header(ui: &mut Ui, min_width: f32, text: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(min_width, 26.0), Sense::click());
    ui.painter().rect_filled(rect, 0.0, quiet_fill(ui, &response, true).unwrap_or(p.sunken));
    put(ui, galley(ui, text, mono(ui, 12.0), faint(ui, &response)), rect.left() + 16.0, rect.center().y);
    ring_in(ui, &response, rect, 0.0);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, text));
    response
}

// ---- cards, banners ----

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum CardKind {
    /// A draft on this Mac.
    Draft,
    /// An AI draft, not posted.
    Ai,
}

/// The sunken band with seams that holds a comment between two lines, and the raised card of radius 8 inset to the code
/// column inside it. `edge` is the card's border colour.
pub(crate) fn card_band(ui: &mut Ui, width: f32, edge: Color32, add: impl FnOnce(&mut Ui)) -> Rect {
    let p = theme::pal(ui);
    let band = egui::Frame::new().fill(p.sunken).inner_margin(Margin::symmetric(0, 10)).show(ui, |ui| {
        ui.set_width(width);
        ui.horizontal_top(|ui| {
            ui.add_space(104.0);
            egui::Frame::new().fill(p.raised).stroke(Stroke::new(1.0, edge)).corner_radius(8).inner_margin(13).show(ui, |ui| {
                ui.set_width((width - 104.0 - 24.0 - 26.0).clamp(200.0, 614.0));
                add(ui);
            });
        });
    });
    seam_top(ui, band.response.rect);
    seam_bottom(ui, band.response.rect);
    band.response.rect
}

/// A folder label of the Files list (frame 105): a chevron, the folder in the code font, and how many of its files are viewed.
/// 24 px. Pressing it folds or opens the folder.
pub(crate) fn folder_row(ui: &mut Ui, dir: &str, folded: bool, viewed: usize, total: usize) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(rect, radius(6.0), fill);
    }
    let at = pos2(rect.left() + 12.0, rect.center().y);
    let stroke = Stroke::new(1.4, p.text_secondary);
    let points = if folded { [vec2(-2.0, -4.0), vec2(2.0, 0.0), vec2(-2.0, 4.0)] } else { [vec2(-4.0, -2.0), vec2(0.0, 2.0), vec2(4.0, -2.0)] };
    ui.painter().line_segment([at + points[0], at + points[1]], stroke);
    ui.painter().line_segment([at + points[1], at + points[2]], stroke);
    let count = format!("{viewed} of {total}");
    let left = put_right(ui, galley(ui, &count, sans(ui, 11.5), faint(ui, &response)), rect.right() - 8.0, rect.center().y);
    put(ui, fit(ui, dir, mono(ui, 11.0), p.text_secondary, left - rect.left() - 36.0), rect.left() + 22.0, rect.center().y);
    ring(ui, &response, rect, 6.0);
    let name = format!("{dir}, {viewed} of {total} viewed, {}", if folded { "folded" } else { "open" });
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, name.clone()));
    response
}

/// A list or pane with nothing to show (DESIGN.md → Empty states): a title in plain words, why it is empty or what will fill it,
/// and up to two next actions (a label and its key, empty for none). Never a picture. The title is a live region, so a list
/// that empties out is announced. Returns the action that was pressed.
pub(crate) fn empty_state(ui: &mut Ui, title: &str, body: &str, actions: &[(&str, &str)]) -> Option<usize> {
    let p = theme::pal(ui);
    let mut pressed = None;
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let said = ui.label(RichText::new(title).font(sans(ui, 15.0)).color(p.graphite));
        ui.ctx().accesskit_node_builder(said.id, |node| node.set_live(egui::accesskit::Live::Polite));
        let room = ui.available_width().min(500.0);
        ui.scope(|ui| {
            ui.set_max_width(room);
            ui.add(egui::Label::new(RichText::new(body).font(sans(ui, 12.5)).color(p.text_secondary)).halign(Align::Center));
        });
        if actions.is_empty() {
            return;
        }
        ui.add_space(2.0);
        let pills: Vec<Pill<'_>> = actions
            .iter()
            .map(|(label, key)| {
                let pill = Pill::new(PillKind::Secondary, label);
                if key.is_empty() { pill } else { pill.key_text(key) }
            })
            .collect();
        let total = pills.iter().map(|pill| pill.width(ui)).sum::<f32>() + 8.0 * pills.len().saturating_sub(1) as f32;
        ui.allocate_ui_with_layout(vec2(total, 28.0), Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (i, pill) in pills.into_iter().enumerate() {
                if pill.show(ui).clicked() {
                    pressed = Some(i);
                }
            }
        });
    });
    pressed
}

/// [`empty_state`] for a reading pane with nothing selected: in the middle of the pane, one line of why, no action.
pub(crate) fn empty_pane(ui: &mut Ui, title: &str, body: &str) {
    // The visible height, not the available one: inside a scroll area the latter has no end.
    let used = ui.cursor().top() - ui.clip_rect().top();
    ui.add_space((ui.clip_rect().height() / 2.0 - 60.0 - used).max(0.0));
    empty_state(ui, title, body, &[]);
}

/// A comment between two lines of the diff. The head has an avatar chip, the name and a tag; `add` draws the body and the
/// actions under the name.
pub(crate) fn comment_card(ui: &mut Ui, width: f32, kind: CardKind, who: &str, tag_text: &str, add: impl FnOnce(&mut Ui)) {
    let p = theme::pal(ui);
    let edge = if kind == CardKind::Ai { p.haze } else { p.border_strong };
    card_band(ui, width, edge, |ui| card_content(ui, kind, who, tag_text, add));
}

/// [`comment_card`] with no line to sit between (frame 79, under a rendered SVG): the card 24 px in from the pane and 14 px
/// from what is above and below it, on the pane's own ground.
pub(crate) fn loose_comment_card(ui: &mut Ui, width: f32, kind: CardKind, who: &str, tag_text: &str, add: impl FnOnce(&mut Ui)) {
    let p = theme::pal(ui);
    let edge = if kind == CardKind::Ai { p.haze } else { p.border_strong };
    padded(ui, Margin { left: 24, right: 24, top: 14, bottom: 14 }, |ui| {
        egui::Frame::new().fill(p.raised).stroke(Stroke::new(1.0, edge)).corner_radius(8).inner_margin(13).show(ui, |ui| {
            ui.set_width((width - 48.0 - 26.0).clamp(200.0, 594.0));
            card_content(ui, kind, who, tag_text, add);
        });
    });
}

/// The head of a comment card (avatar or AI mark, the name, a tag) and `add` under it, in line with the name.
fn card_content(ui: &mut Ui, kind: CardKind, who: &str, tag_text: &str, add: impl FnOnce(&mut Ui)) {
    let p = theme::pal(ui);
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if kind == CardKind::Ai {
                ai_mark(ui);
            } else {
                avatar(ui, who, 20.0);
            }
            label(ui, who, 13.0, p.graphite);
            tag(ui, TagKind::OnTheWay, tag_text);
        });
        ui.add_space(6.0);
        ui.horizontal_top(|ui| {
            ui.add_space(28.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                add(ui);
            });
        });
    });
}

// ---- SVG compare and markup (frame 79) ----

/// What one side of an SVG compare shows in its box.
pub(crate) enum SvgPicture<'a> {
    Drawn { texture: egui::TextureId, size: egui::Vec2 },
    /// Nothing drawn, and why: the change adds or deletes the file, or the side could not be read or drawn.
    Note(&'a str),
}

/// One side of an SVG compare, 240 × 206: a dot and its name with the file's size on the right, the drawing centred on a
/// checkerboard in a 240 × 160 box, and a caption under the box.
pub(crate) fn svg_side(ui: &mut Ui, name: &str, dot: Color32, size: &str, picture: SvgPicture<'_>, caption: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(theme::SVG_BOX_W, 206.0), Sense::hover());
    ui.painter().circle_filled(pos2(rect.left() + 3.0, rect.top() + 8.0), 3.0, dot);
    put(ui, galley(ui, name, sans(ui, 12.5), p.graphite), rect.left() + 12.0, rect.top() + 8.0);
    put_right(ui, galley(ui, size, mono(ui, 11.5), p.text_tertiary), rect.right(), rect.top() + 8.0);
    let frame = Rect::from_min_size(pos2(rect.left(), rect.top() + 24.0), vec2(theme::SVG_BOX_W, theme::SVG_BOX_H));
    checker(ui, frame);
    let alt = match picture {
        SvgPicture::Drawn { texture, size: drawn } => {
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            ui.painter().image(texture, Rect::from_center_size(frame.center(), drawn), uv, Color32::WHITE);
            format!("{name}, {size}, {caption}")
        }
        SvgPicture::Note(text) => {
            let note = ui.painter().layout(text.to_string(), sans(ui, 11.5), p.text_secondary, theme::SVG_ROOM_W);
            let at = frame.center() - note.size() / 2.0;
            let backing = Rect::from_min_size(at, note.size()).expand2(vec2(6.0, 4.0));
            ui.painter().rect_filled(backing, radius(4.0), p.raised);
            ui.painter().galley(at, note, Color32::PLACEHOLDER);
            format!("{name}: {text}")
        }
    };
    ui.painter().rect_stroke(frame, radius(6.0), Stroke::new(theme::sz(ui).hairline, p.border), StrokeKind::Inside);
    put(ui, galley(ui, caption, sans(ui, 11.0), p.text_tertiary), rect.left(), rect.top() + 199.0);
    response.widget_info(|| WidgetInfo::labeled(Role::Image, true, alt.clone()));
    response
}

/// The transparency checkerboard of an SVG box: raised squares and hover squares, the corner squares rounded with the box.
fn checker(ui: &Ui, frame: Rect) {
    let p = theme::pal(ui);
    ui.painter().rect_filled(frame, radius(6.0), p.raised);
    let step = theme::SVG_CHECKER;
    let (cols, rows) = ((frame.width() / step).ceil() as usize, (frame.height() / step).ceil() as usize);
    for row in 0..rows {
        for col in (0..cols).filter(|col| (col + row) % 2 == 1) {
            let square = Rect::from_min_size(frame.min + vec2(col as f32 * step, row as f32 * step), vec2(step, step)).intersect(frame);
            let corner = |at_col: bool, at_row: bool| if at_col && at_row { 6 } else { 0 };
            let (first_col, last_col, first_row, last_row) = (col == 0, col + 1 == cols, row == 0, row + 1 == rows);
            let round = CornerRadius {
                nw: corner(first_col, first_row),
                ne: corner(last_col, first_row),
                sw: corner(first_col, last_row),
                se: corner(last_col, last_row),
            };
            ui.painter().rect_filled(square, round, p.hover);
        }
    }
}

/// The heading over an SVG's markup rows, with the view the view key goes to next on the right (`Structure ⇧S`).
pub(crate) fn svg_markup_head(ui: &mut Ui, width: f32, next: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
    let title = "What changed in the markup";
    put(ui, galley(ui, title, sans(ui, 12.0), p.text_secondary), rect.left() + 24.0, rect.center().y);
    put_right(ui, galley(ui, next, sans(ui, 11.5), p.text_tertiary), rect.right() - 24.0, rect.center().y);
    response.widget_info(|| WidgetInfo::labeled(Role::Heading, true, format!("{title}. {next}")));
    response
}

/// What a markup row says about its value.
pub(crate) enum MarkupValue<'a> {
    Same(&'a str),
    Changed(&'a str, &'a str),
    Added(&'a str),
    Removed(&'a str),
}

/// One row of what changed in an SVG's markup: the element and attribute in the code font, then the old value in a rose chip,
/// an arrow and the new value in a moss chip; an unchanged value as plain text. 24 px in from the pane. Pressing it picks it.
pub(crate) fn svg_markup_row(ui: &mut Ui, width: f32, what: &str, value: &MarkupValue<'_>, selected: bool) -> Response {
    let p = theme::pal(ui);
    let height = if matches!(value, MarkupValue::Same(_)) { 24.0 } else { 26.0 };
    let (outer, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let rect = Rect::from_min_max(pos2(outer.left() + 24.0, outer.top()), pos2(outer.right() - 24.0, outer.bottom()));
    if let Some(fill) = quiet_fill(ui, &response, true).or(selected.then_some(p.hover)) {
        ui.painter().rect_filled(rect, radius(6.0), fill);
    }
    let y = rect.center().y;
    let x = put(ui, galley(ui, what, mono(ui, 12.0), p.text_secondary), rect.left() + 13.0, y) + 12.0;
    let room = (rect.right() - 8.0 - x).max(40.0);
    let chip = |ui: &Ui, text: &str, left: f32, max: f32, added: bool| {
        let (bg, gutter, ink) = if added { (p.moss_bg, p.moss_gutter, p.moss_text) } else { (p.rose_bg, p.rose_gutter, p.rose_text) };
        let text = fit(ui, text, mono(ui, 12.0), ink, (max - 8.0).max(8.0));
        let chip = Rect::from_min_size(pos2(left, y - 9.0), vec2(text.size().x + 8.0, 18.0));
        ui.painter().rect_filled(chip, radius(3.0), bg);
        ui.painter().rect_filled(chip, radius(3.0), gutter);
        put(ui, text, left + 4.0, y);
        chip.right()
    };
    let said = match value {
        MarkupValue::Same(text) => {
            let shown = format!("{text} · unchanged");
            put(ui, fit(ui, &shown, mono(ui, 12.0), p.text_tertiary, room), x, y);
            shown
        }
        MarkupValue::Changed(old, new) => {
            let each = (room - 34.0) / 2.0;
            let right = chip(ui, old, x, each, false);
            let arrow = put(ui, galley(ui, "→", sans(ui, 12.0), p.text_tertiary), right + 12.0, y);
            chip(ui, new, arrow + 12.0, each, true);
            format!("{old} changed to {new}")
        }
        MarkupValue::Added(text) => {
            let shown = if text.is_empty() { "added".to_string() } else { format!("added: {text}") };
            chip(ui, &shown, x, room, true);
            shown
        }
        MarkupValue::Removed(text) => {
            let shown = if text.is_empty() { "removed".to_string() } else { format!("removed: {text}") };
            chip(ui, &shown, x, room, false);
            shown
        }
    };
    ring(ui, &response, rect, 6.0);
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, format!("{what}: {said}")));
    response
}

/// One comment of a thread: an avatar chip, the author and when, then the text under them.
pub(crate) fn comment_row(ui: &mut Ui, author: &str, when: &str, body: &str) {
    let p = theme::pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        avatar(ui, author, 20.0);
        label(ui, author, 12.5, p.graphite);
        if !when.is_empty() {
            label(ui, when, 12.0, p.text_tertiary);
        }
    });
    ui.horizontal_top(|ui| {
        ui.add_space(28.0);
        body_text(ui, body);
    });
}

/// A thread folded to one line (resolved, or outdated): a chip, the first words of the thread, and a press to open it. The
/// whole strip is the button; `action` is the words on its right (`Show original ›`).
pub(crate) fn strip_row(ui: &mut Ui, width: f32, chip: &str, text: &str, action: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 44.0), Sense::click());
    ui.painter().rect_filled(rect, 0.0, p.sunken);
    seam_top(ui, rect);
    seam_bottom(ui, rect);
    let card = Rect::from_min_max(
        pos2(rect.left() + 104.0, rect.top() + 6.0),
        pos2((rect.left() + 104.0 + 640.0).min(rect.right() - 24.0), rect.bottom() - 6.0),
    );
    let strip_fill = if response.is_pointer_button_down_on() {
        p.pressed
    } else if response.hovered() {
        p.hover
    } else {
        p.raised
    };
    ui.painter().rect(card, radius(8.0), strip_fill, Stroke::new(1.0, p.border_strong), StrokeKind::Inside);
    let centre = card.center().y;
    let after_chip = {
        let chip_galley = galley(ui, chip, sans(ui, 11.5), p.text_secondary);
        let chip_rect = Rect::from_min_size(pos2(card.left() + 8.0, centre - 10.0), vec2(chip_galley.size().x + 16.0, 20.0));
        ui.painter().rect_filled(chip_rect, radius(10.0), p.hover);
        put(ui, chip_galley, chip_rect.left() + 8.0, centre);
        chip_rect.right()
    };
    let action_left = put_right(ui, galley(ui, action, sans(ui, 12.5), p.graphite), card.right() - 12.0, centre);
    put(ui, fit(ui, text, sans(ui, 12.5), p.text_secondary, action_left - after_chip - 28.0), after_chip + 12.0, centre);
    ring(ui, &response, card, 8.0);
    let name = format!("{chip}: {text}. {action}");
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, name.clone()));
    response
}

/// A commit in the Commits to review popover (frame 95): a box, its short id, its title and its age. 28 px.
pub(crate) fn commit_row(ui: &mut Ui, ticked: bool, sha: &str, title: &str, age: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(rect, radius(6.0), fill);
    }
    let boxed = Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), vec2(14.0, 14.0));
    if ticked {
        ui.painter().rect_filled(boxed, radius(3.0), p.inverse);
        let stroke = Stroke::new(1.6, p.on_inverse);
        let tick = [boxed.left_center() + vec2(3.0, 0.5), boxed.center_bottom() + vec2(-1.0, -3.5), boxed.right_top() + vec2(-3.0, 4.0)];
        ui.painter().line_segment([tick[0], tick[1]], stroke);
        ui.painter().line_segment([tick[1], tick[2]], stroke);
    } else {
        ui.painter().rect(boxed, radius(3.0), p.raised, Stroke::new(1.0, p.control), StrokeKind::Inside);
    }
    let age_left = put_right(ui, galley(ui, age, sans(ui, 11.5), faint(ui, &response)), rect.right() - 12.0, rect.center().y);
    let after_sha = put(ui, galley(ui, sha, mono(ui, 11.5), faint(ui, &response)), rect.left() + 32.0, rect.center().y);
    put(ui, fit(ui, title, sans(ui, 12.5), p.graphite, age_left - after_sha - 24.0), after_sha + 10.0, rect.center().y);
    ring(ui, &response, rect, 6.0);
    let name = format!("{sha} {title}, {age}");
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, ticked, name.clone()));
    response
}

/// One of a short list of ranges, the chosen one with a tick on its right: a name and a line under it. 44 px.
pub(crate) fn range_row(ui: &mut Ui, chosen: bool, title: &str, note: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    if chosen {
        ui.painter().rect_filled(rect, radius(6.0), p.hover);
    } else if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(rect, radius(6.0), fill);
    }
    put(ui, galley(ui, title, sans(ui, 13.0), p.graphite), rect.left() + 12.0, rect.top() + 15.0);
    put(ui, fit(ui, note, sans(ui, 11.5), p.text_secondary, rect.width() - 56.0), rect.left() + 12.0, rect.top() + 32.0);
    if chosen {
        let at = pos2(rect.right() - 20.0, rect.center().y);
        let stroke = Stroke::new(1.8, p.graphite);
        ui.painter().line_segment([at + vec2(-5.0, 0.0), at + vec2(-1.5, 3.5)], stroke);
        ui.painter().line_segment([at + vec2(-1.5, 3.5), at + vec2(5.0, -4.0)], stroke);
    }
    ring(ui, &response, rect, 6.0);
    let name = format!("{title}, {note}");
    response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, chosen, name.clone()));
    response
}

/// One choice of the Open locally popover (frame 96): its name and, under it, a line in mono that wraps in full; the key that
/// does the same on the right. The `primary` choice is drawn inverse. A choice that cannot run now shows why in its line.
pub(crate) fn local_row(ui: &mut Ui, title: &str, line: &str, key: &str, primary: bool, enabled: bool) -> Response {
    let p = theme::pal(ui);
    let inverse = primary && enabled;
    let (ink, quiet) = match (inverse, enabled) {
        (true, _) => (p.on_inverse, p.on_inverse.gamma_multiply(0.7)),
        (false, true) => (p.graphite, p.text_secondary),
        (false, false) => (p.text_tertiary, p.text_tertiary),
    };
    let cap = (!key.is_empty()).then(|| cap_width(ui, key));
    let room = ui.available_width() - 24.0 - cap.map_or(0.0, |c| c + 8.0);
    let second = (!line.is_empty()).then(|| ui.painter().layout(line.to_string(), mono(ui, 11.0), quiet, room.max(40.0)));
    let height = second.as_ref().map_or(28.0, |g| (25.0 + g.size().y + 5.0).max(44.0));
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), if enabled { Sense::click() } else { Sense::hover() });
    if inverse {
        let fill = if response.is_pointer_button_down_on() {
            p.inverse_pressed
        } else if response.hovered() {
            p.inverse_hover
        } else {
            p.inverse
        };
        ui.painter().rect_filled(rect, radius(6.0), fill);
    } else if let Some(fill) = quiet_fill(ui, &response, enabled) {
        ui.painter().rect_filled(rect, radius(6.0), fill);
    }
    if let Some(width) = cap {
        let tone = if inverse { CapTone::Inverse } else { CapTone::Quiet };
        paint_cap(ui, rect.right() - 12.0 - width, rect.top() + 15.0, key, tone);
    }
    put(ui, fit(ui, title, sans(ui, 12.5), ink, room), rect.left() + 12.0, if second.is_some() { rect.top() + 15.0 } else { rect.center().y });
    if let Some(galley) = second {
        ui.painter().galley(pos2(rect.left() + 12.0, rect.top() + 25.0), galley, Color32::PLACEHOLDER);
    }
    ring(ui, &response, rect, 6.0);
    let name = if line.is_empty() { title.to_string() } else { format!("{title}. {line}") };
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, name.clone()));
    response
}

/// One button of an [`expand_bar`]: its words and the key that does the same.
pub(crate) struct BarButton<'a> {
    pub label: &'a str,
    pub key: Option<String>,
}

/// The band between hunks that stands for lines the diff leaves out (frame 90): two chevrons, what is hidden, and the ways to
/// show it. Returns the button that was pressed.
pub(crate) fn expand_bar(ui: &mut Ui, width: f32, hidden: &str, range: &str, buttons: &[BarButton<'_>]) -> Option<usize> {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 32.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, p.haze_subtle);
    let stroke = Stroke::new(1.5, p.haze_text);
    let icon = pos2(rect.left() + 24.0, rect.center().y);
    for dir in [-1.0, 1.0] {
        let tip = icon + vec2(0.0, dir * 5.5);
        ui.painter().line_segment([tip + vec2(-4.0, -dir * 3.0), tip], stroke);
        ui.painter().line_segment([tip + vec2(4.0, -dir * 3.0), tip], stroke);
    }
    put(ui, galley(ui, hidden, sans(ui, 12.5), p.haze_text), rect.left() + 42.0, rect.center().y);
    let mut pressed = None;
    let inner = Rect::from_min_max(pos2(rect.left() + 42.0, rect.top()), pos2(rect.right() - 16.0, rect.bottom()));
    ui.scope_builder(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (i, button) in buttons.iter().enumerate().rev() {
            let mut pill = Pill::new(PillKind::Quiet, button.label).small();
            if let Some(key) = &button.key {
                pill = pill.key_text(key);
            }
            if pill.show(ui).clicked() {
                pressed = Some(i);
            }
        }
        label(ui, range, 11.5, p.haze_text);
    });
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{hidden}, {range}")));
    pressed
}

fn ai_mark(ui: &mut Ui) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 10.0, p.haze_subtle);
    for (dx, dy) in [(-3.5, -4.0), (0.5, -2.0)] {
        ui.painter().rect_filled(Rect::from_min_size(rect.center() + vec2(dx, dy), vec2(3.0, 8.0)), radius(1.5), p.haze_text);
    }
    response.widget_info(|| WidgetInfo::labeled(Role::Image, true, "AI"));
    response
}

/// The text of a comment: reading size, graphite, wrapping.
pub(crate) fn body_text(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).font(sans(ui, 13.0)).color(theme::pal(ui).graphite)).wrap());
}

/// A plain raised card of radius 8, for the sheets and panels that hold more than a comment.
pub(crate) fn card_frame(ui: &Ui) -> egui::Frame {
    let p = theme::pal(ui);
    egui::Frame::new().fill(p.raised).stroke(Stroke::new(1.0, p.border_strong)).corner_radius(8).inner_margin(12)
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum BannerKind {
    /// Haze: information, work on the way.
    Info,
    /// Ember subtle: needs you.
    Warn,
    /// Rose: an error.
    Error,
}

/// A callout of radius 8: a dot, a title in semibold and what is safe. Returns the response of the title for a live region.
pub(crate) fn banner(ui: &mut Ui, kind: BannerKind, title: &str, body: &str) -> Response {
    let p = theme::pal(ui);
    let (fill, ink, mark) = match kind {
        BannerKind::Info => (p.haze_subtle, p.haze_text, p.haze_text),
        BannerKind::Warn => (p.ember_subtle, p.ember_text, p.ember),
        BannerKind::Error => (p.rose_bg, p.rose_text, p.rose),
    };
    let mut title_response = None;
    egui::Frame::new().fill(fill).corner_radius(8).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (rect, _) = ui.allocate_exact_size(vec2(7.0, 18.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 3.5, mark);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                title_response = Some(ui.add(egui::Label::new(RichText::new(title).font(sans(ui, 12.5)).color(ink).strong()).wrap()));
                if !body.is_empty() {
                    ui.add(egui::Label::new(RichText::new(body).font(sans(ui, 12.0)).color(ink)).wrap());
                }
            });
        });
    });
    title_response.unwrap_or_else(|| ui.label(""))
}

/// A verdict option: a card with a radio, a name and a line under it; the chosen one has a strong border.
pub(crate) fn verdict_card(ui: &mut Ui, selected: bool, title: &str, note: &str) -> Response {
    let p = theme::pal(ui);
    let title_galley = galley(ui, title, sans(ui, 13.0), p.graphite);
    let note_galley = fit(ui, note, sans(ui, 11.5), p.text_secondary, ui.available_width() - 46.0);
    let height = (title_galley.size().y + note_galley.size().y + 14.0).max(48.0);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if selected {
        ui.painter().rect(rect, radius(8.0), p.raised, Stroke::new(1.5, p.graphite), StrokeKind::Inside);
    } else if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(rect, radius(8.0), fill);
    }
    let radio = pos2(rect.left() + 17.0, rect.top() + 16.0);
    ui.painter().circle(radio, 7.0, p.raised, Stroke::new(1.0, if selected { p.graphite } else { p.control }));
    if selected {
        ui.painter().circle_filled(radio, 3.5, p.graphite);
    }
    let left = rect.left() + 34.0;
    put(ui, title_galley, left, rect.top() + 16.0);
    put(ui, note_galley, left, rect.top() + 35.0);
    ring(ui, &response, rect, 8.0);
    response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, selected, title));
    response
}

/// The height [`keys_panel`] takes for `rows` rows.
pub(crate) fn keys_panel_height(rows: usize) -> f32 {
    40.0 + 22.0 * rows as f32
}

/// The keys of the screen: caps and what they do, under a seam.
pub(crate) fn keys_panel(ui: &mut Ui, rows: &[(Vec<String>, String)]) {
    let p = theme::pal(ui);
    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(theme::sz(ui).hairline, p.border));
    ui.add_space(10.0);
    label(ui, "Keys", 12.0, p.text_secondary);
    ui.add_space(4.0);
    let caps_width = |caps: &[String]| caps.iter().map(|cap| cap_width(ui, cap) + 4.0).sum::<f32>();
    let column = rows.iter().map(|(caps, _)| caps_width(caps)).fold(44.0_f32, f32::max) + 4.0;
    for (caps, text) in rows {
        let (row, response) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
        let mut x = row.left();
        for cap in caps {
            x = paint_cap(ui, x, row.center().y, cap, CapTone::Quiet) + 4.0;
        }
        put(ui, fit(ui, text, sans(ui, 12.0), p.text_secondary, row.right() - row.left() - column - 4.0), row.left() + column, row.center().y);
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{text}  {}", caps.join(" "))));
    }
}

// ---- title bar and status bar ----

/// A quiet action of the title bar: a word and its key cap on a 24 px target, radius 6.
pub(crate) fn title_action(ui: &mut Ui, text: &str, key: &str) -> Response {
    let p = theme::pal(ui);
    let text_galley = galley(ui, text, sans(ui, 12.5), p.text_secondary);
    let cap = if key.is_empty() { None } else { Some(cap_width(ui, key)) };
    let width = 16.0 + text_galley.size().x + cap.map_or(0.0, |c| 6.0 + c);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click());
    if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(rect, radius(6.0), fill);
    }
    let right = put(ui, text_galley, rect.left() + 8.0, rect.center().y);
    if !key.is_empty() {
        paint_cap(ui, right + 6.0, rect.center().y, key, CapTone::Quiet);
    }
    ring(ui, &response, rect, 6.0);
    let name = if key.is_empty() { text.to_string() } else { format!("{text}  {key}") };
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, name.clone()));
    response
}

/// A strip of the window chrome. Its parts are drawn in turn: the app puts its fixed items on the right, then a screen puts
/// its own between them and the left edge.
pub(crate) struct Bar {
    rect: Rect,
    right: f32,
}

fn strip(ui: &mut Ui, height: f32, seam_above: bool) -> Bar {
    let rect = ui.allocate_space(vec2(ui.available_width(), height)).1;
    let p = theme::pal(ui);
    ui.painter().rect_filled(rect, 0.0, p.hover);
    if seam_above {
        seam_top(ui, rect);
    } else {
        seam_bottom(ui, rect);
    }
    Bar { rect, right: rect.right() - 14.0 }
}

pub(crate) fn title_bar(ui: &mut Ui) -> Bar {
    strip(ui, theme::TITLE_BAR_HEIGHT, false)
}

pub(crate) fn status_bar(ui: &mut Ui) -> Bar {
    strip(ui, theme::STATUS_BAR_HEIGHT, true)
}

impl Bar {
    /// Width left for [`Bar::left`] between the strip's edge and the items already placed on the right.
    pub(crate) fn room(&self) -> f32 {
        (self.right - self.rect.left() - 14.0).max(0.0)
    }

    pub(crate) fn width(&self) -> f32 {
        self.rect.width()
    }

    /// Items from the left edge, in a row centred on the strip.
    pub(crate) fn left<R>(&self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
        let rect = Rect::from_min_max(pos2(self.rect.left() + 14.0, self.rect.top()), pos2(self.right, self.rect.bottom()));
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
        child.spacing_mut().item_spacing.x = 8.0;
        child.set_clip_rect(rect);
        child.visuals_mut().panel_fill = theme::pal(ui).hover;
        add(&mut child)
    }

    /// Items from the right edge inward; what is left of them stays for [`Bar::left`].
    pub(crate) fn right<R>(&mut self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
        let rect = Rect::from_min_max(self.rect.left_top(), pos2(self.right, self.rect.bottom()));
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::right_to_left(Align::Center)));
        child.spacing_mut().item_spacing.x = 8.0;
        child.set_clip_rect(rect);
        child.visuals_mut().panel_fill = theme::pal(ui).hover;
        let out = add(&mut child);
        let used = child.min_rect();
        if used.width() > 0.0 && used.right() <= self.right + 0.5 {
            self.right = used.left() - 12.0;
        }
        out
    }
}

/// The back button of the title bar: a raised, bordered 24 px target of radius 6. The key is in its name and its tooltip.
pub(crate) fn title_button(ui: &mut Ui, text: &str, key: &str) -> Response {
    let p = theme::pal(ui);
    let text_galley = galley(ui, text, sans(ui, 12.5), p.graphite);
    let (rect, response) = ui.allocate_exact_size(vec2(20.0 + text_galley.size().x, 24.0), Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        p.pressed
    } else if response.hovered() {
        p.hover
    } else {
        p.raised
    };
    ui.painter().rect(rect, radius(6.0), fill, hairline(ui), StrokeKind::Inside);
    put(ui, text_galley, rect.left() + 10.0, rect.center().y);
    ring(ui, &response, rect, 6.0);
    let name = if key.is_empty() { text.to_string() } else { format!("{text}  {key}") };
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, name.clone()));
    if key.is_empty() { response } else { response.on_hover_text(key) }
}

/// `PR Craft / Inbox / Review requested`: the mark, then the trail.
pub(crate) fn breadcrumb(ui: &mut Ui, trail: &[&str]) {
    breadcrumb_with(ui, true, trail);
}

/// [`breadcrumb`], with the words `PR Craft` left out when `brand` is false.
pub(crate) fn breadcrumb_with(ui: &mut Ui, brand: bool, trail: &[&str]) {
    let p = theme::pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        logomark(ui);
        let mut first = !brand;
        if brand {
            label(ui, "PR Craft", 13.0, p.graphite);
        }
        for step in trail {
            if !first {
                label(ui, "/", 13.0, p.text_tertiary);
            }
            first = false;
            label(ui, *step, 13.0, p.text_secondary);
        }
    });
}

/// The width `text` takes at `px`, for choosing what fits.
pub(crate) fn text_width(ui: &Ui, text: &str, px: f32) -> f32 {
    galley(ui, text, sans(ui, px), Color32::PLACEHOLDER).size().x
}

/// A title-bar pill with a dot: `● 3 new`.
pub(crate) fn status_pill(ui: &mut Ui, dot_colour: Color32, text: &str) -> Response {
    let p = theme::pal(ui);
    let text_galley = galley(ui, text, sans(ui, 12.0), p.graphite);
    let (rect, response) = ui.allocate_exact_size(vec2(text_galley.size().x + 38.0, 24.0), Sense::hover());
    ui.painter().rect(rect, radius(12.0), p.raised, hairline(ui), StrokeKind::Inside);
    ui.painter().circle_filled(pos2(rect.left() + 14.0, rect.center().y), 3.0, dot_colour);
    put(ui, text_galley, rect.left() + 25.0, rect.center().y);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    response
}

/// A status-bar item: a dot and a few words in the code font.
pub(crate) fn connection(ui: &mut Ui, dot_colour: Color32, text: &str) -> Response {
    let p = theme::pal(ui);
    let text_galley = galley(ui, text, mono(ui, 11.0), p.text_secondary);
    let (rect, response) = ui.allocate_exact_size(vec2(text_galley.size().x + 16.0, 14.0), Sense::hover());
    ui.painter().circle_filled(pos2(rect.left() + 4.0, rect.center().y), 4.0, dot_colour);
    put(ui, text_galley, rect.left() + 16.0, rect.center().y);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    response
}

// ---- settings and forms ----

/// A page title and the sentence under it.
pub(crate) fn page_title(ui: &mut Ui, title: &str, description: &str) {
    let p = theme::pal(ui);
    ui.add(egui::Label::new(RichText::new(title).size(theme::sz(ui).heading).color(p.graphite)).wrap());
    if !description.is_empty() {
        ui.add_space(6.0);
        note_sized(ui, description, 13.0, p.text_secondary);
    }
    ui.add_space(theme::SPACE_3);
}

/// The caption over a group of rows: `Forges`, `Text`, `Runs`.
pub(crate) fn group_label(ui: &mut Ui, text: &str) {
    ui.add_space(theme::SPACE_3);
    label(ui, text, 11.5, theme::pal(ui).text_tertiary);
    ui.add_space(6.0);
}

/// One setting: its name and what it does on the left, the control on the right, a hairline above. Close a group of rows
/// with [`rule`].
pub(crate) fn form_row<R>(ui: &mut Ui, title: &str, hint: &str, control: impl FnOnce(&mut Ui) -> R) -> R {
    form_row_fit(ui, title, hint, 0.0, control)
}

/// [`form_row`] for a control that is `control_width` px wide or more. The left column never takes what the control needs, so its
/// hint wraps; when the row has no room for both, the control goes on its own line under the name.
pub(crate) fn form_row_fit<R>(ui: &mut Ui, title: &str, hint: &str, control_width: f32, control: impl FnOnce(&mut Ui) -> R) -> R {
    let p = theme::pal(ui);
    rule(ui);
    egui::Frame::new()
        .inner_margin(Margin::symmetric(0, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let width = ui.available_width();
            let words = |ui: &mut Ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                label(ui, title, 13.0, p.graphite);
                if !hint.is_empty() {
                    note_sized(ui, hint, 12.0, p.text_secondary);
                }
            };
            if control_width > 0.0 && width < control_width + 240.0 {
                ui.vertical(words);
                ui.add_space(8.0);
                // A right-to-left layout takes all the height there is; the control gets one line.
                return ui
                    .allocate_ui_with_layout(vec2(width, 28.0), Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        control(ui)
                    })
                    .inner;
            }
            let left = if control_width > 0.0 { (width - control_width - 16.0).max(160.0) } else { width * 0.55 };
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_max_width(left);
                    words(ui);
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    control(ui)
                })
                .inner
            })
            .inner
        })
        .inner
}

/// An on/off switch of 34×20: graphite and knob right when on. Returns the new value when it was pressed.
pub(crate) fn toggle(ui: &mut Ui, on: bool, name: &str) -> Option<bool> {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(34.0, 20.0), Sense::click());
    ui.painter().rect(
        rect,
        radius(10.0),
        match (on, response.is_pointer_button_down_on(), response.hovered()) {
            (true, true, _) => p.inverse_pressed,
            (true, _, true) => p.inverse_hover,
            (true, ..) => p.inverse,
            (false, true, _) => p.border_strong,
            (false, _, true) => p.pressed,
            (false, ..) => p.hover,
        },
        if on { Stroke::NONE } else { Stroke::new(1.0, p.control) },
        StrokeKind::Inside,
    );
    let knob = pos2(if on { rect.right() - 10.0 } else { rect.left() + 10.0 }, rect.center().y);
    ui.painter().circle_filled(knob, 7.0, if on { p.on_inverse } else { p.raised });
    ring(ui, &response, rect, 10.0);
    response.widget_info(|| WidgetInfo::selected(Role::Switch, true, on, name));
    response.clicked().then_some(!on)
}

/// A choice between a few values, as a segmented control on the right of a row. Returns the one pressed.
pub(crate) fn choose<T: Copy + PartialEq>(ui: &mut Ui, group: &str, all: &[(T, String)], current: T) -> Option<T> {
    let labels: Vec<String> = all.iter().map(|(_, text)| text.clone()).collect();
    // A value that is not among the options selects none of them; it is never shown as the first.
    let at = all.iter().position(|(value, _)| *value == current).unwrap_or(usize::MAX);
    segmented(ui, group, group, &labels, at).and_then(|i| all.get(i)).map(|(value, _)| *value)
}

/// A card with a radio on its left, a title and a line under it, and `right` on the far side; the chosen card has a strong
/// border. Returns whether it was pressed.
pub(crate) fn option_card(ui: &mut Ui, selected: bool, enabled: bool, title: &str, note: &str, right: impl FnOnce(&mut Ui)) -> Response {
    let p = theme::pal(ui);
    let width = ui.available_width();
    let height = 62.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), if enabled { Sense::click() } else { Sense::hover() });
    let (edge, stroke) = if selected { (p.graphite, 1.5) } else { (p.border_strong, 1.0) };
    let fill = if enabled { quiet_fill(ui, &response, true).unwrap_or(p.raised) } else { p.raised };
    ui.painter().rect(rect, radius(8.0), fill, Stroke::new(stroke, edge), StrokeKind::Inside);
    let radio = pos2(rect.left() + 24.0, rect.top() + 21.0);
    ui.painter().circle(radio, 7.0, p.raised, Stroke::new(1.0, if selected { p.graphite } else { p.control }));
    if selected {
        ui.painter().circle_filled(radio, 3.5, p.graphite);
    }
    let ink = if enabled { p.graphite } else { p.text_tertiary };
    put(ui, galley(ui, title, sans(ui, 13.0), ink), rect.left() + 46.0, rect.top() + 21.0);
    put(ui, fit(ui, note, sans(ui, 12.0), p.text_secondary, rect.width() * 0.6), rect.left() + 46.0, rect.top() + 42.0);
    let inner = Rect::from_min_max(pos2(rect.left() + rect.width() * 0.5, rect.top()), pos2(rect.right() - 16.0, rect.bottom()));
    let mut child = ui.new_child(UiBuilder::new().id_salt(("option-card", title)).max_rect(inner).layout(Layout::right_to_left(Align::Center)));
    right(&mut child);
    ring(ui, &response, rect, 8.0);
    response.widget_info(|| WidgetInfo::selected(Role::RadioButton, enabled, selected, title));
    response
}

// ---- reviewed changes and notifications ----

/// A change you reviewed, or an event about one: forge mark, title, mono meta line, a status line, and on the right who it waits
/// on. 80 px.
pub(crate) struct ReviewedRow<'a> {
    pub forge: Forge,
    pub title: &'a str,
    pub meta: String,
    pub line: String,
    /// `Waiting on you 1d`, `With dev.sato`.
    pub right: String,
    /// The right-hand words are about you: Ember text and an unread dot.
    pub yours: bool,
    pub selected: bool,
}

pub(crate) fn reviewed_row(ui: &mut Ui, id: &str, row: &ReviewedRow) -> Response {
    let p = theme::pal(ui);
    let height = row_height(ui, 80.0);
    let (rect, response) = keyed(ui, id, vec2(ui.available_width(), height));
    if ui.is_rect_visible(rect) {
        row_ground(ui, rect, row.selected, &response);
        let (top, right) = (rect.top() + 12.0 + theme::sz(ui).row, rect.right() - 20.0);
        if row.yours {
            ui.painter().circle_filled(pos2(rect.left() + 11.0, top + 9.0), 3.0, p.ember);
        }
        forge_disc(ui, pos2(rect.left() + 31.0, top + 9.0), 18.0, row.forge);
        let text_left = rect.left() + 48.0;
        let ink = if row.yours { p.ember_text } else { faint(ui, &response) };
        let right_left = put_right(ui, galley(ui, &row.right, sans(ui, 11.5), ink), right, top + 9.0);
        put(ui, fit(ui, row.title, sans(ui, 13.0), p.graphite, right_left - 12.0 - text_left), text_left, top + 9.5);
        put(ui, fit(ui, &row.meta, mono(ui, 11.5), faint(ui, &response), right - text_left), text_left, top + 30.0);
        put(ui, fit(ui, &row.line, sans(ui, 11.5), p.text_secondary, right - text_left), text_left, top + 50.0);
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(theme::sz(ui).hairline, p.border));
        ring_in(ui, &response, rect, 8.0);
    }
    let name = format!("{}, {}, {}, {}", row.title, row.meta, row.line, row.right);
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, row.selected, name.clone()));
    response
}

/// A band over a group of rows: its name and count on the left, what the group means on the right. 32 px on the sunken tone.
pub(crate) fn group_band(ui: &mut Ui, name: &str, count: usize, meaning: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, p.sunken);
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(theme::sz(ui).hairline, p.border));
    let after = put(ui, galley(ui, name, sans(ui, 12.0), p.graphite), rect.left() + 20.0, rect.center().y);
    put(ui, galley(ui, &count.to_string(), sans(ui, 12.0), p.text_tertiary), after + 8.0, rect.center().y);
    if !meaning.is_empty() {
        put_right(ui, fit(ui, meaning, sans(ui, 11.5), p.text_tertiary, rect.width() * 0.5), rect.right() - 20.0, rect.center().y);
    }
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{name} {count}  {meaning}")));
    response
}

pub(crate) struct EventRow<'a> {
    pub forge: Forge,
    pub text: &'a str,
    pub meta: &'a str,
    pub age: &'a str,
    pub unseen: bool,
    pub selected: bool,
}

/// An event of the notifications list: an unread dot, the event, the change under it, and its age. 60 px.
pub(crate) fn event_row(ui: &mut Ui, id: &str, row: &EventRow) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = keyed(ui, id, vec2(ui.available_width(), row_height(ui, 60.0)));
    if ui.is_rect_visible(rect) {
        row_ground(ui, rect, row.selected, &response);
        let (top, right) = (rect.top() + 12.0 + theme::sz(ui).row, rect.right() - 20.0);
        if row.unseen {
            ui.painter().circle_filled(pos2(rect.left() + 11.0, top + 9.0), 3.0, p.ember);
        }
        forge_disc(ui, pos2(rect.left() + 31.0, top + 9.0), 18.0, row.forge);
        let text_left = rect.left() + 48.0;
        let left = put_right(ui, galley(ui, row.age, sans(ui, 11.5), faint(ui, &response)), right, top + 9.0);
        // The unread state is a word as well as a dot.
        let text_start = if row.unseen { put(ui, galley(ui, "New", sans(ui, 11.5), p.ember_text), text_left, top + 9.5) + 8.0 } else { text_left };
        put(ui, fit(ui, row.text, sans(ui, 13.0), p.graphite, left - 12.0 - text_start), text_start, top + 9.5);
        put(ui, fit(ui, row.meta, mono(ui, 11.5), faint(ui, &response), right - text_left), text_left, top + 31.0);
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(theme::sz(ui).hairline, p.border));
        ring_in(ui, &response, rect, 8.0);
    }
    let name = format!("{}{}, {}, {}", if row.unseen { "New, " } else { "" }, row.text, row.meta, row.age);
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, row.selected, name.clone()));
    response
}

/// A row of a timeline: a dot on a rail, a sentence, and when. 26 px.
pub(crate) fn timeline_row(ui: &mut Ui, dot_colour: Color32, text: &str, when: &str, first: bool, last: bool) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let rail = rect.left() + 5.0;
    let line = Stroke::new(2.0, p.border_strong);
    if !first {
        ui.painter().vline(rail, rect.top()..=(rect.center().y - 4.0), line);
    }
    if !last {
        ui.painter().vline(rail, (rect.center().y + 4.0)..=rect.bottom(), line);
    }
    ui.painter().circle_filled(pos2(rail, rect.center().y), 4.0, dot_colour);
    let left = put_right(ui, galley(ui, when, sans(ui, 11.5), p.text_tertiary), rect.right(), rect.center().y);
    put(ui, fit(ui, text, sans(ui, 12.5), p.graphite, left - 28.0 - rect.left()), rect.left() + 24.0, rect.center().y);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{text}, {when}")));
    response
}

// ---- sheets ----

/// A checkbox row of a list: a box, a name, its scope in the code font, and a count on the right. 32 px, a hairline above.
pub(crate) fn check_row(ui: &mut Ui, on: bool, name: &str, scope: &str, detail: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(rect, radius(6.0), fill);
    }
    let boxed = Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(14.0, 14.0));
    if on {
        ui.painter().rect_filled(boxed, radius(3.0), p.inverse);
        let tick = [
            pos2(boxed.left() + 3.5, boxed.center().y),
            pos2(boxed.left() + 6.0, boxed.bottom() - 4.0),
            pos2(boxed.right() - 3.0, boxed.top() + 4.0),
        ];
        ui.painter().line_segment([tick[0], tick[1]], Stroke::new(1.6, p.on_inverse));
        ui.painter().line_segment([tick[1], tick[2]], Stroke::new(1.6, p.on_inverse));
    } else {
        ui.painter().rect(boxed, radius(3.0), p.raised, Stroke::new(1.0, p.control), StrokeKind::Inside);
    }
    let right = put_right(ui, fit(ui, detail, sans(ui, 12.0), p.text_secondary, rect.width() * 0.4), rect.right() - 4.0, rect.center().y);
    let after = put(ui, fit(ui, name, sans(ui, 13.0), p.graphite, right - rect.left() - 120.0), rect.left() + 28.0, rect.center().y);
    put(ui, fit(ui, scope, mono(ui, 11.5), faint(ui, &response), right - after - 28.0), after + 10.0, rect.center().y);
    ring(ui, &response, rect, 6.0);
    let described = format!("{name}, {scope}, {detail}");
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, on, described.clone()));
    response
}

/// The files × agents table: a header of column names, a row per file in the code font, a dot where the column reads the file.
pub(crate) fn coverage_table(ui: &mut Ui, columns: &[String], rows: &[(String, Vec<bool>)], hover: impl Fn(usize) -> String) {
    let p = theme::pal(ui);
    let width = ui.available_width();
    let first = (width * 0.46).max(160.0);
    let cell = ((width - first) / columns.len().max(1) as f32).max(40.0);
    let (head, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
    ui.painter().rect_filled(head, 0.0, p.sunken);
    put(ui, galley(ui, "File", sans(ui, 11.5), p.text_secondary), head.left() + 12.0, head.center().y);
    for (i, name) in columns.iter().enumerate() {
        let text = fit(ui, name, sans(ui, 11.5), p.text_secondary, cell - 6.0);
        let x = head.left() + first + cell * (i as f32 + 0.5);
        put(ui, Arc::clone(&text), x - text.size().x / 2.0, head.center().y);
    }
    for (n, (path, flags)) in rows.iter().enumerate() {
        let (row, response) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
        ui.painter().hline(row.x_range(), row.top() + 0.5, Stroke::new(theme::sz(ui).hairline, p.border));
        put(ui, fit(ui, path, mono(ui, 11.5), p.graphite, first - 20.0), row.left() + 12.0, row.center().y);
        for (i, &read) in flags.iter().enumerate() {
            let at = pos2(row.left() + first + cell * (i as f32 + 0.5), row.center().y);
            if read {
                ui.painter().circle_filled(at, 3.5, p.graphite);
            } else {
                ui.painter().circle_stroke(at, 3.0, Stroke::new(1.0, p.control));
            }
        }
        let readers: Vec<&str> = columns.iter().zip(flags).filter(|&(_, &read)| read).map(|(name, _)| name.as_str()).collect();
        let name = format!("{path}, read by {}", if readers.is_empty() { "no column".to_string() } else { readers.join(", ") });
        response.widget_info(|| WidgetInfo::labeled(Role::Row, true, name.clone()));
        response.on_hover_text(hover(n));
    }
    let end = ui.cursor().top();
    let frame = Rect::from_min_max(head.min, pos2(head.right(), end));
    ui.painter().rect_stroke(frame, radius(6.0), hairline(ui), StrokeKind::Inside);
}

/// A two-line list row: the name, a line under it, and an optional mono line. Graphite with raised text when selected. 66 px,
/// radius 8.
pub(crate) fn list_row(ui: &mut Ui, id: &str, selected: bool, title: &str, line: &str, mono_line: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = keyed(ui, id, vec2(ui.available_width(), row_height(ui, 66.0)));
    if selected {
        ui.painter().rect_filled(rect, radius(8.0), p.inverse);
    } else if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(rect, radius(8.0), fill);
    }
    let (ink, quiet) = if selected { (p.on_inverse, p.on_inverse_quiet) } else { (p.graphite, p.text_secondary) };
    let (left, width) = (rect.left() + 10.0, rect.width() - 20.0);
    let top = rect.top() + 10.0 + theme::sz(ui).row;
    put(ui, fit(ui, title, sans(ui, 13.0), ink, width), left, top + 7.0);
    put(ui, fit(ui, line, sans(ui, 11.5), quiet, width), left, top + 24.0);
    put(ui, fit(ui, mono_line, mono(ui, 11.0), quiet, width), left, top + 40.0);
    if selected {
        ring_on_inverse(ui, &response, rect, 8.0);
    } else {
        ring_in(ui, &response, rect, 8.0);
    }
    let name = [title, line, mono_line].iter().filter(|t| !t.is_empty()).copied().collect::<Vec<_>>().join(", ");
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, name.clone()));
    response
}

/// Names a text field to assistive technology when its label is drawn apart from it.
pub(crate) fn name_field(ui: &Ui, response: &Response, name: &str) {
    let name = name.to_string();
    ui.ctx().accesskit_node_builder(response.id, |node| node.set_label(name));
}

// ---- runs ----

/// The change a group of runs belongs to: its forge mark and mono reference. 28 px.
pub(crate) fn run_group(ui: &mut Ui, forge: Forge, reference: &str, title: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    forge_disc(ui, pos2(rect.left() + 29.0, rect.center().y + 3.0), 16.0, forge);
    let after = put(ui, galley(ui, reference, mono(ui, 11.5), p.text_secondary), rect.left() + 46.0, rect.center().y + 3.0);
    put(ui, fit(ui, title, sans(ui, 12.0), p.text_tertiary, rect.right() - after - 28.0), after + 10.0, rect.center().y + 3.0);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{reference} {title}")));
    response
}

pub(crate) struct RunRow {
    pub name: String,
    pub detail: String,
    pub clock: String,
    pub dot: Color32,
    /// Tinted Ember subtle, and the detail in Ember text.
    pub stuck: bool,
    pub selected: bool,
}

/// A run: a state dot, the agent and its state, what it last did, and the elapsed time. 52 px.
pub(crate) fn run_row(ui: &mut Ui, id: &str, row: &RunRow) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = keyed(ui, id, vec2(ui.available_width(), row_height(ui, 52.0)));
    let body = rect.shrink2(vec2(12.0, 1.0));
    if row.stuck {
        ui.painter().rect_filled(body, radius(8.0), p.ember_subtle);
    } else if row.selected {
        ui.painter().rect_filled(body, radius(8.0), p.raised);
    } else if let Some(fill) = quiet_fill(ui, &response, true) {
        ui.painter().rect_filled(body, radius(8.0), fill);
    }
    if row.selected {
        ui.painter().rect(body, radius(8.0), Color32::TRANSPARENT, Stroke::new(1.5, p.graphite), StrokeKind::Inside);
    }
    let top = rect.top() + 11.0 + theme::sz(ui).row;
    ui.painter().circle_filled(pos2(body.left() + 14.0, top + 8.0), 3.5, row.dot);
    let right = put_right(ui, galley(ui, &row.clock, mono(ui, 11.5), faint(ui, &response)), body.right() - 12.0, top + 8.0);
    put(ui, fit(ui, &row.name, sans(ui, 13.0), p.graphite, right - body.left() - 40.0), body.left() + 28.0, top + 8.0);
    let ink = if row.stuck { p.ember_text } else { p.text_secondary };
    put(ui, fit(ui, &row.detail, sans(ui, 11.5), ink, body.width() - 56.0), body.left() + 28.0, top + 26.0);
    ring(ui, &response, body, 8.0);
    let name = format!("{}, {}, {}", row.name, row.detail, row.clock);
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, row.selected, name.clone()));
    response
}

/// A theme choice: a miniature window in the colours of the theme, a radio and its name. `System` shows half of each.
pub(crate) fn theme_card(ui: &mut Ui, rect: Rect, response: &Response, kind: pr_craft_core::settings::Theme, name: &str, selected: bool) {
    use pr_craft_core::settings::Theme;
    let p = theme::pal(ui);
    let (edge, width) = if selected {
        (p.graphite, 1.5)
    } else if response.hovered() {
        (p.control, 1.0)
    } else {
        (p.border_strong, 1.0)
    };
    ui.painter().rect(rect, radius(10.0), p.raised, Stroke::new(width, edge), StrokeKind::Inside);
    let view = Rect::from_min_max(rect.min + vec2(7.0, 7.0), pos2(rect.right() - 7.0, rect.bottom() - 36.0));
    let paint = |palette: Palette, area: Rect| {
        ui.painter().rect_filled(area, radius(6.0), palette.paper);
        let side = Rect::from_min_size(area.min, vec2(area.width() * 0.3, area.height()));
        ui.painter().rect_filled(side, radius(6.0), palette.hover);
        for i in 0..3 {
            let y = area.top() + 12.0 + i as f32 * 12.0;
            let bar = Rect::from_min_size(pos2(area.left() + 8.0, y), vec2(area.width() * 0.15, 4.0));
            ui.painter().rect_filled(bar, radius(2.0), if i == 0 { palette.graphite } else { palette.border_strong });
        }
        for i in 0..4 {
            let y = area.top() + 14.0 + i as f32 * 14.0;
            let bar = Rect::from_min_size(pos2(area.left() + area.width() * 0.4, y), vec2(area.width() * 0.4, 4.0));
            ui.painter().rect_filled(bar, radius(2.0), if i == 0 { palette.graphite } else { palette.border_strong });
        }
    };
    match kind {
        Theme::Light => paint(Palette::light(), view),
        Theme::Dark => paint(Palette::dark(), view),
        Theme::System => {
            paint(Palette::light(), view);
            let half = Rect::from_min_max(pos2(view.center().x, view.top()), view.max);
            let clipped = ui.painter().with_clip_rect(half);
            clipped.rect_filled(half, radius(6.0), Palette::dark().paper);
            clipped.rect_filled(
                Rect::from_min_size(pos2(half.left() + 8.0, half.top() + 14.0), vec2(half.width() * 0.6, 4.0)),
                radius(2.0),
                Palette::dark().graphite,
            );
        }
    }
    let radio = pos2(rect.left() + 20.0, rect.bottom() - 18.0);
    ui.painter().circle(radio, 7.0, p.raised, Stroke::new(1.0, if selected { p.graphite } else { p.control }));
    if selected {
        ui.painter().circle_filled(radio, 3.5, p.graphite);
    }
    put(ui, galley(ui, name, sans(ui, 13.0), p.graphite), rect.left() + 36.0, rect.bottom() - 18.0);
    ring(ui, response, rect, 10.0);
}

/// `1 file`, `4 files`.
pub(crate) fn files_word(n: usize) -> String {
    if n == 1 { "1 file".to_string() } else { format!("{n} files") }
}

/// A tag whose accessible name carries a longer sentence after its words.
pub(crate) fn tag_described(ui: &mut Ui, kind: TagKind, text: &str, description: &str) -> Response {
    let (fill, ink) = tag_colours(theme::pal(ui), kind);
    let text_galley = galley(ui, text, sans(ui, 11.5), ink);
    let (rect, response) = ui.allocate_exact_size(vec2(text_galley.size().x + 16.0, 20.0_f32.max(text_galley.size().y + 4.0)), Sense::hover());
    ui.painter().rect_filled(rect, radius(rect.height() / 2.0), fill);
    put(ui, text_galley, rect.left() + 8.0, rect.center().y);
    let name = format!("{text}: {description}");
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, name.clone()));
    response
}

/// A row of a list that is always in, such as the general pass: a filled box that cannot be pressed, a name, a scope, a detail.
pub(crate) fn included_row(ui: &mut Ui, name: &str, scope: &str, detail: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    let boxed = Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(14.0, 14.0));
    ui.painter().rect(boxed, radius(3.0), p.hover, Stroke::new(1.0, p.border_strong), StrokeKind::Inside);
    ui.painter().circle_filled(boxed.center(), 2.5, p.text_tertiary);
    let right = put_right(ui, fit(ui, detail, sans(ui, 12.0), p.text_secondary, rect.width() * 0.4), rect.right() - 4.0, rect.center().y);
    let after = put(ui, fit(ui, name, sans(ui, 13.0), p.graphite, right - rect.left() - 120.0), rect.left() + 28.0, rect.center().y);
    put(ui, fit(ui, scope, mono(ui, 11.5), p.text_tertiary, right - after - 28.0), after + 10.0, rect.center().y);
    let described = format!("{name}, {scope}, {detail}, always included");
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, described.clone()));
    response
}

#[cfg(test)]
mod tests {
    use craft::egui::{Context, Event, Modifiers, PointerButton, RawInput, Shape};
    use pr_craft_core::settings::{Settings, Theme};

    use super::*;
    use crate::theme::{self, Look, System};

    fn channels(c: Color32) -> [f64; 3] {
        [f64::from(c.r()), f64::from(c.g()), f64::from(c.b())]
    }

    /// `c` over the opaque `below`; a `Color32` holds its channels premultiplied.
    fn over(c: Color32, below: [f64; 3]) -> [f64; 3] {
        let keep = 1.0 - f64::from(c.a()) / 255.0;
        let top = channels(c);
        [top[0] + below[0] * keep, top[1] + below[1] * keep, top[2] + below[2] * keep]
    }

    fn luminance(rgb: [f64; 3]) -> f64 {
        let lin = |v: f64| {
            let v = v / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(rgb[0]) + 0.7152 * lin(rgb[1]) + 0.0722 * lin(rgb[2])
    }

    fn contrast(a: [f64; 3], b: [f64; 3]) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Every painted text with the worst contrast of its colours against what is painted under it: the ground, then each fill
    /// that covers the middle of the text, in paint order.
    fn worst_texts(shapes: &[craft::egui::epaint::ClippedShape], ground: Color32) -> Vec<(String, f64)> {
        fn flatten<'a>(shape: &'a Shape, out: &mut Vec<&'a Shape>) {
            match shape {
                Shape::Vec(inner) => inner.iter().for_each(|s| flatten(s, out)),
                other => out.push(other),
            }
        }
        let mut flat = Vec::new();
        shapes.iter().for_each(|c| flatten(&c.shape, &mut flat));
        let mut fills: Vec<(Rect, Color32)> = Vec::new();
        let mut found = Vec::new();
        for shape in flat {
            match shape {
                Shape::Rect(r) if r.fill.a() > 0 => fills.push((r.rect, r.fill)),
                Shape::Text(t) => {
                    let middle = t.pos + t.galley.rect.center().to_vec2();
                    let under = fills.iter().filter(|(rect, _)| rect.contains(middle)).fold(channels(ground), |below, (_, fill)| over(*fill, below));
                    let worst =
                        t.galley.job.sections.iter().map(|section| contrast(over(section.format.color, under), under)).fold(f64::INFINITY, f64::min);
                    found.push((t.galley.job.text.clone(), worst));
                }
                _ => {}
            }
        }
        found
    }

    type Draw = fn(&mut Ui) -> Response;
    /// The colour of the pane a row sits on, from the palette.
    type Ground = fn(&theme::Palette) -> Color32;

    fn rows() -> Vec<(&'static str, Draw)> {
        vec![
            ("nav item", |ui| nav_item(ui, false, "Back to you", "4")),
            ("inbox row", |ui| {
                let row = InboxRow {
                    forge: Forge::GitHub,
                    title: "Retry webhooks",
                    meta: "acme/api #482  ·  nina.k  ·  2h".into(),
                    draft: true,
                    unread: true,
                    selected: false,
                };
                inbox_row(ui, "a", &row)
            }),
            ("stack row", |ui| {
                let layers = vec![
                    Layer { title: "Drop the legacy path", reference: "!1297".into(), draft: true },
                    Layer { title: "Void takes the lock", reference: "!1295".into(), draft: false },
                ];
                let row = StackRow {
                    forge: Forge::GitLab,
                    repo: "billing-service",
                    size: 2,
                    layers,
                    meta: "tokafor  ·  3h".into(),
                    selected: false,
                    unread: true,
                };
                stack_row(ui, "b", &row)
            }),
            ("reviewed row", |ui| {
                let row = ReviewedRow {
                    forge: Forge::GitHub,
                    title: "Pin provider",
                    meta: "infra !88".into(),
                    line: "your last review → 2 commits".into(),
                    right: "With dev.sato".into(),
                    yours: false,
                    selected: false,
                };
                reviewed_row(ui, "c", &row)
            }),
            ("event row", |ui| {
                let row = EventRow { forge: Forge::GitHub, text: "nina.k pushed", meta: "acme/api #479", age: "5h", unseen: false, selected: false };
                event_row(ui, "d", &row)
            }),
            ("file row", |ui| review_file_row(ui, "retry.go", false, false, 8, 3)),
            ("folder row", |ui| folder_row(ui, "internal/webhooks", false, 1, 3)),
            ("draft row", |ui| draft_row(ui, "backoff cap is hard-coded", "retry.go:35")),
            ("hunk header", |ui| hunk_header(ui, 400.0, "@@ -30,7 +30,12 @@ func Deliver")),
            ("commit row", |ui| commit_row(ui, false, "3be0d71", "Add LockInvoice helper", "25h")),
            ("check row", |ui| check_row(ui, false, "Security reviewer", "**/*", "4 files")),
            ("run row", |ui| {
                let row = RunRow {
                    name: "Security · running".into(),
                    detail: "Last output 2 s ago".into(),
                    clock: "1m 12s".into(),
                    dot: Color32::GRAY,
                    stuck: false,
                    selected: false,
                };
                run_row(ui, "e", &row)
            }),
            ("list row", |ui| list_row(ui, "f", false, "Security reviewer", "Secrets, injection", "**/*")),
        ]
    }

    /// Draws `draw` with the pointer over it (and, for `down`, pressed on it) and returns the contrast of its worst text.
    fn contrast_of(draw: Draw, dark: bool, ground: Ground, down: bool) -> Vec<(String, f64)> {
        let ctx = Context::default();
        let look = Look::new(&Settings { theme: if dark { Theme::Dark } else { Theme::Light }, ..Settings::default() }, System::default());
        theme::install(&ctx, look);
        let ground = ground(&look.palette);
        let at = pos2(60.0, 20.0);
        let frame = |events: Vec<Event>| {
            let mut shapes = Vec::new();
            let out = ctx.run_ui(RawInput { events, ..RawInput::default() }, |ui| {
                ui.visuals_mut().panel_fill = ground;
                ui.painter().rect_filled(ui.max_rect(), 0.0, ground);
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                draw(ui);
            });
            shapes.extend(out.shapes.clone());
            out.drop_without_applying_deltas();
            shapes
        };
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(Vec::new());
        frame(vec![Event::PointerMoved(at)]);
        let shapes = if down {
            frame(vec![button(true)]);
            frame(Vec::new())
        } else {
            frame(Vec::new())
        };
        worst_texts(&shapes, ground)
    }

    #[test]
    fn text_on_a_row_under_the_pointer_or_pressed_keeps_4_5_to_1_in_light_and_dark_on_either_ground() {
        let grounds: [(&str, Ground); 2] = [("paper", |p| p.paper), ("side pane", |p| p.hover)];
        for (name, draw) in rows() {
            for dark in [false, true] {
                for (ground_name, ground) in grounds {
                    for down in [false, true] {
                        let texts = contrast_of(draw, dark, ground, down);
                        assert!(!texts.is_empty(), "{name} painted no text");
                        for (text, ratio) in texts {
                            assert!(
                                ratio >= 4.5,
                                "{name} on {ground_name} ({}, {}): {text:?} is {ratio:.2}:1",
                                if dark { "dark" } else { "light" },
                                if down { "pressed" } else { "hovered" }
                            );
                        }
                    }
                }
            }
        }
    }
}
