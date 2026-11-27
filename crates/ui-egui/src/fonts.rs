//! The interface and code fonts, chosen from the fonts installed on this Mac (AGENTS.md rule 25: PR Craft ships no font files).
//! A font that is chosen but not installed keeps its place in Settings, draws in the stock face, and switches on by itself once
//! it is installed and the list is read again. Font files are read on a worker thread, never in a frame.
//!
//! egui's stock fonts have no key symbols (⌘ ⌥ ⇧ ⌃ ↵ ⌫), so whatever the reader chose, faces that have them are added behind
//! every family from the Mac's own fonts ([`SYMBOL_FAMILIES`]). Without them every key cap draws as a box.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use craft::egui::{Context, FontData, FontDefinitions, FontFamily, Id};
use craft::font_catalog::Catalog;
use pr_craft_core::settings::Settings;

/// Names the faces in egui's font definitions.
const UI_FACE: &str = "pr-craft-ui";
const CODE_FACE: &str = "pr-craft-code";

/// Installed macOS families that draw the key symbols, in the order they are tried after the chosen fonts and egui's own. Each
/// one that is installed is added, so a Mac missing one still has another.
const SYMBOL_FAMILIES: [&str; 4] = ["Apple Symbols", "Menlo", "Lucida Grande", "Helvetica Neue"];

/// Whether the two chosen fonts are installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub ui: bool,
    pub code: bool,
}

/// The installed families the two fonts and the symbol fallbacks resolved to; what egui was last given, or is being given.
#[derive(Clone, PartialEq, Eq, Default)]
struct Wanted {
    ui: Option<String>,
    code: Option<String>,
    symbols: Vec<String>,
}

impl Wanted {
    fn of(catalog: &Catalog, settings: &Settings) -> Self {
        Wanted {
            ui: catalog.family(&settings.ui_font),
            code: catalog.family(&settings.code_font),
            symbols: SYMBOL_FAMILIES.iter().filter_map(|name| catalog.family(name)).collect(),
        }
    }
}

/// A face read from disk: its name in egui's definitions, the families it goes into and where in each, the file's bytes and the
/// face's index in it.
struct Face {
    name: String,
    families: Vec<FontFamily>,
    /// First in its families (a chosen font), else last (a fallback for what the others lack).
    first: bool,
    bytes: Vec<u8>,
    index: u32,
}

/// The worker's answer for `wanted`, picked up by a later frame.
#[derive(Clone)]
struct Loading {
    wanted: Wanted,
    done: Arc<Mutex<Option<Vec<Face>>>>,
}

fn catalog() -> Option<&'static Arc<Catalog>> {
    let catalog = Catalog::global();
    if !catalog.is_ready() {
        catalog.scan_in_background();
        return None;
    }
    Some(catalog)
}

/// The installed font families, sorted, private system families included (the picker leaves those out as it lists). A cheap
/// shared handle: it is not copied per frame. Empty until the scan has run.
pub fn installed() -> Arc<[String]> {
    catalog().map(|c| c.families()).unwrap_or_else(|| Arc::from(Vec::new()))
}

pub fn is_listed(family: &str) -> bool {
    !craft::font_catalog::is_private_family(family)
}

/// Whether each chosen font is installed; `None` until the font folders have been read.
pub fn found(settings: &Settings) -> Option<Found> {
    let catalog = catalog()?;
    Some(Found { ui: catalog.family(&settings.ui_font).is_some(), code: catalog.family(&settings.code_font).is_some() })
}

/// The file of `family` in its regular style: bytes and the face's index.
fn read_face(catalog: &Catalog, family: &str) -> Option<(Vec<u8>, u32)> {
    let (info, _) = catalog.resolve(family, "Regular")?;
    let data = catalog.load(&info).ok()?;
    Some((data.bytes.to_vec(), data.index))
}

/// Reads the faces `wanted` names: the chosen fonts, then the symbol fallbacks.
fn read_faces(catalog: &Catalog, wanted: &Wanted) -> Vec<Face> {
    let mut faces = Vec::new();
    for (name, family, face) in [(&wanted.ui, FontFamily::Proportional, UI_FACE), (&wanted.code, FontFamily::Monospace, CODE_FACE)] {
        if let Some((bytes, index)) = name.as_deref().and_then(|n| read_face(catalog, n)) {
            faces.push(Face { name: face.to_string(), families: vec![family], first: true, bytes, index });
        }
    }
    for (i, family) in wanted.symbols.iter().enumerate() {
        if let Some((bytes, index)) = read_face(catalog, family) {
            faces.push(Face {
                name: format!("pr-craft-symbols-{i}"),
                families: vec![FontFamily::Proportional, FontFamily::Monospace],
                first: false,
                bytes,
                index,
            });
        }
    }
    faces
}

/// Puts the chosen fonts first in egui's proportional and monospace families once they are installed and read, and the key
/// symbols behind them whatever was chosen. Does nothing until the font folders are scanned. The files are read on a worker
/// thread; the frame that finds them read installs them.
pub fn apply(ctx: &Context, settings: &Settings) {
    let Some(catalog) = catalog() else { return };
    let wanted = Wanted::of(catalog, settings);
    let applied: Option<Wanted> = ctx.data(|d| d.get_temp(Id::NULL));
    if applied.unwrap_or_default() == wanted {
        return;
    }
    let loading: Option<Loading> = ctx.data(|d| d.get_temp(Id::NULL));
    match loading {
        Some(l) if l.wanted == wanted => {
            let Some(faces) = l.done.lock().ok().and_then(|mut slot| slot.take()) else {
                ctx.request_repaint_after(Duration::from_millis(100));
                return;
            };
            install(ctx, faces);
            ctx.data_mut(|d| {
                d.remove::<Loading>(Id::NULL);
                d.insert_temp(Id::NULL, wanted);
            });
        }
        _ => start(ctx, Arc::clone(catalog), wanted),
    }
}

fn start(ctx: &Context, catalog: Arc<Catalog>, wanted: Wanted) {
    let done = Arc::new(Mutex::new(None));
    ctx.data_mut(|d| d.insert_temp(Id::NULL, Loading { wanted: wanted.clone(), done: Arc::clone(&done) }));
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let faces = read_faces(&catalog, &wanted);
        if let Ok(mut slot) = done.lock() {
            *slot = Some(faces);
        }
        ctx.request_repaint();
    });
}

/// egui's stock definitions with `faces` added.
fn definitions(faces: Vec<Face>) -> FontDefinitions {
    let mut definitions = craft::ui::fonts::deflated_default_definitions();
    for Face { name, families, first, bytes, index } in faces {
        let mut font = FontData::from_owned(bytes);
        font.index = index;
        definitions.font_data.insert(name.clone(), Arc::new(font));
        for family in families {
            let list = definitions.families.entry(family).or_default();
            if first {
                list.insert(0, name.clone());
            } else {
                list.push(name.clone());
            }
        }
    }
    definitions
}

fn install(ctx: &Context, faces: Vec<Face>) {
    craft::ui::fonts::load(ctx, definitions(faces));
}

/// The fonts the app installs for `settings`, installed at once: waits for the font scan and reads the files on this thread.
/// For the render tests, so they draw with exactly the faces the app does.
#[cfg(test)]
pub(crate) fn install_blocking(ctx: &Context, settings: &Settings) {
    let catalog = Catalog::global();
    catalog.scan_in_background();
    let started = std::time::Instant::now();
    while !catalog.is_ready() && started.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(20));
    }
    install(ctx, read_faces(catalog, &Wanted::of(catalog, settings)));
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use craft::egui::{FontId, RawInput};

    use super::*;

    /// A key cap that draws as a box is a shortcut nobody can read: the fonts the app installs must draw every character a
    /// shortcut label can hold, in the interface family and the code family, with the default settings (Geist not installed).
    #[test]
    fn the_installed_fonts_draw_every_glyph_a_shortcut_label_can_hold() {
        let ctx = Context::default();
        install_blocking(&ctx, &Settings::default());
        let missing: Vec<(char, &str)> = {
            let mut missing = Vec::new();
            // Fonts exist only inside a pass.
            ctx.run_ui(RawInput::default(), |ui| {
                ui.ctx().fonts_mut(|fonts| {
                    for (family, name) in [(FontFamily::Proportional, "interface"), (FontFamily::Monospace, "code")] {
                        for c in pr_craft_core::keys::label_glyphs() {
                            if !fonts.has_glyph(&FontId::new(13.0, family.clone()), c) {
                                missing.push((c, name));
                            }
                        }
                    }
                });
            })
            .drop_without_applying_deltas();
            missing
        };
        assert!(missing.is_empty(), "no installed font draws {missing:?}");
    }

    #[test]
    fn the_symbol_fallbacks_are_added_behind_a_chosen_font_and_never_in_front_of_it() {
        let face = |name: &str, first: bool| Face { name: name.into(), families: vec![FontFamily::Proportional], first, bytes: Vec::new(), index: 0 };
        let defs = definitions(vec![face("chosen", true), face("symbols", false)]);
        let list = &defs.families[&FontFamily::Proportional];
        assert_eq!(list.first().map(String::as_str), Some("chosen"));
        assert_eq!(list.last().map(String::as_str), Some("symbols"));
    }
}
