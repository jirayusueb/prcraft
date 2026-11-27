//! The command palette (frame 92): ⌘K opens a card over the screen with a query field and the actions, changes and Settings pages
//! that match it. The keys are the registry's (`palette.*`), so a rebinding shows in the footer and works at once.
//!
//! The palette is modal for the keyboard: it takes its own keys before anything else sees them and takes every other key press
//! away from the screen under it, so typing `j` into the field never moves the inbox.

use std::sync::Arc;

use craft::egui::text::{LayoutJob, TextWrapping};
use craft::egui::{
    self, Align2, Color32, Context, CornerRadius, FontId, Galley, Margin, Order, Response, Role, ScrollArea, Sense, Stroke, StrokeKind, TextEdit,
    TextFormat, Ui, WidgetInfo, pos2, vec2,
};
use pr_craft_core::Change;
use pr_craft_core::palette::{Entry, Facts, Group, Screen, Target, search_with};

use crate::{components, keymap, settings, theme};

const WIDTH: f32 = 640.0;
/// Where the card starts, from the top of the window: just under the title bar.
const TOP: f32 = 120.0;
const LIST_MAX_HEIGHT: f32 = 380.0;
const ROW_HEIGHT: f32 = 48.0;
/// The space between a row and the card's edge.
const ROW_INSET: f32 = 8.0;
const QUEUES: [&str; 3] = ["palette.queue_1", "palette.queue_2", "palette.queue_3"];

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Event {
    None,
    Close,
    /// Run this entry: the caller closes the palette and acts on the target.
    Run(Target),
    /// Show the inbox list at this index of Review requested, Back to you, Notifications.
    Queue(usize),
}

pub(crate) struct Palette {
    query: String,
    selected: usize,
    focus_field: bool,
    /// Tab has moved focus from the field onto the rows: the `selected` row has focus.
    rows_focused: bool,
    /// The entries for `searched`: a keystroke searches once, a frame only draws.
    shown: Vec<Entry>,
    searched: Option<(String, Screen, usize)>,
    /// What the open review can do, so the list holds only keys that would act.
    facts: Option<Facts>,
    /// `facts` changed since the last search.
    facts_changed: bool,
    /// The selection moved by key, so the list scrolls it into view.
    reveal: bool,
    /// What a search covered, and what it left out (a source that has not answered), for the empty result.
    scope: String,
}

/// The Settings tab a `Target::Settings` page names.
pub(crate) fn tab(page: &str) -> Option<settings::Tab> {
    use settings::Tab;
    Some(match page {
        "engine" => Tab::Engine,
        "sees" => Tab::Sees,
        "agents" => Tab::Agents,
        "repos" => Tab::Repos,
        "appearance" => Tab::Appearance,
        "accessibility" => Tab::Accessibility,
        "keyboard" => Tab::Keyboard,
        "sync" => Tab::Sync,
        "permissions" => Tab::Permissions,
        _ => return None,
    })
}

impl Palette {
    /// An open palette with an empty query; the field takes focus on its first frame.
    pub(crate) fn open() -> Self {
        Palette {
            query: String::new(),
            selected: 0,
            focus_field: true,
            rows_focused: false,
            shown: Vec::new(),
            searched: None,
            facts: None,
            facts_changed: false,
            reveal: false,
            scope: String::new(),
        }
    }

    /// An open palette with `query` already typed, for the render tests.
    #[cfg(test)]
    pub(crate) fn typed(query: &str) -> Self {
        Palette { query: query.to_string(), ..Self::open() }
    }

    /// Tells the palette what the open review can do, and searches again when that changed.
    pub(crate) fn set_facts(&mut self, facts: Option<Facts>) {
        if self.facts != facts {
            self.facts = facts;
            self.facts_changed = true;
        }
    }

    /// Tells the palette what a search covers, for the words of an empty result.
    pub(crate) fn set_scope(&mut self, scope: String) {
        self.scope = scope;
    }

    /// Draws the card for `screen` over the window and returns what the reader chose. `changes` are the inbox's, newest first.
    pub(crate) fn ui(&mut self, ctx: &Context, screen: Screen, changes: &[Change]) -> Event {
        self.refresh(ctx, screen, changes);
        let keyed = self.keys(ctx);
        if keyed != Event::None {
            return keyed;
        }
        let mut event = Event::None;
        let p = theme::pal(ctx);
        let frame = egui::Frame::window(&ctx.global_style())
            .fill(p.raised)
            .stroke(Stroke::new(theme::sz(ctx).hairline, p.border_strong))
            .corner_radius(10)
            .inner_margin(0);
        egui::Window::new("Command palette")
            .title_bar(false)
            .frame(frame)
            .order(Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, TOP])
            .min_width(WIDTH)
            .max_width(WIDTH)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                self.query_row(ui);
                self.refresh(ui.ctx(), screen, changes);
                components::rule(ui);
                self.results(ui, &mut event);
                components::rule(ui);
                footer(ui);
            });
        ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { .. })));
        event
    }

    /// Searches again when the query, the screen or the inbox changed, and puts the selection back on the first row when the
    /// query did.
    fn refresh(&mut self, ctx: &Context, screen: Screen, changes: &[Change]) {
        let now = (self.query.clone(), screen, changes.len());
        if !self.facts_changed && self.searched.as_ref() == Some(&now) {
            return;
        }
        let same_query = self.searched.as_ref().is_some_and(|(query, ..)| *query == now.0);
        // Only the review's state changed: the entry the reader is on stays selected wherever it moved to.
        let keep = same_query.then(|| self.shown.get(self.selected).map(|e| e.target.clone())).flatten();
        self.shown = search_with(&self.query, Some(screen), &keymap::installed(ctx), changes, self.facts.as_ref());
        self.selected = match keep.and_then(|target| self.shown.iter().position(|e| e.target == target)) {
            Some(at) => at,
            None if same_query => self.selected.min(self.shown.len().saturating_sub(1)),
            None => 0,
        };
        self.searched = Some(now);
        self.facts_changed = false;
    }

    /// The palette's own keys, taken before the field or the screen under it can see them.
    fn keys(&mut self, ctx: &Context) -> Event {
        if keymap::consume(ctx, "palette.close") {
            return Event::Close;
        }
        if let Some(n) = QUEUES.iter().position(|id| keymap::consume(ctx, id)) {
            return Event::Queue(n);
        }
        if keymap::consume(ctx, "palette.next") {
            self.selected = (self.selected + 1).min(self.shown.len().saturating_sub(1));
            self.reveal = true;
        }
        if keymap::consume(ctx, "palette.prev") {
            self.selected = self.selected.saturating_sub(1);
            self.reveal = true;
        }
        if keymap::consume(ctx, "palette.run") {
            return self.shown.get(self.selected).map_or(Event::None, |entry| Event::Run(entry.target.clone()));
        }
        let (tab, back) = take_tab(ctx);
        if tab {
            self.tab(back);
        }
        // Space on a focused row runs it, as ↵ does; in the field it is a space in the query.
        if self.rows_focused && take_key(ctx, egui::Key::Space) {
            return self.shown.get(self.selected).map_or(Event::None, |entry| Event::Run(entry.target.clone()));
        }
        Event::None
    }

    /// Tab and ⇧Tab walk field, rows, field: focus never leaves the palette while it is open.
    fn tab(&mut self, back: bool) {
        let n = self.shown.len();
        self.reveal = true;
        if n == 0 {
            self.rows_focused = false;
            return;
        }
        match (self.rows_focused, back) {
            (false, false) => self.selected = self.selected.min(n - 1),
            (false, true) => self.selected = n - 1,
            (true, false) if self.selected + 1 < n => self.selected += 1,
            (true, true) if self.selected > 0 => self.selected -= 1,
            _ => {
                self.rows_focused = false;
                return;
            }
        }
        self.rows_focused = true;
    }

    fn query_row(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        let font = FontId::proportional(15.0 * theme::sz(ui).body / 13.5);
        components::padded(ui, Margin { left: 20, right: 20, top: 14, bottom: 12 }, |ui| {
            ui.horizontal(|ui| {
                components::label(ui, "›", 16.0, p.text_tertiary);
                let field = ui.add(
                    TextEdit::singleline(&mut self.query)
                        .id_salt("palette-query")
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY)
                        .font(font)
                        .text_color(p.graphite)
                        .hint_text("Type a command, a change or a setting"),
                );
                components::name_field(ui, &field, "Command palette");
                // The palette is modal: the field has focus unless Tab put it on a row, and Tab stays here for the palette to take.
                keep_tab(ui.ctx(), field.id);
                if field.clicked() || field.gained_focus() {
                    self.rows_focused = false;
                }
                if std::mem::take(&mut self.focus_field) || !self.rows_focused {
                    field.request_focus();
                }
            });
        });
    }

    fn results(&mut self, ui: &mut Ui, event: &mut Event) {
        if self.shown.is_empty() {
            let title = format!("Nothing matches “{}”", self.query.trim());
            let body = if self.scope.is_empty() {
                "Searched commands, Settings pages and the changes of your queue.".to_string()
            } else {
                self.scope.clone()
            };
            let cleared = components::padded(ui, Margin::symmetric(20, 24), |ui| components::empty_state(ui, &title, &body, &[("Clear search", "")]));
            if cleared == Some(0) {
                self.query.clear();
                self.focus_field = true;
            }
            return;
        }
        let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
        let (mut hovered, mut group) = (None, None);
        ScrollArea::vertical().max_height(LIST_MAX_HEIGHT).auto_shrink([false, true]).show(ui, |ui| {
            for (i, entry) in self.shown.iter().enumerate() {
                if group != Some(entry.group) {
                    group = Some(entry.group);
                    group_title(ui, entry.group);
                }
                let selected = i == self.selected;
                let response = row(ui, entry, selected);
                keep_tab(ui.ctx(), response.id);
                if selected && self.rows_focused {
                    response.request_focus();
                }
                if selected && self.reveal {
                    response.scroll_to_me(None);
                }
                if response.clicked() {
                    *event = Event::Run(entry.target.clone());
                }
                if response.hovered() && moved {
                    hovered = Some(i);
                }
            }
        });
        self.reveal = false;
        if let Some(i) = hovered {
            self.selected = i;
        }
    }
}

fn group_title(ui: &mut Ui, group: Group) {
    let tertiary = theme::pal(ui).text_tertiary;
    components::padded(ui, Margin { left: 20, right: 20, top: 10, bottom: 4 }, |ui| components::label(ui, group.label(), 11.5, tertiary));
}

/// `text` on one line, cut with an ellipsis at `width`.
fn fit(ui: &Ui, text: &str, font: FontId, colour: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_string(), TextFormat::simple(font, colour));
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    ui.painter().layout_job(job)
}

/// One entry: its title over its subtitle, its key cap on the right. The selected row is graphite with the inverse ink.
fn row(ui: &mut Ui, entry: &Entry, selected: bool) -> Response {
    let p = theme::pal(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    let card = rect.shrink2(vec2(ROW_INSET, 1.0));
    if selected {
        ui.painter().rect_filled(card, CornerRadius::same(6), p.inverse);
    }
    let (ink, quiet) = if selected { (p.on_inverse, p.on_inverse_quiet) } else { (p.graphite, p.text_secondary) };
    let (body, code) = (theme::sz(ui).body / 13.5, theme::sz(ui).code / 12.5);
    let mut right = card.right() - 12.0;
    if let Some(key) = &entry.key {
        right = cap(ui, right, card.center().y, key, selected, code) - 12.0;
    }
    let (left, width) = (card.left() + 12.0, right - card.left() - 12.0);
    let put = |galley: Arc<Galley>, y: f32| ui.painter().galley(pos2(left, y - galley.size().y / 2.0), galley, Color32::PLACEHOLDER);
    put(fit(ui, &entry.title, FontId::proportional(13.0 * body), ink, width), card.center().y - 8.0);
    put(fit(ui, &entry.subtitle, FontId::proportional(11.5 * body), quiet, width), card.center().y + 9.0);
    if response.has_focus() {
        ui.painter().rect_stroke(card.expand(1.0), CornerRadius::same(7), egui::Stroke::new(2.0, p.focus_ring), egui::StrokeKind::Outside);
    }
    let name = match &entry.key {
        Some(key) => format!("{}, {}, {key}", entry.title, entry.subtitle),
        None => format!("{}, {}", entry.title, entry.subtitle),
    };
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, name.clone()));
    response
}

/// A key cap whose right edge is at `right`; returns its left edge. On the selected row it is the faint light box of a cap on
/// graphite, elsewhere the raised box of `components::key_cap`.
fn cap(ui: &Ui, right: f32, centre_y: f32, label: &str, selected: bool, code: f32) -> f32 {
    let p = theme::pal(ui);
    let ink = if selected { p.on_inverse } else { p.text_secondary };
    let galley = ui.painter().layout_no_wrap(label.to_string(), FontId::monospace(11.0 * code), ink);
    let width = (galley.size().x + 12.0).max(18.0);
    let rect = egui::Rect::from_min_size(pos2(right - width, centre_y - 9.0), vec2(width, 18.0));
    if selected {
        ui.painter().rect_filled(rect, CornerRadius::same(4), p.on_inverse.gamma_multiply(0.15));
    } else {
        ui.painter().rect(rect, CornerRadius::same(4), p.raised, Stroke::new(theme::sz(ui).hairline, p.border_strong), StrokeKind::Inside);
    }
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, Color32::PLACEHOLDER);
    rect.left()
}

/// `⌘1–3` as the bindings read now: the last key of the third joined to the first when the three share their modifiers.
fn queue_keys(ui: &Ui) -> String {
    let [one, _, three] = QUEUES;
    let (first, last) = (keymap::label(ui, one), keymap::label(ui, three));
    let head = first.chars().count().saturating_sub(1);
    if first.chars().take(head).eq(last.chars().take(head)) {
        format!("{first}–{}", last.chars().skip(head).collect::<String>())
    } else {
        format!("{first}–{last}")
    }
}

/// The keys the palette answers to, with what each does; they read from the registry, so a rebinding shows.
fn footer(ui: &mut Ui) {
    let (secondary, sunken) = (theme::pal(ui).text_secondary, theme::pal(ui).sunken);
    let bottom = CornerRadius { nw: 0, ne: 0, sw: 10, se: 10 };
    egui::Frame::new().fill(sunken).corner_radius(bottom).inner_margin(Margin::symmetric(20, 10)).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let queue = queue_keys(ui);
            let groups = [
                (vec![keymap::label(ui, "palette.prev"), keymap::label(ui, "palette.next")], "Move"),
                (vec![keymap::label(ui, "palette.run")], "Run"),
                (vec![queue], "Jump to queue"),
                (vec![keymap::label(ui, "palette.close")], "Close"),
            ];
            for (caps, what) in groups {
                for cap in &caps {
                    components::key_cap(ui, cap);
                }
                components::note(ui, what, secondary);
                ui.add_space(8.0);
            }
        });
    });
}

/// Takes the Tab key presses of this frame so egui does not move focus out of the palette; says whether one came and whether
/// ⇧ was held.
fn take_tab(ctx: &Context) -> (bool, bool) {
    let mut seen = (false, false);
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            egui::Event::Key { key: egui::Key::Tab, pressed: true, modifiers, .. } => {
                seen = (true, modifiers.shift);
                false
            }
            _ => true,
        });
    });
    seen
}

/// Takes the press of `key` this frame, whatever modifiers; says whether there was one.
fn take_key(ctx: &Context, key: egui::Key) -> bool {
    ctx.input_mut(|i| {
        let before = i.events.len();
        i.events.retain(|e| !matches!(e, egui::Event::Key { key: k, pressed: true, .. } if *k == key));
        i.events.len() != before
    })
}

/// Asks egui to leave Tab alone while `id` has focus, so the palette takes it.
fn keep_tab(ctx: &Context, id: egui::Id) {
    ctx.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter { tab: true, ..egui::EventFilter::default() }));
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use craft::egui::{Key, Modifiers, Vec2};
    use egui_kittest::Harness;
    use egui_kittest::kittest::{By, NodeT, Queryable};
    use pr_craft_core::Forge;

    use super::*;

    struct State {
        palette: Palette,
        changes: Vec<Change>,
        last: Event,
        ready: bool,
    }

    /// Changes shaped like an inbox of one forge with invented names.
    fn changes() -> Vec<Change> {
        let one = |number: u64, title: &str| Change {
            forge: Forge::GitHub,
            repo: "acme/api".into(),
            number,
            title: title.into(),
            author: "nina.k".into(),
            url: format!("https://example.test/acme/api/{number}"),
            host: None,
            updated_at: Utc::now(),
            draft: false,
            stack: None,
        };
        vec![one(11, "Retry idempotent webhooks"), one(12, "Pin the provider")]
    }

    fn harness() -> Harness<'static, State> {
        let state = State { palette: Palette::open(), changes: changes(), last: Event::None, ready: false };
        Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_pixels_per_point(1.0).build_ui_state(
            |ui, state: &mut State| {
                if !state.ready {
                    crate::prefs::install_defaults(ui.ctx());
                    let look = theme::Look::default();
                    theme::install(ui.ctx(), look);
                    theme::apply_style(ui.ctx(), &look);
                    crate::fonts::install_blocking(ui.ctx(), &pr_craft_core::settings::Settings::default());
                    state.ready = true;
                }
                let event = state.palette.ui(ui.ctx(), Screen::Inbox, &state.changes);
                if event != Event::None {
                    state.last = event;
                }
            },
            state,
        )
    }

    fn names(harness: &Harness<'_, State>) -> Vec<String> {
        harness.query_all(By::new().role(Role::Button)).filter_map(|node| node.accesskit_node().label().map(|name| name.to_string())).collect()
    }

    fn press(harness: &mut Harness<'_, State>, key: Key) {
        harness.key_press_modifiers(Modifiers::NONE, key);
        harness.run();
    }

    #[test]
    fn typing_narrows_the_list_and_every_row_names_its_title_and_key() {
        let mut harness = harness();
        harness.run();
        let all = names(&harness);
        assert!(all.iter().any(|name| name.starts_with("Open Settings") && name.ends_with("⌘,")), "{all:?}");
        harness.get_by_role(Role::TextInput).type_text("webhooks");
        harness.run();
        let narrowed = names(&harness);
        assert!(narrowed.len() < all.len(), "{narrowed:?}");
        assert!(narrowed.iter().any(|name| name.starts_with("Retry idempotent webhooks")), "{narrowed:?}");
        let field = harness.get_by_role(Role::TextInput);
        assert_eq!(field.accesskit_node().label().as_deref(), Some("Command palette"), "the field is named");
    }

    #[test]
    fn down_then_return_runs_the_second_row_and_escape_closes() {
        let mut harness = harness();
        harness.run();
        press(&mut harness, Key::ArrowDown);
        press(&mut harness, Key::Enter);
        let second = harness.state().palette.shown.get(1).map(|entry| entry.target.clone()).expect("a second row");
        assert_eq!(harness.state().last, Event::Run(second));
        press(&mut harness, Key::Escape);
        assert_eq!(harness.state().last, Event::Close);
    }

    #[test]
    fn tab_and_shift_tab_cycle_inside_the_palette_and_space_runs_the_focused_row() {
        let mut harness = harness();
        harness.run();
        let n = harness.state().palette.shown.len();
        let inside = |harness: &Harness<'_, State>| {
            let field = harness.get_by_role(Role::TextInput).accesskit_node().is_focused();
            let rows = harness.query_all(By::new().role(Role::Button)).filter(|node| node.accesskit_node().is_focused()).count();
            (field, rows)
        };
        assert_eq!(inside(&harness), (true, 0), "the field starts with focus");
        for step in 1..=n {
            press(&mut harness, Key::Tab);
            assert_eq!(inside(&harness), (false, 1), "Tab {step} is on exactly one row");
            assert_eq!(harness.state().palette.selected, step - 1, "the focused row is the selected one");
        }
        press(&mut harness, Key::Tab);
        assert_eq!(inside(&harness), (true, 0), "Tab past the last row comes back to the field, not out of the palette");
        harness.key_press_modifiers(Modifiers::SHIFT, Key::Tab);
        harness.run();
        assert_eq!(inside(&harness), (false, 1), "⇧Tab from the field goes to the last row");
        assert_eq!(harness.state().palette.selected, n - 1);
        press(&mut harness, Key::Space);
        let last = harness.state().palette.shown.get(n - 1).map(|entry| entry.target.clone()).expect("a last row");
        assert_eq!(harness.state().last, Event::Run(last), "Space runs the row that has focus");
    }

    #[test]
    fn a_query_that_matches_nothing_says_so_with_the_query() {
        let mut harness = harness();
        harness.run();
        harness.get_by_role(Role::TextInput).type_text("zzzzqx");
        harness.run();
        assert!(harness.query_by_label_contains("Nothing matches “zzzzqx”").is_some());
        assert!(harness.state().palette.shown.is_empty());
        press(&mut harness, Key::Enter);
        assert_eq!(harness.state().last, Event::None, "↵ runs nothing when nothing is listed");
    }
}
