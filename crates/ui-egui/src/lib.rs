//! PR Craft's egui UI, a `ShellApp` the craft-ui runtime drives (craft-libs docs/craft-framework.md 2.2).

mod activity;
mod agents;
mod ai;
mod checks;
mod commits;
mod components;
mod drafts;
mod fonts;
mod keymap;
mod local;
mod menu;
mod mine;
#[cfg(test)]
mod mine_parity;
mod pages;
mod palette;
mod panel;
#[cfg(test)]
mod parity;
mod prefs;
mod review;
mod settings;
mod share;
mod sheet;
mod theme;
mod threads;
mod views;

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use components::{Bar, Pill, PillKind};
use craft::egui::text::LayoutJob;
use craft::egui::{self, Align, Context, Layout, Margin, OpenUrl, Rect, RichText, ScrollArea, Ui, UiBuilder};
use craft::ui::runtime::env::RootEnv;
use craft::ui::runtime::frame::FrameInput;
use craft::ui::shell::app::ShellApp;
use craft::ui::shell::embed::CloseDecision;
use pr_craft_core::palette::{Facts, Screen, Target};
use pr_craft_core::turn::Turn;
use pr_craft_core::{Change, Forge};
use pr_craft_engine::quota::HostQuota;
use pr_craft_engine::turn::{self, BackItem, BackSource, BackState, EventKind};
use pr_craft_engine::{InboxSource, SourceState};
use pr_craft_forge::Login;

pub const APP_ID: &str = "ai.storyteller.prcraft";
pub const TITLE: &str = "PR Craft";

pub struct PrCraftApp {
    sources: Vec<Source>,
    /// Every listed change, newest update first. Rebuilt when a source answers.
    changes: Vec<Change>,
    /// What the list shows, as indices into `changes`: a stack's layers share one entry, lowest layer first.
    entries: Vec<Vec<usize>>,
    /// The selected row's key: a change's URL, or a notification's id. It survives a re-sync that reorders the list.
    selected: Option<String>,
    focus_selected: bool,
    /// The open review; the inbox shows when there is none.
    review: Option<review::ReviewScreen>,
    /// Which list the middle pane shows.
    view: Inbox,
    /// The forge the queue is narrowed to; `None` shows both.
    forge_filter: Option<Forge>,
    back: Vec<BackSync>,
    /// The in-app notifications, built once every source has answered.
    notifications: Option<turn::Notifications>,
    notify_error: Option<String>,
    /// Rebuild `notifications` as soon as no source is syncing.
    notify_dirty: bool,
    /// Every AI run, read on a timer; the pill, the popover, Activity and the review's runs panel draw from it.
    runs: ai::Runs,
    /// A screen that covers the inbox and the review: Settings or Activity.
    overlay: Overlay,
    /// The popover under the task pill is open.
    popover: bool,
    /// The command palette (⌘K) is open over the screen.
    palette: Option<palette::Palette>,
    /// The look in force, for the next frame's window: the runtime reads the mode before it draws.
    dark: bool,
    battery: prefs::Battery,
    /// When the sources were last asked, by start-up, Re-check or the poll.
    last_check: Instant,
    /// A poll's re-checks are on their way; the interval runs from the moment the last one answers.
    polling: bool,
    /// The saved reviews with something in them (Local drafts), and when they were last read. Only the app reads them; a
    /// fixture never touches this Mac.
    local_drafts: Option<drafts::Drafts>,
    drafts_rx: Option<Receiver<drafts::Drafts>>,
    drafts_live: bool,
    /// The saved reviews are read again when no read is on its way: the queue was just shown, or a review just closed.
    drafts_stale: bool,
    /// The reader's own open changes per forge (Your changes), and the pane of the one shown.
    mine: Vec<mine::MineSync>,
    panel: Option<panel::Panel>,
    /// The CI job log, over the inbox.
    checks: Option<checks::ChecksScreen>,
}

struct Source {
    forge: Forge,
    sync: Sync,
    /// A poll's re-check on its way: the list stays as it was until it answers.
    refresh: Option<Receiver<InboxSource>>,
    /// Why the last poll failed while the list from the poll before it is still shown.
    stale: Option<String>,
}

enum Sync {
    Checking(Receiver<InboxSource>),
    Done(SourceState),
}

/// The lists the inbox shows in its middle pane.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Inbox {
    Queue,
    BackToYou,
    Mine,
    Notifications,
    LocalDrafts,
}

struct BackSync {
    forge: Forge,
    sync: BackFetch,
    /// A poll's re-check on its way: the list stays as it was until it answers.
    refresh: Option<Receiver<BackSource>>,
    /// Why the last poll failed while the list from the poll before it is still shown.
    stale: Option<String>,
}

enum BackFetch {
    Checking(Receiver<BackSource>),
    Done(BackState),
}

impl PrCraftApp {
    /// Starts one sync thread per forge: the CLIs wait on the network, and rows appear as each source answers.
    pub fn new(ctx: &Context) -> Self {
        let sources = Forge::ALL.into_iter().map(|forge| Source { forge, sync: start_sync(ctx, forge), refresh: None, stale: None }).collect();
        let back = Forge::ALL.into_iter().map(|forge| BackSync { forge, sync: start_back(ctx, forge), refresh: None, stale: None }).collect();
        PrCraftApp {
            sources,
            changes: Vec::new(),
            entries: Vec::new(),
            selected: None,
            focus_selected: false,
            review: None,
            view: Inbox::Queue,
            forge_filter: None,
            back,
            notifications: None,
            notify_error: None,
            notify_dirty: false,
            runs: ai::Runs::default(),
            overlay: Overlay::None,
            popover: false,
            palette: None,
            dark: false,
            battery: prefs::Battery::new(),
            last_check: Instant::now(),
            polling: true,
            local_drafts: None,
            drafts_rx: None,
            drafts_live: true,
            drafts_stale: false,
            mine: Forge::ALL.into_iter().map(|forge| mine::MineSync::start(ctx, forge)).collect(),
            panel: None,
            checks: None,
        }
    }

    /// An app that has heard from both forges, with `changes` listed and `selected` chosen, and nothing running: no sync
    /// thread, nothing read from this Mac. For the render tests.
    #[cfg(test)]
    pub(crate) fn fixture(mut changes: Vec<Change>, selected: &str, runs: ai::Runs) -> Self {
        changes.sort_by_key(|change| std::cmp::Reverse(change.updated_at));
        let listed = |forge: Forge| Source {
            forge,
            sync: Sync::Done(SourceState::Listed {
                changes: changes.iter().filter(|change| change.forge == forge).cloned().collect(),
                truncated: false,
                stacks_error: None,
                hosts: Vec::new(),
                signed_out: Vec::new(),
            }),
            refresh: None,
            stale: None,
        };
        PrCraftApp {
            sources: Forge::ALL.into_iter().map(listed).collect(),
            entries: group(&changes),
            selected: changes.iter().find(|change| change.url == selected).map(|change| change.url.clone()),
            changes,
            focus_selected: false,
            review: None,
            view: Inbox::Queue,
            forge_filter: None,
            back: Vec::new(),
            notifications: None,
            notify_error: None,
            notify_dirty: false,
            runs,
            overlay: Overlay::None,
            popover: false,
            dark: false,
            palette: None,
            battery: prefs::Battery::new(),
            last_check: Instant::now().checked_sub(Duration::from_secs(60)).unwrap_or_else(Instant::now),
            polling: false,
            local_drafts: None,
            drafts_rx: None,
            drafts_live: false,
            drafts_stale: false,
            mine: Forge::ALL.into_iter().map(|forge| mine::MineSync::fixture(forge, Vec::new())).collect(),
            panel: None,
            checks: None,
        }
    }

    fn poll(&mut self) {
        let mut answered = false;
        for source in &mut self.sources {
            if let Sync::Checking(rx) = &source.sync {
                let state = match rx.try_recv() {
                    Ok(inbox) => inbox.state,
                    Err(TryRecvError::Empty) => continue,
                    Err(TryRecvError::Disconnected) => SourceState::Failed { error: "the sync stopped before answering".into() },
                };
                source.sync = Sync::Done(state);
                answered = true;
            }
        }
        for source in &mut self.sources {
            let Some(done) = source.refresh.as_ref().and_then(arrived) else { continue };
            source.refresh = None;
            let state = match done {
                Some(inbox) => inbox.state,
                None => SourceState::Failed { error: "the sync stopped before answering".into() },
            };
            // A poll that fails keeps the last good list and says so; only a real answer replaces it.
            if !matches!(state, SourceState::Listed { .. }) && matches!(source.sync, Sync::Done(SourceState::Listed { .. })) {
                source.stale = Some(match state {
                    SourceState::Failed { error } => error,
                    _ => format!("{} cannot act for you right now; Re-check shows the fix", source.forge.cli()),
                });
            } else {
                source.stale = None;
                source.sync = Sync::Done(state);
            }
            answered = true;
        }
        for back in &mut self.back {
            let Some(done) = back.refresh.as_ref().and_then(arrived) else { continue };
            back.refresh = None;
            let state = match done {
                Some(source) => source.state,
                None => BackState::Failed { error: "the sync stopped before answering".into() },
            };
            if matches!(state, BackState::Listed { .. }) || !matches!(back.sync, BackFetch::Done(BackState::Listed { .. })) {
                back.stale = None;
                back.sync = BackFetch::Done(state);
                self.notify_dirty = true;
            } else {
                back.stale = Some(match state {
                    BackState::Failed { error } => error,
                    _ => format!("{} cannot act for you right now", back.forge.cli()),
                });
            }
        }
        for back in &mut self.back {
            if let BackFetch::Checking(rx) = &back.sync {
                let state = match rx.try_recv() {
                    Ok(source) => source.state,
                    Err(TryRecvError::Empty) => continue,
                    Err(TryRecvError::Disconnected) => BackState::Failed { error: "the sync stopped before answering".into() },
                };
                back.sync = BackFetch::Done(state);
                self.notify_dirty = true;
            }
        }
        self.poll_mine();
        if answered {
            self.notify_dirty = true;
            self.changes = self
                .sources
                .iter()
                .filter_map(|source| match &source.sync {
                    Sync::Done(SourceState::Listed { changes, .. }) => Some(changes.iter().cloned()),
                    _ => None,
                })
                .flatten()
                .collect();
            self.changes.sort_by_key(|change| std::cmp::Reverse(change.updated_at));
            self.entries = group(&self.changes);
        }
        if self.notify_dirty && !self.syncing() {
            self.refresh_notifications();
        }
        self.settle_poll();
    }

    /// Starts the interval from the moment the last sync or poll answered, not from when it began: a slow `gh` never makes the
    /// next poll follow at once.
    fn settle_poll(&mut self) {
        if !self.syncing() && !self.refreshing() {
            pr_craft_engine::quota::end_poll();
        }
        if self.polling && !self.syncing() && !self.refreshing() {
            self.polling = false;
            self.last_check = Instant::now();
        }
    }

    /// The selected entry, found by the URL of its first layer.
    fn selected_index(&self) -> Option<usize> {
        let url = self.selected.as_deref()?;
        (0..self.entries.len()).find(|&e| self.first_of(e).is_some_and(|change| change.url == url))
    }

    /// The change an entry opens: its lowest layer in the inbox.
    fn first_of(&self, entry: usize) -> Option<&Change> {
        self.entries.get(entry)?.first().and_then(|&i| self.changes.get(i))
    }

    fn keys(&mut self, ui: &Ui) {
        // A letter typed in a text field (the commit message of a merge) is text, never a queue key or a move.
        if ui.ctx().text_edit_focused() {
            return;
        }
        let (down, up) = (keymap::pressed(ui, "inbox.next"), keymap::pressed(ui, "inbox.prev"));
        for (id, view) in [
            ("inbox.queue", Inbox::Queue),
            ("inbox.back_to_you", Inbox::BackToYou),
            ("inbox.mine", Inbox::Mine),
            ("inbox.notifications", Inbox::Notifications),
            ("inbox.local_drafts", Inbox::LocalDrafts),
        ] {
            if keymap::pressed(ui, id) {
                self.show(view);
            }
        }
        if self.view == Inbox::Notifications {
            if keymap::pressed(ui, "inbox.pause") {
                self.toggle_pause();
            }
            if keymap::pressed(ui, "inbox.mark_seen") {
                self.mark_all_seen();
            }
        }
        let ids = self.nav_ids();
        if (down || up) && !ids.is_empty() {
            let last = ids.len() - 1;
            let at = self.selected.as_deref().and_then(|key| ids.iter().position(|id| id == key));
            let next = match (at, down) {
                (None, _) => 0,
                (Some(i), true) => (i + 1).min(last),
                (Some(i), false) => i.saturating_sub(1),
            };
            self.selected = ids.get(next).cloned();
            self.focus_selected = true;
        }
        if self.view == Inbox::Queue {
            let url = self.selected_index().and_then(|i| self.first_of(i)).map(|change| change.url.clone());
            if let Some(url) = url {
                for (id, item) in [("inbox.open_forge", "forge"), ("inbox.copy_link", "link")] {
                    if keymap::pressed(ui, id) {
                        self.run_inbox_menu(ui.ctx(), &menu::Chosen { target: url.clone(), item });
                    }
                }
            }
        }
    }

    /// What an item of the inbox row menu does; ⌥↵ and ⇧⌘C do the same for the selected row.
    fn run_inbox_menu(&mut self, ctx: &Context, choice: &menu::Chosen) {
        let Some(change) = self.changes.iter().find(|c| c.url == choice.target) else { return };
        match choice.item {
            "open" => {
                let (change, title) = (change.change_ref(), change.title.clone());
                self.open_review(ctx, change, title);
            }
            "forge" => open_on_forge(ctx, &change.url, "inbox", &change.change_ref(), None),
            "link" => {
                ctx.copy_text(change.url.clone());
                self.runs.status = Some(Ok("Copied the link.".to_string()));
            }
            _ => {}
        }
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 2.0;
        components::section_title(ui, "Queue");
        let more = if self.truncated() { "+" } else { "" };
        let back = self.back_items().iter().filter(|item| item.is_back_to_you()).count();
        let new = self.notifications.as_ref().map(|n| format!("{} new", n.unseen)).unwrap_or_default();
        let rows = [
            (Inbox::Queue, "Review requested", format!("{}{more}", self.changes.len()), "inbox.queue"),
            (Inbox::BackToYou, "Back to you", format!("{}{}", back + self.yours_items().len(), self.back_marker()), "inbox.back_to_you"),
            (Inbox::Mine, "Your changes", self.mine_count(), "inbox.mine"),
            (Inbox::Notifications, "Notifications", new, "inbox.notifications"),
            (Inbox::LocalDrafts, "Local drafts", self.local_drafts_count(), "inbox.local_drafts"),
        ];
        for (view, text, count, key) in rows {
            let row = components::nav_item(ui, self.view == view, text, &count).on_hover_text(keymap::label(ui, key));
            if row.clicked() {
                self.show(view);
            }
        }
        ui.add_space(theme::SPACE_2);
        components::note(ui, "Reminders and the 10:00 digest are not available yet.", theme::pal(ui).text_tertiary);
        components::section_title(ui, "Sources");
        let ctx = ui.ctx().clone();
        let mut rechecks = Vec::new();
        let back_busy: Vec<Forge> = self.back.iter().filter(|b| b.refresh.is_some()).map(|b| b.forge).collect();
        for source in &mut self.sources {
            let (forge, cli) = (source.forge.name(), source.forge.cli());
            let p = theme::pal(ui);
            ui.horizontal_top(|ui| {
                ui.add_space(4.0);
                components::forge_mark(ui, source.forge);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    components::label(ui, forge, 13.0, p.graphite);
                    match &source.sync {
                        Sync::Checking(_) => {
                            components::note(ui, format!("Syncing through {cli}…"), p.text_tertiary);
                        }
                        Sync::Done(SourceState::Listed { changes, truncated, stacks_error, hosts, signed_out }) => {
                            let more = if *truncated { "+" } else { "" };
                            components::note(ui, format!("{cli} · {}{more} waiting", changes.len()), p.text_secondary);
                            // One line per host once there is more than one, or when a host failed: rule 15, a host that
                            // could not be read is named, so the count above is not mistaken for all of it.
                            if hosts.len() > 1 || hosts.iter().any(|h| h.error.is_some()) {
                                for host in hosts {
                                    match &host.error {
                                        None => {
                                            let more = if host.truncated { "+" } else { "" };
                                            components::note(ui, format!("{} · {}{more}", host.host, host.listed), p.text_tertiary);
                                        }
                                        Some(error) => live_label(ui, format!("{} · could not be read: {error}", host.host), p.rose_text),
                                    }
                                }
                            }
                            if *truncated {
                                let note = format!("A host returned its limit of {}; older requests may be missing.", pr_craft_forge::PAGE);
                                components::note(ui, note, p.text_secondary);
                            }
                            if !signed_out.is_empty() {
                                components::note(ui, format!("Not signed in, not read: {}.", signed_out.join(", ")), p.text_tertiary);
                            }
                            // Rule 15: a stack that could not be read is unknown, not absent.
                            if let Some(error) = stacks_error {
                                live_label(ui, error.as_str(), p.ember_text);
                            }
                        }
                        Sync::Done(SourceState::Paused { login }) => {
                            let (problem, fix) = match login {
                                Login::LoggedOut { fix } => (format!("{cli} is not logged in. Run this in a terminal, then Re-check."), Some(fix)),
                                Login::CliMissing { fix } => (format!("{cli} is not installed."), Some(fix)),
                                Login::CliOld { message, fix } => (format!("{message}. Run this in a terminal, then Re-check."), Some(fix)),
                                Login::Scope { fix } => (format!("{cli}'s token lacks a scope. Run this in a terminal, then Re-check."), Some(fix)),
                                Login::Failed { error } => (format!("{cli} could not run: {error}."), None),
                                Login::LoggedIn => (String::new(), None),
                            };
                            components::note(ui, format!("Paused. {problem}"), p.ember_text);
                            if let Some(fix) = fix {
                                ui.horizontal(|ui| {
                                    components::mono_label(ui, fix, 11.5, p.graphite);
                                    if Pill::new(PillKind::Secondary, "Copy").small().show(ui).clicked() {
                                        ctx.copy_text(fix.clone());
                                    }
                                });
                            }
                        }
                        Sync::Done(SourceState::Failed { error }) => {
                            components::note(ui, format!("Sync failed: {error}"), p.rose_text);
                        }
                    }
                    if let Some(why) = &source.stale {
                        let note = format!("The last re-check failed: {why}. This is the list from the check before it.");
                        live_label(ui, note, p.ember_text);
                    }
                    // A poll's answer would overwrite this one, so Re-check waits for it.
                    let enabled = recheck_allowed(source, &back_busy);
                    if Pill::new(PillKind::Quiet, &format!("Re-check {forge}")).small().enabled(enabled).show(ui).clicked() {
                        source.stale = None;
                        source.sync = start_sync(&ctx, source.forge);
                        rechecks.push(source.forge);
                    }
                });
            });
            ui.add_space(theme::SPACE_2);
        }
        self.restarted(&ctx, rechecks);
        let repos = self.repo_counts();
        if !repos.is_empty() {
            components::section_title(ui, "Repositories");
            ScrollArea::vertical().id_salt("repositories").auto_shrink([false, true]).show(ui, |ui| {
                for (forge, repo, count) in &repos {
                    components::repo_row(ui, *forge, repo, *count);
                }
            });
        }
    }

    /// Each repository of the listed changes with how many of them are waiting, the most first.
    fn repo_counts(&self) -> Vec<(Forge, String, usize)> {
        let mut counts: Vec<(Forge, String, usize)> = Vec::new();
        for change in &self.changes {
            match counts.iter_mut().find(|(forge, repo, _)| *forge == change.forge && *repo == change.repo) {
                Some(found) => found.2 += 1,
                None => counts.push((change.forge, change.repo.clone(), 1)),
            }
        }
        counts.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
        counts
    }

    /// Whether the forge filter of the list lets `entry` through.
    fn shown(&self, entry: usize) -> bool {
        self.first_of(entry).is_some_and(|change| self.forge_filter.is_none_or(|forge| change.forge == forge))
    }

    fn list(&mut self, ui: &mut Ui) {
        let syncing = self.sources.iter().filter(|source| matches!(source.sync, Sync::Checking(_))).count();
        let blocked = self.sources.iter().any(|source| matches!(&source.sync, Sync::Done(state) if state.incomplete()));
        let truncated = self.truncated();
        let p = theme::pal(ui);
        let n = self.changes.len();
        let mut status = match (syncing, truncated) {
            (0, false) => format!("{n} open."),
            (0, true) => format!("{n}+ open."),
            (_, _) => format!("{n} so far. Syncing your sources, {} of {} answered.", self.sources.len() - syncing, self.sources.len()),
        };
        if blocked {
            status.push_str(" A source is paused or failed, so this count is incomplete.");
        }
        if self.sources.iter().any(|s| s.stale.is_some()) {
            status.push_str(" The last re-check of a source failed, so this list may be out of date.");
        }
        let count = |forge: Forge| self.changes.iter().filter(|change| change.forge == forge).count();
        let options = [String::from("All"), format!("GitHub {}", count(Forge::GitHub)), format!("GitLab {}", count(Forge::GitLab))];
        let selected = match self.forge_filter {
            None => 0,
            Some(Forge::GitHub) => 1,
            Some(Forge::GitLab) => 2,
        };
        let plain = status.strip_suffix(" open.").is_some_and(|n| n.trim_end_matches('+').chars().all(|c| c.is_ascii_digit()));
        let (live, picked) = components::padded(ui, Margin { left: 20, right: 20, top: 14, bottom: 12 }, |ui| {
            ui.set_width(ui.available_width());
            let mut picked = None;
            let mut live = None;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                components::label(ui, "Review requested", 16.0, p.graphite);
                if plain {
                    live = Some(components::label(ui, status.replace('.', ""), 12.0, p.text_tertiary));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    picked = components::segmented(ui, "forge-filter", "Forge filter", &options, selected);
                });
            });
            if !plain {
                live = Some(components::note(ui, &status, p.text_secondary));
            }
            (live, picked)
        });
        // Rule 21: rows arrive while focus sits elsewhere, so the count is a live region.
        if let Some(live) = live {
            ui.ctx().accesskit_node_builder(live.id, |node| node.set_live(egui::accesskit::Live::Polite));
        }
        match picked {
            Some(1) => self.forge_filter = Some(Forge::GitHub),
            Some(2) => self.forge_filter = Some(Forge::GitLab),
            Some(_) => self.forge_filter = None,
            None => {}
        }
        let top = ui.cursor().top();
        components::seam_top(ui, Rect::from_min_size(egui::pos2(ui.max_rect().left(), top), egui::vec2(ui.max_rect().width(), 1.0)));
        ui.add_space(1.0);
        if self.changes.is_empty() && syncing == 0 {
            // Rule 15: a paused source is not an empty queue.
            let stale = self.sources.iter().any(|s| s.stale.is_some());
            let (title, note) = if blocked || stale {
                (
                    "Nothing listed yet",
                    "A source is paused, failed or could not be re-checked, so an empty list is not an all-clear. Sources on the left says how to fix it.",
                )
            } else {
                ("Nothing waits on you", "Every source answered and none has a request for you. Re-check a source to look again.")
            };
            components::padded(ui, Margin::symmetric(20, 36), |ui| components::empty_state(ui, title, note, &[]));
            return;
        }
        // A forge filter that hides every row is not an empty queue either: the other forge's rows are still there.
        if !(0..self.entries.len()).any(|e| self.shown(e)) {
            let (name, hidden) = match self.forge_filter {
                Some(forge) => (forge.name(), self.changes.iter().filter(|c| c.forge != forge).count()),
                None => ("this forge", 0),
            };
            let note = format!("{hidden} from the other forge wait on you. Choose All to see them.");
            let shown = components::padded(ui, Margin::symmetric(20, 36), |ui| {
                components::empty_state(ui, &format!("No {name} changes wait on you"), &note, &[("Show all", "")])
            });
            if shown == Some(0) {
                self.forge_filter = None;
            }
            return;
        }
        let now = Utc::now();
        let mut clicked = None;
        let mut opened = None;
        let mut menu_for = None;
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for entry in (0..self.entries.len()).filter(|&e| self.shown(e)) {
                let members: Vec<&Change> = self.entries[entry].iter().filter_map(|&i| self.changes.get(i)).collect();
                let Some(&change) = members.first() else { continue };
                let selected = self.selected.as_deref() == Some(change.url.as_str());
                // Keyed by URL so focus follows the change when a source answers and the list re-sorts.
                let row = if change.stack.is_none() {
                    let meta = format!("{}  ·  {}  ·  {}", change.reference(), change.author, age(now, change.updated_at));
                    let model = components::InboxRow { forge: change.forge, title: &change.title, meta, draft: change.draft, unread: true, selected };
                    components::inbox_row(ui, change.url.as_str(), &model)
                } else {
                    components::stack_row(ui, change.url.as_str(), &stack_model(&members, now, selected))
                };
                if row.clicked() {
                    clicked = Some(change.url.clone());
                }
                // Right-click, or the context-menu key on a focused row: the menu belongs to this row.
                let by_key = row.has_focus() && keymap::pressed(ui, "app.context_menu");
                if row.secondary_clicked() || by_key {
                    let at = if by_key { menu::below(row.rect) } else { ui.input(|i| i.pointer.interact_pos()).unwrap_or_else(|| row.rect.center()) };
                    let name: String = change.title.chars().take(18).collect();
                    let cut = if name.chars().count() < change.title.chars().count() { "…" } else { "" };
                    let header = format!("{name}{cut} · {}", change.reference());
                    menu_for = Some((change.url.clone(), at, row.id, change.forge.name(), header));
                }
                if row.has_focus() && keymap::pressed(ui, "inbox.open") {
                    opened = Some((change.change_ref(), change.title.clone()));
                }
                if selected && self.focus_selected {
                    row.request_focus();
                    row.scroll_to_me(None);
                }
            }
        });
        self.focus_selected = false;
        if let Some((url, at, restore, forge, header)) = menu_for {
            self.selected = Some(url.clone());
            menu::open(ui.ctx(), inbox_menu(ui, url, at, restore, forge, header));
        }
        if let Some(choice) = menu::take(ui.ctx(), "inbox") {
            self.run_inbox_menu(ui.ctx(), &choice);
        }
        if clicked.is_some() {
            self.selected = clicked;
        }
        if let Some((change, title)) = opened {
            self.review = Some(review::ReviewScreen::open(ui.ctx(), change, title, false));
        }
    }

    /// The reading pane: where the change is, its title and who wrote it, and the action bar at the foot.
    fn detail(&mut self, ui: &mut Ui, pane: Rect) {
        let Some(change) = self.selected_index().and_then(|i| self.first_of(i)) else {
            components::empty_pane(ui, "Nothing selected", &select_hint(ui, "a change"));
            return;
        };
        let p = theme::pal(ui);
        let tag = if change.draft { (components::TagKind::OnTheWay, "Draft") } else { (components::TagKind::YourTurn, "Your turn") };
        detail_head(ui, change, Some(tag));
        let by = format!("By {}, updated {} ago.", change.author, age(Utc::now(), change.updated_at));
        components::note_sized(ui, by, 13.0, p.text_secondary);
        if let Some(place) = change.stack {
            let note = format!("Layer {} of {} in a stack. Review opens this layer on its own diff.", place.index + 1, place.size);
            components::note_sized(ui, note, 13.0, p.text_secondary);
        }
        let open = open_buttons(ui, pane, change).then(|| (change.change_ref(), change.title.clone()));
        if let Some((change, title)) = open {
            self.review = Some(review::ReviewScreen::open(ui.ctx(), change, title, false));
        }
    }

    fn show(&mut self, view: Inbox) {
        if self.view != view {
            self.view = view;
            self.selected = None;
        }
        if view == Inbox::LocalDrafts {
            self.drafts_stale = true;
        }
    }

    fn syncing(&self) -> bool {
        self.sources.iter().any(|s| matches!(s.sync, Sync::Checking(_)))
            || self.back.iter().any(|b| matches!(b.sync, BackFetch::Checking(_)))
            || self.mine.iter().any(|m| m.state.is_none() && m.pending())
    }

    fn refreshing(&self) -> bool {
        self.sources.iter().any(|s| s.refresh.is_some())
            || self.back.iter().any(|b| b.refresh.is_some())
            || self.mine.iter().any(|m| m.state.is_some() && m.pending())
    }

    /// Asks every source again when the poll interval of Settings → Offline and sync has passed, slower while the Mac runs on
    /// battery and the reader asked for that. The lists stay as they are until each source answers.
    fn poll_due(&mut self, ctx: &Context) {
        let on_battery = self.battery.on_battery(ctx) == Some(true);
        let chosen = prefs::get(ctx).settings.poll_every(on_battery);
        let hosts = pr_craft_engine::quota::status(chosen, Utc::now());
        let every = Duration::from_secs(60 * u64::from(poll_minutes(chosen, &hosts)));
        let waited = self.last_check.elapsed();
        if waited < every {
            ctx.request_repaint_after(every - waited);
            return;
        }
        if self.syncing() || self.refreshing() {
            return;
        }
        self.polling = true;
        self.last_check = Instant::now();
        let skip = |forge: Forge| forge == Forge::GitHub && github_paused(&hosts);
        for source in self.sources.iter_mut().filter(|s| !skip(s.forge)) {
            if let Sync::Checking(rx) = start_sync(ctx, source.forge) {
                source.refresh = Some(rx);
            }
        }
        for back in self.back.iter_mut().filter(|b| !skip(b.forge)) {
            if let BackFetch::Checking(rx) = start_back(ctx, back.forge) {
                back.refresh = Some(rx);
            }
        }
        for m in self.mine.iter_mut().filter(|m| !skip(m.forge)) {
            m.restart(ctx);
        }
    }

    /// The keys of the rows J and K walk in the shown list, top first.
    fn nav_ids(&self) -> Vec<String> {
        match self.view {
            Inbox::Queue => (0..self.entries.len()).filter(|&e| self.shown(e)).filter_map(|e| self.first_of(e)).map(|c| c.url.clone()).collect(),
            Inbox::BackToYou => {
                self.yours_items().iter().map(|m| m.change.url.clone()).chain(self.back_items().iter().map(|i| i.change.url.clone())).collect()
            }
            Inbox::Mine => self.mine_ids(),
            Inbox::Notifications => self.notifications.iter().flat_map(|n| &n.events).map(|e| e.id.clone()).collect(),
            Inbox::LocalDrafts => self.local_drafts_ids(),
        }
    }

    /// The reviewed changes of every forge that has answered: those back to you first, then those with the author, newest first.
    fn back_items(&self) -> Vec<&BackItem> {
        let listed = self.back.iter().filter_map(|b| match &b.sync {
            BackFetch::Done(BackState::Listed { items, .. }) => Some(items.iter()),
            _ => None,
        });
        let mut items: Vec<&BackItem> = listed.flatten().collect();
        sort_back(&mut items);
        items
    }

    /// `…` while a source is syncing, `+` when a reviewed change may be missing from the count, else nothing.
    fn back_marker(&self) -> &'static str {
        let mut marker = "";
        for back in &self.back {
            match &back.sync {
                BackFetch::Checking(_) => return "…",
                BackFetch::Done(BackState::Listed { truncated, incomplete, .. }) if *truncated || incomplete.is_some() => marker = "+",
                BackFetch::Done(BackState::Listed { .. }) => {}
                BackFetch::Done(_) => marker = "+",
            }
        }
        marker
    }

    /// Builds the notifications from the sources that have answered, with their seen marks and the pause from this Mac.
    fn refresh_notifications(&mut self) {
        self.notify_dirty = false;
        let back: Vec<BackSource> = self
            .back
            .iter()
            .filter_map(|b| match &b.sync {
                BackFetch::Done(state) => Some(BackSource { forge: b.forge, cli: b.forge.cli(), state: state.clone() }),
                BackFetch::Checking(_) => None,
            })
            .collect();
        let requested: Vec<InboxSource> = self
            .sources
            .iter()
            .filter_map(|s| match &s.sync {
                Sync::Done(state) => Some(InboxSource { forge: s.forge, cli: s.forge.cli(), state: state.clone() }),
                Sync::Checking(_) => None,
            })
            .collect();
        match turn::compose(&back, &requested) {
            Ok(notifications) => {
                self.notifications = Some(notifications);
                self.notify_error = None;
            }
            Err(error) => self.notify_error = Some(error.to_string()),
        }
    }

    fn toggle_pause(&mut self) {
        // Until the notifications are shown there is no pause state on screen to flip.
        if self.notifications.is_none() {
            return;
        }
        // The engine flips the stored pause, not what this window last showed; what it returns is what shows now.
        match turn::toggle_pause() {
            Ok(status) => {
                if let Some(notes) = &mut self.notifications {
                    notes.pause = status.pause;
                }
            }
            Err(error) => self.notify_error = Some(error.to_string()),
        }
    }

    fn mark_seen(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        match turn::seen(ids.clone()) {
            Ok(_) => {
                if let Some(notes) = &mut self.notifications {
                    for event in notes.events.iter_mut().filter(|e| ids.contains(&e.id)) {
                        event.seen = true;
                    }
                    notes.unseen = notes.events.iter().filter(|e| !e.seen).count();
                }
            }
            Err(error) => self.notify_error = Some(error.to_string()),
        }
    }

    fn mark_all_seen(&mut self) {
        let ids = self.notifications.iter().flat_map(|n| &n.events).filter(|e| !e.seen).map(|e| e.id.clone()).collect();
        self.mark_seen(ids);
    }

    /// Back to you (Figma frames 21 and 45): the changes the user reviewed, with whose turn each is.
    fn back_list(&mut self, ui: &mut Ui) {
        let syncing = self.back.iter().filter(|b| matches!(b.sync, BackFetch::Checking(_))).count();
        let items = self.back_items();
        let yours: Vec<pr_craft_core::mine::Mine> = self.yours_items().into_iter().cloned().collect();
        let waiting = items.iter().filter(|i| i.is_back_to_you()).count() + yours.len();
        let blocked = self.back.iter().any(|b| matches!(b.sync, BackFetch::Done(BackState::Paused { .. } | BackState::Failed { .. })));
        let truncated = self.back.iter().any(|b| matches!(&b.sync, BackFetch::Done(BackState::Listed { truncated: true, .. })));
        let incomplete: Vec<&str> = self
            .back
            .iter()
            .filter_map(|b| match &b.sync {
                BackFetch::Done(BackState::Listed { incomplete: Some(text), .. }) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let p = theme::pal(ui);
        let mut status = if syncing == 0 {
            format!("{waiting} waiting on you.")
        } else {
            format!("{waiting} so far. Syncing your sources, {} of {} answered.", self.back.len() - syncing, self.back.len())
        };
        // Rule 15: a review that could not be read is unknown, not "with the author".
        if blocked || truncated || !incomplete.is_empty() {
            status.push_str(" Some reviews could not be read or listed, so this count may be low.");
        }
        if self.back.iter().any(|b| b.stale.is_some()) {
            status.push_str(" The last re-check failed, so this list may be out of date.");
        }
        // Rule 15: unread reviews are not "nobody is waiting".
        let all_read = incomplete.is_empty();
        components::padded(ui, Margin { left: 20, right: 20, top: 14, bottom: 12 }, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                components::label(ui, "Back to you", 16.0, p.graphite);
                let count = components::label(ui, format!("{waiting} waiting on you"), 12.0, p.text_tertiary);
                // Rule 21: rows arrive while focus sits elsewhere, so the count is a live region; its full sentence is its value.
                ui.ctx().accesskit_node_builder(count.id, |node| {
                    node.set_live(egui::accesskit::Live::Polite);
                    node.set_value(status.clone());
                });
            });
            // The sentence is drawn only where it says more than the count.
            if status != format!("{waiting} waiting on you.") {
                live_label(ui, status.clone(), p.text_secondary);
            }
            components::note(ui, "Changes you reviewed that came back: the author pushed, replied or asked again.", p.text_tertiary);
            for text in incomplete {
                live_label(ui, text, p.ember_text);
            }
            for back in &self.back {
                let (name, cli) = (back.forge.name(), back.forge.cli());
                match &back.sync {
                    BackFetch::Done(BackState::Failed { error }) => {
                        live_label(ui, format!("{name}: could not read your reviews: {error}"), p.rose_text);
                    }
                    BackFetch::Done(BackState::Paused { .. }) => {
                        live_label(
                            ui,
                            format!("{name}: {cli} cannot act for you yet, so its reviews are not listed. Sources on the left says how to fix it."),
                            p.ember_text,
                        );
                    }
                    BackFetch::Done(BackState::Listed { truncated: true, .. }) => {
                        live_label(
                            ui,
                            format!("{cli} returned its limit of {}; older reviews may be missing.", pr_craft_forge::PAGE),
                            p.text_secondary,
                        );
                    }
                    _ => {}
                }
            }
            if waiting == 0 && syncing == 0 && !blocked && !truncated && all_read && !self.back.iter().any(|b| b.stale.is_some()) {
                ui.add_space(theme::SPACE_3);
                components::label(ui, "Nobody is waiting on you", 16.0, p.graphite);
                let note = "When an author answers one of your reviews, it comes back here with only what changed since you looked.";
                components::note(ui, note, p.text_secondary);
            }
        });
        components::rule(ui);
        let now = Utc::now();
        let (mut clicked, mut opened, mut opened_mine) = (None, None, None);
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if !yours.is_empty() {
                components::group_band(ui, "Your changes", yours.len(), "A reviewer is waiting on you");
            }
            for m in &yours {
                let selected = self.selected.as_deref() == Some(m.change.url.as_str());
                let status = m.status();
                let model = components::MineRow {
                    forge: m.change.forge,
                    title: &m.change.title,
                    meta: format!("{}  ·  you  ·  {}", m.change.reference(), age(now, m.change.updated_at)),
                    size: m.size(),
                    status: &status.text,
                    tone: status.tone,
                    waiting: true,
                    selected,
                };
                let row = components::mine_row(ui, m.change.url.as_str(), &model);
                if row.clicked() {
                    clicked = Some(m.change.url.clone());
                }
                if row.has_focus() && keymap::pressed(ui, "inbox.open") {
                    opened_mine = Some((m.change.change_ref(), m.change.title.clone()));
                }
                if selected && self.focus_selected {
                    row.request_focus();
                    row.scroll_to_me(None);
                }
            }
            let mut section = None;
            for item in &items {
                if section != Some(item.is_back_to_you()) {
                    section = Some(item.is_back_to_you());
                    let count = items.iter().filter(|i| i.is_back_to_you() == item.is_back_to_you()).count();
                    if item.is_back_to_you() {
                        components::group_band(ui, "Waiting on you", count, "");
                    } else {
                        components::group_band(ui, "With the author", count, "Not counted · comes back when they answer");
                    }
                }
                let selected = self.selected.as_deref() == Some(item.change.url.as_str());
                let (change, review) = (&item.change, &item.assessment.review);
                let when = review.submitted_at.map(|at| format!(" {}", age(now, at))).unwrap_or_default();
                let next = if item.is_back_to_you() { item.reasons_text() } else { "nothing new since your review".to_string() };
                let model = components::ReviewedRow {
                    forge: change.forge,
                    title: &change.title,
                    meta: format!("{}  ·  {}", change.reference(), change.author),
                    line: format!("{}{when}  →  {next}", review.state.you()),
                    right: if item.is_back_to_you() {
                        format!("Waiting on you {}", age(now, change.updated_at))
                    } else {
                        format!("With {}", change.author)
                    },
                    yours: item.is_back_to_you(),
                    selected,
                };
                let row = components::reviewed_row(ui, change.url.as_str(), &model);
                if row.clicked() {
                    clicked = Some(change.url.clone());
                }
                if row.has_focus() && keymap::pressed(ui, "inbox.open") {
                    opened = Some((change.change_ref(), change.title.clone()));
                }
                if selected && self.focus_selected {
                    row.request_focus();
                    row.scroll_to_me(None);
                }
            }
        });
        self.focus_selected = false;
        if clicked.is_some() {
            self.selected = clicked;
        }
        if let Some((change, title)) = opened {
            self.review = Some(review::ReviewScreen::open(ui.ctx(), change, title, true));
        }
        if let Some((change, title)) = opened_mine {
            self.open_review(ui.ctx(), change, title);
        }
    }

    fn back_detail(&mut self, ui: &mut Ui, pane: Rect) {
        if let Some(mine) = self.selected.as_deref().and_then(|url| self.yours_items().into_iter().find(|m| m.change.url == url)).cloned() {
            self.show_panel(ui, pane, mine);
            return;
        }
        let items = self.back_items();
        let Some(item) = self.selected.as_deref().and_then(|url| items.iter().find(|i| i.change.url == url)) else {
            components::empty_pane(ui, "Nothing selected", &select_hint(ui, "a change"));
            return;
        };
        let (change, now, p) = (&item.change, Utc::now(), theme::pal(ui));
        detail_head(ui, change, None);
        let by = format!("By {}, updated {} ago.", change.author, age(now, change.updated_at));
        components::note_sized(ui, by, 13.0, p.text_secondary);
        let review = &item.assessment.review;
        let when = review.submitted_at.map(|at| format!(" {} ago", age(now, at))).unwrap_or_default();
        let commits = match item.assessment.commits_since {
            1 => "1 commit".to_string(),
            n => format!("{n} commits"),
        };
        let line = format!("{}{when}. {commits} since.", review.state.you());
        components::note_sized(ui, line, 13.0, p.text_secondary);
        ui.add_space(theme::SPACE_2);
        if let Turn::BackToYou { reasons } = &item.assessment.turn {
            components::group_label(ui, "Since your review");
            let count = reasons.len() + 1;
            let grey = p.text_tertiary;
            let first = format!("{}{when}", review.state.you());
            components::timeline_row(ui, grey, &first, &review.submitted_at.map(|at| age(now, at)).unwrap_or_default(), true, count == 1);
            for (i, reason) in reasons.iter().enumerate() {
                components::timeline_row(ui, p.ember, &reason.text(), "", false, i + 1 == reasons.len());
            }
        } else {
            let note = format!(
                "With {}. It comes back when they push, reply, resolve your threads, ask you again or mention you, or when your approval is dismissed.",
                change.author
            );
            components::note_sized(ui, note, 13.0, p.text_secondary);
        }
        ui.add_space(theme::SPACE_3);
        let open = open_buttons(ui, pane, change).then(|| (change.change_ref(), change.title.clone()));
        let hint = format!(
            "In the review, {} shows only what changed since your review and {} hands it off.",
            keymap::label(ui, "review.since_review"),
            keymap::label(ui, "review.hand_off")
        );
        components::note(ui, hint, p.text_tertiary);
        if let Some((change, title)) = open {
            self.review = Some(review::ReviewScreen::open(ui.ctx(), change, title, true));
        }
    }

    /// Notifications (Figma frame 07), kept in this window: banners, the Dock badge and the menu-bar item need platform code that is not built.
    fn notifications_list(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        let pad = Margin { left: 20, right: 20, top: 14, bottom: 12 };
        let Some(notes) = &self.notifications else {
            components::padded(ui, pad, |ui| {
                components::label(ui, "Notifications", 16.0, p.graphite);
                if let Some(error) = &self.notify_error {
                    live_label(ui, error.as_str(), p.rose_text);
                }
                live_label(ui, "Notifications appear once your sources have answered.", p.text_secondary);
            });
            return;
        };
        let paused = notes.pause.is_some();
        let (mut pause, mut all_seen) = (false, false);
        components::padded(ui, pad, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                components::label(ui, "Notifications", 16.0, p.graphite);
                let count = components::label(ui, format!("{} new of {}", notes.unseen, notes.events.len()), 12.0, p.text_tertiary);
                ui.ctx().accesskit_node_builder(count.id, |node| node.set_live(egui::accesskit::Live::Polite));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let seen = Pill::new(PillKind::Secondary, "Mark all seen").key(ui, "inbox.mark_seen").small().enabled(notes.unseen > 0);
                    all_seen = seen.show(ui).clicked();
                    pause = Pill::new(PillKind::Secondary, if paused { "Resume" } else { "Pause" }).key(ui, "inbox.pause").small().show(ui).clicked();
                });
            });
            if let Some(error) = &self.notify_error {
                live_label(ui, error.as_str(), p.rose_text);
            }
            let state = pause_line(notes.pause.as_ref());
            live_label(ui, state, p.text_tertiary);
            components::note(ui, notes.system_notice, p.text_tertiary);
            // Rule 15: a source that could not be read leaves its events out, and the list says so.
            for error in &notes.errors {
                live_label(ui, error.as_str(), p.ember_text);
            }
        });
        components::rule(ui);
        let now = Utc::now();
        let (mut clicked, mut opened) = (None, None);
        if notes.events.is_empty() {
            let (title, body) = self.notifications_empty(notes);
            components::padded(ui, Margin::symmetric(20, 36), |ui| components::empty_state(ui, title, &body, &[]));
        }
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for event in &notes.events {
                let selected = self.selected.as_deref() == Some(event.id.as_str());
                let meta = format!("{}  ·  {}", event.change.reference(), event.change.author);
                let when = age(now, event.at);
                let model =
                    components::EventRow { forge: event.change.forge, text: &event.text, meta: &meta, age: &when, unseen: !event.seen, selected };
                let row = components::event_row(ui, &event.id, &model);
                if row.clicked() {
                    clicked = Some(event.id.clone());
                }
                if row.has_focus() && keymap::pressed(ui, "inbox.open") {
                    opened = Some((event.id.clone(), event.change.change_ref(), event.change.title.clone(), event.kind == EventKind::BackToYou));
                }
                if selected && self.focus_selected {
                    row.request_focus();
                    row.scroll_to_me(None);
                }
            }
        });
        self.focus_selected = false;
        if clicked.is_some() {
            self.selected = clicked;
        }
        if pause {
            self.toggle_pause();
        }
        if all_seen {
            self.mark_all_seen();
        }
        if let Some((id, change, title, from_back)) = opened {
            self.mark_seen(vec![id]);
            self.review = Some(review::ReviewScreen::open(ui.ctx(), change, title, from_back));
        }
    }

    /// What an empty Notifications list says (frame 109). Rule 15: "caught up" is said only when every source answered, none is
    /// paused or failed, notifications are not paused and no source was left out; otherwise it says what is missing.
    fn notifications_empty(&self, notes: &turn::Notifications) -> (&'static str, String) {
        if !notes.errors.is_empty() {
            return ("Nothing to show yet", "A source could not be read, so events may be missing. The notes above say which.".into());
        }
        if self.syncing() {
            return ("Still checking your sources", "Events appear once every source has answered.".into());
        }
        let broken = self.sources.iter().any(|s| s.stale.is_some() || matches!(&s.sync, Sync::Done(state) if state.incomplete()));
        if broken {
            return (
                "Nothing to show yet",
                "A source is paused or could not be re-checked, so this list may be missing events. Sources on the left says how to fix it.".into(),
            );
        }
        if notes.pause.is_some() {
            return ("Notifications are paused", "Resume to see what arrived while they were paused.".into());
        }
        ("You are caught up", "New commits, replies, mentions and review requests from both forges land here. Nothing has arrived yet.".into())
    }

    fn notification_detail(&mut self, ui: &mut Ui, pane: Rect) {
        let found = self.selected.as_deref().and_then(|id| self.notifications.as_ref()?.events.iter().find(|e| e.id == id));
        let Some(event) = found else {
            components::empty_pane(ui, "Nothing selected", &select_hint(ui, "a notification"));
            return;
        };
        let p = theme::pal(ui);
        detail_head(ui, &event.change, None);
        components::note_sized(ui, &event.text, 13.0, p.text_secondary);
        let open = open_buttons(ui, pane, &event.change)
            .then(|| (event.id.clone(), event.change.change_ref(), event.change.title.clone(), event.kind == EventKind::BackToYou));
        if let Some((id, change, title, from_back)) = open {
            self.mark_seen(vec![id]);
            self.review = Some(review::ReviewScreen::open(ui.ctx(), change, title, from_back));
        }
    }

    fn truncated(&self) -> bool {
        self.sources.iter().any(|source| matches!(source.sync, Sync::Done(SourceState::Listed { truncated: true, .. })))
    }
}

/// What covers the inbox and the review: Settings (⌘,) or Activity (⌥⌘J).
enum Overlay {
    None,
    Settings(Box<settings::Settings>),
    Activity(Box<activity::Activity>),
}

impl PrCraftApp {
    /// Puts `to` over the screen, unless the open Settings holds an agent form with unsaved changes: replacing it would
    /// drop what was typed. Says why in the status line instead. Every way to another overlay comes through here.
    fn switch_overlay(&mut self, to: Overlay) -> bool {
        if matches!(&self.overlay, Overlay::Settings(settings) if settings.dirty()) {
            self.runs.status = Some(Err("The agent has unsaved changes in Settings. Save the agent, or use Discard changes, first.".into()));
            return false;
        }
        self.overlay = to;
        true
    }

    /// ⌘, opens Settings and ⌥⌘J opens Activity, from any screen.
    fn global_keys(&mut self, ui: &Ui) {
        if keymap::consume(ui, "app.settings") {
            self.switch_overlay(Overlay::Settings(Box::new(settings::Settings::open(settings::Tab::Engine))));
        }
        if keymap::consume(ui, "app.activity") && self.switch_overlay(Overlay::Activity(Box::new(activity::Activity::new()))) {
            self.popover = false;
        }
        if keymap::consume(ui, "app.palette") {
            if self.palette.is_some() {
                self.close_palette(ui.ctx());
            } else {
                self.palette = Some(palette::Palette::open());
            }
        }
    }

    /// The title bar: the app's fixed items on the right, the trail on the left where no screen draws its own.
    fn title_bar(&mut self, ui: &mut Ui, bar: &mut Bar) {
        let mut want = None;
        let p = theme::pal(ui);
        let wide = bar.width() >= theme::WIDE_WINDOW;
        bar.right(ui, |ui| {
            if components::title_action(ui, "Settings", &keymap::label(ui, "app.settings")).clicked() {
                want = Some(Overlay::Settings(Box::new(settings::Settings::open(settings::Tab::Engine))));
            }
            match activity::pill_ui(ui, &self.runs) {
                activity::PillAct::None => {}
                activity::PillAct::Popover => self.popover = !self.popover,
                activity::PillAct::Activity => want = Some(Overlay::Activity(Box::new(activity::Activity::new()))),
            }
            if wide {
                components::tag(ui, components::TagKind::Neutral, &self.sync_text());
            }
            if let Some(notes) = self.notifications.as_ref().filter(|n| n.unseen > 0) {
                components::status_pill(ui, p.ember, &format!("{} new", notes.unseen));
            }
        });
        ui.ctx().request_repaint_after(Duration::from_secs(30));
        if (self.review.is_none() && self.checks.is_none()) || !matches!(self.overlay, Overlay::None) {
            let place = match &self.overlay {
                Overlay::Settings(_) => "Settings",
                Overlay::Activity(_) => "Activity",
                Overlay::None => "Inbox",
            };
            let list = match self.view {
                Inbox::Queue => "Review requested",
                Inbox::BackToYou => "Back to you",
                Inbox::Mine => "Your changes",
                Inbox::Notifications => "Notifications",
                Inbox::LocalDrafts => "Local drafts",
            };
            let page = match &self.overlay {
                Overlay::Settings(settings) => settings.page_name(),
                _ => "",
            };
            let trail: &[&str] = match &self.overlay {
                Overlay::None => &[place, list],
                _ if page.is_empty() => &[place],
                _ => &[place, page],
            };
            bar.left(ui, |ui| components::breadcrumb(ui, trail));
        }
        if let Some(overlay) = want {
            self.switch_overlay(overlay);
        }
    }

    /// How long ago the sources were last asked, for the title bar.
    fn sync_text(&self) -> String {
        if self.syncing() || self.refreshing() {
            return "Syncing…".into();
        }
        match self.last_check.elapsed().as_secs() {
            0..60 => "Synced just now".into(),
            seconds => format!("Synced {} min ago", seconds / 60),
        }
    }

    /// The status bar: one dot and a few words per source.
    fn status_bar(&self, ui: &mut Ui, bar: &Bar) {
        let p = theme::pal(ui);
        bar.left(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            for source in &self.sources {
                let cli = source.forge.cli();
                let (colour, text) = match &source.sync {
                    Sync::Checking(_) => (p.haze, format!("{cli} · syncing")),
                    Sync::Done(state @ SourceState::Listed { changes, truncated, hosts, .. }) => {
                        let waiting = format!("{cli} · {}{} waiting", changes.len(), if *truncated { "+" } else { "" });
                        match hosts.iter().filter(|h| h.error.is_some()).count() {
                            0 => (p.moss, waiting),
                            failed => {
                                debug_assert!(state.incomplete());
                                (p.rose, format!("{waiting} · {failed} host{} failed", if failed == 1 { "" } else { "s" }))
                            }
                        }
                    }
                    Sync::Done(SourceState::Paused { .. }) => (p.ember, format!("{cli} · paused")),
                    Sync::Done(SourceState::Failed { .. }) => (p.rose, format!("{cli} · sync failed")),
                };
                components::connection(ui, colour, &text);
            }
        });
    }

    /// The window: title bar, body, status bar. The body is Settings, Activity, a review, or the inbox.
    fn window(&mut self, ui: &mut Ui) {
        let (p, spacing, avail) = (theme::pal(ui), ui.spacing().item_spacing, ui.available_size());
        ui.painter().rect_filled(ui.available_rect_before_wrap(), 0.0, p.paper);
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        // Which kind of input moved focus last, and the open context menu: both before any screen reads a key.
        theme::note_input(ui.ctx());
        menu::show(ui.ctx());
        self.global_keys(ui);
        let mut title = components::title_bar(ui);
        self.title_bar(ui, &mut title);
        let body = ui.allocate_space(egui::vec2(avail.x, (avail.y - theme::TITLE_BAR_HEIGHT - theme::STATUS_BAR_HEIGHT).max(0.0))).1;
        let status = components::status_bar(ui);
        self.status_bar(ui, &status);
        let mut inner = ui.new_child(UiBuilder::new().id_salt("body").max_rect(body).layout(Layout::top_down(Align::Min)));
        inner.set_clip_rect(body);
        inner.spacing_mut().item_spacing = spacing;
        self.body(&mut inner, &mut title);
    }

    fn body(&mut self, ui: &mut Ui, title: &mut Bar) {
        self.palette_ui(ui.ctx());
        self.status_strip(ui);
        activity::confirm_window(ui.ctx(), &mut self.runs);
        if self.overlay(ui) {
            return;
        }
        if let Some(screen) = &mut self.review {
            match screen.ui(ui, &mut self.runs, title) {
                review::Outcome::Stay => {}
                review::Outcome::Back => {
                    self.review = None;
                    self.drafts_stale = true;
                    self.focus_selected = true;
                }
                review::Outcome::Open(change, title) => self.review = Some(review::ReviewScreen::open(ui.ctx(), change, title, false)),
                review::Outcome::NewAgent => {
                    self.switch_overlay(Overlay::Settings(Box::new(settings::Settings::new_agent(None))));
                }
            }
            return;
        }
        if let Some(screen) = &mut self.checks {
            if matches!(screen.ui(ui, &mut self.runs, title), checks::Outcome::Back) {
                self.checks = None;
                self.focus_selected = true;
            }
            return;
        }
        self.keys(ui);
        self.inbox(ui);
    }

    /// The outcome of the last action and any trouble reading the runs, in a strip under the title bar while there is one.
    fn status_strip(&mut self, ui: &mut Ui) {
        if self.runs.status.is_none() && self.runs.error.is_none() && self.runs.unreadable().is_empty() {
            return;
        }
        let fill = theme::pal(ui).sunken;
        let shown = egui::Frame::new().fill(fill).inner_margin(Margin::symmetric(20, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            activity::status_line(ui, &mut self.runs);
        });
        components::seam_bottom(ui, shown.response.rect);
    }

    /// Three panes split by seams: the sidebar on the hover tone, the list on the canvas, the reading pane on the panel tone.
    /// Under 1280 px the list gives way before the reading pane does.
    fn inbox(&mut self, ui: &mut Ui) {
        self.poll_local_drafts();
        if (self.local_drafts.is_none() || self.drafts_stale) && self.drafts_rx.is_none() {
            self.drafts_stale = false;
            self.refresh_local_drafts(ui.ctx());
        }
        let p = theme::pal(ui);
        let rect = ui.available_rect_before_wrap();
        let side = egui::vec2(theme::SIDEBAR_WIDTH + 1.0, rect.height());
        let list_width = (rect.width() - side.x - 1.0 - theme::DETAIL_MIN_WIDTH).clamp(theme::LIST_MIN_WIDTH, theme::LIST_WIDTH);
        let sidebar = Rect::from_min_size(rect.min, side);
        let list = Rect::from_min_size(sidebar.right_top(), egui::vec2(list_width + 1.0, rect.height()));
        let detail = Rect::from_min_max(list.right_top(), rect.right_bottom());
        components::pane_at(ui, "sidebar", sidebar, p.hover, Margin { left: 12, right: 13, top: 4, bottom: 8 }, |ui| self.sidebar(ui));
        components::seam_right(ui, sidebar);
        components::pane_at(ui, "list", list, p.paper, Margin::ZERO, |ui| match self.view {
            Inbox::Queue => self.list(ui),
            Inbox::BackToYou => self.back_list(ui),
            Inbox::Mine => self.mine_list(ui),
            Inbox::Notifications => self.notifications_list(ui),
            Inbox::LocalDrafts => self.local_drafts_list(ui),
        });
        components::seam_right(ui, list);
        components::pane_at(ui, "detail", detail, p.panel, Margin { left: 32, right: 32, top: 22, bottom: 0 }, |ui| match self.view {
            Inbox::Queue => self.detail(ui, detail),
            Inbox::BackToYou => self.back_detail(ui, detail),
            Inbox::Mine => self.mine_detail(ui, detail),
            Inbox::Notifications => self.notification_detail(ui, detail),
            Inbox::LocalDrafts => self.local_drafts_detail(ui),
        });
    }

    /// Opens another change's review, unless a post or hand-off of the open one is still on its way: replacing it would
    /// lose its answer and let a second submit post twice. Says so where the request came from.
    fn open_review(&mut self, ctx: &Context, change: pr_craft_core::ChangeRef, title: String) -> bool {
        // Leaving Settings for a review would drop an unsaved agent form.
        if matches!(&self.overlay, Overlay::Settings(settings) if settings.dirty()) {
            self.runs.status = Some(Err("The agent has unsaved changes in Settings. Save the agent, or use Discard changes, first.".into()));
            return false;
        }
        if self.review.as_ref().is_some_and(review::ReviewScreen::busy) {
            self.runs.status = Some(Err(review::WAIT_FOR_FORGE.into()));
            return false;
        }
        self.review = Some(review::ReviewScreen::open(ctx, change, title, false));
        true
    }

    /// Draws the popover and the screen that covers the rest. Returns true when the rest must not be drawn.
    fn overlay(&mut self, ui: &mut Ui) -> bool {
        if self.popover {
            match activity::popover(ui.ctx(), &mut self.runs) {
                activity::PopAct::None => {}
                activity::PopAct::Close => self.popover = false,
                activity::PopAct::Activity => {
                    if self.switch_overlay(Overlay::Activity(Box::new(activity::Activity::new()))) {
                        self.popover = false;
                    }
                }
                activity::PopAct::Open(change, title) => {
                    if self.open_review(ui.ctx(), change, title) {
                        self.popover = false;
                        self.overlay = Overlay::None;
                    }
                }
            }
        }
        let exit = match &mut self.overlay {
            Overlay::None => return false,
            Overlay::Settings(settings) => {
                if settings.ui(ui, &self.changes, &mut self.runs) {
                    self.overlay = Overlay::None;
                }
                activity::Exit::Stay
            }
            Overlay::Activity(activity) => activity.ui(ui, &mut self.runs),
        };
        match exit {
            activity::Exit::Stay => {}
            activity::Exit::Close => self.overlay = Overlay::None,
            activity::Exit::Open(change, title) => {
                if self.open_review(ui.ctx(), change, title) {
                    self.overlay = Overlay::None;
                }
            }
        }
        true
    }
}

impl PrCraftApp {
    /// Draws the palette over the screen when it is open, and acts on what the reader chose. It runs before every screen, so the
    /// keys it takes (Esc, ↑, ↓, ↵) never reach the screen under it.
    fn palette_ui(&mut self, ctx: &Context) {
        let Some(palette) = &mut self.palette else { return };
        let screen = match (&self.overlay, &self.review) {
            (Overlay::Settings(_), _) => Screen::Settings,
            (Overlay::Activity(_), _) => Screen::Activity,
            (Overlay::None, Some(_)) => Screen::Review,
            (Overlay::None, None) => Screen::Inbox,
        };
        let facts = match (&self.overlay, &self.review) {
            (Overlay::Settings(settings), _) => Some(Facts::Settings { page: settings.page_id(), agent_form_open: settings.agent_form_open() }),
            (Overlay::Activity(activity), _) => Some(Facts::Activity(activity.facts(&self.runs))),
            (Overlay::None, Some(screen)) => Some(Facts::Review(screen.facts(&self.runs))),
            _ => None,
        };
        palette.set_facts(facts);
        palette.set_scope(search_scope(&self.sources, self.changes.len()));
        match palette.ui(ctx, screen, &self.changes) {
            palette::Event::None => {}
            palette::Event::Close => self.close_palette(ctx),
            palette::Event::Run(target) => {
                self.close_palette(ctx);
                self.run_target(ctx, target);
            }
            palette::Event::Queue(n) => {
                self.close_palette(ctx);
                self.jump_to_queue(n);
            }
        }
    }

    /// Closes the palette and gives back the focus its field held: a screen under it must not read the next key as typing, and a
    /// key the palette ran must reach it as a key.
    fn close_palette(&mut self, ctx: &Context) {
        self.palette = None;
        if let Some(id) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        if let Some(screen) = &mut self.review {
            screen.forget_typing();
        }
        if let Overlay::Settings(settings) = &mut self.overlay {
            settings.forget_typing();
        }
    }

    fn run_target(&mut self, ctx: &Context, target: Target) {
        match target {
            // Leaving with a comment open would discard it (Esc cancels it): the review says so and stays.
            Target::Action { id } if id == "review.back" && self.review.is_some() => {
                if let Some(screen) = &mut self.review {
                    screen.request_back(ctx);
                }
            }
            // The key's own code path runs on the next frame.
            Target::Action { id } if id == "review.submit" => {
                if let Some(screen) = &mut self.review {
                    screen.submit_review(ctx);
                }
            }
            Target::Action { id } => keymap::trigger(ctx, &id),
            Target::Command { id } => self.run_command(ctx, &id),
            Target::Settings { page } => {
                if let Some(tab) = palette::tab(&page) {
                    self.switch_overlay(Overlay::Settings(Box::new(settings::Settings::open(tab))));
                }
            }
            Target::Change { forge, repo, number, host } => {
                let change = pr_craft_core::ChangeRef { forge, repo, number, host };
                let title = self.changes.iter().find(|c| c.change_ref() == change).map(|c| c.title.clone()).unwrap_or_default();
                if self.open_review(ctx, change, title) {
                    self.overlay = Overlay::None;
                }
            }
        }
    }

    /// The engine commands that have a button but no key (`pr_craft_core::palette::UI_COMMANDS`).
    fn run_command(&mut self, ctx: &Context, id: &str) {
        match id {
            "inbox.list" => self.recheck_all(ctx),
            "review.markdown" => self.copy_markdown(ctx),
            other => self.runs.status = Some(Err(format!("The palette cannot run {other}."))),
        }
    }

    /// Puts the open review on the clipboard as Markdown, as its own Copy as Markdown button does.
    fn copy_markdown(&mut self, ctx: &Context) {
        let Some(screen) = &self.review else { return };
        let (change, title) = screen.subject();
        let copied = pr_craft_engine::review::markdown(change, title).map_err(crate::fail);
        self.runs.status = Some(copied.map(|text| {
            ctx.copy_text(text);
            "Copied the review as Markdown.".to_string()
        }));
    }

    /// Shows queue `n` (Review requested, Back to you, Notifications) over whatever is open, unless a post or hand-off of the
    /// open review is still on its way.
    fn jump_to_queue(&mut self, n: usize) {
        if self.review.as_ref().is_some_and(review::ReviewScreen::busy) {
            self.runs.status = Some(Err(review::WAIT_FOR_FORGE.into()));
            return;
        }
        if !self.switch_overlay(Overlay::None) {
            return;
        }
        if self.review.take().is_some() {
            self.focus_selected = true;
            self.drafts_stale = true;
        }
        if let Some(view) = [Inbox::Queue, Inbox::BackToYou, Inbox::Notifications].get(n) {
            self.show(*view);
        }
    }

    /// Re-checks every source the sidebar's Re-check buttons would, each one on its own thread.
    fn recheck_all(&mut self, ctx: &Context) {
        let back_busy: Vec<Forge> = self.back.iter().filter(|b| b.refresh.is_some()).map(|b| b.forge).collect();
        let mut forges = Vec::new();
        for source in &mut self.sources {
            if recheck_allowed(source, &back_busy) {
                source.stale = None;
                source.sync = start_sync(ctx, source.forge);
                forges.push(source.forge);
            }
        }
        self.restarted(ctx, forges);
    }

    /// What follows a re-check of `forges` that has started: the poll clock, their Back-to-you lists and the notifications.
    fn restarted(&mut self, ctx: &Context, forges: Vec<Forge>) {
        // A re-check the reader asked for reads the forges, not the answer held for the poll.
        if !forges.is_empty() {
            pr_craft_engine::mine::invalidate();
        }
        if !forges.is_empty() {
            // The interval runs from when the re-check answers.
            self.polling = true;
            self.last_check = Instant::now();
        }
        for forge in forges {
            if let Some(m) = self.mine.iter_mut().find(|m| m.forge == forge) {
                m.recheck(ctx);
            }
            if let Some(back) = self.back.iter_mut().find(|b| b.forge == forge) {
                back.stale = None;
                // A poll of it already on its way started before this re-check; its answer is never shown.
                back.refresh = None;
                back.sync = start_back(ctx, forge);
            }
            self.notify_dirty = true;
        }
    }
}

/// Whether Re-check may start for `source`: not while a poll of it or of its Back-to-you list is in flight, because that poll's
/// answer would overwrite the one the reader asked for.
fn recheck_allowed(source: &Source, back_busy: &[Forge]) -> bool {
    matches!(source.sync, Sync::Done(_)) && source.refresh.is_none() && !back_busy.contains(&source.forge)
}

/// What a re-check on its way has answered: `None` while it has not, `Some(None)` when its thread ended without an answer.
fn arrived<T>(rx: &Receiver<T>) -> Option<Option<T>> {
    match rx.try_recv() {
        Ok(answer) => Some(Some(answer)),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some(None),
    }
}

fn start_sync(ctx: &Context, forge: Forge) -> Sync {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        // The window may have closed first; nobody is left to tell.
        let _ = tx.send(pr_craft_engine::inbox_source(forge));
        ctx.request_repaint();
    });
    Sync::Checking(rx)
}

/// Opens a forge page in the browser and counts it (`tally.bump`) on its own thread, so the frame never waits; a failed
/// count is ignored. `screen` is where the reader was; `file` is the path the open was about, if any.
pub(crate) fn open_on_forge(ctx: &Context, url: &str, screen: &str, change: &pr_craft_core::ChangeRef, file: Option<&str>) {
    ctx.open_url(OpenUrl::new_tab(url));
    let file_ext = file.and_then(|f| std::path::Path::new(f).extension()).and_then(|e| e.to_str());
    let params = serde_json::json!({ "screen": screen, "forge": change.forge, "repo": change.repo, "number": change.number, "file_ext": file_ext });
    count(params);
}

/// Tests must not write the real data folder (`unsafe` is forbidden, so they cannot point the engine at a temp one), so they keep the bumps here.
#[cfg(test)]
pub(crate) static BUMPED: std::sync::Mutex<Vec<serde_json::Value>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn count(params: serde_json::Value) {
    BUMPED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(params);
}

#[cfg(not(test))]
fn count(params: serde_json::Value) {
    std::thread::spawn(move || {
        let _ = pr_craft_engine::run("tally.bump", &params);
    });
}

/// Starts `inbox.back_to_you` for one forge on its own thread: it reads every change the user reviewed.
fn start_back(ctx: &Context, forge: Forge) -> BackFetch {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        // The window may have closed first; nobody is left to tell.
        let _ = tx.send(turn::back_source(forge));
        ctx.request_repaint();
    });
    BackFetch::Checking(rx)
}

/// Groups `changes` into inbox entries: a stack's layers share one entry, lowest layer first, placed where
/// its first-listed layer sits. Every other change is an entry of its own.
fn group(changes: &[Change]) -> Vec<Vec<usize>> {
    let mut entries: Vec<Vec<usize>> = Vec::new();
    for (i, change) in changes.iter().enumerate() {
        let same_stack = |head: &Change| {
            head.forge == change.forge
                && head.repo == change.repo
                && matches!((head.stack, change.stack), (Some(a), Some(b)) if (a.bottom, a.top) == (b.bottom, b.top))
        };
        let home = entries.iter_mut().find(|e| e.first().and_then(|&f| changes.get(f)).is_some_and(same_stack));
        match home {
            Some(entry) => entry.push(i),
            None => entries.push(vec![i]),
        }
    }
    for entry in &mut entries {
        entry.sort_by_key(|&i| changes.get(i).and_then(|c| c.stack).map_or(0, |s| s.index));
    }
    entries
}

/// A stack of the queue as the inbox draws it: the layers that wait on the user, and how many the whole stack has.
fn stack_model<'a>(members: &[&'a Change], now: DateTime<Utc>, selected: bool) -> components::StackRow<'a> {
    let first = members.first();
    let size = first.and_then(|c| c.stack).map_or(members.len(), |s| s.size);
    let newest = members.iter().map(|c| c.updated_at).max().unwrap_or(now);
    let mut meta = format!("{}  ·  {}", first.map(|c| c.author.as_str()).unwrap_or_default(), age(now, newest));
    if members.len() < size {
        meta.push_str(&format!("  ·  {} in your queue", members.len()));
    }
    components::StackRow {
        forge: first.map_or(Forge::GitHub, |c| c.forge),
        repo: first.map(|c| c.repo.as_str()).unwrap_or_default(),
        size,
        layers: members
            .iter()
            .rev()
            .map(|c| components::Layer { title: c.title.as_str(), reference: format!("{}{}", c.forge.number_sigil(), c.number), draft: c.draft })
            .collect(),
        meta,
        selected,
        unread: true,
    }
}

/// What the window says of a failed engine command: its message and the code a report can quote.
pub(crate) fn fail(error: pr_craft_engine::CommandError) -> String {
    format!("{error} ({})", error.code())
}

/// A status line that arrives while focus sits elsewhere: a polite live region (rule 21).
pub(crate) fn live_label(ui: &mut Ui, text: impl Into<String>, colour: egui::Color32) {
    let label = ui.label(RichText::new(text.into()).size(theme::sz(ui).body).color(colour));
    ui.ctx().accesskit_node_builder(label.id, |node| node.set_live(egui::accesskit::Live::Polite));
}

/// Minutes between polls: the chosen interval, stretched to the longest any GitHub host needs to stay within its hourly budget.
fn poll_minutes(chosen: u32, hosts: &[HostQuota]) -> u32 {
    hosts.iter().map(|h| h.effective_minutes).fold(chosen, u32::max)
}

/// Every GitHub host that has polled is out of points until its limit resets, so GitHub is not asked.
fn github_paused(hosts: &[HostQuota]) -> bool {
    !hosts.is_empty() && hosts.iter().all(|h| h.paused_until.is_some())
}

/// `5m`, `2h`, `3d`: how long since `then`, in its largest whole unit.
fn age(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let since = now.signed_duration_since(then);
    match (since.num_days(), since.num_hours(), since.num_minutes()) {
        (d, _, _) if d > 0 => format!("{d}d"),
        (_, h, _) if h > 0 => format!("{h}h"),
        (_, _, m) => format!("{}m", m.max(1)),
    }
}

/// Whether notifications are paused, and until when: the stored time in the user's zone, or "until you resume".
fn pause_line(pause: Option<&turn::Pause>) -> String {
    match pause {
        None => "Not paused.".to_string(),
        Some(turn::Pause { until: None, .. }) => "Paused until you resume. Events stay listed here.".to_string(),
        Some(turn::Pause { until: Some(until), .. }) => {
            format!("Paused until {}. Events stay listed here.", until.with_timezone(&chrono::Local).format("%a %-d %b %H:%M"))
        }
    }
}

/// Changes back to the user first, then those with the author; newest update first within each.
fn sort_back(items: &mut [&BackItem]) {
    items.sort_by_key(|item| (!item.is_back_to_you(), std::cmp::Reverse(item.change.updated_at)));
}

/// A selectable row or tab. The selected one is filled with the inverse colour, so its text is `on_inverse` in both themes.
pub(crate) fn selectable<'a>(ui: &Ui, selected: bool, atoms: impl egui::IntoAtoms<'a>) -> egui::Button<'a> {
    let button = egui::Button::selectable(selected, atoms);
    if selected { button.fill(theme::pal(ui).inverse) } else { button }
}

/// Adds a [`selectable`] button. egui draws the keyboard-focus ring on its edge in `focus_ring`, which has no contrast on the
/// inverse fill of the selected one, so that one is covered with the ring colour made for it.
pub(crate) fn add_selectable(ui: &mut Ui, selected: bool, button: egui::Button<'_>) -> egui::Response {
    let response = ui.add(button);
    if selected {
        let corner = ui.visuals().widgets.active.corner_radius.nw as f32;
        components::ring_on_inverse(ui, &response, response.rect, corner);
    }
    response
}

/// A full-width selectable row. Density in Settings → Appearance changes its vertical padding; Default leaves it as it was.
/// The trailing grow atom pushes the text to the left edge. Keyed by `id` so focus follows the row when the list re-sorts.
pub(crate) fn row_button(ui: &mut Ui, id: &str, selected: bool, text: LayoutJob) -> egui::Response {
    let atoms = (egui::WidgetText::from(text), egui::Atom::grow());
    let pad = theme::sz(ui).row;
    ui.push_id(id, |ui| {
        if pad != 0.0 {
            let padding = &mut ui.spacing_mut().button_padding;
            padding.y = (padding.y + pad).max(0.0);
        }
        add_selectable(ui, selected, selectable(ui, selected, atoms).min_size(egui::vec2(ui.available_width(), 0.0)))
    })
    .inner
}

/// What the palette searched, for an empty result (frame 106). Rule 15: a source that has not answered, or is paused or failed,
/// left its changes out, and the words say so instead of claiming there is nothing.
fn search_scope(sources: &[Source], changes: usize) -> String {
    let word = if changes == 1 { "1 change".to_string() } else { format!("{changes} changes") };
    let mut said = format!("Searched commands, Settings pages and the {word} in Review requested. Closed and merged changes are not searched.");
    if sources.iter().any(|s| matches!(s.sync, Sync::Checking(_))) {
        said.push_str(" A source is still answering, so its changes may be missing.");
    }
    if sources.iter().any(|s| s.stale.is_some() || matches!(&s.sync, Sync::Done(state) if state.incomplete())) {
        said.push_str(" A source is paused or failed, so its changes were not all searched.");
    }
    said
}

/// The menu of an inbox row (Figma frame 104): the actions the row has elsewhere under the same keys. Snooze, Done, Mark as
/// unread, Copy branch name and the repository filters are not here: PR Craft keeps no inbox state and the list does not carry a
/// branch, so there is nothing for them to repeat.
fn inbox_menu(ui: &Ui, url: String, at: egui::Pos2, restore: egui::Id, forge: &str, header: String) -> menu::Spec {
    let item = |id: &'static str, text: String, key: &str| menu::Entry::Item(menu::Item::new(id, text).key(keymap::label(ui, key)));
    menu::Spec {
        owner: "inbox",
        target: url,
        header: Some(header),
        at,
        restore: Some(restore),
        entries: vec![
            item("open", "Open review".into(), "inbox.open"),
            item("forge", format!("Open on {forge}"), "inbox.open_forge"),
            item("link", "Copy link".into(), "inbox.copy_link"),
        ],
    }
}

/// `Select a change with J and K, then press ↵ to review it.`, with the keys the reader set.
fn select_hint(ui: &Ui, what: &str) -> String {
    let next = keymap::label(ui, "inbox.next");
    format!("Select {what} with {next} and {}, then press {} to review it.", keymap::label(ui, "inbox.prev"), keymap::label(ui, "inbox.open"))
}

/// The top of a reading pane: forge mark, reference, an optional tag on the right, then the title.
fn detail_head(ui: &mut Ui, change: &Change, tag: Option<(components::TagKind, &str)>) {
    let p = theme::pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        components::forge_mark(ui, change.forge);
        components::mono_label(ui, change.reference(), 12.5, p.text_secondary);
        if let Some((kind, text)) = tag {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| components::tag(ui, kind, text));
        }
    });
    ui.add_space(12.0);
    ui.scope(|ui| {
        ui.set_max_width(ui.available_width().min(460.0));
        ui.add(egui::Label::new(RichText::new(&change.title).size(theme::sz(ui).heading).color(p.graphite)).wrap());
    });
    ui.add_space(8.0);
}

/// The detail pane's action bar, pinned to the foot of `pane`: the keys that move, Open on the forge, Open review. Returns
/// whether "Open review" was clicked.
fn open_buttons(ui: &mut Ui, pane: Rect, change: &Change) -> bool {
    let rect = Rect::from_min_max(egui::pos2(pane.left(), pane.bottom() - theme::ACTION_BAR_HEIGHT), pane.right_bottom());
    components::seam_top(ui, rect);
    let inner = rect.shrink2(egui::vec2(32.0, 0.0));
    // The keys that move make room for the actions when the pane is at its narrowest.
    if inner.width() >= 380.0 {
        ui.scope_builder(UiBuilder::new().id_salt("move-keys").max_rect(inner).layout(Layout::left_to_right(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            components::label(ui, "Move", 12.0, theme::pal(ui).text_tertiary);
            for id in ["inbox.next", "inbox.prev"] {
                components::key_cap(ui, &keymap::label(ui, id));
            }
        });
    }
    ui.scope_builder(UiBuilder::new().id_salt("open-actions").max_rect(inner).layout(Layout::right_to_left(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let open = Pill::new(PillKind::Primary, "Open review").key(ui, "inbox.open").show(ui).clicked();
        if Pill::new(PillKind::Secondary, &format!("Open on {}", change.forge.name())).show(ui).clicked() {
            open_on_forge(ui.ctx(), &change.url, "inbox", &change.change_ref(), None);
        }
        open
    })
    .inner
}

impl ShellApp for PrCraftApp {
    fn frame_input(&mut self) -> FrameInput {
        let mode = if self.dark { craft::theme::Mode::Dark } else { craft::theme::Mode::Light };
        FrameInput::new(RootEnv::new().mode(mode))
    }

    fn ui(&mut self, ui: &mut Ui) {
        let look = prefs::apply(ui.ctx());
        self.dark = look.dark;
        self.poll_due(ui.ctx());
        self.poll();
        self.runs.poll(ui.ctx());
        // The open review's post or hand-off answers while Settings or Activity covers it.
        if let Some(screen) = &mut self.review {
            screen.poll(ui.ctx());
        }
        self.window(ui);
        keymap::sweep(ui.ctx());
    }

    /// Ends every AI run before the window goes, so no engine process outlives PR Craft.
    fn close_requested(&mut self) -> CloseDecision {
        ai::stop_everything_on_quit();
        CloseDecision::Close
    }
}

#[cfg(test)]
mod tests {
    use pr_craft_core::stack::StackPlace;

    use super::*;

    /// An invented inbox row; `stack` is `(bottom, top, index, size)`.
    fn change(repo: &str, number: u64, stack: Option<(u64, u64, usize, usize)>) -> Change {
        Change {
            forge: Forge::GitHub,
            repo: repo.into(),
            number,
            title: format!("Change {number}"),
            author: "ada".into(),
            url: format!("https://github.com/{repo}/pull/{number}"),
            updated_at: Utc::now(),
            draft: false,
            host: None,
            stack: stack.map(|(bottom, top, index, size)| StackPlace { bottom, top, index, size }),
        }
    }

    #[test]
    fn a_stack_is_one_entry_with_its_lowest_layer_first() {
        // Newest-updated first, as the inbox lists them: layer 2 of the stack comes before layer 0.
        let changes = [
            change("acme/api", 12, Some((10, 12, 2, 3))),
            change("acme/api", 99, None),
            change("acme/api", 10, Some((10, 12, 0, 3))),
            change("acme/web", 10, Some((10, 11, 0, 2))),
        ];
        assert_eq!(group(&changes), [vec![2, 0], vec![1], vec![3]]);
    }

    #[test]
    fn stacks_forking_from_one_layer_are_separate_entries() {
        // uv-like: layers 11 and 12 both sit on 10, so both report bottom 10 but have different tops.
        let changes = [change("acme/api", 11, Some((10, 11, 1, 2))), change("acme/api", 12, Some((10, 12, 1, 2)))];
        assert_eq!(group(&changes), [vec![0], vec![1]]);
    }

    #[test]
    fn changes_back_to_you_list_before_those_with_the_author_and_newest_first_within_each() {
        use pr_craft_core::turn::{Assessment, MyReview, Reason, ReviewState, Turn};
        let item = |number: u64, age_h: i64, turn: Turn| {
            let mut change = change("acme/api", number, None);
            change.updated_at = Utc::now() - chrono::Duration::hours(age_h);
            let review = MyReview { state: ReviewState::Commented, submitted_at: None, commit: None };
            BackItem { change, assessment: Assessment { review, commits_since: 0, turn } }
        };
        let back = || Turn::BackToYou { reasons: vec![Reason::ReRequested { at: None }] };
        let items = [item(1, 1, Turn::WithAuthor), item(2, 5, back()), item(3, 3, back()), item(4, 2, Turn::WithAuthor)];
        let mut shown: Vec<&BackItem> = items.iter().collect();
        sort_back(&mut shown);
        assert_eq!(shown.iter().map(|i| i.change.number).collect::<Vec<_>>(), [3, 2, 1, 4]);
    }
}

#[cfg(test)]
mod pause_tests {
    use super::*;

    #[test]
    fn the_pause_line_names_the_real_end_time_or_says_until_resumed() {
        assert_eq!(pause_line(None), "Not paused.");
        assert!(pause_line(Some(&turn::Pause { until: None, extra: Default::default() })).contains("until you resume"));
        let until = Utc::now() + chrono::Duration::days(1);
        let line = pause_line(Some(&turn::Pause { until: Some(until), extra: Default::default() }));
        assert!(!line.contains("you resume"), "{line}");
        assert!(line.contains(&until.with_timezone(&chrono::Local).format("%H:%M").to_string()), "{line}");
    }
}

#[cfg(test)]
mod poll_tests {
    use std::sync::mpsc::Sender;

    use super::*;

    fn listed(changes: Vec<Change>) -> SourceState {
        SourceState::Listed { changes, truncated: false, stacks_error: None, hosts: Vec::new(), signed_out: Vec::new() }
    }

    fn change(number: u64) -> Change {
        Change {
            forge: Forge::GitHub,
            repo: "acme/api".into(),
            number,
            title: format!("Change {number}"),
            author: "ada".into(),
            url: format!("https://github.com/acme/api/pull/{number}"),
            updated_at: Utc::now(),
            draft: false,
            host: None,
            stack: None,
        }
    }

    /// An app whose sources have answered with `state`, and whose Back-to-you lists are still on their first read, so no
    /// notification is composed (that reads the data folder). Nothing here starts a `gh` or `glab`.
    fn app(state: &SourceState) -> (PrCraftApp, Vec<Sender<BackSource>>) {
        let mut held = Vec::new();
        let sources = Forge::ALL.into_iter().map(|forge| Source { forge, sync: Sync::Done(state.clone()), refresh: None, stale: None }).collect();
        let back = Forge::ALL
            .into_iter()
            .map(|forge| {
                let (tx, rx) = mpsc::channel();
                held.push(tx);
                BackSync { forge, sync: BackFetch::Checking(rx), refresh: None, stale: None }
            })
            .collect();
        let app = PrCraftApp {
            sources,
            changes: Vec::new(),
            entries: Vec::new(),
            selected: None,
            focus_selected: false,
            review: None,
            view: Inbox::Queue,
            forge_filter: None,
            back,
            notifications: None,
            notify_error: None,
            notify_dirty: false,
            runs: ai::Runs::default(),
            overlay: Overlay::None,
            popover: false,
            palette: None,
            dark: false,
            battery: prefs::Battery::new(),
            last_check: Instant::now(),
            polling: false,
            local_drafts: None,
            drafts_rx: None,
            drafts_live: false,
            drafts_stale: false,
            mine: Vec::new(),
            panel: None,
            checks: None,
        };
        (app, held)
    }

    fn inbox(forge: Forge, state: SourceState) -> InboxSource {
        InboxSource { forge, cli: forge.cli(), state }
    }

    #[test]
    fn a_poll_that_fails_keeps_the_last_good_list_and_says_why_and_a_good_one_replaces_it() {
        let (mut app, _held) = app(&listed(vec![change(1)]));
        app.sources[1].sync = Sync::Done(listed(vec![]));
        app.changes = vec![change(1)];
        let (tx, rx) = mpsc::channel();
        app.sources[0].refresh = Some(rx);
        tx.send(inbox(Forge::GitHub, SourceState::Failed { error: "offline".into() })).expect("send");
        app.poll();
        assert!(matches!(&app.sources[0].sync, Sync::Done(SourceState::Listed { changes, .. }) if changes.len() == 1), "the list stays");
        assert_eq!(app.sources[0].stale.as_deref(), Some("offline"));
        assert!(app.sources[0].refresh.is_none());

        let (tx, rx) = mpsc::channel();
        app.sources[0].refresh = Some(rx);
        tx.send(inbox(Forge::GitHub, listed(vec![]))).expect("send");
        app.poll();
        assert!(matches!(&app.sources[0].sync, Sync::Done(SourceState::Listed { changes, .. }) if changes.is_empty()), "a real answer replaces it");
        assert!(app.sources[0].stale.is_none());
        assert!(app.changes.is_empty(), "the shown changes follow the sources again");
    }

    #[test]
    fn a_failed_back_to_you_poll_keeps_its_list_and_says_so() {
        let (mut app, _held) = app(&listed(vec![]));
        app.back[0].sync = BackFetch::Done(BackState::Listed { items: Vec::new(), truncated: false, incomplete: None, yours: Vec::new() });
        let (tx, rx) = mpsc::channel();
        app.back[0].refresh = Some(rx);
        tx.send(BackSource { forge: Forge::GitHub, cli: "gh", state: BackState::Failed { error: "offline".into() } }).expect("send");
        app.poll();
        assert!(matches!(app.back[0].sync, BackFetch::Done(BackState::Listed { .. })), "the list stays");
        assert_eq!(app.back[0].stale.as_deref(), Some("offline"));
    }

    #[test]
    fn a_source_with_nothing_listed_takes_a_failure_as_its_state() {
        let (mut app, _held) = app(&SourceState::Failed { error: "first".into() });
        let (tx, rx) = mpsc::channel();
        app.sources[1].refresh = Some(rx);
        tx.send(inbox(Forge::GitLab, SourceState::Failed { error: "second".into() })).expect("send");
        app.poll();
        assert!(matches!(&app.sources[1].sync, Sync::Done(SourceState::Failed { error }) if error == "second"));
        assert!(app.sources[1].stale.is_none(), "there was no good list to keep");
    }

    #[test]
    fn the_next_poll_is_one_interval_after_the_last_answer_not_after_its_start() {
        let (mut app, _held) = app(&listed(vec![]));
        for back in &mut app.back {
            back.sync = BackFetch::Done(BackState::Failed { error: "x".into() });
        }
        let long_ago = Instant::now().checked_sub(Duration::from_secs(3600)).expect("an hour before now");
        app.last_check = long_ago;
        app.polling = true;
        let (_tx, rx) = mpsc::channel();
        app.sources[0].refresh = Some(rx);
        app.settle_poll();
        assert!(app.polling && app.last_check == long_ago, "an answer is still on its way");
        app.sources[0].refresh = None;
        app.settle_poll();
        assert!(!app.polling && app.last_check.elapsed() < Duration::from_secs(60), "the clock starts when the last answer came");
    }

    fn quota(host: &str, effective_minutes: u32, paused_until: Option<DateTime<Utc>>) -> HostQuota {
        HostQuota {
            host: host.into(),
            last_cost: 57,
            projected_per_hour: 1140,
            remaining: Some(4000),
            reset_at: paused_until,
            effective_minutes,
            paused_until,
        }
    }

    #[test]
    fn the_next_poll_uses_the_longest_effective_interval_and_a_paused_github_is_skipped() {
        let hosts = [quota("github.com", 3, None), quota("ghe.acme.dev", 7, None)];
        assert_eq!(poll_minutes(1, &hosts), 7);
        assert_eq!(poll_minutes(15, &hosts), 15, "the chosen interval is never shortened");
        assert_eq!(poll_minutes(5, &[]), 5);
        let later = Utc::now() + chrono::Duration::hours(1);
        assert!(!github_paused(&[]));
        assert!(!github_paused(&[quota("github.com", 3, Some(later)), quota("ghe.acme.dev", 3, None)]), "one host still answers");
        assert!(github_paused(&[quota("github.com", 3, Some(later))]));
    }

    #[test]
    fn a_start_up_sync_that_is_still_reading_holds_the_clock() {
        let (mut app, _held) = app(&listed(vec![]));
        app.polling = true;
        let long_ago = Instant::now().checked_sub(Duration::from_secs(3600)).expect("an hour before now");
        app.last_check = long_ago;
        app.settle_poll();
        assert!(app.polling && app.last_check == long_ago, "Back to you is still reading");
    }

    #[test]
    fn re_check_waits_for_a_poll_of_its_source_or_of_its_back_to_you_list() {
        let (mut app, _held) = app(&listed(vec![]));
        assert!(recheck_allowed(&app.sources[0], &[]));
        assert!(!recheck_allowed(&app.sources[0], &[Forge::GitHub]), "its Back to you poll is on its way");
        assert!(recheck_allowed(&app.sources[0], &[Forge::GitLab]), "another forge's poll does not matter");
        let (_tx, rx) = mpsc::channel();
        app.sources[0].refresh = Some(rx);
        assert!(!recheck_allowed(&app.sources[0], &[]), "its own poll is on its way");
        let (_tx, rx) = mpsc::channel();
        app.sources[1].sync = Sync::Checking(rx);
        assert!(!recheck_allowed(&app.sources[1], &[]), "it is already checking");
    }
}
