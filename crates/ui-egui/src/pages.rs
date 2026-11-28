//! Settings pages Appearance (Figma frames 17, 48), Accessibility (100), Keyboard (59), Offline and sync (60) and Permissions
//! (18, 47). Every control calls an engine command through `prefs`; the window keeps no copy of a setting (rule 14).

use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use craft::egui::{self, RichText, ScrollArea, TextEdit, Ui};
use pr_craft_core::keys::{self, Binding, Severity};
use pr_craft_core::settings::{CodeColours, Density, DiffColours, IfChanged, Settings, Theme};
use pr_craft_engine::keys::KeyRow;
use pr_craft_engine::quota::HostQuota;
use serde_json::{Value, json};

use crate::components::{self, BannerKind, Pill, PillKind};
use crate::settings::{note, section, title};
use crate::theme;
use crate::{fonts, keymap, live_label, prefs, selectable};

/// State the pages keep between frames: what was typed in a filter and which shortcut waits for a key. Settings are not here.
pub struct Pages {
    notice: Option<Result<String, String>>,
    filter: String,
    conflicts_only: bool,
    /// The action whose new shortcut the Keyboard page is waiting for.
    capture: Option<&'static str>,
    font_filter: String,
    battery: prefs::Battery,
    /// An `open` on its way, and what to say when it worked.
    opening: Option<(Receiver<Result<(), String>>, String)>,
    /// A `diagnostics.get` on its way; its text goes to the clipboard when it arrives.
    diagnostics: Option<Receiver<Result<String, String>>>,
    updates: Updates,
}

impl Pages {
    pub fn new() -> Self {
        Pages {
            notice: None,
            filter: String::new(),
            conflicts_only: false,
            capture: None,
            font_filter: String::new(),
            battery: prefs::Battery::new(),
            opening: None,
            diagnostics: None,
            updates: Updates::default(),
        }
    }

    /// The Keyboard page waits for a key: nothing else in Settings may act on it.
    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }

    /// Leaving the page or Settings ends a capture.
    pub fn stop_capture(&mut self, ctx: &egui::Context) {
        self.capture = None;
        keymap::set_capturing(ctx, false);
    }

    fn notice_line(&mut self, ui: &mut Ui) {
        if let Some((rx, done)) = &self.opening {
            match rx.try_recv() {
                Ok(said) => {
                    self.notice = Some(said.map(|()| done.clone()));
                    self.opening = None;
                }
                Err(mpsc::TryRecvError::Empty) => ui.ctx().request_repaint_after(Duration::from_millis(100)),
                Err(mpsc::TryRecvError::Disconnected) => self.opening = None,
            }
        }
        if let Some(rx) = &self.diagnostics {
            match rx.try_recv() {
                Ok(said) => {
                    self.notice = Some(said.map(|text| {
                        ui.ctx().copy_text(text);
                        "Diagnostics copied".to_string()
                    }));
                    self.diagnostics = None;
                }
                Err(mpsc::TryRecvError::Empty) => ui.ctx().request_repaint_after(Duration::from_millis(100)),
                Err(mpsc::TryRecvError::Disconnected) => self.diagnostics = None,
            }
        }
        match &self.notice {
            Some(Ok(text)) => live_label(ui, text.as_str(), theme::pal(ui).moss_text),
            Some(Err(error)) => live_label(ui, error.as_str(), theme::pal(ui).rose_text),
            None => {}
        }
    }

    fn open_later(&mut self, target: &[&str], done: String) {
        self.opening = Some((open(target.iter().map(|s| (*s).to_string()).collect()), done));
    }

    /// `settings.set` with `patch`; the notice says what the engine answered.
    fn set(&mut self, ui: &Ui, patch: Value) {
        self.notice = Some(prefs::set(ui.ctx(), &patch).map(|()| "Saved on this Mac.".to_string()));
    }
}

/// A row of labelled values as a segmented control; returns the one clicked when it is not the current one.
fn options<T: Copy + PartialEq>(ui: &mut Ui, label: &str, group: &str, all: &[(T, &str)], current: T) -> Option<T> {
    let shown: Vec<(T, String)> = all.iter().map(|(value, text)| (*value, (*text).to_string())).collect();
    ui.horizontal(|ui| {
        if !label.is_empty() {
            components::label(ui, label, 13.0, theme::pal(ui).graphite);
        }
        // The group is the title of the section the choice sits under, so each option is named "5 min, Check for changes every".
        components::choose(ui, group, &shown, current)
    })
    .inner
    .filter(|value| *value != current)
}

/// `options` as labelled choices, with `current` among them when it is none of them, in order.
fn with_current(options: &[f32], current: f32) -> Vec<(f32, String)> {
    let mut all = options.to_vec();
    if !all.iter().any(|v| (*v - current).abs() < 0.001) {
        all.push(current);
        all.sort_by(f32::total_cmp);
    }
    all.into_iter().map(|v| (v, v.to_string())).collect()
}

/// A setting that is on or off, as a form row with a switch. Returns the new value when it was pressed.
fn switch(ui: &mut Ui, label: &str, detail: &str, on: bool) -> Option<bool> {
    components::form_row(ui, label, detail, |ui| components::toggle(ui, on, label))
}

/// Opens a System Settings pane, or an app, with `open`.
/// Runs `open` with `target` on a worker thread: it can wait on Launch Services, and a frame must not.
fn open(target: Vec<String>) -> Receiver<Result<(), String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let said = match Command::new("open").args(&target).status() {
            Ok(status) if status.success() => Ok(()),
            Ok(_) => Err(format!("macOS could not open {}", target.join(" "))),
            Err(e) => Err(format!("could not run `open`: {e}")),
        };
        // The page may have closed first; nobody is left to tell.
        let _ = tx.send(said);
    });
    rx
}

// ---- Appearance ----

impl Pages {
    pub fn appearance(&mut self, ui: &mut Ui) {
        let prefs = prefs::get(ui.ctx());
        let s = &prefs.settings;
        components::page_title(ui, "Appearance", "Light and dark share every token, so nothing moves when the theme changes.");
        self.notice_line(ui);
        if let Some(error) = &prefs.error {
            live_label(ui, format!("The saved settings could not be read, so the defaults are in use: {error}"), theme::pal(ui).ember_text);
        }
        // From 1280 px the preview takes a column of its own to the right of the rows; below that it follows them.
        if ui.ctx().content_rect().width() >= theme::WIDE_WINDOW {
            let total = ui.available_width();
            let rows = theme::APPEARANCE_ROWS_WIDTH.min(total * 0.6);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.vertical(|ui| {
                    ui.set_width(rows);
                    ui.spacing_mut().item_spacing.x = 8.0;
                    self.appearance_rows(ui, s);
                });
                ui.add_space(40.0);
                ui.vertical(|ui| {
                    ui.set_width((total - rows - 40.0).max(240.0));
                    ui.spacing_mut().item_spacing.x = 8.0;
                    preview(ui);
                });
            });
        } else {
            self.appearance_rows(ui, s);
            ui.add_space(theme::SPACE_2);
            preview(ui);
        }
    }

    fn appearance_rows(&mut self, ui: &mut Ui, s: &Settings) {
        self.theme_row(ui, s);
        section(ui, "Text");
        self.font_row(ui, s, true);
        self.font_row(ui, s, false);
        self.metrics_rows(ui, s);
    }

    fn theme_row(&mut self, ui: &mut Ui, s: &Settings) {
        section(ui, "Theme");
        let all = [(Theme::Light, "Light"), (Theme::Dark, "Dark"), (Theme::System, "Match macOS")];
        let mut picked = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let width = ((ui.available_width() - 24.0) / 3.0).min(220.0);
            for (value, name) in all {
                let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 156.0), egui::Sense::click());
                components::theme_card(ui, rect, &response, value, name, s.theme == value);
                if response.clicked() && s.theme != value {
                    picked = Some(value);
                }
                response.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, s.theme == value, name));
            }
        });
        if let Some(theme) = picked {
            self.set(ui, json!({ "theme": theme }));
        }
        if s.theme == Theme::System {
            let now = match prefs::system(ui.ctx()).dark {
                Some(true) => "macOS reports dark.",
                Some(false) => "macOS reports light.",
                None => "macOS did not report light or dark, so PR Craft shows light.",
            };
            ui.add_space(4.0);
            note(ui, now);
        }
    }

    /// The interface or the code font: a picker over the installed fonts, its sizes, and the notice of frame 48 when the chosen
    /// font is not installed.
    fn font_row(&mut self, ui: &mut Ui, s: &Settings, interface: bool) {
        let (label, hint, name, key, size_key, sizes, size) = if interface {
            (
                "Interface font",
                "Labels, lists and comments.",
                &s.ui_font,
                "ui_font",
                "ui_size",
                &[12.0, 13.0, 13.5, 14.0, 16.0, 18.0, 20.0][..],
                s.ui_size,
            )
        } else {
            (
                "Code font",
                "Diffs, paths and refs. Ligatures follow the font.",
                &s.code_font,
                "code_font",
                "code_size",
                &[11.0, 12.0, 12.5, 13.5, 14.0, 16.0, 20.0][..],
                s.code_size,
            )
        };
        let found = fonts::found(s);
        let shown = with_current(sizes, size);
        let size_group = format!("{label} size");
        let (font, next) = components::form_row_fit(ui, label, hint, 520.0, |ui| {
            let next = components::choose(ui, &size_group, &shown, size).filter(|v| *v != size);
            // The picker gives way to the sizes when the row is narrow.
            let room = (ui.available_width() - 8.0).clamp(96.0, theme::PICKER_WIDTH * 0.55);
            (self.font_picker(ui, key, label, name, room), next)
        });
        if let Some(pick) = font {
            self.set(ui, json!({ key: pick }));
        }
        if let Some(next) = next {
            self.set(ui, json!({ size_key: next }));
        }
        match found {
            None => note(ui, "Reading the installed fonts…"),
            Some(f) if !(if interface { f.ui } else { f.code }) => self.missing_font(ui, name, key, interface),
            Some(_) => {}
        }
    }

    fn font_picker(&mut self, ui: &mut Ui, id: &str, name: &str, current: &str, width: f32) -> Option<String> {
        let mut picked = None;
        let combo = egui::ComboBox::from_id_salt(("font", id)).selected_text(current).width(width).show_ui(ui, |ui| {
            ui.add(TextEdit::singleline(&mut self.font_filter).hint_text("Search installed fonts"));
            let needle = self.font_filter.to_lowercase();
            // Read only while the list is open, and shared, not copied.
            let installed = fonts::installed();
            ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                for family in installed.iter().filter(|f| fonts::is_listed(f) && f.to_lowercase().contains(&needle)) {
                    if ui.selectable_label(family == current, family).clicked() {
                        picked = Some(family.clone());
                    }
                }
            });
        });
        // Rule 21: the picker has no visible label of its own; the row's name is its name.
        let label = format!("{name}, {current}");
        ui.ctx().accesskit_node_builder(combo.response.id, |node| node.set_label(label));
        picked
    }

    /// Frame 48: the font is kept, the stock face is in use, and the way out is in the same place.
    fn missing_font(&mut self, ui: &mut Ui, name: &str, key: &str, interface: bool) {
        let what = if interface { "Interface text" } else { "Diffs" };
        live_label(ui, format!("! {name} is not installed on this Mac."), theme::pal(ui).ember_text);
        note(ui, &format!("{what} use the stock font until it is. The setting is kept, so it switches on its own once the font is installed."));
        ui.horizontal(|ui| {
            let default = if interface { Settings::default().ui_font } else { Settings::default().code_font };
            if name != default && Pill::new(PillKind::Secondary, &format!("Use {default}")).small().show(ui).clicked() {
                self.set(ui, json!({ key: default }));
            }
            if Pill::new(PillKind::Secondary, "Open Font Book").small().show(ui).clicked() {
                self.open_later(&["-a", "Font Book"], "Opened Font Book. Install the font, then come back.".to_string());
            }
        });
    }

    fn metrics_rows(&mut self, ui: &mut Ui, s: &Settings) {
        let default = Settings::default().code_line_height;
        let lines = with_current(&[14.0, 16.0, 18.0, default, 22.0, 24.0, 28.0, 32.0], s.code_line_height);
        if let Some(v) = components::form_row(ui, "Code line height", "Rows in the diff, in pixels.", |ui| {
            components::choose(ui, "Code line height", &lines, s.code_line_height).filter(|v| *v != s.code_line_height)
        }) {
            self.set(ui, json!({ "code_line_height": v }));
        }
        let all = [(Density::Compact, "Compact".to_string()), (Density::Default, "Default".to_string()), (Density::Roomy, "Roomy".to_string())];
        if let Some(d) = components::form_row(ui, "Density", "Padding of the rows in lists.", |ui| {
            components::choose(ui, "Density", &all, s.density).filter(|v| *v != s.density)
        }) {
            self.set(ui, json!({ "density": d }));
        }
        let all = [(CodeColours::PrCraft, "PR Craft".to_string()), (CodeColours::HighContrast, "High contrast".to_string())];
        let hint = "Not applied yet: the diff has no syntax colours, so this choice is kept for when it does.";
        if let Some(c) = components::form_row(ui, "Code colours", hint, |ui| {
            components::choose(ui, "Code colours", &all, s.code_colours).filter(|v| *v != s.code_colours)
        }) {
            self.set(ui, json!({ "code_colours": c }));
        }
        let detail = if s.follow_macos_accessibility {
            "Also on when macOS Reduce Motion is on."
        } else {
            "macOS Reduce Motion is not followed (Accessibility)."
        };
        if let Some(on) = switch(ui, "Reduce motion", detail, s.reduce_motion) {
            self.set(ui, json!({ "reduce_motion": on }));
        }
        components::rule(ui);
    }
}

/// "Preview at these settings": three diff rows in the code font, size and line height in force.
fn preview(ui: &mut Ui) {
    section(ui, "Preview at these settings");
    let (p, s) = (theme::pal(ui), theme::sz(ui));
    let rows = [
        ("  41    if err := mark(inv); err != nil {", p.paper),
        ("  42 −  return tx(func(t Tx) error {", p.rose_bg),
        ("  43 +  return err", p.moss_bg),
    ];
    egui::Frame::new().fill(p.raised).stroke((s.hairline, p.border)).inner_margin(theme::SPACE_2).show(ui, |ui| {
        for (line, background) in rows {
            let mut text = RichText::new(line).monospace().size(s.code).color(p.graphite).background_color(background);
            if let Some(height) = s.line {
                text = text.line_height(Some(height));
            }
            ui.label(text);
        }
    });
}

// ---- Accessibility ----

impl Pages {
    pub fn accessibility(&mut self, ui: &mut Ui) {
        let prefs = prefs::get(ui.ctx());
        let s = &prefs.settings;
        title(ui, "Accessibility");
        note(ui, "On top of macOS settings. PR Craft follows Reduce Motion, Increase Contrast and the system text size by default.");
        self.notice_line(ui);
        ui.add_space(theme::SPACE_2);
        let system = prefs::system(ui.ctx());
        let detail = "Reduce Motion, Increase Contrast and the text size. Reduce Transparency is not followed.";
        if let Some(on) = switch(ui, "Follow macOS accessibility settings", detail, s.follow_macos_accessibility) {
            self.set(ui, json!({ "follow_macos_accessibility": on }));
        }
        let answer = |v: Option<bool>| match v {
            Some(true) => "on",
            Some(false) => "off",
            None => "not reported",
        };
        let scale = system.text_scale.map_or("none".to_string(), |x| format!("×{x:.2}"));
        note(
            ui,
            &format!(
                "macOS now: Reduce Motion {}, Increase Contrast {}, text size {scale}.",
                answer(system.reduced_motion),
                answer(system.high_contrast)
            ),
        );
        if let Some(on) = switch(ui, "Increase contrast", "Hairlines become 2 px, tertiary text darkens to secondary.", s.increase_contrast) {
            self.set(ui, json!({ "increase_contrast": on }));
        }
        let detail = "egui draws the ring on whatever has focus, after a click too, so it is always visible. This makes it thicker.";
        if let Some(on) = switch(ui, "Keep focus rings visible", detail, s.focus_rings) {
            self.set(ui, json!({ "focus_rings": on }));
        }
        self.line_ratio(ui, s);
        let all = [(DiffColours::MossAndRose, "Moss and Rose"), (DiffColours::BlueAndOrange, "Blue and orange")];
        section(ui, "Diff colours");
        note(ui, "Colour-blind safe swaps Moss/Rose for blue/orange. Added and removed lines also carry + and −.");
        if let Some(c) = options(ui, "", "Diff colours", &all, s.diff_colours) {
            self.set(ui, json!({ "diff_colours": c }));
        }
        ui.add_space(theme::SPACE_2);
        section(ui, "Not wired yet");
        note(ui, "Mark changes with symbols: always on, the diff draws + and − beside every changed line, so there is no switch.");
        note(ui, "Announce new drafts and findings: not built. Status messages use live regions; per-draft VoiceOver text is not written.");
        note(ui, "VoiceOver reading of a diff line (\"added\", \"removed\"): not built. Reduce Transparency is not followed.");
    }

    /// Frame 100's line-height ratios, written as the same pixel setting Appearance has.
    fn line_ratio(&mut self, ui: &mut Ui, s: &Settings) {
        section(ui, "Code line height");
        note(ui, "Applies to the diff. Relative to the code size; the same setting as Appearance.");
        let ratio = s.code_line_height / s.code_size;
        ui.horizontal(|ui| {
            for r in [1.4_f32, 1.6, 1.8] {
                let picked = (ratio - r).abs() < 0.03;
                let ink = if picked { theme::pal(ui).on_inverse } else { theme::pal(ui).graphite };
                if crate::add_selectable(ui, picked, selectable(ui, picked, RichText::new(r.to_string()).color(ink))).clicked() && !picked {
                    self.set(ui, json!({ "code_line_height": (r * s.code_size * 2.0).round() / 2.0 }));
                }
            }
        });
    }
}

// ---- Keyboard ----

fn flags(row: &KeyRow) -> Vec<String> {
    let mut found: Vec<String> = row
        .conflicts
        .iter()
        .map(|c| match c.severity {
            Severity::Refused => format!("Conflict: {}", c.reason),
            Severity::Warning => format!("Also used elsewhere: {}", c.reason),
        })
        .collect();
    if !row.customised {
        found.extend(keys::action(row.id).and_then(keys::default_note).map(|n| format!("Also used elsewhere: {n}")));
    }
    found
}

impl Pages {
    pub fn keyboard(&mut self, ui: &mut Ui) {
        let prefs = prefs::get(ui.ctx());
        components::page_title(
            ui,
            "Keyboard",
            "Every shortcut, where it works, and whether it collides with macOS or a common app. Shortcuts apply only while PR Craft is in front. Text fields keep ⌘A, ⌘C, ⌘V, ⌘X and ⌘Z, and Tab, ↵ and the arrows keep their normal jobs in lists and menus.",
        );
        self.notice_line(ui);
        self.capture_step(ui, &prefs.keys);
        let elsewhere = prefs.keys.iter().filter(|r| !flags(r).is_empty()).count();
        if elsewhere > 0 {
            let title = if elsewhere == 1 {
                "1 shortcut is also used elsewhere".to_string()
            } else {
                format!("{elsewhere} shortcuts are also used elsewhere")
            };
            let banner = components::banner(
                ui,
                BannerKind::Info,
                &title,
                "Flagged rows say where. They only act inside PR Craft, so nothing breaks; change them if your fingers expect the other app.",
            );
            ui.ctx().accesskit_node_builder(banner.id, |node| node.set_live(egui::accesskit::Live::Polite));
            ui.add_space(theme::SPACE_2);
        }
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut self.filter).hint_text("Search shortcuts").desired_width(theme::PICKER_WIDTH));
            ui.checkbox(&mut self.conflicts_only, "Show conflicts only");
            let any = prefs.keys.iter().any(|r| r.customised);
            if Pill::new(PillKind::Secondary, "Reset all").small().enabled(any).show(ui).clicked() {
                self.notice = Some(prefs::reset_keys(ui.ctx(), None).map(|n| format!("{n} shortcuts are back to their defaults.")));
            }
        });
        self.table(ui, &prefs.keys);
    }

    /// While a shortcut waits for its key: shows the prompt, and takes the next key press as the answer.
    fn capture_step(&mut self, ui: &mut Ui, rows: &[KeyRow]) {
        let Some(id) = self.capture else {
            keymap::set_capturing(ui.ctx(), false);
            return;
        };
        keymap::set_capturing(ui.ctx(), true);
        let label = rows.iter().find(|r| r.id == id).map_or(id, |r| r.label);
        live_label(ui, format!("Press the new shortcut for “{label}”. Esc cancels; use Reset to give an action Esc back."), theme::pal(ui).graphite);
        let Some(pressed) = keymap::captured(ui) else { return };
        match pressed {
            Ok(b) if b == Binding::new("Escape", &[]).unwrap_or(b.clone()) => self.stop_capture(ui.ctx()),
            Ok(b) => {
                self.stop_capture(ui.ctx());
                self.notice = Some(prefs::rebind(ui.ctx(), id, &b.key, &b.modifiers).map(|warnings| {
                    let base = format!("{label} is now {}.", b.label());
                    if warnings.is_empty() { base } else { format!("{base} Warning: {}", warnings.join(" ")) }
                }));
            }
            Err(reason) => self.notice = Some(Err(reason)),
        }
    }

    fn table(&mut self, ui: &mut Ui, rows: &[KeyRow]) {
        let needle = self.filter.to_lowercase();
        let only_flagged = self.conflicts_only;
        let shown = |r: &&KeyRow| {
            let hay = format!("{} {} {} {}", r.label, r.scope, r.shortcut, r.id).to_lowercase();
            hay.contains(&needle) && (!only_flagged || !flags(r).is_empty())
        };
        let mut scope = "";
        let mut asked = None;
        for row in rows.iter().filter(shown) {
            if row.scope != scope {
                scope = row.scope;
                if !scope.is_empty() {
                    components::rule(ui);
                }
                section(ui, scope);
            }
            self.row(ui, row, &mut asked);
        }
        if let Some(id) = asked {
            self.capture = Some(id);
        }
    }

    /// One shortcut: its cap, what it does, where it collides, and Change (and Reset when it was changed).
    fn row(&mut self, ui: &mut Ui, row: &KeyRow, asked: &mut Option<&'static str>) {
        components::rule(ui);
        let p = theme::pal(ui);
        let found = flags(row);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_min_height(36.0);
            let (cell, _) = ui.allocate_exact_size(egui::vec2(96.0, 36.0), egui::Sense::hover());
            // Painted in the cell the row reserved, so the labels line up whatever the cap's width.
            let mut cap = ui.new_child(egui::UiBuilder::new().max_rect(cell).layout(egui::Layout::left_to_right(egui::Align::Center)));
            components::key_cap(&mut cap, &row.shortcut);
            components::label(ui, row.label, 13.0, p.graphite);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Pill::new(PillKind::Quiet, "Change").small().show(ui).on_hover_text(format!("Rebind {}", row.label)).clicked() {
                    *asked = Some(row.id);
                }
                if row.customised
                    && Pill::new(PillKind::Quiet, "Reset")
                        .small()
                        .show(ui)
                        .on_hover_text(format!("Reset {} to {}", row.label, row.default.label()))
                        .clicked()
                {
                    self.notice =
                        Some(prefs::reset_keys(ui.ctx(), Some(row.id)).map(|_| format!("{} is back to {}.", row.label, row.default.label())));
                }
                if let Some(first) = found.first() {
                    let app = keys::action(row.id).and_then(keys::default_app).filter(|_| !row.customised);
                    let tag = match (row.conflicts.iter().any(|c| c.severity == Severity::Refused), app) {
                        (true, _) => "Conflict".to_string(),
                        (false, Some(app)) => format!("Also {app}"),
                        (false, None) => "Also elsewhere".to_string(),
                    };
                    components::tag_described(ui, components::TagKind::YourTurn, &tag, first);
                }
            });
        });
        // The whole reason, in words, under the row.
        for flag in &found {
            components::padded(ui, egui::Margin { left: 96, right: 0, top: 0, bottom: 6 }, |ui| {
                components::note_sized(ui, flag.as_str(), 12.0, p.text_secondary);
            });
        }
    }
}

// ---- Offline and sync ----

impl Pages {
    pub fn sync(&mut self, ui: &mut Ui) {
        let prefs = prefs::get(ui.ctx());
        let s = &prefs.settings;
        title(ui, "Offline and sync");
        note(ui, "Drafts always live on this Mac. Every post is made when you submit; there is no queue of posts to send later.");
        self.notice_line(ui);
        ui.add_space(theme::SPACE_2);
        section(ui, "Check for changes every");
        note(ui, "Webhooks are not used; gh and glab are polled.");
        let all = [(1_u32, "1 min"), (5, "5 min"), (15, "15 min")];
        if let Some(m) = options(ui, "", "Check for changes every", &all, s.poll_minutes) {
            self.set(ui, json!({ "poll_minutes": m }));
        }
        let on_battery = self.battery.on_battery(ui.ctx());
        let power = match on_battery {
            Some(true) => "The Mac is on battery.",
            Some(false) => "The Mac is on mains power.",
            None => "PR Craft could not read whether the Mac is on battery (pmset gave no answer).",
        };
        let every = s.poll_every(on_battery == Some(true));
        live_label(ui, format!("Checking every {every} min now. {power}"), theme::pal(ui).graphite);
        for line in guard_lines(every, &pr_craft_engine::quota::status(every, Utc::now())) {
            live_label(ui, line, theme::pal(ui).graphite);
        }
        let detail = format!("Every {} min while on battery power.", pr_craft_core::settings::BATTERY_POLL_MINUTES);
        if let Some(on) = switch(ui, "Slow down on battery", &detail, s.slower_on_battery) {
            self.set(ui, json!({ "slower_on_battery": on }));
        }
        ui.add_space(theme::SPACE_2);
        section(ui, "If the PR changed while you were offline");
        note(ui, "For example, new commits under your queued comments. Stored, not applied: nothing is queued yet, so there is nothing to decide.");
        let all = [(IfChanged::Ask, "Ask me"), (IfChanged::PostAnyway, "Post anyway")];
        if let Some(v) = options(ui, "", "If the PR changed while you were offline", &all, s.if_changed_offline) {
            self.set(ui, json!({ "if_changed_offline": v }));
        }
        ui.add_space(theme::SPACE_2);
        section(ui, "Waiting to post");
        note(ui, "Nothing waits here: M2 has no offline queue. A post that fails keeps your drafts and verdict on this Mac, to copy or retry.");
        self.data_folder(ui);
        ui.add_space(theme::SPACE_2);
        section(ui, "Diagnostics");
        note(ui, "The app version, macOS, forge sign-ins and the last log lines, with secrets masked, to paste into a report.");
        if ui.add_enabled(self.diagnostics.is_none(), egui::Button::new("Copy diagnostics")).clicked() {
            self.copy_diagnostics();
        }
    }

    /// `diagnostics.get` on a worker thread (it asks the forge CLIs), then its `text` on the clipboard.
    fn copy_diagnostics(&mut self) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let said = pr_craft_engine::run("diagnostics.get", &Value::Null).map_err(crate::fail).and_then(|answer| {
                answer["text"].as_str().map(String::from).ok_or_else(|| "diagnostics.get answered without text (E-ENCODE)".to_string())
            });
            // The page may have closed first; nobody is left to tell.
            let _ = tx.send(said);
        });
        self.diagnostics = Some(rx);
    }

    fn data_folder(&mut self, ui: &mut Ui) {
        ui.add_space(theme::SPACE_2);
        match pr_craft_engine::settings::data_dir() {
            Ok(dir) => {
                note(ui, &format!("Drafts and settings: {}", dir.display()));
                if ui.button("Show in Finder").clicked() {
                    self.open_later(&[&dir.display().to_string()], "Opened the folder.".to_string());
                }
            }
            Err(e) => live_label(ui, e.to_string(), theme::pal(ui).rose_text),
        }
    }
}

// ---- About ----

#[derive(Clone, Copy)]
enum Step {
    Check,
    Download,
}

/// The update check on the About page: the command on its way, what it answered, and the zip once saved.
#[derive(Default)]
struct Updates {
    running: Option<(Step, Receiver<Result<Value, String>>)>,
    line: Option<Result<String, String>>,
    /// The version `update.download` would fetch.
    offer: Option<String>,
    saved: Option<String>,
}

impl Updates {
    fn finish(&mut self, step: Step, said: Result<Value, String>) {
        self.line = Some(said.and_then(|answer| match step {
            Step::Check => {
                let latest = answer["latest"].as_str();
                self.offer = None;
                match (latest, answer["newer"].as_bool() == Some(true)) {
                    (Some(latest), true) if !answer["asset"].is_null() => {
                        self.offer = Some(latest.to_string());
                        Ok(format!("PR Craft {latest} is available"))
                    }
                    (Some(latest), true) => Ok(format!("PR Craft {latest} is available, but its release has no zip for this Mac.")),
                    (None, _) => Ok("No PR Craft release is published yet.".to_string()),
                    _ => Ok("You have the latest version".to_string()),
                }
            }
            Step::Download => {
                self.offer = None;
                self.saved = Some(answer["path"].as_str().ok_or("update.download answered without a path (E-ENCODE)")?.to_string());
                Ok("Saved to Downloads".to_string())
            }
        }));
    }

    fn start(&mut self, step: Step, command: &'static str, params: Value) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // The page may have closed first; nobody is left to tell.
            let _ = tx.send(pr_craft_engine::run(command, &params).map_err(crate::fail));
        });
        self.running = Some((step, rx));
        self.line = None;
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some((step, rx)) = self.running.take() {
            match rx.try_recv() {
                Ok(said) => self.finish(step, said),
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100));
                    self.running = Some((step, rx));
                }
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
    }
}

/// What PR Craft does not do yet, each with the way round it.
const KNOWN_LIMITS: [&str; 5] = [
    "A suggested change shows as a comment, with no button to apply it. Open on forge to apply it.",
    "Markdown tables show as text. Open on forge to see the table.",
    "A GitLab comment on several lines is pinned to the last line. Open on forge to see the whole range.",
    "Conflicting file names are not told apart here. Open on forge to see them.",
    "A comment pinned to a point on an image or to a notebook cell is not shown on the diff. Open on forge to read it.",
];

impl Pages {
    pub fn about(&mut self, ui: &mut Ui) {
        self.updates.poll(ui.ctx());
        let commit = option_env!("PR_CRAFT_COMMIT").unwrap_or("dev");
        title(ui, "About");
        note(ui, &format!("PR Craft {}, commit {}", env!("CARGO_PKG_VERSION"), commit.get(..7).unwrap_or(commit)));
        self.notice_line(ui);
        ui.add_space(theme::SPACE_2);
        section(ui, "Updates");
        if ui.add_enabled(self.updates.running.is_none(), egui::Button::new("Check for updates")).clicked() {
            self.updates.start(Step::Check, "update.check", Value::Null);
        }
        match &self.updates.line {
            Some(Ok(text)) => live_label(ui, text.as_str(), theme::pal(ui).moss_text),
            Some(Err(error)) => live_label(ui, error.as_str(), theme::pal(ui).rose_text),
            None if self.updates.running.is_some() => live_label(ui, "Working…", theme::pal(ui).graphite),
            None => {}
        }
        if self.updates.offer.is_some() && ui.add_enabled(self.updates.running.is_none(), egui::Button::new("Download")).clicked() {
            self.updates.start(Step::Download, "update.download", json!({}));
        }
        if let Some(path) = self.updates.saved.clone()
            && ui.button("Show in Finder").clicked()
        {
            self.open_later(&["-R", &path], "Opened the folder.".to_string());
        }
        note(ui, "Updates are not installed for you: quit PR Craft, open the zip, and replace the app in Applications");
        ui.add_space(theme::SPACE_2);
        section(ui, "Known limits");
        for limit in KNOWN_LIMITS {
            note(ui, &format!("•  {limit}"));
        }
    }
}

// ---- Permissions ----

/// What PR Craft asks macOS for, why, and where the pane is. `None` for a grant with no System Settings pane.
const GRANTS: [(&str, &str, &[&str], &str); 6] = [
    (
        "Notifications",
        "Banners come from Script Editor and have no buttons: macOS lets PR Craft post them only through AppleScript. Allow notifications for Script Editor in System Settings → Notifications.",
        &["x-apple.systempreferences:com.apple.Notifications-Settings.extension"],
        "Open Notifications settings",
    ),
    (
        "Automation · Terminal",
        "Starts the claude CLI for summaries, review agents and babysit.",
        &["x-apple.systempreferences:com.apple.preference.security?Privacy_Automation"],
        "Allow in Settings",
    ),
    (
        "Files and folders",
        "Agents read the checkouts you share with them.",
        &["x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders"],
        "Open Files and Folders",
    ),
    ("Keychain", "Stores the Z.ai API key. gh and glab keep their own logins.", &["-a", "Keychain Access"], "Show in Keychain"),
    (
        "Launch at login",
        "Starts hidden in the menu bar so notifications arrive after a restart. Not built yet.",
        &["x-apple.systempreferences:com.apple.LoginItems-Settings.extension"],
        "Open Login Items",
    ),
    (
        "Menu bar item",
        "Waiting count and quick actions. Not built yet.",
        &["x-apple.systempreferences:com.apple.ControlCenter-Settings.extension"],
        "Menu bar settings",
    ),
];

impl Pages {
    pub fn permissions(&mut self, ui: &mut Ui) {
        title(ui, "Permissions");
        note(ui, "What PR Craft asks macOS for, and why. Each button opens the right pane in System Settings.");
        self.notice_line(ui);
        live_label(
            ui,
            "! PR Craft cannot read these grants yet, so none is shown as granted or denied. Check each in System Settings.",
            theme::pal(ui).ember_text,
        );
        ui.add_space(theme::SPACE_2);
        for (name, why, target, button) in GRANTS {
            section(ui, name);
            note(ui, why);
            if name == "Notifications" {
                let on = prefs::get(ui.ctx()).settings.banners;
                if let Some(on) = switch(ui, "Show macOS banners", "One banner for each new review request, change back to you or new commits.", on) {
                    self.set(ui, json!({ "banners": on }));
                }
            }
            note(ui, "Status: not readable by PR Craft.");
            if ui.button(button).clicked() {
                self.open_later(target, format!("Opened {name}. The status is not shown here; PR Craft cannot read it."));
            }
            ui.add_space(theme::SPACE_2);
        }
        note(ui, "PR Craft never asks for Full Disk Access, Accessibility or Screen Recording.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_answer_says_what_it_found_and_offers_the_download_only_when_there_is_a_zip() {
        let mut u = Updates::default();
        u.finish(Step::Check, Ok(json!({ "latest": "0.2.0", "newer": true, "asset": { "name": "z", "size": 1 } })));
        assert_eq!(u.line, Some(Ok("PR Craft 0.2.0 is available".to_string())));
        assert_eq!(u.offer.as_deref(), Some("0.2.0"));
        u.finish(Step::Download, Ok(json!({ "path": "/Users/a/Downloads/z.zip" })));
        assert_eq!((u.line, u.saved.as_deref(), u.offer), (Some(Ok("Saved to Downloads".to_string())), Some("/Users/a/Downloads/z.zip"), None));
        let mut u = Updates::default();
        u.finish(Step::Check, Ok(json!({ "latest": "0.1.0", "newer": false, "asset": null })));
        assert_eq!(u.line, Some(Ok("You have the latest version".to_string())));
        u.finish(Step::Check, Err("offline (E-UPDATE)".into()));
        assert_eq!(u.line, Some(Err("offline (E-UPDATE)".to_string())));
    }

    #[test]
    fn permissions_says_where_banners_come_from_and_names_their_switch() {
        let text = crate::ai::tests::drawn(&egui::Context::default(), |ui| Pages::new().permissions(ui));
        assert!(text.contains("Banners come from Script Editor and have no buttons"), "{text}");
        assert!(text.contains("Allow notifications for Script Editor in System Settings → Notifications."), "{text}");
        assert!(text.contains("Show macOS banners"), "{text}");
    }

    #[test]
    fn a_row_with_a_conflict_or_another_apps_chord_is_flagged_in_words() {
        let row = |id: &str| {
            let action = keys::action(id).expect("shipped");
            let binding = action.default_binding();
            let effective = keys::effective(&Default::default());
            KeyRow {
                id: action.id,
                scope: action.scope.label(),
                label: action.label,
                shortcut: binding.label(),
                default: binding.clone(),
                customised: false,
                conflicts: keys::check(action, &binding, &effective),
                binding,
            }
        };
        let share = row("review.share");
        assert!(flags(&share).iter().any(|f| f.starts_with("Also used elsewhere") && f.contains("Mail")), "{:?}", flags(&share));
        assert!(flags(&row("review.comment")).is_empty());
    }
}

/// What the GitHub quota guard does, per host, when it differs from the chosen `every` minutes or has paused the host.
fn guard_lines(every: u32, hosts: &[HostQuota]) -> Vec<String> {
    let clock = |at: DateTime<Utc>| at.with_timezone(&Local).format("%H:%M");
    let mut lines = Vec::new();
    for host in hosts {
        if let Some(until) = host.paused_until {
            lines.push(format!("{}: Paused until {}: GitHub rate limit", host.host, clock(until)));
        } else if host.effective_minutes != every {
            lines.push(format!(
                "{}: Polling every {} min to stay under 1,250 points an hour (\u{2248}{} points/h)",
                host.host, host.effective_minutes, host.projected_per_hour
            ));
        }
    }
    lines
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    fn host(effective_minutes: u32, paused_until: Option<DateTime<Utc>>) -> HostQuota {
        HostQuota {
            host: "github.com".into(),
            last_cost: 57,
            projected_per_hour: 1140,
            remaining: Some(900),
            reset_at: paused_until,
            effective_minutes,
            paused_until,
        }
    }

    #[test]
    fn sync_says_what_the_guard_does_only_when_it_acts() {
        assert!(guard_lines(5, &[host(5, None)]).is_empty());
        assert_eq!(guard_lines(1, &[host(3, None)]), ["github.com: Polling every 3 min to stay under 1,250 points an hour (\u{2248}1140 points/h)"]);
        let until: DateTime<Utc> = "2026-11-28T10:30:00Z".parse().expect("a time");
        let lines = guard_lines(5, &[host(5, Some(until))]);
        assert_eq!(lines, [format!("github.com: Paused until {}: GitHub rate limit", until.with_timezone(&Local).format("%H:%M"))]);
    }
}
