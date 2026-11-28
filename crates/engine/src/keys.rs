//! `keys.list`, `keys.rebind` and `keys.reset`: every shortcut with its binding, and the ones the user changed, saved as
//! `keys.json` in the data folder. What may be bound is decided in `pr_craft_core::keys`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use pr_craft_core::keys::{self, ACTIONS, Action, Binding, Conflict, Modifier, Severity};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CommandError, params, store};

static SAVING: Mutex<()> = Mutex::new(());

/// One shortcut as `keys.list` shows it.
#[derive(Debug, Clone, Serialize)]
pub struct KeyRow {
    pub id: &'static str,
    pub scope: &'static str,
    pub label: &'static str,
    pub binding: Binding,
    /// The binding as written on a button: `⌥⌘J`.
    pub shortcut: String,
    pub default: Binding,
    pub customised: bool,
    /// What is wrong with, or worth knowing about, the binding it has now.
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rebound {
    pub row: KeyRow,
    /// Reasons the binding was saved anyway: other apps use the same chord.
    pub warnings: Vec<Conflict>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rebind {
    action: String,
    key: String,
    #[serde(default)]
    modifiers: Vec<Modifier>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reset {
    action: Option<String>,
}

fn dir() -> Result<PathBuf, CommandError> {
    store::data_dir().map_err(CommandError::read)
}

fn lock() -> Result<MutexGuard<'static, ()>, CommandError> {
    SAVING.lock().map_err(|_| CommandError::write("an earlier save crashed; restart PR Craft"))
}

/// The saved bindings that still name an action and a key of the table. One that does not (a hand edit, a removed
/// action) is ignored, not an error: the action keeps its default.
fn load(dir: &Path) -> Result<BTreeMap<String, Binding>, CommandError> {
    let saved = store::load_keys(dir).map_err(CommandError::read)?;
    Ok(saved.into_iter().filter(|(id, b)| keys::action(id).is_some() && Binding::new(&b.key, &b.modifiers).as_ref() == Some(b)).collect())
}

fn row(action: &'static Action, saved: &BTreeMap<String, Binding>, effective: &BTreeMap<&'static str, Binding>) -> KeyRow {
    let binding = effective.get(action.id).cloned().unwrap_or_else(|| action.default_binding());
    KeyRow {
        id: action.id,
        scope: action.scope.label(),
        label: action.label,
        shortcut: binding.label(),
        default: action.default_binding(),
        customised: saved.contains_key(action.id),
        conflicts: keys::check(action, &binding, effective),
        binding,
    }
}

/// The binding of every action, as saved.
pub fn bindings() -> Result<BTreeMap<&'static str, Binding>, CommandError> {
    Ok(keys::effective(&load(&dir()?)?))
}

/// `keys.list`: every action in table order.
pub fn list() -> Result<Vec<KeyRow>, CommandError> {
    list_in(&dir()?)
}

pub fn list_in(dir: &Path) -> Result<Vec<KeyRow>, CommandError> {
    let saved = load(dir)?;
    let effective = keys::effective(&saved);
    Ok(ACTIONS.iter().map(|a| row(a, &saved, &effective)).collect())
}

/// `keys.rebind`: gives `action` a new binding. A refused binding is an error that says why and saves nothing.
pub fn rebind(value: &Value) -> Result<Rebound, CommandError> {
    rebind_in(&dir()?, value)
}

fn rebind_in(dir: &Path, value: &Value) -> Result<Rebound, CommandError> {
    let p: Rebind = params(value)?;
    let action = keys::action(&p.action).ok_or_else(|| CommandError::Invalid(format!("`{}` is not a shortcut (see keys.list)", p.action)))?;
    let binding = Binding::new(&p.key, &p.modifiers).ok_or_else(|| CommandError::Invalid(format!("`{}` is not a key a shortcut can use", p.key)))?;
    let _guard = lock()?;
    let mut saved = load(dir)?;
    let found = keys::check(action, &binding, &keys::effective(&saved));
    let (refused, warnings): (Vec<Conflict>, Vec<Conflict>) = found.into_iter().partition(|c| c.severity == Severity::Refused);
    if !refused.is_empty() {
        let reasons: Vec<String> = refused.into_iter().map(|c| c.reason).collect();
        return Err(CommandError::Invalid(format!("{} was not changed. {}", action.label, reasons.join(" "))));
    }
    if binding == action.default_binding() {
        saved.remove(action.id);
    } else {
        saved.insert(action.id.to_string(), binding);
    }
    store::save_keys(dir, &saved).map_err(CommandError::write)?;
    let effective = keys::effective(&saved);
    Ok(Rebound { row: row(action, &saved, &effective), warnings })
}

/// `keys.reset`: one action back to its default, or every action when none is named. Returns how many were changed.
pub fn reset(value: &Value) -> Result<usize, CommandError> {
    reset_in(&dir()?, value)
}

fn reset_in(dir: &Path, value: &Value) -> Result<usize, CommandError> {
    let p: Reset = params(value)?;
    let _guard = lock()?;
    let mut saved = load(dir)?;
    let before = saved.len();
    match p.action {
        Some(id) => {
            let Some(action) = keys::action(&id) else {
                return Err(CommandError::Invalid(format!("`{id}` is not a shortcut (see keys.list)")));
            };
            let mut without = saved.clone();
            without.remove(&id);
            let refused: Vec<String> = keys::check(action, &action.default_binding(), &keys::effective(&without))
                .into_iter()
                .filter(|c| c.severity == Severity::Refused)
                .map(|c| c.reason)
                .collect();
            if !refused.is_empty() {
                return Err(CommandError::Invalid(format!(
                    "{} was not reset to {}. {} Change that shortcut first, or use keys.reset with no action to reset them all.",
                    action.label,
                    action.default_binding().label(),
                    refused.join(" ")
                )));
            }
            saved.remove(&id);
        }
        None => saved.clear(),
    }
    let changed = before - saved.len();
    if changed > 0 {
        store::save_keys(dir, &saved).map_err(CommandError::write)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn shortcut(dir: &Path, id: &str) -> Option<String> {
        list_in(dir).expect("list").into_iter().find(|r| r.id == id).map(|r| r.shortcut)
    }

    #[test]
    fn a_rebind_is_saved_listed_refused_when_taken_and_reset() {
        let dir = std::env::temp_dir().join(format!("pr-craft-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(shortcut(&dir, "review.comment").as_deref(), Some("C"));

        let rebound = rebind_in(&dir, &json!({"action": "review.comment", "key": "Y", "modifiers": ["alt"]})).expect("a free binding");
        assert_eq!(rebound.row.shortcut, "⌥Y");
        assert!(rebound.row.customised);
        assert_eq!(shortcut(&dir, "review.comment").as_deref(), Some("⌥Y"), "a second read sees it");

        let refused = rebind_in(&dir, &json!({"action": "review.viewed", "key": "Q", "modifiers": ["cmd"]})).expect_err("macOS Quit");
        assert!(refused.to_string().contains("Quit"), "{refused}");
        let taken = rebind_in(&dir, &json!({"action": "review.viewed", "key": "H"})).expect_err("Hand off has H");
        assert!(taken.to_string().contains("Hand off"), "{taken}");
        assert!(rebind_in(&dir, &json!({"action": "review.viewed", "key": "Hyper"})).is_err());
        assert!(rebind_in(&dir, &json!({"action": "nope", "key": "A"})).is_err());
        assert_eq!(shortcut(&dir, "review.viewed").as_deref(), Some("V"), "a refusal saves nothing");

        let warned = rebind_in(&dir, &json!({"action": "review.viewed", "key": "0", "modifiers": ["alt", "cmd"]})).expect("a warning, not a refusal");
        assert!(!warned.warnings.is_empty());

        assert_eq!(reset_in(&dir, &json!({"action": "review.comment"})).expect("reset"), 1);
        assert_eq!(shortcut(&dir, "review.comment").as_deref(), Some("C"));
        assert_eq!(reset_in(&dir, &json!({})).expect("reset all"), 1, "only the warned binding was left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resetting_one_action_is_refused_when_another_took_its_default_key() {
        let dir = std::env::temp_dir().join(format!("pr-craft-keys-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        rebind_in(&dir, &json!({"action": "inbox.queue", "key": "Z"})).expect("free");
        rebind_in(&dir, &json!({"action": "inbox.notifications", "key": "Q"})).expect("Q is free now");
        let refused = reset_in(&dir, &json!({"action": "inbox.queue"})).expect_err("Q is taken");
        assert!(refused.to_string().contains("Notifications"), "{refused}");
        assert_eq!(shortcut(&dir, "inbox.queue").as_deref(), Some("Z"), "nothing changed");
        assert_eq!(reset_in(&dir, &json!({})).expect("reset all is always clean"), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_binding_for_a_removed_action_or_an_unknown_key_is_ignored() {
        let dir = std::env::temp_dir().join(format!("pr-craft-keys-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("keys.json"), r#"{"gone.action": {"key": "A"}, "review.comment": {"key": "Hyper"}}"#).expect("write");
        assert_eq!(shortcut(&dir, "review.comment").as_deref(), Some("C"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
