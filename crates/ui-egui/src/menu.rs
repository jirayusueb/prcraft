//! App-drawn context menus (DESIGN.md → Context menus): raised paper, radius 10, 5 px inset, 26 px items with the shortcut
//! right-aligned in tertiary, the row under the pointer or the keyboard in graphite-inverse, a 12 px check slot, and an optional
//! header naming what was clicked. A screen opens one with [`open`] (right-click, the context-menu key ⇧F10) and reads the
//! choice with [`take`]; every item it offers repeats an action that has its own key or button (AGENTS.md rule 14).
//!
//! The menu is modal for the keyboard: ↑ ↓ ↵ and Esc come from the registry (`menu.*`, `popover.close`), every other key is
//! swallowed while it is open, and closing it gives focus back to the control it was opened from (rule 21).

use craft::egui::accesskit::Live;
use craft::egui::epaint::Shadow;
use craft::egui::{
    self, Area, Color32, Context, CornerRadius, Event, EventFilter, FontId, Id, Order, Pos2, Rect, Role, Sense, Stroke, Ui, Vec2, WidgetInfo, pos2,
    vec2,
};

use crate::{keymap, theme};

const ITEM_HEIGHT: f32 = 26.0;
/// The room before a label: the row's padding, the 12 px check slot and the gap after it.
const LABEL_LEFT: f32 = 8.0 + 12.0 + 6.0;
const MIN_WIDTH: f32 = 220.0;

/// One choice of a menu.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    /// What [`take`] returns when it is chosen.
    pub id: &'static str,
    pub label: String,
    /// The shortcut of the action it repeats, as it reads on a button: `⇧⌘C`. Empty when the action has no key.
    pub key: String,
    /// Why it cannot be chosen now; it takes the shortcut's place and is part of the item's name.
    pub unavailable: Option<String>,
    pub checked: bool,
}

impl Item {
    pub(crate) fn new(id: &'static str, label: impl Into<String>) -> Self {
        Item { id, label: label.into(), key: String::new(), unavailable: None, checked: false }
    }

    pub(crate) fn key(mut self, key: impl Into<String>) -> Self {
        self.key = key.into();
        self
    }

    pub(crate) fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }

    pub(crate) fn unavailable(mut self, why: impl Into<String>) -> Self {
        self.unavailable = Some(why.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Entry {
    Item(Item),
    /// A hairline between groups.
    Rule,
}

/// A menu to open.
#[derive(Clone, Debug)]
pub(crate) struct Spec {
    /// The screen part that reads the choice with [`take`].
    pub owner: &'static str,
    /// What the menu belongs to (a change's URL, a folder's path); [`take`] hands it back with the choice.
    pub target: String,
    /// A tertiary line naming what was clicked: `Lines 42–44 · invoicer.go`.
    pub header: Option<String>,
    pub at: Pos2,
    pub entries: Vec<Entry>,
    /// The control that gets focus back when the menu closes.
    pub restore: Option<Id>,
}

#[derive(Clone)]
struct State {
    spec: Spec,
    /// Index into `spec.entries` of the item that is on.
    selected: usize,
    /// Move focus to the selected item on this frame.
    focus: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chosen {
    pub target: String,
    pub item: &'static str,
}

fn state_id() -> Id {
    Id::NULL.with("context-menu-state")
}

fn chosen_id() -> Id {
    Id::NULL.with("context-menu-chosen")
}

fn item_id(index: usize) -> Id {
    Id::NULL.with(("context-menu-item", index))
}

fn state(ctx: &Context) -> Option<State> {
    ctx.data(|d| d.get_temp::<Option<State>>(state_id())).flatten()
}

fn available(entry: &Entry) -> bool {
    matches!(entry, Entry::Item(item) if item.unavailable.is_none())
}

/// Opens `spec`, closing any menu that was open. The first item that can be chosen is on.
pub(crate) fn open(ctx: &Context, mut spec: Spec) {
    // Focus goes back to the control that had it when the menu opened, unless the opener names another.
    spec.restore = spec.restore.or_else(|| ctx.memory(|m| m.focused()));
    let selected = spec.entries.iter().position(available).unwrap_or(0);
    ctx.data_mut(|d| d.insert_temp(state_id(), Some(State { spec, selected, focus: true })));
    ctx.request_repaint();
}

fn rect_id() -> Id {
    Id::NULL.with("context-menu-rect")
}

/// Where the open menu is drawn, for a popover that closes on a press away from it: a press inside a menu is not away.
pub(crate) fn rect(ctx: &Context) -> Option<Rect> {
    ctx.data(|d| d.get_temp::<Option<Rect>>(rect_id())).flatten()
}

pub(crate) fn close(ctx: &Context) {
    ctx.data_mut(|d| d.insert_temp(rect_id(), None::<Rect>));
    ctx.data_mut(|d| d.insert_temp(state_id(), None::<State>));
}

/// The choice made this frame in a menu `owner` opened, once.
pub(crate) fn take(ctx: &Context, owner: &'static str) -> Option<Chosen> {
    let found = ctx.data(|d| d.get_temp::<Option<(&'static str, Chosen)>>(chosen_id())).flatten()?;
    if found.0 != owner {
        return None;
    }
    ctx.data_mut(|d| d.insert_temp(chosen_id(), None::<(&'static str, Chosen)>));
    Some(found.1)
}

/// The next item that can be chosen from `from`, wrapping; `from` itself when there is no other.
fn step(entries: &[Entry], from: usize, forward: bool) -> usize {
    let n = entries.len();
    (1..=n).map(|k| if forward { (from + k) % n } else { (from + n - k % n) % n }).find(|&i| available(&entries[i])).unwrap_or(from)
}

fn sans(ui: &Ui, px: f32) -> FontId {
    FontId::proportional(px * theme::sz(ui).body / 13.5)
}

fn width_of(ui: &Ui, text: &str, px: f32) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), sans(ui, px), Color32::PLACEHOLDER).size().x
}

/// Draws the open menu and takes its keys. Call first in every frame, before any screen reads a key.
pub(crate) fn show(ctx: &Context) {
    let Some(mut st) = state(ctx) else { return };
    let mut leave = keymap::consume(ctx, "popover.close");
    let mut pick = None;
    let mut moved_by_key = false;
    if keymap::consume(ctx, "menu.next") {
        st.selected = step(&st.spec.entries, st.selected, true);
        moved_by_key = true;
    }
    if keymap::consume(ctx, "menu.prev") {
        st.selected = step(&st.spec.entries, st.selected, false);
        moved_by_key = true;
    }
    if keymap::consume(ctx, "menu.run") {
        pick = Some(st.selected);
    }
    // Every other key belongs to the menu while it is open: a screen under it must not move.
    ctx.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Key { .. })));
    let p = theme::pal(ctx);
    let moved = ctx.input(|i| i.pointer.delta() != Vec2::ZERO);
    let (mut hover, mut click) = (None, None);
    let frame = egui::Frame::new()
        .fill(p.raised)
        .stroke(Stroke::new(theme::sz(ctx).hairline, p.border_strong))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(5)
        .shadow(Shadow { offset: [0, 12], blur: 32, spread: 0, color: Color32::from_black_alpha(36) });
    let spec = st.spec.clone();
    let shown = Area::new(Id::NULL.with("context-menu")).order(Order::Foreground).fixed_pos(spec.at).constrain(true).show(ctx, |ui| {
        frame.show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let items = || spec.entries.iter().filter_map(|e| if let Entry::Item(item) = e { Some(item) } else { None });
            let right = |item: &Item| item.unavailable.as_deref().unwrap_or(&item.key).to_string();
            let label_room = items().map(|i| width_of(ui, &i.label, 13.0)).fold(0.0, f32::max);
            let right_room = items().map(|i| width_of(ui, &right(i), 12.0)).fold(0.0, f32::max);
            let header_room = spec.header.as_deref().map_or(0.0, |h| width_of(ui, h, 11.5) + 16.0);
            let width = (LABEL_LEFT + label_room + 28.0 + right_room + 8.0).max(header_room).max(MIN_WIDTH);
            if let Some(header) = &spec.header {
                let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
                let galley = ui.painter().layout_no_wrap(header.clone(), sans(ui, 11.5), p.text_tertiary);
                ui.painter().galley(pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0), galley, Color32::PLACEHOLDER);
            }
            for (i, entry) in spec.entries.iter().enumerate() {
                match entry {
                    Entry::Rule => {
                        let (rect, _) = ui.allocate_exact_size(vec2(width, 9.0), Sense::hover());
                        ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(theme::sz(ui).hairline, p.border));
                    }
                    Entry::Item(item) => {
                        let enabled = item.unavailable.is_none();
                        let (rect, _) = ui.allocate_exact_size(vec2(width, ITEM_HEIGHT), Sense::hover());
                        let response = ui.interact(rect, item_id(i), if enabled { Sense::click() } else { Sense::hover() });
                        ui.memory_mut(|m| {
                            m.set_focus_lock_filter(
                                response.id,
                                EventFilter { horizontal_arrows: true, vertical_arrows: true, tab: true, escape: true },
                            );
                        });
                        if enabled && response.hovered() && moved {
                            hover = Some(i);
                        }
                        if enabled && response.clicked() {
                            click = Some(i);
                        }
                        let on = enabled && i == st.selected;
                        if on {
                            let fill = if response.is_pointer_button_down_on() { p.inverse_pressed } else { p.inverse };
                            ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
                        }
                        let (ink, quiet) = match (on, enabled) {
                            (true, _) => (p.on_inverse, p.on_inverse_quiet),
                            (false, true) => (p.graphite, p.text_tertiary),
                            (false, false) => (p.text_tertiary, p.text_tertiary),
                        };
                        if item.checked {
                            let at = pos2(rect.left() + 8.0 + 6.0, rect.center().y);
                            let stroke = Stroke::new(1.6, ink);
                            ui.painter().line_segment([at + vec2(-4.0, 0.0), at + vec2(-1.0, 3.0)], stroke);
                            ui.painter().line_segment([at + vec2(-1.0, 3.0), at + vec2(4.5, -3.5)], stroke);
                        }
                        let label = ui.painter().layout_no_wrap(item.label.clone(), sans(ui, 13.0), ink);
                        ui.painter().galley(pos2(rect.left() + LABEL_LEFT, rect.center().y - label.size().y / 2.0), label, Color32::PLACEHOLDER);
                        let side = right(item);
                        if !side.is_empty() {
                            let galley = ui.painter().layout_no_wrap(side, sans(ui, 12.0), quiet);
                            let at = pos2(rect.right() - 8.0 - galley.size().x, rect.center().y - galley.size().y / 2.0);
                            ui.painter().galley(at, galley, Color32::PLACEHOLDER);
                        }
                        let mut name = item.label.clone();
                        if let Some(why) = &item.unavailable {
                            name = format!("{name}, unavailable: {why}");
                        } else if !item.key.is_empty() {
                            name = format!("{name}  {}", item.key);
                        }
                        if item.checked {
                            response.widget_info(|| WidgetInfo::selected(Role::MenuItemCheckBox, enabled, true, name.clone()));
                        } else {
                            response.widget_info(|| WidgetInfo::labeled(Role::MenuItem, enabled, name.clone()));
                        }
                        if let Some(why) = &item.unavailable {
                            response.on_hover_text(why);
                        }
                    }
                }
            }
        })
    });
    // The menu is named and announced when it opens (rule 21): a role, the thing it belongs to, and a live region.
    let named = match &spec.header {
        Some(header) => format!("Context menu, {header}"),
        None => "Context menu".to_string(),
    };
    ctx.accesskit_node_builder(shown.response.id, |node| {
        node.set_role(Role::Menu);
        node.set_label(named);
        node.set_live(Live::Polite);
    });
    if let Some(i) = hover {
        st.selected = i;
    }
    if let Some(i) = click {
        pick = Some(i);
    }
    ctx.data_mut(|d| d.insert_temp(rect_id(), Some(shown.response.rect)));
    let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|at| !shown.response.rect.contains(at)));
    let chosen = pick.and_then(|i| match spec.entries.get(i) {
        Some(Entry::Item(item)) if item.unavailable.is_none() => Some(item.id),
        _ => None,
    });
    if let Some(item) = chosen {
        ctx.data_mut(|d| d.insert_temp(chosen_id(), Some((spec.owner, Chosen { target: spec.target.clone(), item }))));
        leave = true;
    }
    if leave || outside {
        close(ctx);
        // A press elsewhere takes focus where it landed.
        if let (true, Some(id)) = (!outside || leave, spec.restore) {
            ctx.memory_mut(|m| m.request_focus(id));
        }
        ctx.request_repaint();
        return;
    }
    if st.focus || moved_by_key || hover.is_some() {
        ctx.memory_mut(|m| m.request_focus(item_id(st.selected)));
        st.focus = false;
    }
    ctx.data_mut(|d| d.insert_temp(state_id(), Some(st)));
}

/// Where a menu opened from the keyboard goes: just under the left edge of the control, inside the window.
pub(crate) fn below(rect: Rect) -> Pos2 {
    rect.left_bottom() + vec2(8.0, 2.0)
}

#[cfg(test)]
mod tests {
    use craft::egui::{Key, Modifiers, RawInput};

    use super::*;

    fn press(key: Key) -> RawInput {
        RawInput {
            events: vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }],
            ..RawInput::default()
        }
    }

    fn run(ctx: &Context, input: RawInput) {
        ctx.run_ui(input, |ui| show(ui.ctx())).drop_without_applying_deltas();
    }

    fn spec() -> Spec {
        let item = |id: &'static str| Entry::Item(Item::new(id, id));
        Spec {
            owner: "test",
            target: "row-1".into(),
            header: Some("Lines 1–2 · a.go".into()),
            at: pos2(40.0, 40.0),
            entries: vec![item("first"), Entry::Item(Item::new("blocked", "Blocked").unavailable("Not built yet")), Entry::Rule, item("last")],
            restore: None,
        }
    }

    #[test]
    fn arrows_skip_what_cannot_be_chosen_and_enter_chooses_with_its_target() {
        let ctx = Context::default();
        open(&ctx, spec());
        run(&ctx, RawInput::default());
        assert!(state(&ctx).is_some());
        run(&ctx, press(Key::ArrowDown));
        run(&ctx, press(Key::Enter));
        assert_eq!(take(&ctx, "test"), Some(Chosen { target: "row-1".into(), item: "last" }), "↓ passed the unavailable item");
        assert!(state(&ctx).is_none(), "choosing closes the menu");
        assert_eq!(take(&ctx, "test"), None, "a choice is read once");
    }

    #[test]
    fn escape_closes_without_choosing_and_other_keys_are_swallowed() {
        let ctx = Context::default();
        open(&ctx, spec());
        run(&ctx, RawInput::default());
        let mut seen = true;
        ctx.run_ui(press(Key::J), |ui| {
            show(ui.ctx());
            seen = ui.input(|i| i.key_pressed(Key::J));
        })
        .drop_without_applying_deltas();
        assert!(!seen, "a screen under an open menu never sees a key");
        run(&ctx, press(Key::Escape));
        assert!(state(&ctx).is_none());
        assert_eq!(take(&ctx, "test"), None);
    }

    #[test]
    fn closing_the_menu_gives_focus_back_to_the_control_that_had_it_even_when_the_opener_named_none() {
        let ctx = Context::default();
        let before = Id::NULL.with("control");
        let focused = |ctx: &Context| ctx.memory(|m| m.focused());
        let frame = |ctx: &Context, input: RawInput| {
            ctx.run_ui(input, |ui| {
                ui.interact(Rect::from_min_size(pos2(0.0, 0.0), vec2(20.0, 20.0)), before, Sense::click());
                show(ui.ctx());
            })
            .drop_without_applying_deltas();
        };
        frame(&ctx, RawInput::default());
        ctx.memory_mut(|m| m.request_focus(before));
        frame(&ctx, RawInput::default());
        assert_eq!(focused(&ctx), Some(before));
        open(&ctx, spec());
        frame(&ctx, RawInput::default());
        frame(&ctx, RawInput::default());
        assert_ne!(focused(&ctx), Some(before), "the menu has focus on its own item while it is open");
        frame(&ctx, press(Key::Escape));
        frame(&ctx, RawInput::default());
        assert_eq!(focused(&ctx), Some(before), "Esc returns focus to where it was");
    }

    #[test]
    fn the_open_menu_reports_where_it_is_so_a_press_inside_it_is_not_away_from_a_popover() {
        let ctx = Context::default();
        assert_eq!(rect(&ctx), None);
        open(&ctx, spec());
        run(&ctx, RawInput::default());
        let shown = rect(&ctx).expect("an open menu has a place");
        assert!(shown.contains(pos2(60.0, 60.0)), "{shown:?} holds the first item");
        run(&ctx, press(Key::Escape));
        assert_eq!(rect(&ctx), None, "a closed menu is nowhere");
    }

    #[test]
    fn a_choice_goes_only_to_the_screen_that_opened_the_menu() {
        let ctx = Context::default();
        open(&ctx, spec());
        run(&ctx, RawInput::default());
        run(&ctx, press(Key::Enter));
        assert_eq!(take(&ctx, "other"), None);
        assert_eq!(take(&ctx, "test").map(|c| c.item), Some("first"));
    }

    #[test]
    fn the_menu_is_named_and_its_items_are_named_to_assistive_technology() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::{By, NodeT, Queryable};
        let mut harness = Harness::builder().with_size(vec2(400.0, 300.0)).build_ui(|ui| {
            crate::prefs::glyphs_for_tests(ui.ctx());
            ui.label("under");
            show(ui.ctx());
        });
        open(&harness.ctx, spec());
        harness.run();
        assert!(harness.query_by_label_contains("Context menu, Lines 1–2 · a.go").is_some(), "the menu names what it belongs to");
        assert!(
            harness
                .query_all(By::new().role(Role::MenuItem))
                .any(|n| n.accesskit_node().label().is_some_and(|l| l.contains("unavailable: Not built yet")))
        );
    }
}
