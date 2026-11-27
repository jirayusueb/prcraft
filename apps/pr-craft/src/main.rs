//! PR Craft desktop app.

use std::process::ExitCode;

use craft::behavior::placement::Defaults;
use craft::ui::shell::app::{self, AppSpec};
use pr_craft_ui_egui::{APP_ID, PrCraftApp, TITLE};

/// First window size, and the smallest window PRODUCT.md allows.
const WINDOW: [f32; 2] = [1440.0, 900.0];
const WINDOW_MIN: [f32; 2] = [1100.0, 720.0];

fn main() -> ExitCode {
    let mut spec = AppSpec::new(APP_ID, TITLE, Defaults::new(WINDOW, WINDOW_MIN));
    // The frames draw the traffic lights over the app's own title bar: macOS keeps its frame with the title bar hidden.
    if cfg!(target_os = "macos") {
        use craft::window::policy::{Backend, TitleBarPref, decide_for};
        spec = spec.frame(decide_for(craft::window::Prefix::default(), Backend::from_env(), TitleBarPref::Integrated));
    }
    match app::run(spec, PrCraftApp::new) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
