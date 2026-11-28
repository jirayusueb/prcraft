//! The user's choices in Settings → Appearance, Accessibility and Offline and sync. A flat set of named values, so
//! `settings.set {"theme": "dark"}` changes one without knowing the rest. No I/O.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::extra::Extra;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Light,
    Dark,
    /// Whatever macOS is set to now.
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    Default,
    Roomy,
}

impl Density {
    /// Px added to the vertical padding of a list row. `Default` adds nothing: it is the row the app drew before density was a
    /// setting. Compact trims the padding it has; Roomy adds to it. (Figma frame 17 names 56, 66 and 76 px rows; the app's rows
    /// are as tall as their text, so the steps are relative to that.)
    pub fn row_padding(self) -> f32 {
        match self {
            Density::Compact => -1.0,
            Density::Default => 0.0,
            Density::Roomy => 6.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeColours {
    PrCraft,
    /// Every syntax colour lifted to 7:1.
    HighContrast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffColours {
    MossAndRose,
    /// Blue and orange, for colour-blind readers.
    BlueAndOrange,
}

/// What to do with a queued post when its change moved while the Mac was offline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IfChanged {
    Ask,
    PostAnyway,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// Family names of installed fonts. A name that is not installed keeps its place here and the UI says so.
    pub ui_font: String,
    pub ui_size: f32,
    pub code_font: String,
    pub code_size: f32,
    /// Px per diff row.
    pub code_line_height: f32,
    pub density: Density,
    pub code_colours: CodeColours,
    pub reduce_motion: bool,
    /// Take Reduce Motion, Increase Contrast and the text size from macOS when it reports them.
    pub follow_macos_accessibility: bool,
    pub increase_contrast: bool,
    /// Draw the focus ring after a mouse click too.
    pub focus_rings: bool,
    pub diff_colours: DiffColours,
    pub poll_minutes: u32,
    pub slower_on_battery: bool,
    pub if_changed_offline: IfChanged,
    /// Post a macOS banner for each new notification.
    pub banners: bool,
    /// Where "Check out in a worktree" puts one folder per change. Empty until the user picks one.
    pub checkouts_root: String,
    /// The local clone of each repository, by [`crate::checkout::clone_key`]. Only folders the user named are ever read.
    pub clones: BTreeMap<String, String>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::Light,
            ui_font: "Geist".into(),
            ui_size: 13.5,
            code_font: "Geist Mono".into(),
            code_size: 12.5,
            code_line_height: 20.0,
            density: Density::Default,
            code_colours: CodeColours::PrCraft,
            reduce_motion: false,
            follow_macos_accessibility: true,
            increase_contrast: false,
            focus_rings: false,
            diff_colours: DiffColours::MossAndRose,
            poll_minutes: 5,
            slower_on_battery: true,
            if_changed_offline: IfChanged::Ask,
            banners: true,
            checkouts_root: String::new(),
            clones: BTreeMap::new(),
            extra: Extra::default(),
        }
    }
}

/// The poll interval on battery, when `slower_on_battery` is on and the interval is shorter (Figma frame 60).
pub const BATTERY_POLL_MINUTES: u32 = 5;

impl Settings {
    /// Rejects values the UI could not draw: sizes outside a readable range, an interval of zero, an empty font name.
    pub fn validate(&self) -> Result<(), String> {
        let range = |name: &str, value: f32, low: f32, high: f32| {
            if (low..=high).contains(&value) { Ok(()) } else { Err(format!("{name} must be between {low} and {high}, not {value}")) }
        };
        range("ui_size", self.ui_size, 12.0, 20.0)?;
        range("code_size", self.code_size, 11.0, 20.0)?;
        range("code_line_height", self.code_line_height, 14.0, 32.0)?;
        range("poll_minutes", self.poll_minutes as f32, 1.0, 60.0)?;
        for (name, font) in [("ui_font", &self.ui_font), ("code_font", &self.code_font)] {
            if font.trim().is_empty() || font.chars().any(char::is_control) || font.len() > 100 {
                return Err(format!("{name} must be an installed font family name"));
            }
        }
        let folder = |name: &str, path: &str| {
            if path.is_empty() || (path.starts_with('/') && path.len() <= 4096 && !path.chars().any(char::is_control)) {
                Ok(())
            } else {
                Err(format!("{name} must be a full folder path starting with /"))
            }
        };
        folder("checkouts_root", &self.checkouts_root)?;
        for (key, path) in &self.clones {
            if crate::checkout::parse_clone_key(key).is_none() {
                return Err(format!("`{key}` is not a repository key such as gh:acme/api"));
            }
            folder(&format!("the clone of {key}"), path)?;
        }
        Ok(())
    }

    /// Minutes between checks for new work right now: the chosen interval, or the battery one when on battery.
    pub fn poll_every(&self, on_battery: bool) -> u32 {
        if on_battery && self.slower_on_battery { self.poll_minutes.max(BATTERY_POLL_MINUTES) } else { self.poll_minutes }
    }

    /// `self` with the named values of `patch` replaced. A name that is not a setting, a value of the wrong type, or a
    /// value that fails [`Settings::validate`] is an error and nothing changes.
    pub fn patched(&self, patch: &Value) -> Result<Settings, String> {
        let Some(patch) = patch.as_object() else { return Err("settings.set takes an object of setting names and values".into()) };
        let Ok(Value::Object(mut merged)) = serde_json::to_value(self) else { return Err("the settings could not be encoded".into()) };
        let known = serde_json::to_value(Settings::default()).ok().and_then(|v| v.as_object().cloned()).unwrap_or_default();
        if let Some(name) = patch.keys().find(|name| !known.contains_key(*name)) {
            return Err(format!("`{name}` is not a setting"));
        }
        merged.extend(patch.iter().map(|(k, v)| (k.clone(), v.clone())));
        let next: Settings = serde_json::from_value(Value::Object(Map::from_iter(merged))).map_err(|e| e.to_string())?;
        next.validate()?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_patch_changes_only_the_named_values() {
        let next = Settings::default().patched(&json!({"theme": "dark", "poll_minutes": 15})).expect("valid");
        assert_eq!(next.theme, Theme::Dark);
        assert_eq!(next.poll_minutes, 15);
        assert_eq!(next.ui_font, Settings::default().ui_font);
    }

    #[test]
    fn an_unknown_name_a_wrong_type_or_an_unreadable_size_is_refused() {
        let base = Settings::default();
        assert!(base.patched(&json!({"colour": "dark"})).is_err());
        assert!(base.patched(&json!({"theme": "purple"})).is_err());
        assert!(base.patched(&json!({"ui_size": 2.0})).is_err());
        // DESIGN sets no text below 11 px: Small text is 12.5/13.5 of the interface size, so the floor is 12.
        assert!(base.patched(&json!({"ui_size": 11.0})).is_err());
        assert!(base.patched(&json!({"ui_size": 12.0})).is_ok());
        assert!(base.patched(&json!({"code_size": 10.0})).is_err());
        assert!(base.patched(&json!({"poll_minutes": 0})).is_err());
        assert!(base.patched(&json!({"code_font": "  "})).is_err());
        assert!(base.patched(&json!(["dark"])).is_err());
    }

    #[test]
    fn battery_slows_a_short_interval_only_when_asked() {
        let fast = Settings { poll_minutes: 1, ..Settings::default() };
        assert!(fast.poll_every(true) > fast.poll_every(false));
        assert_eq!(fast.poll_every(false), 1);
        let off = Settings { slower_on_battery: false, ..fast };
        assert_eq!(off.poll_every(true), 1);
        let slow = Settings { poll_minutes: 15, ..Settings::default() };
        assert_eq!(slow.poll_every(true), 15, "battery never makes it faster");
    }

    #[test]
    fn a_setting_a_newer_build_wrote_is_kept_but_not_patchable() {
        let file = json!({"theme": "dark", "from_the_future": {"a": 1}});
        let loaded: Settings = serde_json::from_value(file.clone()).expect("unknown names load");
        assert_eq!(loaded.theme, Theme::Dark);
        assert_eq!(serde_json::to_value(&loaded).expect("saves")["from_the_future"], json!({"a": 1}));
        let kept = loaded.patched(&json!({"poll_minutes": 9})).expect("known name");
        assert_eq!(kept.poll_minutes, 9);
        assert_eq!(kept.extra.0["from_the_future"], json!({"a": 1}));
        assert!(loaded.patched(&json!({"from_the_future": 2})).is_err(), "an unknown name is still not settable");
        let missing: Settings = serde_json::from_value(json!({})).expect("defaults");
        assert_eq!(missing, Settings::default());
    }
}
