//! The saved bindings of `pr_craft_core::keys`, in the egui context. Every screen asks here for the key an action has now, so
//! a rebinding applies on the next frame, and hints and buttons show the binding the reader set.

use std::collections::BTreeMap;
use std::sync::Arc;

use craft::egui::{Event, Id, Key, Modifiers};
use pr_craft_core::keys::{self, Binding, Modifier};

use crate::theme::HasContext;

fn slot() -> Id {
    Id::NULL
}

/// Makes `bindings` the keys every screen reads from now on.
pub fn install(ctx: &craft::egui::Context, bindings: BTreeMap<&'static str, Binding>) {
    ctx.data_mut(|d| d.insert_temp(slot(), Arc::new(bindings)));
}

/// The installed bindings; the defaults before the first frame installs any.
pub(crate) fn installed(c: &impl HasContext) -> Arc<BTreeMap<&'static str, Binding>> {
    c.context().data(|d| d.get_temp(slot())).unwrap_or_else(|| Arc::new(keys::effective(&BTreeMap::new())))
}

/// What `id` is bound to: the installed binding, else its default. `None` for a name that is not an action.
fn binding(c: &impl HasContext, id: &str) -> Option<Binding> {
    installed(c).get(id).cloned()
}

/// The egui key and modifier pattern of a binding. `None` for a key name egui does not know; the tests hold that none of the
/// table's keys is one.
fn parts(binding: &Binding) -> Option<(Key, Modifiers)> {
    let key = Key::from_name(&binding.key)?;
    let mut modifiers = Modifiers::NONE;
    for m in &binding.modifiers {
        match m {
            Modifier::Cmd => modifiers |= Modifiers::COMMAND,
            Modifier::Ctrl => modifiers |= Modifiers::CTRL,
            Modifier::Alt => modifiers |= Modifiers::ALT,
            Modifier::Shift => modifiers |= Modifiers::SHIFT,
        }
    }
    Some((key, modifiers))
}

/// While the Keyboard page waits for a new shortcut, no action fires: the key being pressed is the answer, not a command. The
/// switch holds for the pass it was set in and the next, so a page that stops drawing (Settings closed) cannot leave it on.
#[derive(Clone, Copy)]
struct Capturing(u64);

pub fn set_capturing(ctx: &craft::egui::Context, on: bool) {
    let until = if on { ctx.cumulative_pass_nr() + 1 } else { 0 };
    ctx.data_mut(|d| d.insert_temp(slot(), Capturing(until)));
}

pub fn capturing(c: &impl HasContext) -> bool {
    let now = c.context().cumulative_pass_nr();
    c.context().data(|d| d.get_temp::<Capturing>(slot())).is_some_and(|c| c.0 >= now)
}

/// A press of an action that no key made (the palette ran it): the action and the pass it fires in.
#[derive(Clone, Default)]
struct Triggered {
    id: String,
    at: u64,
}

/// Presses action `id` on the next frame as if its key had gone down with its exact modifiers: [`pressed`], [`fresh`] and
/// [`consume`] report it once, then it is dropped by [`sweep`]. Nothing fires while the Keyboard page is capturing.
pub fn trigger(ctx: &craft::egui::Context, id: &str) {
    let at = ctx.cumulative_pass_nr() + 1;
    ctx.data_mut(|d| d.insert_temp(slot(), Triggered { id: id.to_string(), at }));
    ctx.request_repaint();
}

/// Drops a triggered press that its pass has gone by. Call at the end of every frame.
pub fn sweep(ctx: &craft::egui::Context) {
    let now = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        if d.get_temp::<Triggered>(slot()).is_some_and(|t| t.at <= now) {
            d.remove_temp::<Triggered>(slot());
        }
    });
}

fn virtual_press(c: &impl HasContext, id: &str) -> bool {
    let now = c.context().cumulative_pass_nr();
    !capturing(c) && c.context().data(|d| d.get_temp::<Triggered>(slot())).is_some_and(|t| t.id == id && t.at == now)
}

fn pattern(c: &impl HasContext, id: &str) -> Option<(Key, Modifiers)> {
    if capturing(c) {
        return None;
    }
    parts(&binding(c, id)?)
}

/// The shortcut the reader just pressed, for the Keyboard page: the first key press of this frame that a shortcut may use,
/// with the modifiers it came with. `Err` says why a pressed key cannot be a shortcut. `None` when no key went down.
pub fn captured(c: &impl HasContext) -> Option<Result<Binding, String>> {
    c.context().input_mut(|i| {
        // A modifier going down is the start of a chord, not the answer.
        let at = i.events.iter().position(|e| matches!(e, Event::Key { key, pressed: true, repeat: false, .. } if !is_modifier_key(*key)))?;
        // The key is taken: a focused Rebind button must not read the same ↵ or Space and start another capture.
        match i.events.remove(at) {
            Event::Key { key, modifiers, .. } => Some(from_press(key, modifiers)),
            _ => None,
        }
    })
}

fn is_modifier_key(key: Key) -> bool {
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

fn from_press(key: Key, modifiers: Modifiers) -> Result<Binding, String> {
    let name = match key {
        Key::ArrowUp => "ArrowUp",
        Key::ArrowDown => "ArrowDown",
        Key::ArrowLeft => "ArrowLeft",
        Key::ArrowRight => "ArrowRight",
        other => other.name(),
    };
    let held: Vec<Modifier> = [
        (modifiers.command || modifiers.mac_cmd, Modifier::Cmd),
        (modifiers.ctrl, Modifier::Ctrl),
        (modifiers.alt, Modifier::Alt),
        (modifiers.shift, Modifier::Shift),
    ]
    .into_iter()
    // `?` is typed with ⇧, so the ⇧ is part of the key, not a modifier of the shortcut.
    .filter_map(|(on, m)| on.then_some(m))
    .filter(|m| !(key == Key::Questionmark && *m == Modifier::Shift))
    .collect();
    Binding::new(name, &held).ok_or_else(|| format!("{name} cannot be a shortcut. Press a letter, digit, F key, arrow or punctuation key."))
}

/// How `id`'s binding reads on a button or in a hint: `⌥⌘J`, `⌘↵`, `J`.
pub fn label(c: &impl HasContext, id: &str) -> String {
    binding(c, id).map(|b| b.label()).unwrap_or_default()
}

/// `text` and the binding of `id`, as the app writes a button: `Settings  ⌘,`.
pub fn hint(c: &impl HasContext, text: &str, id: &str) -> String {
    format!("{text}  {}", label(c, id))
}

/// Whether the modifiers held at a key event are exactly the binding's: ⇧, ⌥, ⌃ and ⌘ each on or off as the binding says, so a
/// shortcut on ⌘↵ never fires on ⇧⌘↵, and one on ⌘J never fires on ⌃⌘J. egui's own matching lets extra ⇧ and ⌥ through.
/// ⌘ is `command` or `mac_cmd` (a Mac reports both); ⌃ is `ctrl` itself, as this app runs on macOS only.
fn exact(key: Key, held: Modifiers, wanted: Modifiers) -> bool {
    let shift = held.shift == wanted.shift || key == Key::Questionmark;
    shift && held.alt == wanted.alt && held.ctrl == wanted.ctrl && (held.command || held.mac_cmd) == wanted.command
}

/// Whether the key of `id` went down this frame with exactly its modifiers. Does not take the key from anyone else.
pub fn pressed(c: &impl HasContext, id: &str) -> bool {
    virtual_press(c, id) || event(c, id, |_| true)
}

/// [`pressed`], and takes the key press, so a text field or a screen under this one never sees it.
pub fn consume(c: &impl HasContext, id: &str) -> bool {
    if virtual_press(c, id) {
        c.context().data_mut(|d| d.remove_temp::<Triggered>(slot()));
        return true;
    }
    let Some((key, wanted)) = pattern(c, id) else { return false };
    c.context().input_mut(|i| {
        let before = i.events.len();
        i.events.retain(|e| !matches!(e, Event::Key { key: k, pressed: true, modifiers: m, .. } if *k == key && exact(key, *m, wanted)));
        i.events.len() != before
    })
}

/// [`pressed`] for a press that is not a key repeat: holding the key does not press it again.
pub fn fresh(c: &impl HasContext, id: &str) -> bool {
    virtual_press(c, id) || event(c, id, |repeat| !repeat)
}

/// Whether a press of the key of `id`, with exactly its modifiers, came in this frame and `wanted` takes its repeat flag. The
/// event carries the modifiers held when it happened, so a chord is judged as pressed, not as the window sees it later.
fn event(c: &impl HasContext, id: &str, wanted: impl Fn(bool) -> bool) -> bool {
    pattern(c, id).is_some_and(|(key, mods)| {
        c.context().input(|i| {
            i.events.iter().any(
                |e| matches!(e, Event::Key { key: k, pressed: true, repeat, modifiers: m, .. } if *k == key && wanted(*repeat) && exact(key, *m, mods)),
            )
        })
    })
}

/// The keys that type a character and are bound to an action outside the confirm step. The confirm step is modal, so it takes
/// these before the screens under it can act on them.
pub fn typing_keys(c: &impl HasContext) -> Vec<Key> {
    let map = installed(c);
    let under = |id: &str| keys::action(id).is_some_and(|a| a.scope != keys::Scope::Confirm);
    let mut found: Vec<Key> = map.iter().filter(|(id, b)| under(id) && b.types()).filter_map(|(_, b)| parts(b)).map(|(key, _)| key).collect();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use craft::egui::{Context, RawInput};

    use super::*;

    fn press(key: Key, modifiers: Modifiers) -> RawInput {
        RawInput { events: vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }], ..RawInput::default() }
    }

    #[test]
    fn every_default_key_of_the_table_is_a_key_egui_knows() {
        for action in keys::ACTIONS {
            let binding = action.default_binding();
            assert!(parts(&binding).is_some(), "{} is bound to {:?}, which egui does not name", action.id, binding.key);
        }
    }

    /// Pressing the key and modifiers an action ships with fires that action, and only while the page is not capturing.
    #[test]
    fn every_action_is_reachable_by_pressing_its_binding() {
        let ctx = Context::default();
        for action in keys::ACTIONS {
            let (key, modifiers) = parts(&action.default_binding()).expect("a key egui names");
            let mut seen = false;
            ctx.run_ui(press(key, modifiers), |ui| seen = pressed(ui, action.id)).drop_without_applying_deltas();
            assert!(seen, "{} does not fire on {}", action.id, action.default_binding().label());
        }
        let first = keys::ACTIONS[0];
        let (key, modifiers) = parts(&first.default_binding()).expect("a key egui names");
        set_capturing(&ctx, true);
        let mut seen = true;
        ctx.run_ui(press(key, modifiers), |ui| seen = pressed(ui, first.id)).drop_without_applying_deltas();
        assert!(!seen, "nothing fires while the Keyboard page waits for a new shortcut");
    }

    #[test]
    fn a_pressed_chord_becomes_a_binding_and_an_unusable_key_says_why() {
        let ctx = Context::default();
        let mut got = None;
        let chord = Modifiers { alt: true, mac_cmd: true, command: true, ..Modifiers::NONE };
        ctx.run_ui(press(Key::J, chord), |ui| got = captured(ui)).drop_without_applying_deltas();
        assert_eq!(got, Some(Ok(Binding::new("J", &[Modifier::Alt, Modifier::Cmd]).expect("a key"))));
        ctx.run_ui(press(Key::ArrowUp, Modifiers::NONE), |ui| got = captured(ui)).drop_without_applying_deltas();
        assert_eq!(got, Some(Ok(Binding::new("ArrowUp", &[]).expect("a key"))));
        ctx.run_ui(press(Key::Plus, Modifiers::NONE), |ui| got = captured(ui)).drop_without_applying_deltas();
        assert!(matches!(got, Some(Err(_))));
    }

    #[test]
    fn shift_command_return_does_not_trigger_command_return_but_command_return_still_does() {
        let mac = Modifiers { mac_cmd: true, command: true, ..Modifiers::NONE };
        let with_shift = Modifiers { shift: true, ..mac };
        let with_ctrl = Modifiers { ctrl: true, ..mac };
        let fires = |held: Modifiers| {
            let (mut seen, mut taken) = (false, false);
            let ctx = Context::default();
            ctx.run_ui(press(Key::Enter, held), |ui| seen = pressed(ui, "review.submit")).drop_without_applying_deltas();
            ctx.run_ui(press(Key::Enter, held), |ui| taken = consume(ui, "review.submit")).drop_without_applying_deltas();
            (seen, taken)
        };
        assert_eq!(fires(mac), (true, true));
        assert_eq!(fires(with_shift), (false, false), "⇧⌘↵ is another shortcut");
        assert_eq!(fires(with_ctrl), (false, false), "⌃⌘↵ is another shortcut");
        assert_eq!(fires(Modifiers::ALT), (false, false));
    }

    #[test]
    fn capture_waits_for_the_key_after_a_modifier_and_keeps_control_with_command() {
        let ctx = Context::default();
        let mut got = None;
        let events = vec![
            Event::Key { key: Key::AltLeft, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::ALT },
            Event::Key {
                key: Key::J,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers { ctrl: true, mac_cmd: true, command: true, ..Modifiers::NONE },
            },
        ];
        ctx.run_ui(RawInput { events, ..RawInput::default() }, |ui| got = captured(ui)).drop_without_applying_deltas();
        assert_eq!(got, Some(Ok(Binding::new("J", &[Modifier::Ctrl, Modifier::Cmd]).expect("a key"))));
        let mut seen_after = true;
        let enter = press(Key::Enter, Modifiers::NONE);
        ctx.run_ui(enter, |ui| {
            got = captured(ui);
            seen_after = ui.input(|i| i.key_pressed(Key::Enter));
        })
        .drop_without_applying_deltas();
        assert!(got.is_some() && !seen_after, "the captured ↵ is gone, so a focused Rebind button cannot press itself again");
        let only_modifier = vec![Event::Key { key: Key::ShiftLeft, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::SHIFT }];
        ctx.run_ui(RawInput { events: only_modifier, ..RawInput::default() }, |ui| got = captured(ui)).drop_without_applying_deltas();
        assert_eq!(got, None, "a modifier alone is not an answer");
    }

    #[test]
    fn a_rebound_key_fires_its_action_and_the_old_key_stops() {
        let ctx = Context::default();
        let mut map = keys::effective(&BTreeMap::new());
        map.insert("review.comment", Binding::new("Y", &[Modifier::Alt]).expect("a key"));
        install(&ctx, map);
        let mut seen = (false, false);
        ctx.run_ui(press(Key::Y, Modifiers::ALT), |ui| seen.0 = pressed(ui, "review.comment")).drop_without_applying_deltas();
        ctx.run_ui(press(Key::C, Modifiers::NONE), |ui| seen.1 = pressed(ui, "review.comment")).drop_without_applying_deltas();
        assert!(seen.0, "the new binding fires");
        assert!(!seen.1, "the old key does not");
    }

    #[test]
    fn a_binding_needs_exactly_its_modifiers() {
        let ctx = Context::default();
        let mut seen = (true, true);
        let shifted = Modifiers { shift: true, ..Modifiers::NONE };
        ctx.run_ui(press(Key::J, shifted), |ui| seen.0 = pressed(ui, "inbox.next")).drop_without_applying_deltas();
        ctx.run_ui(press(Key::J, Modifiers::NONE), |ui| seen.1 = pressed(ui, "inbox.next")).drop_without_applying_deltas();
        assert!(!seen.0, "⇧J is not J");
        assert!(seen.1);
    }

    #[test]
    fn a_held_key_is_not_a_fresh_press() {
        let ctx = Context::default();
        let held = Event::Key { key: Key::R, physical_key: None, pressed: true, repeat: true, modifiers: Modifiers::NONE };
        let mut seen = (false, false);
        ctx.run_ui(press(Key::R, Modifiers::NONE), |ui| seen.0 = fresh(ui, "activity.retry")).drop_without_applying_deltas();
        ctx.run_ui(RawInput { events: vec![held], ..RawInput::default() }, |ui| seen.1 = fresh(ui, "activity.retry")).drop_without_applying_deltas();
        assert!(seen.0, "the first press");
        assert!(!seen.1, "key repeat is not a press");
    }

    #[test]
    fn a_real_mac_command_press_matches_the_command_shortcut() {
        // On macOS egui reports ⌘ as both `mac_cmd` and `command`; `==` with `Modifiers::COMMAND` would never match.
        let mac = Modifiers { mac_cmd: true, command: true, ..Modifiers::default() };
        let saw = |held: Modifiers| {
            let mut seen = false;
            Context::default().run_ui(press(Key::Backspace, held), |ui| seen = fresh(ui, "settings.delete_agent")).drop_without_applying_deltas();
            seen
        };
        assert!(saw(mac), "⌘⌫ on a Mac");
        assert!(saw(Modifiers::COMMAND), "Ctrl-Backspace elsewhere");
        assert!(!saw(Modifiers::NONE), "plain Backspace is not ⌘⌫");
        assert!(!saw(Modifiers { shift: true, ..mac }), "⇧⌘⌫ is another shortcut");
    }

    #[test]
    fn a_triggered_action_is_pressed_on_the_next_frame_only() {
        let ctx = Context::default();
        let frame = |ctx: &Context, act: bool| {
            let mut now = (false, false, false);
            ctx.run_ui(RawInput::default(), |ui| {
                now = (pressed(ui, "inbox.next"), fresh(ui, "inbox.next"), pressed(ui, "inbox.prev"));
                if act {
                    trigger(ui.ctx(), "inbox.next");
                }
                sweep(ui.ctx());
            })
            .drop_without_applying_deltas();
            now
        };
        assert_eq!(frame(&ctx, true), (false, false, false), "not in the frame that triggers it");
        assert_eq!(frame(&ctx, false), (true, true, false), "the next frame, and only the action that was named");
        assert_eq!(frame(&ctx, false), (false, false, false), "gone the frame after");
        trigger(&ctx, "inbox.next");
        set_capturing(&ctx, true);
        assert_eq!(frame(&ctx, false), (false, false, false), "nothing fires while a new shortcut is being captured");
    }

    #[test]
    fn a_hint_shows_the_current_binding() {
        let ctx = Context::default();
        let mut map = keys::effective(&BTreeMap::new());
        map.insert("app.settings", Binding::new("F2", &[]).expect("a key"));
        install(&ctx, map);
        assert_eq!(hint(&ctx, "Settings", "app.settings"), "Settings  F2");
    }
}
