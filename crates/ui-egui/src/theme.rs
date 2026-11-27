//! The tokens of DESIGN.md and the Figma variables `Theme`, light and dark under the same names. Every colour and text size the
//! UI paints comes from the [`Palette`] and [`Sizes`] of the current [`Look`], which sits in the egui context: a screen asks
//! `theme::pal(ui)` and `theme::sz(ui)` each time it paints, so a change of theme applies on the next frame.

use craft::egui::text::LayoutJob;
use craft::egui::{Color32, Context, Ui};
use pr_craft_core::settings::{DiffColours, Settings};

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// A colour at `percent` opacity, the way Figma's `Theme` variables hold the dark tints.
fn tint(rgb: u32, percent: u8) -> Color32 {
    Color32::from_rgba_unmultiplied((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, (percent as u16 * 255 / 100) as u8)
}

/// Colours by token. Light and dark have the same fields; a screen never asks which one it is in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// `bg/canvas`: the window.
    pub paper: Color32,
    pub panel: Color32,
    /// `bg/raised`: cards, inputs and menus.
    pub raised: Color32,
    pub sunken: Color32,
    pub hover: Color32,
    /// Stone/150: a control under the pointer button, one step past `hover`. In dark, the nearest surface step.
    pub pressed: Color32,
    /// `bg/inverse`: the fill of the selected row.
    pub inverse: Color32,
    /// A graphite pill under the pointer: Stone/800.
    pub inverse_hover: Color32,
    /// A graphite pill pressed: Stone/900.
    pub inverse_pressed: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    /// The edge of a control (a button, a field): 3:1 against the surface it sits on.
    pub control: Color32,
    /// `text/primary`. Named for the ink of the light theme; in dark it is the light ink.
    pub graphite: Color32,
    pub text_secondary: Color32,
    pub text_tertiary: Color32,
    /// `text/on-inverse`: text on the selected row.
    pub on_inverse: Color32,
    /// The weaker text on the selected row, a menu's key and the palette's subtitle: `on_inverse` at 75%.
    pub on_inverse_quiet: Color32,
    pub ember: Color32,
    pub ember_text: Color32,
    /// Selected diff lines.
    pub ember_subtle: Color32,
    pub haze: Color32,
    pub haze_subtle: Color32,
    pub haze_text: Color32,
    pub focus_ring: Color32,
    /// The ring drawn inside a selected row filled with `inverse`, where `focus_ring` would vanish in dark.
    pub focus_ring_on_inverse: Color32,
    /// The fill behind selected text.
    pub selection: Color32,
    pub moss: Color32,
    /// `diff/add-bg`. Blue when the reader chose the colour-blind pair.
    pub moss_bg: Color32,
    /// `diff/add-gutter`, laid over `moss_bg`.
    pub moss_gutter: Color32,
    /// The changed words of an added line, laid over `moss_bg`.
    pub moss_mark: Color32,
    pub moss_text: Color32,
    pub rose: Color32,
    /// `diff/del-bg`. Orange when the reader chose the colour-blind pair.
    pub rose_bg: Color32,
    /// `diff/del-gutter`, laid over `rose_bg`.
    pub rose_gutter: Color32,
    /// The changed words of a removed line, laid over `rose_bg`.
    pub rose_mark: Color32,
    pub rose_text: Color32,
}

impl Palette {
    pub fn light() -> Self {
        Palette {
            paper: hex(0xF3F1EC),
            panel: hex(0xFBFAF8),
            raised: hex(0xFFFFFF),
            sunken: hex(0xF6F4F0),
            hover: hex(0xECE9E3),
            pressed: hex(0xE4E1DA),
            inverse: hex(0x1D1C21),
            inverse_hover: hex(0x36353A),
            inverse_pressed: hex(0x26252A),
            border: hex(0xE4E1DA),
            border_strong: hex(0xD8D4CC),
            control: hex(0x7E7A74),
            graphite: hex(0x1D1C21),
            text_secondary: hex(0x4D4B48),
            text_tertiary: hex(0x64615C),
            on_inverse: hex(0xFFFFFF),
            on_inverse_quiet: Color32::WHITE.gamma_multiply(0.75),
            ember: hex(0xF0603A),
            ember_text: hex(0xA03000),
            ember_subtle: hex(0xFFE7DF),
            haze: hex(0xC2BCEF),
            haze_subtle: hex(0xECEAF9),
            haze_text: hex(0x453E6F),
            focus_ring: hex(0x726AAC),
            focus_ring_on_inverse: hex(0xC2BCEF),
            selection: hex(0xDAD8F9),
            moss: hex(0x31A667),
            moss_bg: hex(0xEEFAF2),
            moss_gutter: hex(0x31A667).gamma_multiply(0.14),
            moss_mark: hex(0x31A667).gamma_multiply(0.3),
            moss_text: hex(0x006F3D),
            rose: hex(0xE94E6E),
            rose_bg: hex(0xFFF4F4),
            rose_gutter: hex(0xE94E6E).gamma_multiply(0.14),
            rose_mark: hex(0xE94E6E).gamma_multiply(0.3),
            rose_text: hex(0xA81741),
        }
    }

    /// The `dark/…` values of the Figma `Theme` collection; `night-*` of DESIGN.md are its surfaces.
    pub fn dark() -> Self {
        Palette {
            paper: hex(0x111114),
            panel: hex(0x1B1B20),
            raised: hex(0x202026),
            sunken: hex(0x16161A),
            hover: hex(0x26262D),
            pressed: hex(0x2E2E36),
            inverse: hex(0xE9E7EE),
            inverse_hover: hex(0xF6F5FA),
            inverse_pressed: hex(0xDAD8E1),
            border: hex(0x2E2E36),
            border_strong: hex(0x3B3B44),
            control: hex(0x8F8D99),
            graphite: hex(0xE9E7EE),
            text_secondary: hex(0xB3B1BB),
            text_tertiary: hex(0xA09EAA),
            on_inverse: hex(0x111114),
            on_inverse_quiet: hex(0x4F4F59),
            ember: hex(0xFF7B52),
            ember_text: hex(0xFFAD93),
            ember_subtle: tint(0xF0603A, 18),
            haze: hex(0xA49FDE),
            haze_subtle: tint(0xA49FDE, 20),
            haze_text: hex(0xDAD8F9),
            focus_ring: hex(0xC2BCEF),
            focus_ring_on_inverse: hex(0x453E6F),
            selection: hex(0x5A538D),
            moss: hex(0x5EBF84),
            moss_bg: tint(0x31A667, 12),
            moss_gutter: tint(0x31A667, 11),
            moss_mark: hex(0x5EBF84).gamma_multiply(0.3),
            moss_text: hex(0x90D7A8),
            rose: hex(0xFF748B),
            rose_bg: tint(0xE94E6E, 12),
            rose_gutter: tint(0xE94E6E, 11),
            rose_mark: hex(0xFF748B).gamma_multiply(0.3),
            rose_text: hex(0xFFA8B2),
        }
    }

    /// Added and removed lines in blue and orange (Azure and Ochre of the Figma primitives).
    fn blue_and_orange(mut self, dark: bool) -> Self {
        (self.moss_bg, self.rose_bg) = if dark { (tint(0x458CF3, 12), tint(0xC37C00, 12)) } else { (hex(0xF2F7FF), hex(0xFEF5EB)) };
        let gutter = if dark { 11 } else { 12 };
        (self.moss_gutter, self.rose_gutter) = (tint(0x458CF3, gutter), tint(0xC37C00, gutter));
        (self.moss_mark, self.rose_mark) = (tint(0x458CF3, 30), tint(0xC37C00, 30));
        self
    }

    /// Increase contrast (Figma frame 100): tertiary text darkens to secondary and hairlines take the control edge.
    fn high_contrast(mut self) -> Self {
        self.text_tertiary = self.text_secondary;
        self.border = self.control;
        self.border_strong = self.control;
        self
    }

    /// Every colour, in field order: [`restyle`] swaps one palette's colours for another's by position.
    fn fields(&self) -> [Color32; 36] {
        [
            self.paper,
            self.panel,
            self.raised,
            self.sunken,
            self.hover,
            self.pressed,
            self.inverse,
            self.inverse_hover,
            self.inverse_pressed,
            self.border,
            self.border_strong,
            self.control,
            self.graphite,
            self.text_secondary,
            self.text_tertiary,
            self.on_inverse,
            self.ember,
            self.ember_text,
            self.ember_subtle,
            self.haze,
            self.haze_subtle,
            self.haze_text,
            self.focus_ring,
            self.selection,
            self.moss,
            self.moss_bg,
            self.moss_text,
            self.rose,
            self.rose_bg,
            self.rose_text,
            self.on_inverse_quiet,
            self.moss_gutter,
            self.rose_gutter,
            self.focus_ring_on_inverse,
            self.moss_mark,
            self.rose_mark,
        ]
    }
}

/// Text sizes and row metrics, from the interface size, the code size and line height, and density.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sizes {
    pub body: f32,
    pub title: f32,
    /// Rendered Markdown headings of level 1 and 2.
    pub heading: f32,
    pub code: f32,
    /// Px of one diff row. `None` is the row the app always drew (as tall as its text): the stored default of 20 means that.
    pub line: Option<f32>,
    /// Px added to the vertical padding of a list row; 0 is the row the app always drew.
    pub row: f32,
    /// Width of a hairline: 1, or 2 with Increase contrast.
    pub hairline: f32,
}

/// What macOS reports that PR Craft may follow (Settings → Accessibility).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct System {
    pub dark: Option<bool>,
    pub high_contrast: Option<bool>,
    pub reduced_motion: Option<bool>,
    /// The desktop text scale, `None` when macOS gives none.
    pub text_scale: Option<f32>,
}

/// Everything the current theme gives a screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub dark: bool,
    pub palette: Palette,
    pub sizes: Sizes,
    pub reduce_motion: bool,
    pub focus_rings: bool,
}

impl Look {
    /// The look `settings` ask for. `system` is what macOS reports; it is read only when the reader follows macOS, and the
    /// theme `System` reads its mode whatever that choice is.
    pub fn new(settings: &Settings, system: System) -> Self {
        use pr_craft_core::settings::Theme;
        let dark = match settings.theme {
            Theme::Light => false,
            Theme::Dark => true,
            Theme::System => system.dark.unwrap_or(false),
        };
        let follow = settings.follow_macos_accessibility;
        let high = settings.increase_contrast || (follow && system.high_contrast == Some(true));
        let scale = if follow { system.text_scale.unwrap_or(1.0).clamp(0.8, 2.0) } else { 1.0 };
        let mut palette = if dark { Palette::dark() } else { Palette::light() };
        if settings.diff_colours == DiffColours::BlueAndOrange {
            palette = palette.blue_and_orange(dark);
        }
        if high {
            palette = palette.high_contrast();
        }
        let k = settings.ui_size / 13.5 * scale;
        let sizes = Sizes {
            body: settings.ui_size * scale,
            title: 18.0 * k,
            heading: 22.0 * k,
            code: settings.code_size * scale,
            line: (settings.code_line_height != Settings::default().code_line_height).then_some(settings.code_line_height * scale),
            row: settings.density.row_padding(),
            hairline: if high { 2.0 } else { 1.0 },
        };
        Look {
            dark,
            palette,
            sizes,
            reduce_motion: settings.reduce_motion || (follow && system.reduced_motion == Some(true)),
            focus_rings: settings.focus_rings,
        }
    }
}

impl Default for Look {
    fn default() -> Self {
        Look::new(&Settings::default(), System::default())
    }
}

/// Anything that reaches the egui context: the `Ui` a screen paints in, or the context a window is shown from.
pub trait HasContext {
    fn context(&self) -> &Context;
}

impl HasContext for Context {
    fn context(&self) -> &Context {
        self
    }
}

impl HasContext for Ui {
    fn context(&self) -> &Context {
        self.ctx()
    }
}

fn slot() -> craft::egui::Id {
    craft::egui::Id::NULL
}

/// Makes `look` the one every screen paints with from now on.
pub fn install(ctx: &Context, look: Look) {
    ctx.data_mut(|d| d.insert_temp(slot(), look));
}

/// The installed look; the light defaults before the first frame installs one.
pub fn look(c: &impl HasContext) -> Look {
    c.context().data(|d| d.get_temp(slot())).unwrap_or_default()
}

pub fn pal(c: &impl HasContext) -> Palette {
    look(c).palette
}

pub fn sz(c: &impl HasContext) -> Sizes {
    look(c).sizes
}

/// Whether the reader last acted with the keyboard. A focus ring is drawn only then, or when Accessibility keeps rings on: a
/// press with the pointer moves focus too, but the control under the pointer already shows where it is.
#[derive(Clone, Copy)]
struct Modality {
    keyboard: bool,
}

/// Reads this frame's input events: a key press makes focus keyboard focus, a pointer press makes it pointer focus. Call once a
/// frame, before anything is drawn.
pub fn note_input(ctx: &Context) {
    use craft::egui::Event;
    let keyboard = ctx.input(|i| {
        i.events.iter().rev().find_map(|e| match e {
            // A bare modifier (⇧ held for a range select after a click) is not keyboard navigation.
            Event::Key { pressed: true, key, .. } if !is_modifier(*key) => Some(true),
            Event::PointerButton { pressed: true, .. } => Some(false),
            _ => None,
        })
    });
    if let Some(keyboard) = keyboard {
        let changed = ctx.data(|d| d.get_temp::<Modality>(slot())).is_none_or(|m| m.keyboard != keyboard);
        ctx.data_mut(|d| d.insert_temp(slot(), Modality { keyboard }));
        if changed {
            let stroke = active_stroke(&look(ctx), keyboard);
            ctx.all_styles_mut(|style| style.visuals.widgets.active.bg_stroke = stroke);
        }
    }
}

fn is_modifier(key: craft::egui::Key) -> bool {
    use craft::egui::Key;
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
            | Key::CapsLock
    )
}

/// Whether a control with focus draws its ring now.
pub fn ring_visible(c: &impl HasContext) -> bool {
    look(c).focus_rings || c.context().data(|d| d.get_temp::<Modality>(slot())).is_some_and(|m| m.keyboard)
}

/// The look layout jobs are laid out with when they are cached, so one cache serves every theme: [`restyle`] turns them into
/// the current look when they are painted.
pub fn baked() -> Look {
    Look::default()
}

/// `job`, laid out with the [`baked`] look, in the colours and sizes of the current one.
pub fn restyle(c: &impl HasContext, job: &LayoutJob) -> LayoutJob {
    let (from, to) = (baked(), look(c));
    let mut out = job.clone();
    let scale = to.sizes.body / from.sizes.body;
    let (was, now) = (from.palette.fields(), to.palette.fields());
    let swap = |colour: Color32| was.iter().position(|w| *w == colour).and_then(|i| now.get(i).copied()).unwrap_or(colour);
    for section in &mut out.sections {
        let format = &mut section.format;
        format.color = swap(format.color);
        format.background = swap(format.background);
        format.underline.color = swap(format.underline.color);
        format.strikethrough.color = swap(format.strikethrough.color);
        format.font_id.size *= scale;
    }
    out
}

/// The edge of egui's own widgets while pressed or focused: the focus ring for keyboard focus, else the control edge.
fn active_stroke(look: &Look, keyboard: bool) -> craft::egui::Stroke {
    let p = look.palette;
    if keyboard || look.focus_rings {
        craft::egui::Stroke::new(if look.focus_rings { 3.0 } else { 2.0 }, p.focus_ring)
    } else {
        craft::egui::Stroke::new(1.0, p.control)
    }
}

/// Gives egui's own widgets, windows and text fields the colours of `look`. Both of egui's styles take them, so a change of the
/// operating system's mode never brings the other palette back.
pub fn apply_style(ctx: &Context, look: &Look) {
    let (p, hairline) = (look.palette, look.sizes.hairline);
    let animation = if look.reduce_motion { 0.0 } else { 1.0 / 12.0 };
    ctx.all_styles_mut(|style| {
        style.animation_time = animation;
        let v = &mut style.visuals;
        v.panel_fill = p.paper;
        v.window_fill = p.raised;
        v.window_stroke.color = p.border;
        v.extreme_bg_color = p.raised;
        v.faint_bg_color = p.sunken;
        v.code_bg_color = p.sunken;
        v.hyperlink_color = p.haze_text;
        v.warn_fg_color = p.ember_text;
        v.error_fg_color = p.rose_text;
        v.selection.bg_fill = p.selection;
        v.selection.stroke.color = p.haze_text;
        // Placeholder text is the weak colour; egui's default (the ink at 60%) is 4.47:1 on a raised field in light.
        v.weak_text_color = Some(p.text_tertiary);
        let sizes = look.sizes;
        for (text_style, size) in [
            (craft::egui::TextStyle::Body, sizes.body),
            (craft::egui::TextStyle::Button, sizes.body),
            (craft::egui::TextStyle::Small, sizes.body * 12.5 / 13.5),
            (craft::egui::TextStyle::Heading, sizes.title),
            (craft::egui::TextStyle::Monospace, sizes.code),
        ] {
            if let Some(font) = style.text_styles.get_mut(&text_style) {
                font.size = size;
            }
        }
        let w = &mut v.widgets;
        for (widget, fill, stroke) in [
            (&mut w.noninteractive, p.panel, craft::egui::Stroke::new(hairline, p.border)),
            (&mut w.inactive, p.raised, craft::egui::Stroke::new(1.0, p.control)),
            (&mut w.hovered, p.hover, craft::egui::Stroke::new(1.0, p.control)),
            (&mut w.active, p.pressed, active_stroke(look, false)),
            (&mut w.open, p.hover, craft::egui::Stroke::new(1.0, p.control)),
        ] {
            widget.bg_fill = fill;
            widget.weak_bg_fill = fill;
            widget.bg_stroke = stroke;
            widget.fg_stroke.color = p.graphite;
        }
    });
}
/// Spacing steps 2 and 3.
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;

/// The window chrome (Figma frames 01 and 02): the title bar and the status bar.
pub const TITLE_BAR_HEIGHT: f32 = 39.0;
pub const STATUS_BAR_HEIGHT: f32 = 27.0;

/// Pane widths of the inbox (Figma frame 01).
pub const SIDEBAR_WIDTH: f32 = 248.0;
pub const LIST_WIDTH: f32 = 560.0;
/// The review panel pins its Keys panel at the foot only when it is at least this tall.
pub const KEYS_PINNED_HEIGHT: f32 = 780.0;
/// Below 1280 px the list narrows to this before the reading pane gives up its minimum.
pub const LIST_MIN_WIDTH: f32 = 420.0;
pub const DETAIL_MIN_WIDTH: f32 = 320.0;
/// The strip of actions at the foot of a reading pane.
pub const ACTION_BAR_HEIGHT: f32 = 56.0;
/// Pane widths of the review screen (Figma frame 02).
pub const FILES_WIDTH: f32 = 260.0;
/// Below this window width the review's side panes narrow.
pub const WIDE_WINDOW: f32 = 1280.0;
pub const FILES_NARROW: f32 = 220.0;
pub const PANEL_NARROW: f32 = 260.0;
pub const PANEL_WIDTH: f32 = 300.0;
/// Rows of the summary field.
pub const SUMMARY_ROWS: usize = 2;
/// Height of the stack strip on the review screen before it scrolls; about five layers.
pub const STACK_STRIP_HEIGHT: f32 = 120.0;
/// The tallest an image is drawn on the review screen before it scales down.
pub const IMAGE_MAX_HEIGHT: f32 = 480.0;
/// Images with no side longer than this grow, up to four times, so an icon is visible.
pub const SMALL_IMAGE: f32 = 128.0;
/// One side of the SVG compare (frame 79): the checkerboard box, its squares, the room the drawing may fill in it, and how
/// far a small drawing grows (a 24 px icon shows at 60 px). The drawing is rasterised at twice the size it is shown.
pub const SVG_BOX_W: f32 = 240.0;
pub const SVG_BOX_H: f32 = 160.0;
pub const SVG_CHECKER: f32 = 8.0;
pub const SVG_ROOM_W: f32 = 200.0;
pub const SVG_ROOM_H: f32 = 120.0;
pub const SVG_GROW: f32 = 2.5;
/// Lines of one rendered block or cell that are laid out; the rest shows as a count and the source view has them all.
pub const MAX_BLOCK_LINES: usize = 60;
/// How far below and above the window a file's header may be before its rendered view starts loading.
pub const PREFETCH: f32 = 600.0;
/// Characters of one line, and of one block's text, that are laid out. A minified or one-line file never lays out whole.
pub const MAX_LINE_CHARS: usize = 2000;
pub const MAX_BLOCK_CHARS: usize = 20_000;
/// Settings: the page list, and the list of agents or repositories beside an editor.
pub const SETTINGS_NAV_WIDTH: f32 = 240.0;
/// The widest a Settings page reads before it stops growing.
pub const SETTINGS_PAGE_WIDTH: f32 = 800.0;
/// The rows of Appearance, and with the preview beside them the page's widest.
pub const APPEARANCE_ROWS_WIDTH: f32 = 640.0;
pub const APPEARANCE_WIDE_WIDTH: f32 = 1120.0;
pub const AGENT_LIST_WIDTH: f32 = 252.0;
/// The widest the agent editor's fields grow.
pub const AGENT_FORM_WIDTH: f32 = 680.0;
/// The AI review sheet and the run details window.
pub const SHEET_WIDTH: f32 = 680.0;
/// The Activity screen's run list, and the popover under the task pill.
/// The filters pane, and the detail pane at its widest.
pub const ACTIVITY_FILTERS_WIDTH: f32 = 240.0;
pub const ACTIVITY_DETAIL_WIDTH: f32 = 500.0;
pub const POPOVER_WIDTH: f32 = 380.0;
/// Height of a prompt in the preview before it scrolls.
pub const PROMPT_HEIGHT: f32 = 240.0;
/// Height of the runs list in the review panel before it scrolls.
pub const RUNS_LIST_HEIGHT: f32 = 260.0;
/// The AI review sheet and the confirm step hang this far below the top; the sheet leaves the margin at the bottom and
/// is never shorter than the minimum, and the footer row is its own height.
pub const SHEET_TOP: f32 = 44.0;
pub const SHEET_MARGIN: f32 = 120.0;
pub const SHEET_MIN_HEIGHT: f32 = 240.0;
/// The popover under the task pill hangs this far below the top.
pub const POPOVER_TOP: f32 = 40.0;
/// Text fields and pickers in Settings.
pub const PICKER_WIDTH: f32 = 320.0;
pub const PATTERN_FIELD_WIDTH: f32 = 240.0;
pub const SKILL_FIELD_WIDTH: f32 = 280.0;
/// One line of a prompt in the preview.
pub const PROMPT_ROW_HEIGHT: f32 = 16.5;
/// The Share sheet: its controls, and the box the preview is drawn in (Figma frames 19, 43, 63).
pub const SHARE_CONTROLS_WIDTH: f32 = 300.0;
pub const SHARE_PREVIEW_WIDTH: f32 = 520.0;
pub const SHARE_PREVIEW_HEIGHT: f32 = 400.0;

#[cfg(test)]
mod tests {
    use pr_craft_core::diff::LineKind;

    use super::*;

    /// `c` laid over the opaque colour `below`, as 0-255 channels. A `Color32` holds its channels premultiplied.
    fn over(c: Color32, below: [f64; 3]) -> [f64; 3] {
        let keep = 1.0 - f64::from(c.a()) / 255.0;
        [f64::from(c.r()) + below[0] * keep, f64::from(c.g()) + below[1] * keep, f64::from(c.b()) + below[2] * keep]
    }

    fn channels(c: Color32) -> [f64; 3] {
        over(c, [0.0; 3])
    }

    fn luminance(rgb: [f64; 3]) -> f64 {
        let lin = |v: f64| {
            let v = v / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(rgb[0]) + 0.7152 * lin(rgb[1]) + 0.0722 * lin(rgb[2])
    }

    /// WCAG contrast of `ink` on `fill`, where either may be translucent and `under` is the opaque surface below them.
    fn contrast(ink: Color32, fill: Color32, under: Color32) -> f64 {
        let fill = over(fill, channels(under));
        let (a, b) = (luminance(over(ink, fill)), luminance(fill));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn palettes() -> [(&'static str, Palette); 4] {
        [
            ("light", Palette::light()),
            ("dark", Palette::dark()),
            ("light, blue and orange", Palette::light().blue_and_orange(false)),
            ("dark, blue and orange", Palette::dark().blue_and_orange(true)),
        ]
    }

    /// Every pair body text is painted in: ink on a surface, ink on a tint, text on the selected row and on selected text.
    fn text_pairs(p: Palette) -> Vec<(String, Color32, Color32)> {
        let surfaces = [("paper", p.paper), ("panel", p.panel), ("raised", p.raised), ("sunken", p.sunken), ("hover", p.hover)];
        let inks = [
            ("graphite", p.graphite),
            ("text_secondary", p.text_secondary),
            ("text_tertiary", p.text_tertiary),
            ("ember_text", p.ember_text),
            ("moss_text", p.moss_text),
            ("rose_text", p.rose_text),
            ("haze_text", p.haze_text),
        ];
        let mut pairs = Vec::new();
        for (ink_name, ink) in inks {
            for (surface, bg) in surfaces {
                pairs.push((format!("{ink_name} on {surface}"), ink, bg));
            }
        }
        for (tint_name, fill) in [("ember_subtle", p.ember_subtle), ("moss_bg", p.moss_bg), ("rose_bg", p.rose_bg), ("haze_subtle", p.haze_subtle)] {
            pairs.push((format!("graphite on {tint_name}"), p.graphite, fill));
            pairs.push((format!("text_secondary on {tint_name}"), p.text_secondary, fill));
        }
        pairs.push(("ember_text on ember_subtle".into(), p.ember_text, p.ember_subtle));
        pairs.push(("haze_text on haze_subtle".into(), p.haze_text, p.haze_subtle));
        pairs.push(("on_inverse on inverse".into(), p.on_inverse, p.inverse));
        // The key and the subtitle on the selected row of a menu or the palette, and on a graphite pill under the pointer.
        for (fill_name, fill) in [("inverse", p.inverse), ("inverse_hover", p.inverse_hover), ("inverse_pressed", p.inverse_pressed)] {
            pairs.push((format!("on_inverse_quiet on {fill_name}"), p.on_inverse_quiet, fill));
        }
        pairs.push(("haze_text on selection".into(), p.haze_text, p.selection));
        // The numbers and sign of a diff line sit on the gutter tint, which sits on the row tint, on the diff pane.
        for (name, kind, selected) in [
            ("context", LineKind::Context, false),
            ("added", LineKind::Added, false),
            ("removed", LineKind::Removed, false),
            ("selected", LineKind::Context, true),
        ] {
            let (row, gutter, ink) = crate::components::diff_colours(p, kind, selected);
            let [r, g, b] = over(gutter, over(row, channels(p.panel))).map(|v| v.round().clamp(0.0, 255.0) as u8);
            pairs.push((format!("diff gutter numbers, {name}"), ink, Color32::from_rgb(r, g, b)));
            let [r, g, b] = over(row, channels(p.panel)).map(|v| v.round().clamp(0.0, 255.0) as u8);
            pairs.push((format!("diff sign, {name}"), ink, Color32::from_rgb(r, g, b)));
        }
        // A context line under the pointer takes the hover tint at 60%; its numbers sit on that.
        let [r, g, b] = over(p.hover.gamma_multiply(0.6), channels(p.panel)).map(|v| v.round().clamp(0.0, 255.0) as u8);
        pairs.push(("diff numbers, context line under the pointer".into(), p.text_tertiary, Color32::from_rgb(r, g, b)));
        // The changed words are a stronger tint of the line's own colour, with the code in ink on it.
        for (name, row, mark) in [("added", p.moss_bg, p.moss_mark), ("removed", p.rose_bg, p.rose_mark)] {
            let [r, g, b] = over(mark, over(row, channels(p.panel))).map(|v| v.round().clamp(0.0, 255.0) as u8);
            pairs.push((format!("graphite on the changed words of a {name} line"), p.graphite, Color32::from_rgb(r, g, b)));
        }
        pairs
    }

    /// The weaker of the two surfaces a tint is laid on: the canvas and a raised card.
    fn worst(ink: Color32, fill: Color32, p: Palette) -> f64 {
        [p.paper, p.raised].into_iter().map(|under| contrast(ink, fill, under)).fold(f64::INFINITY, f64::min)
    }

    #[test]
    fn body_text_passes_4_5_to_1_in_every_palette() {
        for (name, p) in palettes() {
            for (pair, ink, bg) in text_pairs(p) {
                let ratio = worst(ink, bg, p);
                assert!(ratio >= 4.5, "{name}: {pair} is {ratio:.2}:1");
            }
        }
    }

    #[test]
    fn increase_contrast_keeps_every_pair_and_never_lowers_tertiary_text() {
        for (name, base) in [("light", Palette::light()), ("dark", Palette::dark())] {
            let high = base.high_contrast();
            assert!(contrast(high.text_tertiary, high.paper, high.paper) >= contrast(base.text_tertiary, base.paper, base.paper), "{name}");
            for (pair, ink, bg) in text_pairs(high) {
                let ratio = worst(ink, bg, high);
                assert!(ratio >= 4.5, "{name} with Increase contrast: {pair} is {ratio:.2}:1");
            }
        }
    }

    #[test]
    fn a_control_edge_is_3_to_1_against_the_surfaces_it_sits_on() {
        for (name, p) in palettes() {
            for (surface, bg) in [("paper", p.paper), ("panel", p.panel), ("raised", p.raised), ("sunken", p.sunken), ("hover", p.hover)] {
                let ratio = contrast(p.control, bg, p.paper);
                assert!(ratio >= 3.0, "{name}: control on {surface} is {ratio:.2}:1");
            }
        }
    }

    /// The key and the subtitle on a selected row read as the quieter of the two texts on it.
    #[test]
    fn the_quiet_text_on_a_selected_row_is_weaker_than_its_main_text() {
        for (name, p) in palettes() {
            for (fill_name, fill) in [("inverse", p.inverse), ("inverse_hover", p.inverse_hover), ("inverse_pressed", p.inverse_pressed)] {
                let (main, quiet) = (contrast(p.on_inverse, fill, fill), contrast(p.on_inverse_quiet, fill, fill));
                assert!(quiet < main, "{name}: on_inverse_quiet ({quiet:.2}:1) is not weaker than on_inverse ({main:.2}:1) on {fill_name}");
            }
        }
    }

    #[test]
    fn the_focus_ring_is_3_to_1_against_the_surfaces_it_is_drawn_on() {
        for (name, p) in palettes() {
            let on_surface =
                [("paper", p.paper), ("panel", p.panel), ("raised", p.raised), ("sunken", p.sunken), ("hover", p.hover), ("pressed", p.pressed)];
            let on_inverse = [("inverse", p.inverse), ("inverse_hover", p.inverse_hover), ("inverse_pressed", p.inverse_pressed)];
            for (ring, surfaces) in [(p.focus_ring, on_surface.as_slice()), (p.focus_ring_on_inverse, on_inverse.as_slice())] {
                for (surface, bg) in surfaces {
                    let ratio = contrast(ring, *bg, p.paper);
                    assert!(ratio >= 3.0, "{name}: focus ring on {surface} is {ratio:.2}:1");
                }
            }
        }
    }

    #[test]
    fn dark_has_dark_surfaces_and_light_ink_where_light_has_the_reverse() {
        let (light, dark) = (Palette::light(), Palette::dark());
        let lum = |c: Color32| luminance(channels(c));
        assert!(lum(dark.paper) < lum(light.paper) && lum(dark.raised) < lum(light.raised));
        assert!(lum(dark.graphite) > lum(light.graphite) && lum(dark.text_secondary) > lum(light.text_secondary));
        assert!(lum(dark.graphite) > lum(dark.paper) && lum(light.graphite) < lum(light.paper));
    }

    #[test]
    fn the_look_follows_macos_only_where_the_reader_asked_to() {
        use pr_craft_core::settings::Theme;
        let system = System { dark: Some(true), high_contrast: Some(true), reduced_motion: Some(true), text_scale: Some(1.25) };
        let own = Settings { theme: Theme::Light, follow_macos_accessibility: false, ..Settings::default() };
        let look = Look::new(&own, system);
        assert!(!look.dark && !look.reduce_motion && look.sizes.hairline == 1.0 && look.sizes.body == own.ui_size);
        let follow = Settings { theme: Theme::System, ..Settings::default() };
        let look = Look::new(&follow, system);
        assert!(look.dark && look.reduce_motion && look.sizes.hairline > 1.0 && look.sizes.body > follow.ui_size);
        let theme_only = Settings { theme: Theme::System, follow_macos_accessibility: false, ..Settings::default() };
        assert!(Look::new(&theme_only, system).dark, "Match macOS is the theme choice, not the accessibility one");
    }

    /// Hover is one step up the ramp from the ground and pressed is two, in light and in dark; a graphite pill steps the same
    /// way; and the words stay readable on each.
    #[test]
    fn hover_and_pressed_step_away_from_the_ground_and_keep_their_text_readable() {
        let lum = |c: Color32| luminance(channels(c));
        for (name, p) in palettes() {
            let away = |c: Color32| (lum(c) - lum(p.paper)).abs();
            assert!(away(p.hover) > 0.0 && away(p.pressed) > away(p.hover), "{name}: pressed is a step past hover");
            let inverse_gap = |c: Color32| (lum(c) - lum(p.inverse)).abs();
            assert!(inverse_gap(p.inverse_hover) > 0.0, "{name}: a graphite pill changes on hover");
            for (pair, ink, fill) in [
                ("graphite on hover", p.graphite, p.hover),
                ("graphite on pressed", p.graphite, p.pressed),
                ("secondary on pressed", p.text_secondary, p.pressed),
                ("secondary on hover", p.text_secondary, p.hover),
                ("secondary on the second step of a side pane", p.text_secondary, p.border_strong),
                ("on_inverse on inverse_hover", p.on_inverse, p.inverse_hover),
                ("on_inverse on inverse_pressed", p.on_inverse, p.inverse_pressed),
            ] {
                let ratio = contrast(ink, fill, fill);
                assert!(ratio >= 4.5, "{name}: {pair} is {ratio:.2}:1");
            }
        }
    }
}
