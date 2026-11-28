//! Every keyboard shortcut PR Craft has: one table of actions with their default bindings, the bindings a user may
//! have changed, and the rules that flag a binding that collides with another action, macOS or typing. No I/O.
//!
//! A binding that collides is judged, not just marked: [`Severity::Refused`] bindings are not saved (macOS or the text
//! field would take the key, or two actions in one place would fire together); [`Severity::Warning`] bindings are saved
//! and shown with their reason (another app uses the same chord, but only while that app is in front).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::extra::Extra;

/// A modifier key. Declared in the order macOS writes them: ⌃ ⌥ ⇧ ⌘.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Cmd,
}

impl Modifier {
    pub fn symbol(self) -> &'static str {
        match self {
            Modifier::Ctrl => "⌃",
            Modifier::Alt => "⌥",
            Modifier::Shift => "⇧",
            Modifier::Cmd => "⌘",
        }
    }
}

/// The keys a shortcut may use, as egui names them, each with how it is written on a shortcut.
const KEYS: &[(&str, &str)] = &[
    ("A", "A"),
    ("B", "B"),
    ("C", "C"),
    ("D", "D"),
    ("E", "E"),
    ("F", "F"),
    ("G", "G"),
    ("H", "H"),
    ("I", "I"),
    ("J", "J"),
    ("K", "K"),
    ("L", "L"),
    ("M", "M"),
    ("N", "N"),
    ("O", "O"),
    ("P", "P"),
    ("Q", "Q"),
    ("R", "R"),
    ("S", "S"),
    ("T", "T"),
    ("U", "U"),
    ("V", "V"),
    ("W", "W"),
    ("X", "X"),
    ("Y", "Y"),
    ("Z", "Z"),
    ("0", "0"),
    ("1", "1"),
    ("2", "2"),
    ("3", "3"),
    ("4", "4"),
    ("5", "5"),
    ("6", "6"),
    ("7", "7"),
    ("8", "8"),
    ("9", "9"),
    ("F1", "F1"),
    ("F2", "F2"),
    ("F3", "F3"),
    ("F4", "F4"),
    ("F5", "F5"),
    ("F6", "F6"),
    ("F7", "F7"),
    ("F8", "F8"),
    ("F9", "F9"),
    ("F10", "F10"),
    ("F11", "F11"),
    ("F12", "F12"),
    ("Enter", "↵"),
    ("Escape", "Esc"),
    ("Tab", "Tab"),
    ("Space", "Space"),
    ("Backspace", "⌫"),
    ("Delete", "⌦"),
    ("ArrowUp", "↑"),
    ("ArrowDown", "↓"),
    ("ArrowLeft", "←"),
    ("ArrowRight", "→"),
    ("Comma", ","),
    ("Period", "."),
    ("Slash", "/"),
    ("Backslash", "\\"),
    ("Semicolon", ";"),
    ("Quote", "'"),
    ("Backtick", "`"),
    ("Minus", "-"),
    ("Equals", "="),
    ("OpenBracket", "["),
    ("CloseBracket", "]"),
];

/// Every character a [`Binding::label`] can hold: the four modifier symbols and each key's label. A font that draws all of
/// them can draw any shortcut.
pub fn label_glyphs() -> std::collections::BTreeSet<char> {
    [Modifier::Cmd, Modifier::Ctrl, Modifier::Alt, Modifier::Shift]
        .iter()
        .flat_map(|m| m.symbol().chars())
        .chain(KEYS.iter().flat_map(|(_, label)| label.chars()))
        .collect()
}

/// Keys that type a character into a text field. Esc, ↵, Tab, the arrows, F keys and ⌫ do not.
fn types_a_character(key: &str) -> bool {
    !(matches!(key, "Enter" | "Escape" | "Tab" | "Backspace" | "Delete" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight")
        || key.len() > 1 && key.starts_with('F') && key[1..].bytes().all(|b| b.is_ascii_digit()))
}

/// A key and the modifiers held with it. `modifiers` is kept sorted and without repeats, so equal bindings compare equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Binding {
    pub key: String,
    #[serde(default)]
    pub modifiers: Vec<Modifier>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Binding {
    /// `key` is an egui key name from the table (`J`, `Enter`, `Comma`); `None` for any other word.
    pub fn new(key: &str, modifiers: &[Modifier]) -> Option<Binding> {
        let (name, _) = KEYS.iter().find(|(name, _)| name.eq_ignore_ascii_case(key))?;
        let mut modifiers = modifiers.to_vec();
        modifiers.sort();
        modifiers.dedup();
        Some(Binding { key: (*name).to_string(), modifiers, extra: Extra::default() })
    }

    pub fn has(&self, modifier: Modifier) -> bool {
        self.modifiers.contains(&modifier)
    }

    /// How it reads on a button or a hint: `⌥⌘J`, `⌘↵`, `J`.
    pub fn label(&self) -> String {
        let key = KEYS.iter().find(|(name, _)| *name == self.key).map_or(self.key.as_str(), |(_, label)| label);
        self.modifiers.iter().map(|m| m.symbol()).chain(std::iter::once(key)).collect()
    }

    /// Whether this binding types a character into a text field when pressed there: a character key with no ⌘, ⌃ or ⌥.
    pub fn types(&self) -> bool {
        types_a_character(&self.key) && !(self.has(Modifier::Cmd) || self.has(Modifier::Ctrl) || self.has(Modifier::Alt))
    }
}

/// Where a shortcut works. A shortcut of [`Scope::Everywhere`] works on every screen, so it may not share a binding with
/// any other action; the others share bindings with the other scopes, since only one of those is on screen at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Everywhere,
    Inbox,
    Review,
    Sheet,
    Share,
    Confirm,
    Popover,
    Palette,
    Activity,
    Settings,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Everywhere => "Everywhere",
            Scope::Inbox => "Inbox",
            Scope::Review => "Review",
            Scope::Sheet => "AI review sheet",
            Scope::Share => "Share sheet",
            Scope::Confirm => "Confirm step",
            Scope::Popover => "Popover",
            Scope::Palette => "Command palette",
            Scope::Activity => "Activity",
            Scope::Settings => "Settings",
        }
    }

    /// Whether the two work on screen at once, so one key cannot do two things. An overlay (a sheet, the share sheet, the
    /// confirm step, the popover) sits over every screen, so it overlaps all of them.
    fn overlaps(self, other: Scope) -> bool {
        self == other || self.is_overlay() || other.is_overlay() || self == Scope::Everywhere || other == Scope::Everywhere
    }

    fn is_overlay(self) -> bool {
        matches!(self, Scope::Sheet | Scope::Share | Scope::Confirm | Scope::Popover | Scope::Palette)
    }
}

/// One thing a key does.
#[derive(Debug, Clone, Copy)]
pub struct Action {
    /// `scope.name`; never changes, it is what a saved binding is filed under.
    pub id: &'static str,
    pub scope: Scope,
    pub label: &'static str,
    key: &'static str,
    modifiers: &'static [Modifier],
    /// The action also fires while a text field has focus, so a binding that types a character would swallow typing.
    pub works_while_typing: bool,
}

impl Action {
    /// The binding the app ships with. A default always names a key of the table; the tests hold that.
    pub fn default_binding(&self) -> Binding {
        let mut modifiers = self.modifiers.to_vec();
        modifiers.sort();
        Binding { key: self.key.to_string(), modifiers, extra: Extra::default() }
    }
}

use Modifier::{Alt, Cmd, Shift};

const fn act(id: &'static str, scope: Scope, label: &'static str, key: &'static str, modifiers: &'static [Modifier]) -> Action {
    Action { id, scope, label, key, modifiers, works_while_typing: false }
}

/// An action that fires while a text field has focus.
const fn typing(id: &'static str, scope: Scope, label: &'static str, key: &'static str, modifiers: &'static [Modifier]) -> Action {
    Action { id, scope, label, key, modifiers, works_while_typing: true }
}

/// Every shortcut, with its default. The screens read their keys and hints from here, through the saved bindings.
pub const ACTIONS: &[Action] = &[
    typing("app.settings", Scope::Everywhere, "Open Settings", "Comma", &[Cmd]),
    typing("app.activity", Scope::Everywhere, "Open Activity", "J", &[Alt, Cmd]),
    typing("app.palette", Scope::Everywhere, "Open the command palette", "K", &[Cmd]),
    act("app.context_menu", Scope::Everywhere, "Open the context menu", "F10", &[Shift]),
    act("inbox.queue", Scope::Inbox, "Review requested", "Q", &[]),
    act("inbox.back_to_you", Scope::Inbox, "Back to you", "B", &[]),
    act("inbox.mine", Scope::Inbox, "Your changes", "Y", &[]),
    act("inbox.notifications", Scope::Inbox, "Notifications", "N", &[]),
    act("inbox.local_drafts", Scope::Inbox, "Local drafts", "D", &[]),
    act("inbox.next", Scope::Inbox, "Next row", "J", &[]),
    act("inbox.prev", Scope::Inbox, "Previous row", "K", &[]),
    act("inbox.open", Scope::Inbox, "Open the change", "Enter", &[]),
    act("inbox.open_forge", Scope::Inbox, "Open the change on its forge", "Enter", &[Alt]),
    act("inbox.copy_link", Scope::Inbox, "Copy the change's link", "C", &[Shift, Cmd]),
    act("mine.primary", Scope::Inbox, "Do the main action on your change: ready for review, merge, or ask again", "Enter", &[Cmd]),
    act("inbox.pause", Scope::Inbox, "Pause or resume notifications", "P", &[]),
    act("inbox.mark_seen", Scope::Inbox, "Mark every notification seen", "M", &[]),
    typing("review.back", Scope::Review, "Back to the inbox", "Escape", &[]),
    typing("review.submit", Scope::Review, "Submit the review", "Enter", &[Cmd]),
    typing("review.share", Scope::Review, "Share as image", "I", &[Shift, Cmd]),
    typing("review.ai_review", Scope::Review, "AI review", "A", &[Alt, Cmd]),
    typing("review.stop_runs", Scope::Review, "Stop this change's AI runs", "Period", &[Cmd]),
    act("review.keep_ai", Scope::Review, "Keep the AI draft", "A", &[]),
    act("review.keep_ai_edit", Scope::Review, "Keep the AI draft and edit it", "A", &[Shift]),
    act("review.discard_ai", Scope::Review, "Discard the AI draft", "X", &[]),
    act("review.next_ai", Scope::Review, "Next AI draft", "ArrowDown", &[Alt]),
    act("review.prev_ai", Scope::Review, "Previous AI draft", "ArrowUp", &[Alt]),
    act("review.whole_stack", Scope::Review, "This change or the whole stack", "S", &[]),
    act("review.since_review", Scope::Review, "Changes since your review", "R", &[]),
    act("review.hand_off", Scope::Review, "Hand off the review", "H", &[]),
    act("review.next_hunk", Scope::Review, "Next hunk", "J", &[]),
    act("review.prev_hunk", Scope::Review, "Previous hunk", "K", &[]),
    act("review.next_file", Scope::Review, "Next file", "N", &[]),
    act("review.prev_file", Scope::Review, "Previous file", "P", &[]),
    act("review.next_draft", Scope::Review, "Next draft", "CloseBracket", &[]),
    act("review.prev_draft", Scope::Review, "Previous draft", "OpenBracket", &[]),
    act("review.next_outdated", Scope::Review, "Next draft on a changed line", "D", &[Shift]),
    act("review.edit_draft", Scope::Review, "Edit the draft", "E", &[]),
    act("review.comment", Scope::Review, "Comment on the selected lines", "C", &[]),
    act("review.viewed", Scope::Review, "Mark the file viewed", "V", &[]),
    act("review.viewed_folder", Scope::Review, "Mark every file of the folder viewed", "V", &[Shift]),
    act("review.fold_folder", Scope::Review, "Fold the folder", "ArrowLeft", &[]),
    act("review.unfold_folder", Scope::Review, "Open the folder", "ArrowRight", &[]),
    act("review.fold_all", Scope::Review, "Fold every folder", "ArrowLeft", &[Alt]),
    act("review.copy_path", Scope::Review, "Copy the folder's path", "C", &[Alt, Cmd]),
    act("review.toggle_source", Scope::Review, "Rendered or source view", "S", &[Shift]),
    act("review.image_two_up", Scope::Review, "Image: 2-up", "1", &[]),
    act("review.image_swipe", Scope::Review, "Image: Swipe", "2", &[]),
    act("review.image_onion", Scope::Review, "Image: Onion", "3", &[]),
    act("review.image_difference", Scope::Review, "Image: Difference", "4", &[]),
    act("review.layout", Scope::Review, "Unified or split view", "L", &[]),
    act("review.hide_whitespace", Scope::Review, "Hide whitespace changes", "W", &[]),
    act("review.word_highlight", Scope::Review, "Highlight changed words", "W", &[Shift]),
    act("review.wrap", Scope::Review, "Wrap long lines", "Z", &[Alt]),
    act("review.diff_options", Scope::Review, "Diff options", "O", &[]),
    act("review.expand_above", Scope::Review, "Show 20 more lines above this hunk", "K", &[Shift]),
    act("review.expand_below", Scope::Review, "Show 20 more lines below this hunk", "J", &[Shift]),
    act("review.expand_all", Scope::Review, "Expand all lines around this hunk", "E", &[Alt]),
    act("review.next_thread", Scope::Review, "Next thread", "T", &[]),
    act("review.prev_thread", Scope::Review, "Previous thread", "T", &[Shift]),
    act("review.reply_thread", Scope::Review, "Reply to the thread", "R", &[Alt]),
    act("review.resolve_thread", Scope::Review, "Resolve or reopen the thread", "R", &[Shift]),
    act("review.commits", Scope::Review, "Pick commits to review", "C", &[Shift]),
    act("review.prev_commit", Scope::Review, "Previous commit", "Comma", &[]),
    act("review.next_commit", Scope::Review, "Next commit", "Period", &[]),
    typing("review.open_locally", Scope::Review, "Open this change locally", "O", &[Cmd]),
    typing("review.worktree", Scope::Review, "Check out in a worktree", "O", &[Shift, Cmd]),
    typing("sheet.start", Scope::Sheet, "Start the AI review", "Enter", &[Cmd]),
    typing("sheet.close", Scope::Sheet, "Close the sheet", "Escape", &[]),
    act("sheet.preview", Scope::Sheet, "Preview what is sent", "P", &[]),
    act("sheet.next", Scope::Sheet, "Next agent", "J", &[]),
    act("sheet.prev", Scope::Sheet, "Previous agent", "K", &[]),
    typing("share.save", Scope::Share, "Save PNG", "S", &[Cmd]),
    typing("share.close", Scope::Share, "Close the share sheet", "Escape", &[]),
    typing("confirm.start", Scope::Confirm, "Start the paid run", "Enter", &[Cmd]),
    typing("confirm.cancel", Scope::Confirm, "Cancel the paid run", "Escape", &[]),
    typing("popover.close", Scope::Popover, "Close the popover", "Escape", &[]),
    act("menu.next", Scope::Popover, "Next menu item", "ArrowDown", &[]),
    act("menu.prev", Scope::Popover, "Previous menu item", "ArrowUp", &[]),
    act("menu.run", Scope::Popover, "Choose the menu item", "Enter", &[]),
    typing("palette.close", Scope::Palette, "Close the palette", "Escape", &[]),
    act("palette.run", Scope::Palette, "Run the selected action", "Enter", &[]),
    act("palette.next", Scope::Palette, "Next result", "ArrowDown", &[]),
    act("palette.prev", Scope::Palette, "Previous result", "ArrowUp", &[]),
    typing("palette.queue_1", Scope::Palette, "Jump to Review requested", "1", &[Cmd]),
    typing("palette.queue_2", Scope::Palette, "Jump to Back to you", "2", &[Cmd]),
    typing("palette.queue_3", Scope::Palette, "Jump to Notifications", "3", &[Cmd]),
    act("activity.close", Scope::Activity, "Close Activity", "Escape", &[]),
    typing("review.close_run", Scope::Review, "Close the run's details", "Escape", &[]),
    typing("activity.stop_all", Scope::Activity, "Stop every run", "Period", &[Cmd]),
    act("activity.filter_1", Scope::Activity, "Filter: first", "1", &[]),
    act("activity.filter_2", Scope::Activity, "Filter: second", "2", &[]),
    act("activity.filter_3", Scope::Activity, "Filter: third", "3", &[]),
    act("activity.filter_4", Scope::Activity, "Filter: fourth", "4", &[]),
    act("activity.filter_5", Scope::Activity, "Filter: fifth", "5", &[]),
    act("activity.next", Scope::Activity, "Next run", "J", &[]),
    act("activity.prev", Scope::Activity, "Previous run", "K", &[]),
    act("activity.open", Scope::Activity, "Open the run's change", "Enter", &[]),
    act("activity.stop", Scope::Activity, "Stop the run", "S", &[]),
    act("activity.kill", Scope::Activity, "Kill the run's process", "X", &[]),
    act("activity.retry", Scope::Activity, "Retry the run", "R", &[]),
    act("settings.close", Scope::Settings, "Leave Settings", "Escape", &[]),
    act("settings.page.engine", Scope::Settings, "Page: AI engine", "1", &[]),
    act("settings.page.sees", Scope::Settings, "Page: Privacy", "2", &[]),
    act("settings.page.agents", Scope::Settings, "Page: Review agents", "3", &[]),
    act("settings.page.repos", Scope::Settings, "Page: Repositories", "4", &[]),
    act("settings.page.appearance", Scope::Settings, "Page: Appearance", "5", &[]),
    act("settings.page.accessibility", Scope::Settings, "Page: Accessibility", "6", &[]),
    act("settings.page.keyboard", Scope::Settings, "Page: Keyboard", "7", &[]),
    act("settings.page.sync", Scope::Settings, "Page: Offline and sync", "8", &[]),
    act("settings.page.permissions", Scope::Settings, "Page: Permissions", "9", &[]),
    act("settings.page.about", Scope::Settings, "Page: About", "0", &[]),
    typing("settings.new_agent", Scope::Settings, "New review agent", "N", &[Cmd]),
    typing("settings.save_agent", Scope::Settings, "Save the agent", "S", &[Cmd]),
    act("settings.delete_agent", Scope::Settings, "Delete the agent", "Backspace", &[Cmd]),
    act("settings.next", Scope::Settings, "Next in the list", "J", &[]),
    act("settings.prev", Scope::Settings, "Previous in the list", "K", &[]),
    act("settings.preview", Scope::Settings, "Preview the AI review", "P", &[]),
];

pub fn action(id: &str) -> Option<&'static Action> {
    ACTIONS.iter().find(|a| a.id == id)
}

/// macOS takes these before any app sees them, or they are the chord every Mac app reserves for the same job. PR Craft
/// refuses them, except as the default of the action that is that job (⌘, opens Settings).
const SYSTEM: &[(&str, &[Modifier], &str)] = &[
    ("Q", &[Cmd], "Quit"),
    ("W", &[Cmd], "Close Window"),
    ("H", &[Cmd], "Hide"),
    ("M", &[Cmd], "Minimize"),
    ("Tab", &[Cmd], "the app switcher"),
    ("Space", &[Cmd], "Spotlight"),
    ("Q", &[Modifier::Ctrl, Cmd], "Lock Screen"),
    ("Comma", &[Cmd], "Settings"),
    ("Escape", &[Alt, Cmd], "Force Quit"),
    ("Backtick", &[Cmd], "Switch Windows"),
    ("3", &[Shift, Cmd], "Screenshot"),
    ("4", &[Shift, Cmd], "Screenshot of a selection"),
    ("5", &[Shift, Cmd], "the screenshot toolbar"),
    ("Space", &[Alt, Cmd], "Finder search"),
    ("ArrowLeft", &[Modifier::Ctrl], "Spaces (previous)"),
    ("ArrowRight", &[Modifier::Ctrl], "Spaces (next)"),
    ("ArrowUp", &[Modifier::Ctrl], "Mission Control"),
    ("ArrowDown", &[Modifier::Ctrl], "App Exposé"),
];

/// Text editing chords: a text field in PR Craft uses them, so an action bound to one would fight the field.
const EDITING: &[(&str, &[Modifier], &str)] = &[
    ("A", &[Cmd], "Select All"),
    ("C", &[Cmd], "Copy"),
    ("V", &[Cmd], "Paste"),
    ("X", &[Cmd], "Cut"),
    ("Z", &[Cmd], "Undo"),
    ("Z", &[Shift, Cmd], "Redo"),
];

/// Chords other apps use that PR Craft does not own. They only act while PR Craft is in front, so they are warned about.
const OTHER_APPS: &[(&str, &[Modifier], &str, &str)] =
    &[("I", &[Shift, Cmd], "Mail", "Show Inspector"), ("0", &[Alt, Cmd], "Xcode", "the inspector toggle")];

/// Whether a text field leaves the binding alone: Esc or an F key by themselves, or any key held with ⌘ or ⌃. A bare key, ⇧ or
/// ⌥ with a letter, ⌫, ⌦, ↵, Tab and the arrows are what a field uses to edit.
fn safe_while_typing(binding: &Binding) -> bool {
    let is_f_key = binding.key.len() > 1 && binding.key.starts_with('F') && binding.key[1..].bytes().all(|b| b.is_ascii_digit());
    binding.has(Modifier::Cmd) || binding.has(Modifier::Ctrl) || ((is_f_key || binding.key == "Escape") && binding.modifiers.is_empty())
}

/// Keys a text field reads with ⌘ held: caret to the line or text edge, and delete to the line edge.
const CMD_TEXT_KEYS: &[&str] = &["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Backspace", "Delete"];

/// Keys a text field reads with ⌃ held, as macOS's Emacs-style editing keys: H K U W ⌫ ⌦ delete, A B E F N P move. ⌘ held
/// as well does not change that.
const CTRL_EDIT_KEYS: &[&str] = &["H", "K", "U", "W", "A", "B", "E", "F", "N", "P", "Backspace", "Delete"];

/// Whether a text field uses the chord for editing, so an action on it that works while typing would take it from the field.
fn text_field_chord(binding: &Binding) -> bool {
    let cmd = binding.has(Modifier::Cmd) && CMD_TEXT_KEYS.contains(&binding.key.as_str());
    let ctrl = binding.has(Modifier::Ctrl) && CTRL_EDIT_KEYS.contains(&binding.key.as_str());
    cmd || ctrl
}

fn matches(table_key: &str, table_mods: &[Modifier], binding: &Binding) -> bool {
    binding.key == table_key && binding.modifiers.len() == table_mods.len() && table_mods.iter().all(|m| binding.has(*m))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Not saved: the binding would not work, or would break something.
    Refused,
    /// Saved, and shown with the reason.
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    pub severity: Severity,
    pub reason: String,
}

fn refused(reason: String) -> Conflict {
    Conflict { severity: Severity::Refused, reason }
}

/// Everything wrong or worth knowing about giving `action` the binding `binding`, while every other action keeps what
/// `effective` says. Empty when the binding is clean.
pub fn check(action: &Action, binding: &Binding, effective: &BTreeMap<&'static str, Binding>) -> Vec<Conflict> {
    let mut found = Vec::new();
    let label = binding.label();
    for other in ACTIONS.iter().filter(|o| o.id != action.id && o.scope.overlaps(action.scope)) {
        let theirs = effective.get(other.id);
        // Layers shipped to share a key (a sheet's J over the review's J): the layer on top takes it first. Only a moved
        // binding makes a new collision.
        let shipped_together = *binding == action.default_binding() && theirs == Some(&other.default_binding());
        if theirs == Some(binding) && !shipped_together {
            found.push(refused(format!("{label} already does “{}” in {}.", other.label, other.scope.label())));
        }
    }
    if let Some((_, _, name)) = SYSTEM.iter().find(|(k, m, _)| matches(k, m, binding)).filter(|_| *binding != action.default_binding()) {
        found.push(refused(format!("{label} is macOS’s {name}. macOS acts on it before PR Craft sees it.")));
    }
    if let Some((_, _, name)) = EDITING.iter().find(|(k, m, _)| matches(k, m, binding)) {
        found.push(refused(format!("{label} is {name} in a text field; an action on it would fight the field.")));
    }
    // The palette's field has focus the whole time it is open, so a key that types would land in the query.
    if action.scope == Scope::Palette && binding.types() {
        found.push(refused(format!("{label} types a character, and the command palette's field has focus while it is open. Use ⌘, ⌃ or ⌥ with the key, or a key that does not type.")));
    }
    if action.works_while_typing && !safe_while_typing(binding) {
        found.push(refused(format!(
            "“{}” works while a text field has focus, so {label} would take keys the field uses for editing. Use ⌘ or ⌃ with the key, or Esc or an F key alone.",
            action.label
        )));
    }
    if action.works_while_typing && text_field_chord(binding) {
        found.push(refused(format!(
            "“{}” works while a text field has focus, and {label} moves the caret or deletes text there. Pick another chord.",
            action.label
        )));
    }
    if let Some((_, _, app, job)) = OTHER_APPS.iter().find(|(k, m, _, _)| matches(k, m, binding)).filter(|_| *binding != action.default_binding()) {
        let reason = format!("{label} is also {job} in {app}. It only acts while PR Craft is in front, so nothing breaks.");
        found.push(Conflict { severity: Severity::Warning, reason });
    }
    found
}

/// The warnings the shipped default of `action` carries, shown in the table so the user knows before changing anything.
pub fn default_note(action: &Action) -> Option<String> {
    let binding = action.default_binding();
    OTHER_APPS
        .iter()
        .find(|(k, m, _, _)| matches(k, m, &binding))
        .map(|(_, _, app, job)| format!("{} is also {job} in {app}. It only acts while PR Craft is in front.", binding.label()))
}

/// The other app that uses `action`'s default binding, by name: `Xcode`, `Mail`.
pub fn default_app(action: &Action) -> Option<&'static str> {
    let binding = action.default_binding();
    OTHER_APPS.iter().find(|(k, m, _, _)| matches(k, m, &binding)).map(|(_, _, app, _)| *app)
}

/// The binding of every action: its default, or what the user saved.
pub fn effective(saved: &BTreeMap<String, Binding>) -> BTreeMap<&'static str, Binding> {
    ACTIONS.iter().map(|a| (a.id, saved.get(a.id).cloned().unwrap_or_else(|| a.default_binding()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(key: &str, modifiers: &[Modifier]) -> Binding {
        Binding::new(key, modifiers).expect("a key of the table")
    }

    fn refusals(id: &str, binding: &Binding, saved: &BTreeMap<String, Binding>) -> Vec<String> {
        let action = action(id).expect("a shipped action");
        check(action, binding, &effective(saved)).into_iter().filter(|c| c.severity == Severity::Refused).map(|c| c.reason).collect()
    }

    #[test]
    fn every_default_is_a_key_of_the_table_and_clean() {
        let defaults = effective(&BTreeMap::new());
        for action in ACTIONS {
            let binding = action.default_binding();
            assert_eq!(Binding::new(&binding.key, &binding.modifiers), Some(binding.clone()), "{} names a key the table lacks", action.id);
            let hard: Vec<_> = check(action, &binding, &defaults).into_iter().filter(|c| c.severity == Severity::Refused).collect();
            assert!(hard.is_empty(), "{} ships a refused binding: {hard:?}", action.id);
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = ACTIONS.iter().map(|a| a.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len());
    }

    #[test]
    fn two_actions_in_one_scope_may_not_share_a_binding() {
        let reasons = refusals("inbox.queue", &bind("B", &[]), &BTreeMap::new());
        assert!(reasons.iter().any(|r| r.contains("Back to you")), "{reasons:?}");
    }

    #[test]
    fn scopes_that_are_never_on_screen_together_may_share_a_binding() {
        assert!(refusals("review.comment", &bind("Q", &[]), &BTreeMap::new()).is_empty());
    }

    #[test]
    fn an_everywhere_shortcut_collides_with_every_scope() {
        let reasons = refusals("app.settings", &bind("N", &[Cmd]), &BTreeMap::new());
        assert!(reasons.iter().any(|r| r.contains("New review agent")), "{reasons:?}");
    }

    #[test]
    fn a_saved_binding_counts_as_taken() {
        let saved = BTreeMap::from([("inbox.queue".to_string(), bind("Z", &[]))]);
        assert!(refusals("inbox.notifications", &bind("Z", &[]), &saved).iter().any(|r| r.contains("Review requested")));
        assert!(refusals("inbox.notifications", &bind("Q", &[]), &saved).is_empty(), "the old key of the moved action is free");
    }

    #[test]
    fn macos_system_shortcuts_are_refused() {
        for (key, mods) in
            [("Q", vec![Cmd]), ("W", vec![Cmd]), ("Tab", vec![Cmd]), ("Space", vec![Cmd]), ("3", vec![Shift, Cmd]), ("Backtick", vec![Cmd])]
        {
            let reasons = refusals("review.comment", &bind(key, &mods), &BTreeMap::new());
            assert!(reasons.iter().any(|r| r.contains("macOS")), "{key}: {reasons:?}");
        }
        assert!(refusals("app.settings", &bind("Comma", &[Cmd]), &BTreeMap::new()).is_empty(), "Settings keeps the chord macOS gives Settings");
        assert!(!refusals("app.activity", &bind("Comma", &[Cmd]), &BTreeMap::new()).is_empty(), "no other action may take it");
    }

    #[test]
    fn a_binding_a_text_field_uses_is_refused_for_an_action_that_works_in_a_text_field() {
        let refused = |key: &str, mods: &[Modifier]| {
            refusals("review.submit", &bind(key, mods), &BTreeMap::new()).iter().any(|r| r.contains("text field has focus"))
        };
        for (key, mods) in [
            ("Z", vec![]),
            ("Z", vec![Shift]),
            ("Z", vec![Alt]),
            ("Backspace", vec![]),
            ("Delete", vec![]),
            ("ArrowUp", vec![Alt]),
            ("Enter", vec![]),
        ] {
            assert!(refused(key, &mods), "{key} {mods:?}");
        }
        assert!(!refused("F5", &[]), "an F key alone");
        assert!(!refused("Escape", &[]), "Esc alone");
        assert!(!refused("Z", &[Modifier::Ctrl]), "⌃ with a key");
        assert!(!refused("Y", &[Cmd]), "⌘ with a key");
        assert!(refusals("review.comment", &bind("Z", &[]), &BTreeMap::new()).is_empty(), "letter shortcuts already pause while typing");
    }

    #[test]
    fn a_chord_the_text_field_edits_with_is_refused_for_a_typing_action_but_other_chords_pass() {
        let fights = |key: &str, mods: &[Modifier]| {
            refusals("review.share", &bind(key, mods), &BTreeMap::new()).iter().any(|r| r.contains("moves the caret or deletes"))
        };
        for (key, mods) in [
            ("ArrowLeft", vec![Cmd]),
            ("ArrowDown", vec![Cmd]),
            ("Backspace", vec![Cmd]),
            ("Delete", vec![Cmd]),
            ("K", vec![Modifier::Ctrl]),
            ("H", vec![Modifier::Ctrl]),
            ("A", vec![Modifier::Ctrl]),
            ("K", vec![Modifier::Ctrl, Cmd]),
            ("Backspace", vec![Modifier::Ctrl]),
            ("Delete", vec![Modifier::Ctrl]),
        ] {
            assert!(fights(key, &mods), "{key} {mods:?}");
        }
        assert!(!fights("Y", &[Cmd]), "⌘Y is free");
        assert!(!fights("Y", &[Modifier::Ctrl]), "⌃Y is free");
        // An action that already pauses while typing may use them.
        assert!(refusals("review.comment", &bind("K", &[Modifier::Ctrl]), &BTreeMap::new()).is_empty());
    }

    #[test]
    fn the_run_details_window_has_its_own_escape_that_works_while_typing() {
        let close = action("review.close_run").expect("shipped");
        assert!(close.works_while_typing && close.scope == Scope::Review);
        assert!(!refusals("review.close_run", &bind("V", &[]), &BTreeMap::new()).is_empty(), "a bare letter is refused for it");
    }

    #[test]
    fn control_arrows_and_option_command_space_are_macos_shortcuts() {
        for (key, mods) in [("ArrowLeft", vec![Modifier::Ctrl]), ("Space", vec![Alt, Cmd])] {
            let reasons = refusals("review.comment", &bind(key, &mods), &BTreeMap::new());
            assert!(reasons.iter().any(|r| r.contains("macOS")), "{key}: {reasons:?}");
        }
    }

    #[test]
    fn an_overlay_collides_with_every_screen_but_shipped_pairs_stay_clean() {
        // The sheet's P is on screen over the review: moving a review action onto it is a collision.
        assert!(refusals("review.comment", &bind("P", &[]), &BTreeMap::new()).iter().any(|r| r.contains("Preview")));
        // The sheet's J and the review's J ship together; the sheet takes it first.
        let defaults = effective(&BTreeMap::new());
        let sheet = action("sheet.next").expect("shipped");
        assert!(check(sheet, &sheet.default_binding(), &defaults).is_empty());
    }

    #[test]
    fn another_apps_chord_is_a_warning_not_a_refusal() {
        let action = action("review.comment").expect("shipped");
        let found = check(action, &bind("I", &[Shift, Cmd]), &effective(&BTreeMap::new()));
        assert!(found.iter().any(|c| c.severity == Severity::Warning && c.reason.contains("Mail")), "{found:?}");
        assert!(found.iter().all(|c| c.severity == Severity::Warning || c.reason.contains("Share")), "{found:?}");
        assert_eq!(default_note(action_of("review.share")).map(|n| n.contains("Mail")), Some(true));
    }

    fn action_of(id: &str) -> &'static Action {
        action(id).expect("shipped")
    }
    #[test]
    fn a_palette_action_may_not_take_a_key_that_types_into_the_palettes_field() {
        let typed = |id: &str, key: &str, mods: &[Modifier]| refusals(id, &bind(key, mods), &BTreeMap::new()).iter().any(|r| r.contains("palette"));
        assert!(typed("palette.next", "J", &[]), "a bare letter");
        assert!(typed("palette.run", "Y", &[Shift]), "a shifted letter");
        assert!(!typed("palette.next", "ArrowDown", &[]), "arrows do not type");
        assert!(!typed("palette.queue_1", "1", &[Cmd]), "⌘ with a digit does not type");
    }

    #[test]
    fn labels_follow_the_order_macos_writes_modifiers() {
        assert_eq!(bind("J", &[Cmd, Alt]).label(), "⌥⌘J");
        assert_eq!(bind("Enter", &[Cmd]).label(), "⌘↵");
        assert_eq!(bind("Comma", &[Cmd, Cmd]).label(), "⌘,");
        assert_eq!(bind("j", &[]), bind("J", &[]));
        assert!(Binding::new("Hyper", &[]).is_none());
    }

    #[test]
    fn the_context_menu_keys_are_registered_and_the_whole_stack_key_stays_free_of_suggest() {
        let defaults = effective(&BTreeMap::new());
        let label = |id: &str| defaults.get(id).map(Binding::label);
        assert_eq!(label("app.context_menu").as_deref(), Some("⇧F10"));
        for id in ["menu.next", "menu.prev", "menu.run", "popover.close"] {
            assert_eq!(action(id).map(|a| a.scope), Some(Scope::Popover), "{id}");
        }
        assert_eq!(label("inbox.open_forge").as_deref(), Some("⌥↵"));
        assert_eq!(label("inbox.copy_link").as_deref(), Some("⇧⌘C"));
        assert_eq!(label("review.viewed_folder").as_deref(), Some("⇧V"));
        assert_eq!(label("review.copy_path").as_deref(), Some("⌥⌘C"));
        assert_eq!(label("review.whole_stack").as_deref(), Some("S"), "Suggest a change is not built, so S stays the whole-stack key");
    }
}
