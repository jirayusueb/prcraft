//! `notify.banners`: one macOS banner per new unseen notification event, through `osascript`. macOS lets an app without a
//! signed bundle post banners only this way, so they come from Script Editor and carry no buttons.

use std::io;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use pr_craft_core::settings::Settings;
use serde::Serialize;

use crate::turn::{self, Event, EventKind, Notifications};
use crate::{CommandError, store};

/// Banners per call; the rest of the new events share one summary banner.
const MAX_BANNERS: usize = 3;
/// Characters of a title or body that reach the script.
const MAX_CHARS: usize = 200;
const DEADLINE: Duration = Duration::from_secs(5);

/// What `notify.banners` answers.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Banners {
    /// Banners macOS took.
    pub posted: usize,
    /// "Banner not shown: <reason>" for the first banner that failed.
    pub failed: Option<String>,
}

/// How a command is run: [`pr_craft_forge::bounded::run`] outside tests.
type Runner<'a> = &'a dyn Fn(&mut Command, Duration) -> io::Result<Output>;

/// `s` as the inside of an AppleScript string on one line: control characters dropped, cut at [`MAX_CHARS`], `\` and `"` escaped.
fn script_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(MAX_CHARS).flat_map(|c| if matches!(c, '\\' | '"') { vec!['\\', c] } else { vec![c] }).collect()
}

fn post_with(run: Runner, title: &str, body: &str) -> Result<(), String> {
    let script = format!("display notification \"{}\" with title \"{}\"", script_text(body), script_text(title));
    let output = run(Command::new("/usr/bin/osascript").current_dir("/").arg("-e").arg(script), DEADLINE).map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    Err(if said.trim().is_empty() { format!("osascript {}", output.status) } else { said.trim().to_string() })
}

/// Posts one banner. Rule 15: it only shows if the user allowed notifications for Script Editor.
pub fn post(title: &str, body: &str) -> Result<(), String> {
    post_with(&pr_craft_forge::bounded::run, title, body)
}

/// `notify.banners`: reads the events, then [`banners`].
pub fn command() -> Result<Banners, CommandError> {
    banners(&turn::list()?)
}

/// Banners for the unseen events of `notes` that were not bannered before. Nothing posts while notifications are paused or
/// Settings → Permissions has banners off; those events stay new.
pub fn banners(notes: &Notifications) -> Result<Banners, CommandError> {
    let dir = store::data_dir().map_err(CommandError::read)?;
    let settings = store::load_settings(&dir).map_err(CommandError::read)?;
    banners_in(&dir, notes, &settings, &pr_craft_forge::bounded::run)
}

fn banners_in(dir: &Path, notes: &Notifications, settings: &Settings, run: Runner) -> Result<Banners, CommandError> {
    let none = Banners { posted: 0, failed: None };
    if !settings.banners || notes.pause.is_some() {
        return Ok(none);
    }
    let _guard = turn::lock()?;
    let mut state = store::load_notify(dir).map_err(CommandError::read)?;
    let new: Vec<&Event> = notes.events.iter().filter(|e| !e.seen && !state.bannered.contains(&e.id)).collect();
    if new.is_empty() {
        return Ok(none);
    }
    let (single, rest) = new.split_at(new.len().min(MAX_BANNERS));
    let mut shown = Vec::new();
    for event in single {
        let title = match event.kind {
            EventKind::ReviewRequested => "Review requested",
            EventKind::BackToYou => "Back to you",
        };
        shown.push((title.to_string(), format!("{}: {}", event.change.reference(), event.text)));
    }
    if !rest.is_empty() {
        shown.push(("PR Craft".to_string(), format!("{} more changes need you", rest.len())));
    }
    let mut result = none;
    for (title, body) in shown {
        match post_with(run, &title, &body) {
            Ok(()) => result.posted += 1,
            Err(reason) => {
                result.failed.get_or_insert(format!("Banner not shown: {reason}"));
            }
        }
    }
    // A failed banner is not retried on every poll.
    state.bannered.extend(new.iter().map(|e| e.id.clone()));
    store::save_notify(dir, &state).map_err(CommandError::write)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use pr_craft_core::{Change, Forge, Utc};

    use super::*;
    use crate::turn::Pause;

    fn event(id: &str, seen: bool) -> Event {
        let change = Change {
            forge: Forge::GitHub,
            repo: "acme/api".into(),
            number: 1,
            title: "t".into(),
            author: "nina".into(),
            url: "https://github.com/acme/api/pull/1".into(),
            updated_at: Utc::now(),
            draft: false,
            host: None,
            stack: None,
        };
        Event { id: id.into(), kind: EventKind::ReviewRequested, change, text: "asked you to review".into(), at: Utc::now(), seen }
    }

    fn notes(events: Vec<Event>) -> Notifications {
        Notifications { events, unseen: 0, errors: Vec::new(), pause: None, system_notice: "" }
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("prcraft-banner-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// A runner that records the script of each call and exits with `code`.
    fn fake(calls: &RefCell<Vec<String>>, code: i32) -> impl Fn(&mut Command, Duration) -> io::Result<Output> + '_ {
        move |command, _| {
            calls.borrow_mut().push(command.get_args().nth(1).expect("script").to_string_lossy().into_owned());
            Ok(Output {
                status: ExitStatus::from_raw(code << 8),
                stdout: Vec::new(),
                stderr: if code == 0 { vec![] } else { b"not allowed\n".to_vec() },
            })
        }
    }

    #[test]
    fn quotes_backslashes_newlines_and_a_long_body_make_one_safe_line() {
        assert_eq!(script_text("say \"hi\"\\\nnow\u{7}"), "say \\\"hi\\\"\\\\now");
        let long = script_text(&"x".repeat(500));
        assert_eq!(long.len(), MAX_CHARS);
        let calls = RefCell::new(Vec::new());
        post_with(&fake(&calls, 0), "T\"", "b\nc").expect("posts");
        assert_eq!(calls.borrow()[0], "display notification \"bc\" with title \"T\\\"\"");
    }

    #[test]
    fn a_new_event_gets_one_banner_even_after_a_restart_and_a_seen_one_gets_none() {
        let dir = dir("once");
        let n = notes(vec![event("a", false), event("b", true)]);
        let calls = RefCell::new(Vec::new());
        let got = banners_in(&dir, &n, &Settings::default(), &fake(&calls, 0)).expect("banners");
        assert_eq!(got, Banners { posted: 1, failed: None });
        assert!(calls.borrow()[0].contains("acme/api #1: asked you to review"));
        let again = banners_in(&dir, &n, &Settings::default(), &fake(&calls, 0)).expect("banners");
        assert_eq!(again.posted, 0, "a fresh load of the state still knows the id");
        assert_eq!(calls.borrow().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_posts_while_paused_or_switched_off() {
        let dir = dir("off");
        let calls = RefCell::new(Vec::new());
        let mut n = notes(vec![event("a", false)]);
        n.pause = Some(Pause { until: None, extra: Default::default() });
        banners_in(&dir, &n, &Settings::default(), &fake(&calls, 0)).expect("paused");
        n.pause = None;
        banners_in(&dir, &n, &Settings { banners: false, ..Settings::default() }, &fake(&calls, 0)).expect("off");
        assert!(calls.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn more_than_three_new_events_post_three_and_one_summary() {
        let dir = dir("many");
        let n = notes((0..5).map(|i| event(&format!("e{i}"), false)).collect());
        let calls = RefCell::new(Vec::new());
        let got = banners_in(&dir, &n, &Settings::default(), &fake(&calls, 0)).expect("banners");
        assert_eq!(got.posted, 4);
        assert!(calls.borrow()[3].contains("2 more changes need you"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failing_banner_is_reported_and_its_id_is_not_retried() {
        let dir = dir("fail");
        let n = notes(vec![event("a", false)]);
        let calls = RefCell::new(Vec::new());
        let got = banners_in(&dir, &n, &Settings::default(), &fake(&calls, 1)).expect("banners");
        assert_eq!(got.posted, 0);
        assert_eq!(got.failed.as_deref(), Some("Banner not shown: not allowed"));
        banners_in(&dir, &n, &Settings::default(), &fake(&calls, 1)).expect("banners");
        assert_eq!(calls.borrow().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
