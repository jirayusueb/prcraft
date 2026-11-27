//! `settings.get` and `settings.set`: Appearance, Accessibility and Offline and sync, saved as `settings.json` in the
//! data folder. The GUI and the CLI read the same file, so a change from one shows in the other.

use std::process::Command;
use std::sync::Mutex;

use pr_craft_core::settings::Settings;
use serde::Serialize;
use serde_json::Value;

use crate::{CommandError, store};

static SAVING: Mutex<()> = Mutex::new(());

/// What `settings.get` answers: the settings, and whether the Mac runs on battery right now (`null` when it cannot say).
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub settings: Settings,
    pub on_battery: Option<bool>,
}

/// Where drafts and settings live on this Mac (`$PR_CRAFT_DATA_DIR`, else Application Support).
pub fn data_dir() -> Result<std::path::PathBuf, CommandError> {
    store::data_dir().map_err(CommandError::Store)
}

/// The saved settings; the defaults when none were saved. A file that does not read is an error and stays on disk.
pub fn load() -> Result<Settings, CommandError> {
    load_in(&data_dir()?)
}

/// [`load`] from the data folder `dir`.
pub fn load_in(dir: &std::path::Path) -> Result<Settings, CommandError> {
    store::load_settings(dir).map_err(CommandError::Store)
}

/// `settings.get`.
pub fn get() -> Result<SettingsView, CommandError> {
    Ok(SettingsView { settings: load()?, on_battery: on_battery() })
}

/// `settings.set`: changes the named values and saves. Returns the settings as saved.
pub fn set(patch: &Value) -> Result<Settings, CommandError> {
    let dir = store::data_dir().map_err(CommandError::Store)?;
    let _guard = SAVING.lock().map_err(|_| CommandError::Store("an earlier save crashed; restart PR Craft".into()))?;
    let current = store::load_settings(&dir).map_err(CommandError::Store)?;
    let next = current.patched(&with_home(patch)).map_err(CommandError::Invalid)?;
    crate::local::check_settings(&current, &next).map_err(CommandError::Invalid)?;
    store::save_settings(&dir, &next).map_err(CommandError::Store)?;
    Ok(next)
}

/// `patch` with a leading `~` of the checkouts folder and of each clone folder written as the home folder, so the file holds
/// full paths.
fn with_home(patch: &Value) -> Value {
    let Some(home) = std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()) else { return patch.clone() };
    let expand = |text: &str| match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => text.to_string(),
    };
    let mut patch = patch.clone();
    if let Some(Value::String(root)) = patch.get_mut("checkouts_root") {
        *root = expand(root);
    }
    if let Some(Value::Object(clones)) = patch.get_mut("clones") {
        for path in clones.values_mut() {
            if let Value::String(text) = path {
                *text = expand(text);
            }
        }
    }
    patch
}

/// Whether the Mac runs on battery, from `pmset -g batt`; `None` when that does not answer in a form this reads.
pub fn on_battery() -> Option<bool> {
    let out = Command::new("pmset").args(["-g", "batt"]).output().ok()?;
    parse_power(&String::from_utf8_lossy(&out.stdout))
}

/// The first line of `pmset -g batt` is `Now drawing from 'Battery Power'` or `'AC Power'`.
fn parse_power(out: &str) -> Option<bool> {
    let line = out.lines().find(|l| l.contains("Now drawing from"))?;
    if line.contains("Battery Power") {
        Some(true)
    } else if line.contains("AC Power") || line.contains("UPS Power") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmset_output_says_battery_or_mains_and_nothing_else_is_a_guess() {
        assert_eq!(parse_power("Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t83%; discharging"), Some(true));
        assert_eq!(parse_power("Now drawing from 'AC Power'\n"), Some(false));
        assert_eq!(parse_power("pmset: unknown option"), None);
        assert_eq!(parse_power("Now drawing from 'Something New'"), None);
    }
}
