//! `tally.bump` and `tally.get`: every "Open on GitHub/GitLab" the app performs, counted on this Mac with where it came from,
//! so a trial member fills the daily tally from `tally.get` instead of memory. Saved as `tally.json`; nothing leaves the Mac.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use chrono::{Days, NaiveDate};
use pr_craft_core::extra::Extra;
use pr_craft_core::{DateTime, Forge, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CommandError, params, store};

pub const COMMANDS: [&str; 2] = ["tally.bump", "tally.get"];

/// Days of bumps kept; older ones are dropped on the next save.
const KEPT_DAYS: u64 = 60;

static SAVING: Mutex<()> = Mutex::new(());

/// Where one open came from: the screen, the change, and the file when the open was about one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub screen: String,
    pub forge: Forge,
    pub repo: String,
    pub number: u64,
    #[serde(default)]
    pub file_ext: Option<String>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bump {
    pub at: DateTime<Utc>,
    pub origin: Origin,
    pub version: String,
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// `tally.json`: bumps by local date (`2026-11-28`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tally {
    pub days: BTreeMap<String, Vec<Bump>>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// A bump with a guess at why the member had to leave the app.
#[derive(Debug, Clone, Serialize)]
pub struct Suggested {
    pub origin: Origin,
    pub reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Day {
    pub date: String,
    pub bumps: Vec<Bump>,
    pub suggested: Vec<Suggested>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Get {
    date: Option<String>,
}

/// The one table of reasons, by screen (and the file kind on the review screen).
fn reason(origin: &Origin) -> &'static str {
    let ext = origin.file_ext.as_deref().map(str::to_ascii_lowercase);
    match (origin.screen.as_str(), ext.as_deref()) {
        ("checks", _) => "read a CI log on the forge",
        ("conflict", _) => "resolved a merge conflict on the forge",
        ("review", Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ipynb")) => "commented on an image or notebook",
        _ => "opened the change on the forge",
    }
}

fn lock() -> Result<MutexGuard<'static, ()>, CommandError> {
    SAVING.lock().map_err(|_| CommandError::write("an earlier save crashed; restart PR Craft"))
}

fn dir() -> Result<std::path::PathBuf, CommandError> {
    store::data_dir().map_err(CommandError::read)
}

fn day_key(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// Adds one bump to the local date of `now`, and drops the days older than [`KEPT_DAYS`]. Returns how many that day holds.
fn bump_in(dir: &Path, origin: Origin, now: DateTime<Local>) -> Result<usize, CommandError> {
    let _guard = lock()?;
    let mut tally = store::load_tally(dir).map_err(CommandError::read)?;
    let today = now.date_naive();
    let oldest = day_key(today.checked_sub_days(Days::new(KEPT_DAYS)).unwrap_or(today));
    tally.days.retain(|date, _| *date >= oldest);
    let day = tally.days.entry(day_key(today)).or_default();
    day.push(Bump { at: now.with_timezone(&Utc), origin, version: env!("CARGO_PKG_VERSION").to_string(), extra: Extra::default() });
    let count = day.len();
    store::save_tally(dir, &tally).map_err(CommandError::write)?;
    Ok(count)
}

fn get_in(dir: &Path, date: Option<String>, today: NaiveDate) -> Result<Day, CommandError> {
    let date = match date {
        Some(date) => day_key(
            NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map_err(|_| CommandError::Invalid(format!("`{date}` is not a date; write it as YYYY-MM-DD")))?,
        ),
        None => day_key(today),
    };
    let bumps = store::load_tally(dir).map_err(CommandError::read)?.days.remove(&date).unwrap_or_default();
    let suggested = bumps.iter().map(|b| Suggested { origin: b.origin.clone(), reason: reason(&b.origin) }).collect();
    Ok(Day { date, bumps, suggested })
}

/// `tally.bump` and `tally.get`.
pub fn command(id: &str, value: &Value) -> Option<Result<Value, CommandError>> {
    let run = || -> Result<Value, CommandError> {
        match id {
            "tally.bump" => {
                let origin: Origin = params(value)?;
                let now = Local::now();
                let count = bump_in(&dir()?, origin, now)?;
                Ok(serde_json::json!({ "date": day_key(now.date_naive()), "count": count }))
            }
            _ => {
                let date = if value.is_null() { None } else { params::<Get>(value)?.date };
                Ok(serde_json::to_value(get_in(&dir()?, date, Local::now().date_naive())?)?)
            }
        }
    };
    COMMANDS.contains(&id).then(run)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    fn origin(screen: &str, file_ext: Option<&str>) -> Origin {
        Origin {
            screen: screen.into(),
            forge: Forge::GitHub,
            repo: "a/b".into(),
            number: 7,
            file_ext: file_ext.map(Into::into),
            extra: Extra::default(),
        }
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pr-craft-tally-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn at(y: i32, m: u32, d: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, 12, 0, 0).single().expect("a valid local time")
    }

    #[test]
    fn a_bump_is_read_back_with_origin_and_version_and_filtered_by_date() {
        let dir = temp("get");
        bump_in(&dir, origin("inbox", None), at(2026, 11, 27)).expect("bump");
        assert_eq!(bump_in(&dir, origin("checks", Some("rs")), at(2026, 11, 28)).expect("bump"), 1);
        let day = get_in(&dir, Some("2026-11-28".into()), at(2026, 11, 28).date_naive()).expect("get");
        assert_eq!(day.bumps.len(), 1);
        assert_eq!(day.bumps[0].origin, origin("checks", Some("rs")));
        assert_eq!(day.bumps[0].version, env!("CARGO_PKG_VERSION"));
        assert_eq!(day.suggested[0].reason, "read a CI log on the forge");
        let other = get_in(&dir, None, at(2026, 11, 27).date_naive()).expect("get");
        assert_eq!(other.date, "2026-11-27");
        assert_eq!(other.bumps[0].origin.screen, "inbox");
        assert!(get_in(&dir, Some("yesterday".into()), at(2026, 11, 28).date_naive()).is_err());
    }

    #[test]
    fn days_older_than_sixty_are_dropped_on_save_and_unknown_keys_survive() {
        let dir = temp("prune");
        bump_in(&dir, origin("inbox", None), at(2026, 9, 28)).expect("old bump");
        let mut tally = store::load_tally(&dir).expect("load");
        tally.extra.0.insert("later".into(), json!({"kept": true}));
        tally.days.get_mut("2026-09-28").expect("day").iter_mut().for_each(|b| {
            b.extra.0.insert("note".into(), json!(1));
        });
        store::save_tally(&dir, &tally).expect("save");
        bump_in(&dir, origin("inbox", None), at(2026, 11, 28)).expect("bump 61 days later");
        let tally = store::load_tally(&dir).expect("load");
        assert!(!tally.days.contains_key("2026-09-28"), "61 days old");
        assert_eq!(tally.extra.0.get("later"), Some(&json!({"kept": true})));
        bump_in(&dir, origin("inbox", None), at(2026, 11, 29)).expect("bump");
        assert!(store::load_tally(&dir).expect("load").days.contains_key("2026-11-28"));
    }

    #[test]
    fn the_reason_follows_the_screen() {
        assert_eq!(reason(&origin("checks", None)), "read a CI log on the forge");
        assert_eq!(reason(&origin("conflict", None)), "resolved a merge conflict on the forge");
        assert_eq!(reason(&origin("review", Some("PNG"))), "commented on an image or notebook");
        assert_eq!(reason(&origin("review", Some("ipynb"))), "commented on an image or notebook");
        assert_eq!(reason(&origin("review", Some("rs"))), "opened the change on the forge");
        assert_eq!(reason(&origin("review", None)), "opened the change on the forge");
        assert_eq!(reason(&origin("inbox", None)), "opened the change on the forge");
        assert_eq!(reason(&origin("thread", Some("png"))), "opened the change on the forge");
    }
}
