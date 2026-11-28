//! Runs on screen: the task pill and its popover (Figma frames 49, 70, 57), the Activity screen (61, 62), the runs panel
//! of a review (13, 35) and the run tree with its detail (50, 67, 68, 56). Every action goes through `Runs::act`.

use chrono::{DateTime, Local, Utc};
use craft::egui::{self, Align, Align2, Context, Layout, Margin, Rect, RichText, ScrollArea, Ui};
use pr_craft_core::ChangeRef;
use pr_craft_core::agent::StuckRules;
use pr_craft_core::run::{Node, NodeStatus, Run, RunState, SpawnedBy};

use crate::ai::{self, RunFilter, Runs};
use crate::components::{self, BannerKind, Pill, PillKind};
use crate::keymap;
use crate::review::card;
use crate::settings::{note, section};
use crate::theme;
use crate::{live_label, selectable};

/// `10:36:01`, in the reader's zone.
fn at(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local).format("%H:%M:%S").to_string()
}

fn state_colour(ui: &Ui, run: &Run) -> egui::Color32 {
    let p = theme::pal(ui);
    match run.state {
        RunState::Done => p.moss_text,
        RunState::Failed | RunState::Killed => p.rose_text,
        RunState::LooksStuck => p.ember_text,
        _ => p.text_secondary,
    }
}

// ---- actions ----

/// Stop: ends the process at once and keeps drafts of any answer already finished.
pub(crate) fn stop(ctx: &Context, runs: &mut Runs, run: &Run) {
    let (id, name) = (run.id.clone(), run.agent.name.clone());
    runs.act(ctx, move || ai::run_stop(&id).map(|()| format!("Stopped {name}.")));
}

pub(crate) fn kill(ctx: &Context, runs: &mut Runs, run: &Run) {
    let (id, name) = (run.id.clone(), run.agent.name.clone());
    runs.act(ctx, move || ai::run_kill(&id).map(|()| format!("Killed {name}.")));
}

/// Retry for a run that ended, Kill and retry for a stuck one. It asks first: it spends usage, and what it reads is named.
/// The confirm names the engine the start will use now (the agent's current one), not the one the old run had.
pub(crate) fn retry(runs: &mut Runs, run: &Run) {
    match engine_now(runs, run) {
        Ok(engine) => runs.ask(ai::PaidStart::retry(run, engine)),
        Err(error) => runs.status = Some(Err(error)),
    }
}

/// The engine a start of `run`'s agent would use now: the saved agent's, or the default for the General review.
fn engine_now(runs: &Runs, run: &Run) -> Result<pr_craft_core::agent::Engine, String> {
    #[cfg(test)]
    if let Some(lookup) = runs.engine_for {
        return lookup(run);
    }
    let _ = runs;
    match &run.agent.id {
        Some(id) => ai::agents()?
            .into_iter()
            .find(|agent| agent.id == *id)
            .map(|agent| agent.engine)
            .ok_or_else(|| format!("{} no longer exists, so it cannot be run again.", run.agent.name)),
        None => ai::engine_get().map(|view| view.settings.engine),
    }
}

/// The confirm step of a paid start, drawn over whatever screen is up. Its start and cancel keys are taken here before any
/// screen under it sees them, and so are the character keys of the screens under it.
pub(crate) fn confirm_window(ctx: &Context, runs: &mut Runs) {
    let Some(paid) = runs.confirm.clone() else { return };
    let (start, cancel) = (keymap::consume(ctx, "confirm.start"), keymap::consume(ctx, "confirm.cancel"));
    // A held ↵ or Space repeats: it must not press the focused Start button.
    let activation_held = ctx.input(|i| {
        i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::Enter | egui::Key::Space, pressed: true, repeat: true, .. }))
    });
    let blocked = keymap::typing_keys(ctx);
    ctx.input_mut(|i| {
        i.events.retain(|e| !matches!(e, egui::Event::Key { key, modifiers, .. } if blocked.contains(key) && !modifiers.command));
    });
    let (mut start, mut cancel) = (start, cancel);
    let focus = std::mem::take(&mut runs.confirm_focus);
    egui::Window::new("Confirm")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_TOP, [0.0, theme::SHEET_TOP])
        .min_width(theme::POPOVER_WIDTH)
        .show(ctx, |ui| {
            live_label(ui, "Start a paid run?", theme::pal(ui).graphite);
            for line in paid.lines() {
                note(ui, &line);
            }
            ui.horizontal(|ui| {
                let button = ui.button(keymap::hint(ui, "Start", "confirm.start"));
                if focus {
                    button.request_focus();
                }
                start |= button.clicked() && !activation_held;
                cancel |= ui.button(keymap::hint(ui, "Cancel", "confirm.cancel")).clicked();
            });
        });
    if start {
        runs.confirm = None;
        let activity_key = keymap::label(ctx, "app.activity");
        runs.act_done(ctx, move || paid.run(activity_key));
    } else if cancel {
        runs.confirm = None;
    }
}

pub(crate) fn stop_all(ctx: &Context, runs: &mut Runs) {
    runs.act(ctx, || ai::stop_all().map(|ids| if ids.is_empty() { "No run was going.".to_string() } else { format!("Stopped {} runs.", ids.len()) }));
}

/// Stop, Kill and Kill and retry for an active run; Retry for one that failed, was killed or stopped.
fn run_buttons(ui: &mut Ui, runs: &mut Runs, run: &Run, keys: bool) {
    let pill = |ui: &mut Ui, kind: PillKind, label: &str, id: &str, enabled: bool| {
        let pill = Pill::new(kind, label).small().enabled(enabled);
        (if keys { pill.key(ui, id) } else { pill }).show(ui).clicked()
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        if run.state.is_active() {
            if run.state == RunState::LooksStuck && pill(ui, PillKind::Secondary, "Kill and retry", "activity.retry", true) {
                retry(runs, run);
            }
            if pill(ui, PillKind::Secondary, "Stop", "activity.stop", !run.stopping) {
                stop(ui.ctx(), runs, run);
            }
            if pill(ui, PillKind::Danger, "Kill", "activity.kill", true) {
                kill(ui.ctx(), runs, run);
            }
        } else if pill(ui, PillKind::Secondary, "Retry", "activity.retry", true) {
            retry(runs, run);
        }
    });
}

/// The outcome of the last action and any trouble reading the runs, as a live region. Drawn once, under the title bar,
/// for every screen: an action's failure is shown wherever it was asked from.
pub(crate) fn status_line(ui: &mut Ui, runs: &mut Runs) {
    let mut dismiss = false;
    ui.horizontal_wrapped(|ui| {
        match &runs.status {
            Some(Ok(text)) if runs.warning.as_deref() == Some(text.as_str()) => live_label(ui, text.as_str(), theme::pal(ui).ember_text),
            Some(Ok(text)) => live_label(ui, text.as_str(), theme::pal(ui).moss_text),
            Some(Err(error)) => live_label(ui, error.as_str(), theme::pal(ui).rose_text),
            None => {}
        }
        dismiss = runs.status.is_some() && ui.button("Dismiss").clicked();
    });
    if dismiss {
        runs.status = None;
    }
    if let Some(error) = &runs.error {
        live_label(ui, format!("Could not read the runs: {error}"), theme::pal(ui).rose_text);
    }
    for name in runs.unreadable() {
        live_label(ui, format!("A damaged run file is skipped: {name}"), theme::pal(ui).ember_text);
    }
}

// ---- the title area ----

/// What the title-area button asks for.
pub(crate) enum PillAct {
    None,
    Popover,
    Activity,
}

/// The pill while a run goes (frames 49, 70): where its review stands; else a plain Activity button.
pub(crate) fn pill_ui(ui: &mut Ui, runs: &Runs) -> PillAct {
    let Some(pill) = ai::pill(runs.all()) else {
        return if crate::components::title_action(ui, "Activity", &keymap::label(ui, "app.activity")).clicked() {
            PillAct::Activity
        } else {
            PillAct::None
        };
    };
    let text = if pill.stuck { format!("Looks stuck · {}", pill.text) } else { pill.text.clone() };
    let kind = if pill.stuck { crate::components::PillKind::Warn } else { crate::components::PillKind::Secondary };
    let button = crate::components::Pill::new(kind, &text).small().show(ui);
    let button = button.on_hover_text(format!("{}\nClick to open the runs; {} shows every run.", pill.detail, keymap::label(ui, "app.activity")));
    if button.clicked() { PillAct::Popover } else { PillAct::None }
}

/// What the popover under the pill asks for.
pub(crate) enum PopAct {
    None,
    Close,
    Activity,
    Open(ChangeRef, String),
}

/// The popover (frame 57): the runs that are going and the latest that ended, each with Open and Stop.
pub(crate) fn popover(ctx: &Context, runs: &mut Runs) -> PopAct {
    let mut act = PopAct::None;
    if keymap::consume(ctx, "popover.close") {
        return PopAct::Close;
    }
    let now = Utc::now();
    let shown: Vec<Run> =
        runs.all().iter().filter(|r| r.state.is_active()).chain(runs.all().iter().filter(|r| !r.state.is_active()).take(3)).cloned().collect();
    egui::Window::new("Activity")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::RIGHT_TOP, [-theme::SPACE_3, theme::POPOVER_TOP])
        .default_width(theme::POPOVER_WIDTH)
        .min_width(theme::POPOVER_WIDTH)
        .show(ctx, |ui| {
            let tally = ai::Tally::of(runs.all().iter().filter(|r| RunFilter::Failed.matches(r, now) || r.state.is_active()));
            section(ui, &format!("Activity  ·  {} going · {} look stuck · {} failed today", tally.going, tally.stuck, tally.failed + tally.killed));
            for run in &shown {
                ui.separator();
                ui.label(
                    RichText::new(format!("{}  ·  {}", run.change.reference(), run.agent.name))
                        .size(theme::sz(ui).body)
                        .color(theme::pal(ui).graphite),
                );
                let line = format!("{} · {}", ai::state_word(run), ai::clock(ai::elapsed_secs(run, now)));
                ui.label(RichText::new(line).size(theme::sz(ui).body).color(state_colour(ui, run)));
                ui.horizontal(|ui| {
                    if ui.button("Open").clicked() {
                        act = PopAct::Open(run.change.clone(), run.change_title.clone());
                    }
                    if run.state.is_active() && ui.add_enabled(!run.stopping, egui::Button::new("Stop")).clicked() {
                        stop(ui.ctx(), runs, run);
                    }
                });
            }
            if shown.is_empty() {
                note(ui, "No AI run yet.");
            }
            ui.separator();
            if ui.button(keymap::hint(ui, "Open Activity", "app.activity")).clicked() {
                act = PopAct::Activity;
            }
        });
    act
}

// ---- the Activity screen ----

pub enum Exit {
    Stay,
    Close,
    Open(ChangeRef, String),
}

pub struct Activity {
    filter: RunFilter,
    selected: Option<String>,
    node: Option<u32>,
    stuck: StuckRules,
    focus_selected: bool,
}

impl Activity {
    pub fn new() -> Self {
        let stuck = ai::engine_get().map(|v| v.settings.stuck).unwrap_or_default();
        Activity { filter: RunFilter::Running, selected: None, node: None, stuck, focus_selected: false }
    }

    /// The screen on `filter` with `selected` chosen, reading nothing from this Mac, for the render tests.
    #[cfg(test)]
    pub(crate) fn fixture(filter: RunFilter, selected: &str) -> Self {
        Activity { filter, selected: Some(selected.into()), node: None, stuck: StuckRules::default(), focus_selected: false }
    }

    /// What the screen can do with the run selected, for the command palette.
    pub(crate) fn facts(&self, runs: &Runs) -> pr_craft_core::palette::ActivityFacts {
        let run = self.selected.as_deref().and_then(|id| runs.all().iter().find(|r| r.id == id));
        pr_craft_core::palette::ActivityFacts {
            selected: run.is_some(),
            active: run.is_some_and(|r| r.state.is_active()),
            stuck: run.is_some_and(|r| r.state == RunState::LooksStuck),
            any_active: runs.all().iter().any(|r| r.state.is_active()),
        }
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, runs: &mut Runs) -> Exit {
        let now = Utc::now();
        let shown: Vec<Run> = runs.all().iter().filter(|r| self.filter.matches(r, now)).cloned().collect();
        // The selection is a run id. A run that left the filter selects nothing, so S and X never hit another run.
        if self.selected.as_deref().is_some_and(|id| !shown.iter().any(|r| r.id == id)) {
            self.selected = None;
            self.node = None;
        }
        let mut exit = self.keys(ui, runs, &shown);
        let p = theme::pal(ui);
        let rect = ui.available_rect_before_wrap();
        let filters = Rect::from_min_size(rect.min, egui::vec2(theme::ACTIVITY_FILTERS_WIDTH + 1.0, rect.height()));
        let detail_width = theme::ACTIVITY_DETAIL_WIDTH.min((rect.width() - filters.width()) * 0.5);
        let detail = Rect::from_min_max(egui::pos2(rect.right() - detail_width, rect.top()), rect.right_bottom());
        let list = Rect::from_min_max(filters.right_top(), egui::pos2(detail.left(), rect.bottom()));
        components::pane_at(ui, "activity-filters", filters, p.hover, Margin { left: 8, right: 9, top: 14, bottom: 8 }, |ui| {
            if self.filter_pane(ui, runs, now) {
                exit = Exit::Close;
            }
        });
        components::seam_right(ui, filters);
        components::pane_at(ui, "activity-list", list, p.paper, Margin::ZERO, |ui| {
            if let Some(open) = self.list(ui, runs, &shown, now) {
                exit = open;
            }
        });
        components::seam_right(ui, list);
        components::pane_at(ui, "activity-detail", detail, p.hover, Margin { left: 20, right: 20, top: 16, bottom: 0 }, |ui| {
            ScrollArea::vertical().id_salt("activity-detail").auto_shrink([false, false]).show(ui, |ui| {
                if let Some(open) = self.detail(ui, runs, &shown, now) {
                    exit = open;
                }
            });
        });
        exit
    }

    /// The filters with their counts, and Back. Returns true when Back was pressed.
    fn filter_pane(&mut self, ui: &mut Ui, runs: &Runs, now: DateTime<Utc>) -> bool {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            components::label(ui, "Activity", 13.5, theme::pal(ui).graphite);
        });
        ui.add_space(theme::SPACE_2);
        for (n, filter) in RunFilter::ALL.into_iter().enumerate() {
            let count = runs.all().iter().filter(|r| filter.matches(r, now)).count();
            let key = keymap::label(ui, &format!("activity.filter_{}", n + 1));
            if components::nav_item(ui, self.filter == filter, filter.label(), &count.to_string()).on_hover_text(key).clicked() {
                self.filter = filter;
            }
        }
        ui.add_space(theme::SPACE_3);
        let keys = |id: &str| keymap::label(ui, id);
        let hint = format!(
            "{} {} pick a run · {} on a focused row opens its run · {} stop · {} kill · {} retry, which asks first.",
            keys("activity.next"),
            keys("activity.prev"),
            keys("activity.open"),
            keys("activity.stop"),
            keys("activity.kill"),
            keys("activity.retry")
        );
        components::note(ui, &hint, theme::pal(ui).text_tertiary);
        ui.add_space(theme::SPACE_2);
        components::note(
            ui,
            "macOS banners for stuck runs are not available yet. A stuck run shows here and in the title-bar pill.",
            theme::pal(ui).text_tertiary,
        );
        ui.add_space(theme::SPACE_2);
        Pill::new(PillKind::Secondary, "‹ Back").key(ui, "activity.close").small().show(ui).clicked()
    }

    /// J K walk the list, ↵ on a row opens its run, S stops, X kills, R asks to retry, ⌘. stops everything, 1 to 5 pick the filter.
    fn keys(&mut self, ui: &Ui, runs: &mut Runs, shown: &[Run]) -> Exit {
        if keymap::consume(ui, "activity.stop_all") {
            stop_all(ui.ctx(), runs);
        }
        if keymap::pressed(ui, "activity.close") {
            return Exit::Close;
        }
        for (n, filter) in RunFilter::ALL.into_iter().enumerate() {
            if keymap::pressed(ui, &format!("activity.filter_{}", n + 1)) {
                self.filter = filter;
            }
        }
        let at = self.selected.as_deref().and_then(|id| shown.iter().position(|r| r.id == id));
        let (down, up) = (keymap::pressed(ui, "activity.next"), keymap::pressed(ui, "activity.prev"));
        if down || up {
            let next = ai::nav_step(at, shown.len(), down);
            self.selected = next.and_then(|i| shown.get(i)).map(|r| r.id.clone());
            self.node = None;
            self.focus_selected = true;
        }
        let Some(run) = at.and_then(|i| shown.get(i)) else { return Exit::Stay };
        // A key held down does not press again: every one of these starts, stops or kills something.
        if keymap::fresh(ui, "activity.stop") && run.state.is_active() {
            stop(ui.ctx(), runs, run);
        }
        if keymap::fresh(ui, "activity.kill") && run.state.is_active() {
            kill(ui.ctx(), runs, run);
        }
        if keymap::fresh(ui, "activity.retry") && (run.state == RunState::LooksStuck || !run.state.is_active()) {
            retry(runs, run);
        }
        Exit::Stay
    }

    /// The run list. A row with focus opens its run on ↵, so ↵ on a button never opens anything.
    fn list(&mut self, ui: &mut Ui, runs: &mut Runs, shown: &[Run], now: DateTime<Utc>) -> Option<Exit> {
        let p = theme::pal(ui);
        components::padded(ui, Margin { left: 20, right: 20, top: 14, bottom: 10 }, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                components::label(ui, self.filter.label(), 16.0, p.graphite);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let any = runs.active().next().is_some();
                    if Pill::new(PillKind::Quiet, "Stop all").key(ui, "activity.stop_all").small().enabled(any).show(ui).clicked() {
                        stop_all(ui.ctx(), runs);
                    }
                });
            });
            components::note(
                ui,
                format!("Runs are flagged after {} min without output. Change this in Settings → AI engine.", self.stuck.quiet_secs / 60),
                p.text_tertiary,
            );
        });
        components::rule(ui);
        if shown.is_empty() {
            components::padded(ui, Margin::symmetric(20, 36), |ui| self.empty(ui, runs, now));
            return None;
        }
        ScrollArea::vertical()
            .id_salt("activity-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let (mut clicked, mut open) = (None, None);
                let mut group: Option<String> = None;
                for run in shown {
                    let reference = run.change.reference();
                    if group.as_deref() != Some(reference.as_str()) {
                        group = Some(reference.clone());
                        components::run_group(ui, run.change.forge, &reference, &run.change_title);
                    }
                    let selected = self.selected.as_deref() == Some(run.id.as_str());
                    let model = components::RunRow {
                        name: format!("{} · {}", run.agent.name, ai::state_word(run)),
                        detail: ai::last_output(run, now),
                        clock: ai::clock(ai::elapsed_secs(run, now)),
                        dot: state_colour(ui, run),
                        stuck: run.state == RunState::LooksStuck,
                        selected,
                    };
                    let row = components::run_row(ui, &run.id, &model);
                    if row.clicked() {
                        clicked = Some(run.id.clone());
                    }
                    if row.has_focus() && keymap::fresh(ui, "activity.open") {
                        open = Some(Exit::Open(run.change.clone(), run.change_title.clone()));
                    }
                    if selected && self.focus_selected {
                        row.request_focus();
                        row.scroll_to_me(None);
                    }
                }
                self.focus_selected = false;
                if clicked.is_some() {
                    self.selected = clicked;
                    self.node = None;
                }
                open
            })
            .inner
    }

    /// Nothing under this filter (frame 110). Rule 15: "nothing is running" is said only when every run record was read; else
    /// the list says what it could not read.
    fn empty(&mut self, ui: &mut Ui, runs: &Runs, now: DateTime<Utc>) {
        if let Some(error) = &runs.error {
            let body = format!("{error}. A run may be going that is not listed. Activity reads again on its own.");
            components::empty_state(ui, "The runs could not be read", &body, &[]);
            return;
        }
        if !runs.unreadable().is_empty() {
            let body = format!("{} could not be read, so this list may be missing runs.", runs.unreadable().join(", "));
            components::empty_state(ui, "Some runs could not be read", &body, &[]);
            return;
        }
        let count = |filter: RunFilter| runs.all().iter().filter(|r| filter.matches(r, now)).count();
        let (done, failed) = (count(RunFilter::Done), count(RunFilter::Failed));
        let finished = if done == 1 { "1 run finished".to_string() } else { format!("{done} runs finished") };
        let today = format!("Today: {finished}, {}.", if failed == 0 { "none failed".to_string() } else { format!("{failed} failed") });
        let (title, body) = match self.filter {
            RunFilter::Running => ("Nothing is running", format!("Review agents show here while they work. {today}")),
            RunFilter::Stuck => ("Nothing looks stuck", format!("A run is flagged after {} min without output. {today}", self.stuck.quiet_secs / 60)),
            RunFilter::Done => ("Nothing finished today", "A run that finishes shows here until midnight.".to_string()),
            RunFilter::Failed => ("Nothing failed today", "A run that fails or is killed shows here until midnight.".to_string()),
            RunFilter::All => ("No runs yet", "Start an AI review from a change. Its runs show here.".to_string()),
        };
        let offer: &[(&str, &str)] = if self.filter == RunFilter::Running && done > 0 { &[("Show done today", "")] } else { &[] };
        if components::empty_state(ui, title, &body, offer) == Some(0) {
            self.filter = RunFilter::Done;
        }
    }

    /// The selected run (frame 61): what it is, why it looks stuck, its last lines, its tree and the buttons.
    fn detail(&mut self, ui: &mut Ui, runs: &mut Runs, shown: &[Run], now: DateTime<Utc>) -> Option<Exit> {
        let Some(run) = self.selected.as_deref().and_then(|id| shown.iter().find(|r| r.id == id)) else {
            let hint = format!(
                "Pick a run with {} and {} to see its log and what it read.",
                keymap::label(ui, "activity.next"),
                keymap::label(ui, "activity.prev")
            );
            components::empty_pane(ui, "No run selected", &hint);
            return None;
        };
        let mut open = None;
        run_header(ui, run, now, Some(&self.stuck));
        ui.add_space(theme::SPACE_2);
        run_buttons(ui, runs, run, true);
        if Pill::new(PillKind::Quiet, "Open the run").small().show(ui).clicked() {
            open = Some(Exit::Open(run.change.clone(), run.change_title.clone()));
        }
        run_body(ui, run, &mut self.node);
        open
    }
}

/// Who and what: the agent and change, then pid and clocks, the state in words and why a run failed or looks stuck.
fn run_header(ui: &mut Ui, run: &Run, now: DateTime<Utc>, rules: Option<&StuckRules>) {
    let p = theme::pal(ui);
    components::label(ui, format!("{} on {}", run.agent.name, run.change.reference()), 15.0, p.graphite);
    let pid = run.pid.map_or("not started".to_string(), |p| format!("pid {p}"));
    let last = run.last_output.map_or("no output yet".to_string(), |t| format!("last output {}", at(t)));
    components::mono_label(ui, format!("{} · {pid} · started {} · {last}", run.engine.name(), at(run.started)), 11.5, p.text_tertiary);
    ui.add_space(4.0);
    components::label(ui, format!("{} · {}", ai::state_word(run), ai::clock(ai::elapsed_secs(run, now))), 12.5, state_colour(ui, run));
    if run.state == RunState::LooksStuck {
        let quiet = now.signed_duration_since(run.last_output.unwrap_or(run.started)).num_seconds();
        let mut body = "Kill and retry starts it again; the other runs keep going.".to_string();
        if let Some(rules) = rules {
            body.push_str(&format!(" Flagged after {} min of quiet.", rules.quiet_secs / 60));
        }
        ui.add_space(theme::SPACE_2);
        let banner = components::banner(ui, BannerKind::Warn, &format!("Looks stuck: {} without output", ai::clock(quiet)), &body);
        ui.ctx().accesskit_node_builder(banner.id, |node| node.set_live(egui::accesskit::Live::Polite));
    }
    if run.retries > 0 {
        note(ui, "Retried once automatically after it went quiet.");
    }
    if let Some(error) = &run.error {
        live_label(ui, format!("Failed: {error}"), p.rose_text);
    }
}

/// Last lines, the drafts it made, then the tree.
fn run_body(ui: &mut Ui, run: &Run, node: &mut Option<u32>) {
    ui.add_space(theme::SPACE_2);
    section(ui, "Last lines");
    if run.tail.is_empty() {
        note(ui, "The engine has printed nothing yet.");
    }
    if !run.tail.is_empty() {
        egui::Frame::new().fill(theme::pal(ui).raised).stroke((1.0, theme::pal(ui).border)).corner_radius(8).inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for line in &run.tail {
                components::mono_label(ui, line, 11.5, theme::pal(ui).graphite);
            }
        });
    }
    let kept = run.drafts.iter().filter(|d| d.state == pr_craft_core::run::DraftState::Kept).count();
    note(
        ui,
        &format!(
            "{} AI drafts, {kept} kept. {} findings dropped for naming no line of the diff. A transcript view is not built.",
            run.drafts.len(),
            run.dropped
        ),
    );
    ui.add_space(theme::SPACE_2);
    tree_ui(ui, run, node);
}

// ---- the run tree (frames 50, 67, 68, 56) ----

/// Spawned nodes that count against the extra limit: not planned, not refused.
fn spawned(run: &Run) -> usize {
    run.tree.iter().filter(|n| !n.planned && n.status != NodeStatus::Refused).count()
}

fn status_word(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Queued => "Queued",
        NodeStatus::Running => "Running",
        NodeStatus::Done => "Done",
        NodeStatus::Failed => "Failed",
        NodeStatus::Skipped => "Skipped",
        NodeStatus::Stopped => "Stopped",
        NodeStatus::Refused => "Refused",
    }
}

fn spawned_by(by: &SpawnedBy) -> String {
    match by {
        SpawnedBy::Skill { name } => format!("skill {name}"),
        SpawnedBy::Subagent { node } => format!("subagent {node}"),
        SpawnedBy::Runner => "the runner".into(),
    }
}

/// The spawn budget and the nodes, then the selected node's detail.
fn tree_ui(ui: &mut Ui, run: &Run, selected: &mut Option<u32>) {
    section(ui, "Run tree");
    let refused = run.tree.iter().filter(|n| n.status == NodeStatus::Refused).count();
    let depth = run.tree.iter().map(|n| n.depth).max().unwrap_or(0);
    let limits = run.limits;
    note(
        ui,
        &format!(
            "Spawn budget: {} of {} subagents · depth {depth} of {} ({refused} refused) · {} at once",
            spawned(run),
            limits.extra,
            limits.depth,
            limits.at_once
        ),
    );
    if run.tree.len() <= 1 {
        note(ui, "Subagent spawning is not wired: the engine runs with no tools, so it cannot start any. The tree holds the run's own step.");
    }
    for node in &run.tree {
        let is = *selected == Some(node.id);
        let by = node.spawned_by.as_ref().map(|b| format!(" · spawned by {}", spawned_by(b))).unwrap_or_default();
        let kind = if node.planned { "planned" } else { "spawned" };
        let label = format!(
            "{}{}  {}  ·  {kind}{by}",
            "    ".repeat(node.depth as usize),
            status_word(node.status),
            node.task.lines().next().unwrap_or_default()
        );
        let colour = if is { theme::pal(ui).on_inverse } else { theme::pal(ui).graphite };
        let tab = selectable(ui, is, RichText::new(label).size(theme::sz(ui).body).color(colour)).min_size(egui::vec2(ui.available_width(), 0.0));
        if crate::add_selectable(ui, is, tab).clicked() {
            *selected = Some(node.id);
        }
    }
    if let Some(node) = selected.and_then(|id| run.tree.iter().find(|n| n.id == id)) {
        node_detail(ui, run, node);
    }
}

fn node_detail(ui: &mut Ui, run: &Run, node: &Node) {
    card(ui, |ui| {
        section(ui, &format!("{}  ›  {}", run.agent.name, node.task.lines().next().unwrap_or_default()));
        if let Some(refusal) = node.refusal {
            live_label(ui, format!("Refused at the limit: {refusal}. The spawn was logged and nothing ran."), theme::pal(ui).rose_text);
        }
        if let Some(by) = &node.spawned_by {
            note(ui, &format!("Spawned by {}.", spawned_by(by)));
        }
        if let Some(reason) = &node.reason {
            note(ui, &format!("Rule: {reason}"));
        }
        note(ui, &format!("Task: {}", node.task));
        let time = |t: Option<DateTime<Utc>>| t.map_or("not yet".to_string(), at);
        note(
            ui,
            &format!(
                "{} · started {} · ended {} · depth {} · {} findings",
                status_word(node.status),
                time(node.started),
                time(node.ended),
                node.depth,
                node.findings
            ),
        );
        if node.files_read.is_empty() {
            note(ui, "Files read: none. The engine has no tools; the diff is in its prompt. Read-only: it never edits files or posts.");
        } else {
            note(ui, &format!("Files read: {}", node.files_read.join(", ")));
        }
    });
}

// ---- the runs panel of a review ----

/// What the runs panel asks of the review.
pub(crate) enum PanelAct {
    None,
    /// Open the AI review sheet to run again.
    RunAgain,
    Details(String),
}

/// The runs of one change (frames 13 and 35): state, elapsed time, last output or why it failed, with Stop and Kill.
pub(crate) fn runs_panel(ui: &mut Ui, runs: &mut Runs, change: &ChangeRef) -> PanelAct {
    let now = Utc::now();
    let mine: Vec<Run> = runs.of(change).cloned().collect();
    let mut act = PanelAct::None;
    section(ui, &format!("Agents on {}", change.reference()));
    if mine.is_empty() {
        note(ui, "No AI run on this change yet.");
    } else {
        note(ui, &format!("{} runs · {}", mine.len(), ai::Tally::of(mine.iter()).summary()));
    }
    ScrollArea::vertical().id_salt("review-runs").max_height(theme::RUNS_LIST_HEIGHT).show(ui, |ui| {
        for run in &mine {
            card(ui, |ui| {
                ui.label(RichText::new(&run.agent.name).size(theme::sz(ui).body).color(theme::pal(ui).graphite));
                let drafts = match run.drafts.len() {
                    0 => String::new(),
                    n => format!(" · {n} AI drafts"),
                };
                let line = format!("{} · {}{drafts}", ai::state_word(run), ai::clock(ai::elapsed_secs(run, now)));
                ui.label(RichText::new(line).size(theme::sz(ui).body).color(state_colour(ui, run)));
                match (&run.error, run.state.is_active()) {
                    (Some(error), _) => live_label(ui, format!("Failed: {error}"), theme::pal(ui).rose_text),
                    (None, true) => note(ui, &ai::last_output(run, now)),
                    (None, false) => {}
                }
                ui.horizontal_wrapped(|ui| {
                    if run.state.is_active() {
                        if ui.add_enabled(!run.stopping, egui::Button::new("Stop")).clicked() {
                            stop(ui.ctx(), runs, run);
                        }
                        if ui.button("Kill").clicked() {
                            kill(ui.ctx(), runs, run);
                        }
                        if run.state == RunState::LooksStuck && ui.button("Kill and retry").clicked() {
                            retry(runs, run);
                        }
                    } else if matches!(run.state, RunState::Failed | RunState::Killed) && ui.button("Retry it").clicked() {
                        retry(runs, run);
                    }
                    if ui.button("Details").clicked() {
                        act = PanelAct::Details(run.id.clone());
                    }
                });
            });
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.button(keymap::hint(ui, "Run again", "review.ai_review")).clicked() {
            act = PanelAct::RunAgain;
        }
        let going = mine.iter().any(|r| r.state.is_active());
        if ui.add_enabled(going, egui::Button::new(keymap::hint(ui, "Stop these", "review.stop_runs"))).clicked() {
            stop_change(ui.ctx(), runs, &mine);
        }
    });
    act
}

/// Stop for every run of the change that is going.
pub(crate) fn stop_change(ctx: &Context, runs: &mut Runs, mine: &[Run]) {
    let ids: Vec<String> = mine.iter().filter(|r| r.state.is_active() && !r.stopping).map(|r| r.id.clone()).collect();
    if ids.is_empty() {
        return;
    }
    runs.act(ctx, move || {
        let results: Vec<Result<(), String>> = ids.iter().map(|id| ai::run_stop(id)).collect();
        match results.iter().find_map(|r| r.as_ref().err()) {
            Some(error) => Err(error.clone()),
            None => Ok(format!("Stopped {} runs.", ids.len())),
        }
    });
}

/// The details window of one run, opened from the panel: the Activity detail, in a window. Returns false once closed.
pub(crate) fn detail_window(ctx: &Context, runs: &mut Runs, id: &str, node: &mut Option<u32>) -> bool {
    let Some(run) = runs.find(id).cloned() else { return false };
    let mut open = !keymap::consume(ctx, "review.close_run");
    egui::Window::new(format!("Run: {}", run.agent.name)).open(&mut open).collapsible(false).default_width(theme::SHEET_WIDTH).vscroll(true).show(
        ctx,
        |ui| {
            let now = Utc::now();
            run_header(ui, &run, now, None);
            run_buttons(ui, runs, &run, false);
            run_body(ui, &run, node);
        },
    );
    open
}

#[cfg(test)]
mod tests {
    use craft::egui::{Context, Key, Modifiers};

    use super::*;
    use crate::ai::tests::{change, drawn, run};

    fn ago(secs: i64) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::seconds(secs)
    }

    fn stuck_and_failed() -> Runs {
        let mut failed = run("f", 482, RunState::Failed, ago(95));
        failed.error = Some("claude exited 124 after 5 min".into());
        let mut stuck = run("s", 482, RunState::LooksStuck, ago(400));
        stuck.pid = Some(48211);
        stuck.last_output = Some(ago(380));
        stuck.tail = vec!["read migrations/0087_invoice_lock.sql".into()];
        let mut runs = Runs::with(vec![failed, stuck, run("other", 7, RunState::Running, ago(5))]);
        runs.engine_for = Some(|_| Ok(pr_craft_core::agent::Engine::ClaudeCode));
        runs
    }

    /// One frame with `events` as the input.
    fn frame_with(ctx: &Context, events: Vec<craft::egui::Event>, mut draw: impl FnMut(&mut Ui)) {
        let input = craft::egui::RawInput { events, ..craft::egui::RawInput::default() };
        ctx.run_ui(input, &mut draw).drop_without_applying_deltas();
    }

    fn key(key: Key, repeat: bool) -> craft::egui::Event {
        craft::egui::Event::Key { key, physical_key: None, pressed: true, repeat, modifiers: Modifiers::NONE }
    }

    fn activity(filter: RunFilter, selected: Option<&str>) -> Activity {
        Activity { filter, selected: selected.map(String::from), node: None, stuck: StuckRules::default(), focus_selected: false }
    }

    #[test]
    fn the_runs_panel_lists_this_changes_runs_with_why_one_failed_and_nothing_of_another_change() {
        let mut runs = stuck_and_failed();
        let text = drawn(&Context::default(), |ui| {
            let _ = runs_panel(ui, &mut runs, &change(482));
        });
        assert!(text.contains("claude exited 124 after 5 min"), "the failure reason is in words:\n{text}");
        assert!(text.contains(ai::state_word(&run("x", 1, RunState::LooksStuck, ago(1)))), "{text}");
        assert!(!text.contains(ai::state_word(&run("x", 1, RunState::Running, ago(1)))), "a run of another change is not listed:\n{text}");
        let other = drawn(&Context::default(), |ui| {
            let _ = runs_panel(ui, &mut runs, &change(7));
        });
        assert!(!other.contains("claude exited 124"), "{other}");
    }

    #[test]
    fn activity_shows_the_selected_runs_pid_and_last_lines_and_only_the_runs_of_its_filter() {
        let mut runs = stuck_and_failed();
        let mut stuck = activity(RunFilter::Stuck, Some("s"));
        let text = drawn(&Context::default(), |ui| {
            let _ = stuck.ui(ui, &mut runs);
        });
        assert!(text.contains("pid 48211") && text.contains("read migrations/0087_invoice_lock.sql"), "{text}");
        let mut done = activity(RunFilter::Done, None);
        let text = drawn(&Context::default(), |ui| {
            let _ = done.ui(ui, &mut runs);
        });
        assert!(!text.contains("48211"), "the stuck run is not in the Done filter:\n{text}");
    }

    #[test]
    fn a_selected_run_that_leaves_the_filter_selects_nothing_so_s_and_x_cannot_hit_another_run() {
        let mut runs = stuck_and_failed();
        let mut view = activity(RunFilter::Stuck, Some("s"));
        let ctx = Context::default();
        drawn(&ctx, |ui| {
            let _ = view.ui(ui, &mut runs);
        });
        assert_eq!(view.selected.as_deref(), Some("s"));
        // The stuck run produced output and is running again; the same filter now holds no run.
        runs = Runs::with(vec![run("s", 482, RunState::Running, ago(400)), run("other", 7, RunState::LooksStuck, ago(900))]);
        frame_with(&ctx, vec![key(Key::S, false)], |ui| {
            let _ = view.ui(ui, &mut runs);
        });
        assert_eq!(view.selected.as_deref(), None, "it did not move to the other stuck run");
        assert!(runs.confirm.is_none());
    }

    #[test]
    fn r_asks_before_starting_and_a_held_r_asks_nothing() {
        let mut runs = stuck_and_failed();
        let ctx = Context::default();
        let mut view = activity(RunFilter::All, Some("f"));
        frame_with(&ctx, vec![key(Key::R, false)], |ui| {
            let _ = view.ui(ui, &mut runs);
        });
        assert!(runs.confirm.take().is_some(), "R asks first");
        // The key is still down on the next frame: that press is a repeat.
        frame_with(&ctx, vec![key(Key::R, false)], |ui| {
            let _ = view.ui(ui, &mut runs);
        });
        assert!(runs.confirm.is_none(), "a held R asks nothing more");
    }

    #[test]
    fn the_confirm_step_names_the_agent_waits_for_a_yes_and_esc_cancels_it() {
        let mut runs = stuck_and_failed();
        let failed = runs.find("f").cloned().expect("run");
        retry(&mut runs, &failed);
        let ctx = Context::default();
        let text = drawn(&ctx, |ui| confirm_window(ui.ctx(), &mut runs));
        assert!(text.contains(&failed.agent.name) && text.contains("usage"), "{text}");
        assert!(runs.confirm.is_some(), "drawing it starts nothing");
        frame_with(&ctx, vec![key(Key::Escape, false)], |ui| confirm_window(ui.ctx(), &mut runs));
        assert!(runs.confirm.is_none());
    }

    #[test]
    fn the_popover_lists_the_active_runs_and_counts_those_that_failed_apart() {
        let mut runs = stuck_and_failed();
        let text = drawn(&Context::default(), |ui| {
            let _ = popover(ui.ctx(), &mut runs);
        });
        assert!(text.contains("acme/api #482") && text.contains("acme/api #7"), "{text}");
        let failed_in_header = text.lines().find(|l| l.contains("going")).is_some_and(|l| l.contains('1') && l.contains("failed"));
        assert!(failed_in_header, "the failed run is counted on its own, not as done:\n{text}");
    }

    #[test]
    fn an_action_failure_and_a_failed_run_read_are_drawn_once_by_the_status_line_not_by_each_screen() {
        let mut runs = stuck_and_failed();
        runs.status = Some(Err("run abc already ended".into()));
        runs.error = Some("run.list failed".into());
        let line = drawn(&Context::default(), |ui| status_line(ui, &mut runs));
        assert!(line.contains("run abc already ended") && line.contains("run.list failed"), "{line}");
        let mut view = activity(RunFilter::All, None);
        let screen = drawn(&Context::default(), |ui| {
            let _ = view.ui(ui, &mut runs);
        });
        assert!(!screen.contains("run abc already ended"), "Activity does not draw it a second time:\n{screen}");
        assert!(screen.contains("banners for stuck runs are not available"), "the missing banners are disclosed:\n{screen}");
        let popped = drawn(&Context::default(), |ui| {
            let _ = popover(ui.ctx(), &mut runs);
        });
        assert!(!popped.contains("run abc already ended"), "{popped}");
    }

    #[test]
    fn while_the_confirm_step_is_open_the_keys_of_the_screens_under_it_do_nothing() {
        let mut runs = stuck_and_failed();
        let failed = runs.find("f").cloned().expect("run");
        retry(&mut runs, &failed);
        let mut seen = (true, true);
        frame_with(&Context::default(), vec![key(Key::R, false), key(Key::Escape, false)], |ui| {
            confirm_window(ui.ctx(), &mut runs);
            seen = (ui.input(|i| i.key_pressed(Key::R)), ui.input(|i| i.key_pressed(Key::Escape)));
        });
        assert_eq!(seen, (false, false), "R and Esc were taken by the confirm step");
    }

    #[test]
    fn a_retry_names_the_engine_to_be_used_now_not_the_one_of_the_old_run() {
        let mut runs = stuck_and_failed();
        runs.engine_for = Some(|_| Ok(pr_craft_core::agent::Engine::ZaiGlm));
        let failed = runs.find("f").cloned().expect("run");
        retry(&mut runs, &failed);
        let paid = runs.confirm.expect("asked");
        assert_eq!(paid.engine, pr_craft_core::agent::Engine::ZaiGlm, "the lookup, not run.engine, decides");
    }
}
