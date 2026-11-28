//! Fuzzy matching and the palette's entries. No I/O.
//!
//! The palette lists three groups for one line of text: actions (a key of the registry, or an engine command that has a button
//! but no key), changes from the inbox, and the Settings pages. [`search`] ranks each group and keeps the lists short.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::keys::{self, Action, Binding, Scope};
use crate::{Change, Forge};

/// How many actions one search lists.
const ACTION_LIMIT: usize = 16;
/// How many changes a typed query lists, and how many an empty one does (the newest).
const CHANGE_LIMIT: usize = 8;
const RECENT_LIMIT: usize = 5;

/// Score of one matched character, so that a longer match outweighs the penalties of its gaps.
const MATCH: i64 = 16;
const CONSECUTIVE: i64 = 15;
const WORD_START: i64 = 10;
const GAP_MAX: usize = 8;
const LATE_MAX: usize = 20;

fn lower(text: &str) -> Vec<char> {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// Whether the characters of `query` occur in `text` in order, ignoring case, and how well. Higher is better: characters that
/// follow each other and characters that start a word score more, a gap between matches and a late first match score less. An
/// empty query matches everything with 0; any real match scores at least 1.
pub fn fuzzy(query: &str, text: &str) -> Option<u32> {
    let query = lower(query.trim());
    let Some(first) = query.first() else { return Some(0) };
    let text = lower(text);
    // Each occurrence of the first character is tried as the start, so a word start beats an earlier scattered hit.
    (0..text.len()).filter(|&start| text.get(start) == Some(first)).filter_map(|start| run(&query, &text, start)).max()
}

/// The score of matching `query` in `text` from the occurrence of its first character at `start`, taking each next character at
/// its first occurrence.
fn run(query: &[char], text: &[char], start: usize) -> Option<u32> {
    let mut score = -(start.min(LATE_MAX) as i64);
    let (mut at, mut previous) = (start, None::<usize>);
    for c in query {
        let found = at + text.get(at..)?.iter().position(|t| t == c)?;
        score += MATCH;
        match previous {
            Some(p) if found == p + 1 => score += CONSECUTIVE,
            Some(p) => score -= (found - p - 1).min(GAP_MAX) as i64,
            None => {}
        }
        if found.checked_sub(1).and_then(|before| text.get(before)).is_none_or(|before| !before.is_alphanumeric()) {
            score += WORD_START;
        }
        previous = Some(found);
        at = found + 1;
    }
    u32::try_from(score.max(1)).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Actions,
    Changes,
    Settings,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Actions => "Actions",
            Group::Changes => "Changes",
            Group::Settings => "Settings",
        }
    }
}

/// What running an entry does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// Presses the key of a registry action (an id of [`keys::ACTIONS`]).
    Action { id: String },
    /// Runs an engine command that has a button in the app but no key.
    Command { id: String },
    Change {
        forge: Forge,
        repo: String,
        number: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        host: Option<String>,
    },
    /// Opens a Settings page: `engine`, `sees`, `agents`, `repos`, `appearance`, `accessibility`, `keyboard`, `sync` or `permissions`.
    Settings { page: String },
}

/// What running an entry takes, so automation never treats a GUI-only entry as runnable: an engine command (`inbox.list`,
/// `review.markdown`, `review.open` for a change) or a press of a screen's own key, which only the app can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Runs {
    EngineCommand,
    GuiOnly,
}

impl Runs {
    pub fn of(target: &Target) -> Runs {
        match target {
            Target::Command { .. } | Target::Change { .. } => Runs::EngineCommand,
            Target::Action { .. } | Target::Settings { .. } => Runs::GuiOnly,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub group: Group,
    pub title: String,
    pub subtitle: String,
    /// The action's binding as it reads on a key cap, such as `⌘↵`; `None` for what has no key.
    pub key: Option<String>,
    pub target: Target,
    pub score: u32,
    /// `engine_command` or `gui_only`.
    pub runs: Runs,
}

/// The screen the palette was opened on. The CLI has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    Inbox,
    Review,
    Settings,
    Activity,
}

impl Screen {
    pub fn scope(self) -> Scope {
        match self {
            Screen::Inbox => Scope::Inbox,
            Screen::Review => Scope::Review,
            Screen::Settings => Scope::Settings,
            Screen::Activity => Scope::Activity,
        }
    }
}

/// An engine command that the app offers as a button without a key.
pub struct UiCommand {
    pub id: &'static str,
    pub title: &'static str,
    pub screen: Screen,
}

pub const UI_COMMANDS: &[UiCommand] = &[
    UiCommand { id: "inbox.list", title: "Re-check the queues", screen: Screen::Inbox },
    UiCommand { id: "review.markdown", title: "Copy the review as Markdown", screen: Screen::Review },
];

/// Each Settings page: its id in [`Target::Settings`], its name (the one on the Settings tab) and the action that opens it.
const PAGES: [(&str, &str, &str); 10] = [
    ("engine", "AI engine", "settings.page.engine"),
    ("sees", "Privacy", "settings.page.sees"),
    ("agents", "Review agents", "settings.page.agents"),
    ("repos", "Repositories", "settings.page.repos"),
    ("appearance", "Appearance", "settings.page.appearance"),
    ("accessibility", "Accessibility", "settings.page.accessibility"),
    ("keyboard", "Keyboard", "settings.page.keyboard"),
    ("sync", "Offline and sync", "settings.page.sync"),
    ("permissions", "Permissions", "settings.page.permissions"),
    ("about", "About", "settings.page.about"),
];

/// What the open review can do right now, so the palette offers only keys that would act. Without it (the CLI) every action of the
/// screen is listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReviewFacts {
    /// Opened from Back to you.
    pub from_back: bool,
    /// A read-only comparison (the whole stack, since your review, picked commits) replaces the diff.
    pub readonly: bool,
    /// AI drafts wait to be kept or discarded.
    pub ai_drafts: bool,
    /// The change sits in a stack of two or more.
    pub stacked: bool,
    /// The forge has review threads on this change.
    pub threads: bool,
    /// Submit would submit: no hand-off, comment box or edit is open for ⌘↵ to take instead.
    pub can_submit: bool,
    /// A run's details window is open for Esc to close.
    pub run_detail_open: bool,
    /// The review holds at least one draft.
    pub drafts: bool,
    /// At least one draft sits on a line that changed since it was written.
    pub outdated_drafts: bool,
    /// The current file has more than one view (rendered or source, image modes).
    pub file_offers_views: bool,
    /// The current file is drawn as hunks, so there are hunk edges to read more lines around.
    pub diff_drawn: bool,
    /// The current file has an image view (2-up, swipe, onion, difference).
    pub file_offers_images: bool,
    /// The filter leaves a current file to act on.
    pub file_current: bool,
    /// The current file sits in a folder (not at the root of the change), for the folder keys.
    pub file_in_folder: bool,
    /// An AI run of this change is going, for Stop to stop.
    pub runs_going: bool,
}

impl ReviewFacts {
    /// Whether the review action `id` would do something now.
    pub fn allows(&self, id: &str) -> bool {
        match id {
            "review.submit" => self.can_submit,
            "review.close_run" => self.run_detail_open,
            "review.next_draft" | "review.prev_draft" | "review.edit_draft" => self.drafts && !self.readonly,
            "review.next_outdated" => self.outdated_drafts && !self.readonly,
            "review.toggle_source" => self.file_offers_views && !self.readonly,
            "review.image_two_up" | "review.image_swipe" | "review.image_onion" | "review.image_difference" => {
                self.file_offers_images && !self.readonly
            }
            "review.stop_runs" => self.runs_going,
            "review.viewed" | "review.fold_all" => self.file_current && !self.readonly,
            "review.fold_folder" | "review.unfold_folder" | "review.viewed_folder" | "review.copy_path" => {
                self.file_current && self.file_in_folder && !self.readonly
            }
            "review.expand_above" | "review.expand_below" | "review.expand_all" => self.diff_drawn && !self.readonly,
            "review.since_review" => self.from_back,
            "review.whole_stack" => self.stacked,
            "review.keep_ai" | "review.keep_ai_edit" | "review.discard_ai" | "review.next_ai" | "review.prev_ai" => self.ai_drafts && !self.readonly,
            "review.next_thread" | "review.prev_thread" | "review.resolve_thread" | "review.reply_thread" => self.threads && !self.readonly,
            "review.comment" | "review.layout" | "review.hide_whitespace" | "review.word_highlight" | "review.wrap" | "review.diff_options" => {
                !self.readonly
            }
            _ => true,
        }
    }
}

/// What the screen under the palette can do, so only keys that would act are offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facts {
    Review(ReviewFacts),
    /// Settings, on the page with this id (`engine`, `sees`, `agents`, `repos`, `appearance`, `accessibility`, `keyboard`, `sync`, `permissions`).
    Settings {
        page: &'static str,
        /// The agent editor is open on the Review agents page.
        agent_form_open: bool,
    },
    /// Activity, with the run selected in it.
    Activity(ActivityFacts),
}

/// What the Activity screen can do with the run it has selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActivityFacts {
    /// A run is selected.
    pub selected: bool,
    /// The selected run is going (not ended).
    pub active: bool,
    /// The selected run looks stuck.
    pub stuck: bool,
    /// Some run is going, for Stop all.
    pub any_active: bool,
}

impl Facts {
    fn allows(&self, action: &Action) -> bool {
        match (self, action.scope) {
            (Facts::Review(review), Scope::Review) => review.allows(action.id),
            (Facts::Settings { page, agent_form_open }, Scope::Settings) => match action.id {
                "settings.save_agent" | "settings.delete_agent" => *page == "agents" && *agent_form_open,
                "settings.next" | "settings.prev" => matches!(*page, "agents" | "repos"),
                "settings.preview" => *page == "sees",
                _ => true,
            },
            (Facts::Activity(a), Scope::Activity) => match action.id {
                "activity.stop" | "activity.kill" => a.selected && a.active,
                "activity.retry" => a.selected && (a.stuck || !a.active),
                "activity.open" => a.selected,
                "activity.stop_all" => a.any_active,
                _ => true,
            },
            _ => true,
        }
    }
}

/// Whether the palette lists `action`: it works on `screen` (anywhere, without one), and it is not the palette's own, not a
/// Settings page (the Settings group lists those) and not a step inside a sheet, the confirm step or the popover.
fn offered(action: &Action, screen: Option<Screen>, facts: Option<&Facts>) -> bool {
    if facts.is_some_and(|f| !f.allows(action)) {
        return false;
    }
    if action.id.starts_with("palette.") || action.id == "app.palette" || action.id == "app.context_menu" || action.id.starts_with("settings.page.") {
        return false;
    }
    match screen {
        Some(screen) => action.scope == Scope::Everywhere || action.scope == screen.scope(),
        None => !matches!(action.scope, Scope::Sheet | Scope::Share | Scope::Confirm | Scope::Popover | Scope::Palette),
    }
}

/// Best score first. An empty query keeps the order given (the table's, or newest first); a typed one breaks ties by title.
fn rank(entries: &mut [Entry], typed: bool) {
    entries.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| if typed { a.title.cmp(&b.title) } else { std::cmp::Ordering::Equal }));
}

fn actions(text: &str, screen: Option<Screen>, bindings: &BTreeMap<&'static str, Binding>, facts: Option<&Facts>) -> Vec<Entry> {
    let registry = keys::ACTIONS.iter().filter(|a| offered(a, screen, facts)).map(|a| {
        let key = bindings.get(a.id).map(Binding::label);
        (a.label, a.scope.label(), key, Target::Action { id: a.id.to_string() })
    });
    let commands = UI_COMMANDS
        .iter()
        .filter(|c| screen.is_none_or(|s| s == c.screen))
        .map(|c| (c.title, c.screen.scope().label(), None, Target::Command { id: c.id.to_string() }));
    let mut found: Vec<Entry> = registry
        .chain(commands)
        .filter_map(|(title, subtitle, key, target)| {
            let score = fuzzy(text, title)?;
            Some(Entry {
                group: Group::Actions,
                title: title.to_string(),
                subtitle: subtitle.to_string(),
                key,
                runs: Runs::of(&target),
                target,
                score,
            })
        })
        .collect();
    rank(&mut found, !text.trim().is_empty());
    found.truncate(ACTION_LIMIT);
    found
}

fn changes(text: &str, changes: &[Change]) -> Vec<Entry> {
    let typed = !text.trim().is_empty();
    let mut recent: Vec<&Change> = changes.iter().collect();
    recent.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
    let mut found: Vec<Entry> = recent
        .into_iter()
        .filter_map(|c| {
            let reference = c.reference();
            let score = [fuzzy(text, &c.title), fuzzy(text, &reference), fuzzy(text, &c.author)].into_iter().flatten().max()?;
            let target = Target::Change { forge: c.forge, repo: c.repo.clone(), number: c.number, host: c.host.clone() };
            Some(Entry {
                group: Group::Changes,
                title: c.title.clone(),
                subtitle: format!("{reference} · {}", c.author),
                key: None,
                runs: Runs::of(&target),
                target,
                score,
            })
        })
        .collect();
    rank(&mut found, typed);
    found.truncate(if typed { CHANGE_LIMIT } else { RECENT_LIMIT });
    found
}

fn settings(text: &str, bindings: &BTreeMap<&'static str, Binding>) -> Vec<Entry> {
    let mut found: Vec<Entry> = PAGES
        .iter()
        .filter_map(|(page, name, action)| {
            let title = format!("Settings: {name}");
            let score = fuzzy(text, &title)?;
            let key = bindings.get(action).map(Binding::label);
            Some(Entry {
                group: Group::Settings,
                title,
                subtitle: Scope::Settings.label().to_string(),
                key,
                runs: Runs::GuiOnly,
                target: Target::Settings { page: (*page).to_string() },
                score,
            })
        })
        .collect();
    rank(&mut found, !text.trim().is_empty());
    found
}

/// What the palette lists for `text` on `screen`: actions, then changes, then Settings pages, each best match first. Every
/// target is real: an action is one of the registry, a command is one of [`UI_COMMANDS`]. `bindings` are the effective keys, so
/// a rebinding shows on the key cap.
pub fn search(text: &str, screen: Option<Screen>, bindings: &BTreeMap<&'static str, Binding>, inbox: &[Change]) -> Vec<Entry> {
    search_with(text, screen, bindings, inbox, None)
}

/// [`search`] on a review whose state is known, so an action that would do nothing there is not offered.
pub fn search_with(
    text: &str,
    screen: Option<Screen>,
    bindings: &BTreeMap<&'static str, Binding>,
    inbox: &[Change],
    facts: Option<&Facts>,
) -> Vec<Entry> {
    let mut all = actions(text, screen, bindings, facts);
    all.extend(changes(text, inbox));
    all.extend(settings(text, bindings));
    all
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::Utc;

    #[test]
    fn every_entry_says_whether_automation_can_run_it() {
        let mut found = search("", None, &keys::effective(&BTreeMap::new()), &[]);
        found.extend(search("Re-check", None, &keys::effective(&BTreeMap::new()), &[]));
        for entry in &found {
            let engine = matches!(entry.target, Target::Command { .. } | Target::Change { .. });
            assert_eq!(entry.runs == Runs::EngineCommand, engine, "{}", entry.title);
        }
        assert!(found.iter().any(|e| e.runs == Runs::GuiOnly), "key actions are GUI-only");
        assert!(found.iter().any(|e| e.runs == Runs::EngineCommand), "the re-check is an engine command");
    }

    fn defaults() -> BTreeMap<&'static str, Binding> {
        keys::effective(&BTreeMap::new())
    }

    /// Changes shaped like an inbox of two forges, `age` hours old.
    fn change(repo: &str, number: u64, title: &str, author: &str, age: i64) -> Change {
        Change {
            forge: Forge::GitHub,
            repo: repo.into(),
            number,
            title: title.into(),
            author: author.into(),
            url: format!("https://example.test/{repo}/{number}"),
            host: None,
            updated_at: Utc::now() - Duration::hours(age),
            draft: false,
            stack: None,
        }
    }

    fn titles(entries: &[Entry], group: Group) -> Vec<&str> {
        entries.iter().filter(|e| e.group == group).map(|e| e.title.as_str()).collect()
    }

    #[test]
    fn a_query_matches_the_characters_in_order_in_any_case() {
        assert!(fuzzy("rvw", "Review requested").is_some());
        assert!(fuzzy("REVIEW", "review requested").is_some());
        assert_eq!(fuzzy("wr", "Review requested"), fuzzy("wr", "review requested"));
        assert_eq!(fuzzy("xyz", "Review requested"), None);
        assert_eq!(fuzzy("qr", "Review requested"), None, "the order of the query is kept");
        assert_eq!(fuzzy("", "anything"), Some(0));
        assert_eq!(fuzzy("   ", "anything"), Some(0));
    }

    #[test]
    fn consecutive_characters_at_a_word_start_beat_scattered_ones() {
        let together = fuzzy("rev", "Review requested");
        let scattered = fuzzy("rev", "Remove every vendor");
        assert!(together > scattered, "{together:?} against {scattered:?}");
        assert!(fuzzy("q", "xx queue") > fuzzy("q", "xxxqueue"), "the same place, but only the first starts a word");
        assert!(fuzzy("q", "Quit") > fuzzy("q", "Review requested"), "an early match beats a late one");
    }

    #[test]
    fn every_action_listed_is_in_the_registry_with_its_binding() {
        let bindings = defaults();
        for screen in [None, Some(Screen::Inbox), Some(Screen::Review), Some(Screen::Settings), Some(Screen::Activity)] {
            for text in ["", "open", "next", "page"] {
                for entry in search(text, screen, &bindings, &[]) {
                    if let Target::Action { id } = &entry.target {
                        let action = keys::action(id).unwrap_or_else(|| panic!("{id} is not an action"));
                        assert_eq!(entry.key.as_deref(), Some(bindings[id.as_str()].label().as_str()));
                        assert_eq!(entry.title, action.label);
                    }
                }
            }
        }
    }

    #[test]
    fn the_palette_does_not_list_itself_nor_the_steps_of_another_overlay() {
        for entry in search("", None, &defaults(), &[]) {
            if let Target::Action { id } = &entry.target {
                let scope = keys::action(id).map(|a| a.scope);
                assert!(!id.starts_with("palette.") && id != "app.palette", "{id}");
                assert!(!matches!(scope, Some(Scope::Sheet | Scope::Share | Scope::Confirm | Scope::Popover)), "{id}");
            }
        }
    }

    #[test]
    fn a_screen_lists_its_own_actions_and_those_that_work_everywhere() {
        let bindings = defaults();
        let on_inbox = search("", Some(Screen::Inbox), &bindings, &[]);
        assert!(titles(&on_inbox, Group::Actions).contains(&"Open Settings"));
        assert!(titles(&on_inbox, Group::Actions).contains(&"Re-check the queues"));
        assert!(!titles(&on_inbox, Group::Actions).contains(&"Next hunk"));
        let on_review = search("hunk", Some(Screen::Review), &bindings, &[]);
        assert!(titles(&on_review, Group::Actions).contains(&"Next hunk"));
        assert!(search("Copy the review", Some(Screen::Inbox), &bindings, &[]).iter().all(|e| e.group != Group::Actions));
        assert!(titles(&search("Copy the review", Some(Screen::Review), &bindings, &[]), Group::Actions).contains(&"Copy the review as Markdown"));
    }

    #[test]
    fn an_empty_query_lists_a_dozen_actions_the_newest_changes_and_every_settings_page() {
        let inbox: Vec<Change> = (0..9).map(|n| change("acme/api", n, &format!("Change {n}"), "mira", n as i64 + 1)).collect();
        let all = search("", None, &defaults(), &inbox);
        assert_eq!(titles(&all, Group::Actions).len(), ACTION_LIMIT);
        let shown = titles(&all, Group::Changes);
        assert!(shown.len() < inbox.len(), "the inbox is cut to the newest few");
        assert_eq!(shown.first(), Some(&"Change 0"), "newest first");
        assert_eq!(titles(&all, Group::Settings).len(), PAGES.len());
        let order: Vec<Group> = all.iter().map(|e| e.group).collect();
        assert!(order.windows(2).all(|w| (w[0] as u8) <= (w[1] as u8)), "actions, then changes, then settings");
    }

    #[test]
    fn a_change_matches_on_its_title_reference_and_author_and_at_most_eight_are_listed() {
        let inbox = vec![
            change("acme/api", 482, "Retry idempotent webhooks", "nina.k", 2),
            change("acme/web", 2210, "Move settings page", "dev.sato", 5),
            change("infra-modules", 88, "Pin the provider", "r.amini", 6),
        ];
        let bindings = defaults();
        let found = |text: &str| titles(&search(text, None, &bindings, &inbox), Group::Changes).into_iter().map(String::from).collect::<Vec<_>>();
        assert_eq!(found("webhooks"), ["Retry idempotent webhooks"]);
        assert_eq!(found("web #2210"), ["Move settings page"]);
        assert_eq!(found("amini"), ["Pin the provider"]);
        let many: Vec<Change> = (0..20).map(|n| change("acme/api", n, "Same title", "mira", 1)).collect();
        assert_eq!(titles(&search("same", None, &bindings, &many), Group::Changes).len(), CHANGE_LIMIT);
        let hit = &search("webhooks", None, &bindings, &inbox)[0];
        assert!(hit.subtitle.starts_with("acme/api #482") && hit.subtitle.ends_with("nina.k"));
    }

    #[test]
    fn a_settings_page_shows_the_key_that_opens_it_and_a_rebinding() {
        let mut bindings = defaults();
        let engine = |bindings: &BTreeMap<&'static str, Binding>| {
            search("ai engine", None, bindings, &[]).into_iter().find(|e| e.group == Group::Settings).and_then(|e| e.key)
        };
        assert_eq!(engine(&bindings), Some(defaults()["settings.page.engine"].label()), "the shipped key shows");
        bindings.insert("settings.page.engine", Binding::new("F2", &[]).expect("a key of the table"));
        assert_eq!(engine(&bindings), Some("F2".to_string()));
    }

    #[test]
    fn a_review_offers_only_the_keys_that_would_act_in_its_state() {
        let ids = |facts: ReviewFacts| -> Vec<String> {
            let found = search_with("", Some(Screen::Review), &defaults(), &[], Some(&Facts::Review(facts)));
            let mut all = found;
            for q in ["since", "keep", "comment", "stack", "thread", "submit"] {
                all.extend(search_with(q, Some(Screen::Review), &defaults(), &[], Some(&Facts::Review(facts))));
            }
            all.into_iter().filter_map(|e| if let Target::Action { id } = e.target { Some(id) } else { None }).collect()
        };
        let quiet = ids(ReviewFacts { can_submit: true, ..ReviewFacts::default() });
        for gone in ["review.since_review", "review.keep_ai", "review.whole_stack", "review.next_thread"] {
            assert!(!quiet.iter().any(|id| id == gone), "{gone} would do nothing here");
        }
        assert!(quiet.iter().any(|id| id == "review.comment"), "comment works in a normal review");
        let busy = ids(ReviewFacts { from_back: true, ai_drafts: true, stacked: true, threads: true, can_submit: false, ..ReviewFacts::default() });
        for shown in ["review.since_review", "review.keep_ai", "review.whole_stack", "review.next_thread"] {
            assert!(busy.iter().any(|id| id == shown), "{shown} acts here");
        }
        assert!(!busy.iter().any(|id| id == "review.submit"), "submit is not offered while ⌘↵ would do something else");
        let readonly = ids(ReviewFacts { readonly: true, can_submit: true, ..ReviewFacts::default() });
        assert!(!readonly.iter().any(|id| id == "review.comment"), "nothing to comment on in a read-only view");
        assert!(readonly.iter().any(|id| id == "review.next_hunk"), "stepping still works there");
    }

    #[test]
    fn keys_that_need_a_drawn_diff_a_draft_a_second_view_or_a_run_window_wait_for_them() {
        let on = |facts: ReviewFacts, id: &str| facts.allows(id);
        let plain = ReviewFacts::default();
        for (id, with) in [
            ("review.close_run", ReviewFacts { run_detail_open: true, ..plain }),
            ("review.next_draft", ReviewFacts { drafts: true, ..plain }),
            ("review.toggle_source", ReviewFacts { file_offers_views: true, ..plain }),
            ("review.image_swipe", ReviewFacts { file_offers_images: true, ..plain }),
            ("review.stop_runs", ReviewFacts { runs_going: true, ..plain }),
            ("review.viewed", ReviewFacts { file_current: true, ..plain }),
            ("review.expand_above", ReviewFacts { diff_drawn: true, ..plain }),
            ("review.expand_all", ReviewFacts { diff_drawn: true, ..plain }),
            ("review.fold_all", ReviewFacts { file_current: true, ..plain }),
            ("review.fold_folder", ReviewFacts { file_current: true, file_in_folder: true, ..plain }),
            ("review.copy_path", ReviewFacts { file_current: true, file_in_folder: true, ..plain }),
        ] {
            assert!(!on(plain, id), "{id} has nothing to act on");
            assert!(on(with, id), "{id} acts once its state is there");
            assert!(
                !on(ReviewFacts { readonly: true, ..with }, id) || id == "review.close_run" || id == "review.stop_runs",
                "{id} is not for a read-only view"
            );
        }
    }

    #[test]
    fn the_folder_keys_wait_for_a_current_file_that_is_in_a_folder() {
        let at_root = ReviewFacts { file_current: true, ..ReviewFacts::default() };
        for id in ["review.fold_folder", "review.unfold_folder", "review.viewed_folder", "review.copy_path"] {
            assert!(!at_root.allows(id), "{id} does nothing for a file at the root");
        }
        assert!(at_root.allows("review.fold_all"), "folding every folder needs only a current file");
    }

    #[test]
    fn a_markdown_file_with_two_views_offers_no_image_modes_and_settings_keys_list_only_where_the_page_reads_them() {
        let md = ReviewFacts { file_offers_views: true, ..ReviewFacts::default() };
        assert!(md.allows("review.toggle_source") && !md.allows("review.image_swipe"));
        let on = |page: &'static str, id: &str| {
            let action = keys::action(id).expect("a shipped action");
            Facts::Settings { page, agent_form_open: true }.allows(action)
        };
        assert!(on("agents", "settings.delete_agent") && !on("appearance", "settings.delete_agent"));
        assert!(on("repos", "settings.next") && !on("keyboard", "settings.next"));
        assert!(on("sees", "settings.preview") && !on("engine", "settings.preview"));
        assert!(on("appearance", "settings.close"), "leaving works on every page");
        let listed = search_with(
            "Delete the agent",
            Some(Screen::Settings),
            &defaults(),
            &[],
            Some(&Facts::Settings { page: "appearance", agent_form_open: true }),
        );
        assert!(listed.iter().all(|e| e.title != "Delete the agent"));
    }

    #[test]
    fn the_agent_form_keys_and_the_activity_keys_list_only_when_they_would_act() {
        let act = |facts: Facts, id: &str| facts.allows(keys::action(id).expect("a shipped action"));
        let settings = |open| Facts::Settings { page: "agents", agent_form_open: open };
        assert!(
            !act(settings(false), "settings.save_agent") && !act(settings(false), "settings.delete_agent"),
            "no editor, nothing to save or delete"
        );
        assert!(act(settings(true), "settings.save_agent") && act(settings(true), "settings.delete_agent"));
        let activity = |a: ActivityFacts, id: &str| act(Facts::Activity(a), id);
        let none = ActivityFacts::default();
        let going = ActivityFacts { selected: true, active: true, any_active: true, ..none };
        let stuck = ActivityFacts { stuck: true, ..going };
        let ended = ActivityFacts { selected: true, ..none };
        for id in ["activity.stop", "activity.kill", "activity.retry", "activity.open"] {
            assert!(!activity(none, id), "{id} needs a selected run");
        }
        assert!(activity(going, "activity.stop") && activity(going, "activity.kill") && !activity(going, "activity.retry"));
        assert!(activity(stuck, "activity.retry"), "a stuck run can be killed and retried");
        assert!(
            activity(ended, "activity.retry")
                && activity(ended, "activity.open")
                && !activity(ended, "activity.stop")
                && !activity(ended, "activity.kill")
        );
        assert!(!activity(ended, "activity.stop_all") && activity(going, "activity.stop_all"), "Stop all needs a run going");
    }

    #[test]
    fn what_matches_nothing_is_left_out() {
        assert!(search("zzzzqx", None, &defaults(), &[change("acme/api", 1, "Retry", "nina.k", 1)]).is_empty());
    }
}
