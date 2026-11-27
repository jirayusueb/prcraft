//! What the engine holds in `settings.json` and `keys.json`, read into the egui context so every screen and every Settings page
//! sees one copy. A change goes through the engine command first and is read back, so the window never shows a value the
//! engine did not save (rule 14). Theme, fonts, sizes and keys apply on the next frame and are there again after a restart.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime};

use craft::egui::{Context, Id};
use craft::ui::runtime::env::{system_appearance, system_text_scale};
use pr_craft_core::keys::Modifier;
use pr_craft_core::settings::Settings;
use pr_craft_engine::keys::KeyRow;
use serde_json::{Value, json};

use crate::theme::{self, Look, System};
use crate::{fonts, keymap};

/// One read of the engine's settings and shortcuts.
#[derive(Clone)]
pub struct Prefs {
    pub settings: Settings,
    pub keys: Vec<KeyRow>,
    /// Why the saved settings or keys could not be read, if they could not; the defaults are in use then.
    pub error: Option<String>,
    stamps: Stamps,
}

fn read() -> Prefs {
    match pr_craft_engine::settings::data_dir() {
        Ok(dir) => read_in(&dir),
        Err(e) => Prefs {
            settings: Settings::default(),
            keys: pr_craft_core::keys::ACTIONS.iter().map(default_row).collect(),
            error: Some(e.to_string()),
            stamps: [None, None],
        },
    }
}

fn read_in(dir: &Path) -> Prefs {
    // Looked at first: a write that lands while the files are read then shows as a change at the next look.
    let stamps = stamps(dir);
    let mut error = None;
    let settings = pr_craft_engine::settings::load_in(dir).unwrap_or_else(|e| {
        error = Some(e.to_string());
        Settings::default()
    });
    let keys = pr_craft_engine::keys::list_in(dir).unwrap_or_else(|e| {
        error = Some(e.to_string());
        pr_craft_core::keys::ACTIONS.iter().map(default_row).collect()
    });
    Prefs { settings, keys, error, stamps }
}

/// The row of an action with its default binding, for when the saved shortcuts could not be read.
fn default_row(action: &'static pr_craft_core::keys::Action) -> KeyRow {
    let binding = action.default_binding();
    KeyRow {
        id: action.id,
        scope: action.scope.label(),
        label: action.label,
        shortcut: binding.label(),
        default: binding.clone(),
        customised: false,
        conflicts: Vec::new(),
        binding,
    }
}

/// How often the two files are looked at for a change made by another process (`pr-craft-cli settings.set`, a second window).
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// Looks at the two files at most every 2 s and reloads when they changed since the copy in force was read.
fn check_files(ctx: &Context) {
    let last: Option<Instant> = ctx.data(|d| d.get_temp(Id::NULL));
    match last {
        Some(at) if at.elapsed() < WATCH_EVERY => {
            ctx.request_repaint_after(WATCH_EVERY - at.elapsed());
            return;
        }
        _ => ctx.data_mut(|d| {
            d.insert_temp(Id::NULL, Instant::now());
        }),
    }
    if let Ok(dir) = pr_craft_engine::settings::data_dir() {
        reload_if_changed(ctx, &dir);
    }
    ctx.request_repaint_after(WATCH_EVERY);
}

/// Reloads from `dir` when `settings.json` or `keys.json` there changed since the copy in force was read. True when it did.
fn reload_if_changed(ctx: &Context, dir: &Path) -> bool {
    let Some(held) = ctx.data(|d| d.get_temp::<Arc<Prefs>>(Id::NULL)) else { return false };
    if stamps(dir) == held.stamps {
        return false;
    }
    install(ctx, read_in(dir));
    ctx.request_repaint();
    true
}

/// When each file was last written; `None` for one that is not there.
type Stamps = [Option<SystemTime>; 2];

fn stamps(dir: &Path) -> Stamps {
    ["settings.json", "keys.json"].map(|name| std::fs::metadata(dir.join(name)).and_then(|m| m.modified()).ok())
}

fn install(ctx: &Context, prefs: Prefs) -> Arc<Prefs> {
    keymap::install(ctx, prefs.keys.iter().map(|row| (row.id, row.binding.clone())).collect());
    let prefs = Arc::new(prefs);
    ctx.data_mut(|d| d.insert_temp(Id::NULL, Arc::clone(&prefs)));
    prefs
}

/// The settings and shortcuts now in force. Reads the engine on the first call.
pub fn get(ctx: &Context) -> Arc<Prefs> {
    match ctx.data(|d| d.get_temp::<Arc<Prefs>>(Id::NULL)) {
        Some(prefs) => prefs,
        None => install(ctx, read()),
    }
}

/// Reads the engine again. Called after every change, and by anything that knows another process wrote the files.
pub fn reload(ctx: &Context) {
    install(ctx, read());
    ctx.request_repaint();
}

/// `settings.set` with `patch`, then the new settings take effect. The error is the engine's own words.
pub fn set(ctx: &Context, patch: &Value) -> Result<(), String> {
    pr_craft_engine::run("settings.set", patch).map_err(|e| e.to_string())?;
    reload(ctx);
    Ok(())
}

/// `keys.rebind`. Returns the reasons a saved binding still carries (another app uses the chord), or the reason it was refused.
pub fn rebind(ctx: &Context, action: &str, key: &str, modifiers: &[Modifier]) -> Result<Vec<String>, String> {
    let params = json!({ "action": action, "key": key, "modifiers": modifiers });
    let answer = pr_craft_engine::run("keys.rebind", &params).map_err(|e| e.to_string())?;
    reload(ctx);
    let warnings = answer["warnings"].as_array().map(|all| all.iter().filter_map(|w| w["reason"].as_str().map(str::to_owned)).collect());
    Ok(warnings.unwrap_or_default())
}

/// `keys.reset`: one action, or every action when `action` is `None`. Returns how many went back to their default.
pub fn reset_keys(ctx: &Context, action: Option<&str>) -> Result<usize, String> {
    let answer = pr_craft_engine::run("keys.reset", &json!({ "action": action }))
        // The engine's words are for the CLI; this window has a button for it.
        .map_err(|e| e.to_string().replace("use keys.reset with no action to reset them all", "use Reset all"))?;
    reload(ctx);
    Ok(answer["reset"].as_u64().unwrap_or(0) as usize)
}

/// What macOS reports, as far as craft's reader knows. A field is `None` where macOS gave no answer.
pub fn system(ctx: &Context) -> System {
    let appearance = system_appearance(ctx).appearance;
    System {
        dark: appearance.mode.map(|m| m == craft::theme::Mode::Dark),
        high_contrast: appearance.contrast.map(|c| c == craft::theme::Contrast::High),
        reduced_motion: appearance.reduced_motion,
        text_scale: system_text_scale(ctx),
    }
}

/// Runs at the top of every frame: installs the look and the shortcuts the saved settings ask for, and tells the window when
/// it changed. Returns the look in force.
pub fn apply(ctx: &Context) -> Look {
    check_files(ctx);
    let prefs = get(ctx);
    let look = Look::new(&prefs.settings, system(ctx));
    let installed: Option<Look> = ctx.data(|d| d.get_temp(Id::NULL));
    if installed != Some(look) {
        theme::install(ctx, look);
        theme::apply_style(ctx, &look);
        ctx.request_repaint();
    }
    fonts::apply(ctx, &prefs.settings);
    look
}

/// The defaults in force, read from no file, so a render never shows this Mac's settings.
#[cfg(test)]
pub(crate) fn install_defaults(ctx: &Context) {
    let keys = pr_craft_core::keys::ACTIONS.iter().map(default_row).collect();
    install(ctx, Prefs { settings: Settings::default(), keys, error: None, stamps: [None, None] });
}

/// How long `pmset` answers are kept, so a page that shows the battery state does not run it every frame.
const BATTERY_AGE: Duration = Duration::from_secs(60);

/// The battery state, read off the UI thread every minute. `None` until the first answer, and when the Mac does not say.
pub struct Battery {
    on_battery: Option<bool>,
    asked: Option<Instant>,
    pending: Option<Receiver<Option<bool>>>,
}

impl Battery {
    pub fn new() -> Self {
        Battery { on_battery: None, asked: None, pending: None }
    }

    /// Whether the Mac runs on battery now, as of the last read.
    pub fn on_battery(&mut self, ctx: &Context) -> Option<bool> {
        if let Some(answer) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.on_battery = answer;
            self.pending = None;
        }
        if self.pending.is_none() && self.asked.is_none_or(|at| at.elapsed() >= BATTERY_AGE) {
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                // The window may have closed first; nobody is left to tell.
                let _ = tx.send(pr_craft_engine::settings::on_battery());
                ctx.request_repaint();
            });
            self.pending = Some(rx);
            self.asked = Some(Instant::now());
        }
        self.on_battery
    }
}

#[cfg(test)]
mod tests {
    use pr_craft_core::settings::Theme;

    use super::*;

    #[test]
    fn a_change_another_process_wrote_to_either_file_is_read_without_a_restart_and_a_quiet_folder_is_not() {
        let dir = std::env::temp_dir().join(format!("pr-craft-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let ctx = Context::default();
        install(&ctx, read_in(&dir));
        assert_eq!(get(&ctx).settings.theme, Theme::Light);
        assert!(!reload_if_changed(&ctx, &dir), "nothing was written");

        std::fs::write(dir.join("settings.json"), r#"{"theme": "dark"}"#).expect("write");
        assert!(reload_if_changed(&ctx, &dir), "settings.json changed");
        assert_eq!(get(&ctx).settings.theme, Theme::Dark);
        assert!(!reload_if_changed(&ctx, &dir), "and once read it is quiet again");

        std::fs::write(dir.join("keys.json"), r#"{"review.comment": {"key": "Y", "modifiers": ["alt"]}}"#).expect("write");
        assert!(reload_if_changed(&ctx, &dir), "keys.json changed");
        assert_eq!(keymap::label(&ctx, "review.comment"), "⌥Y", "the new binding is the one screens read");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
