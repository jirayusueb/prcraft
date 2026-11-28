//! The CI job log (Figma frame 94): the checks of a change on the left, the log of the chosen one in the middle with the failing
//! lines tinted, and what is not built on the right. The log is read by `checks.log` and cut to its last lines; the screen says so
//! when it was cut. Re-running a job asks first. An AI explanation of the failure is not built, so the right side says that.

use std::time::{Duration, Instant};

use craft::egui::{self, Align, Color32, Context, FontId, Layout, Margin, OpenUrl, Rect, RichText, ScrollArea, Sense, Ui, vec2};
use pr_craft_core::ChangeRef;
use pr_craft_core::mine::{Check, CheckState, Checks};
use pr_craft_engine::mine::{self as engine, ChecksList, LogResult};

use crate::components::{self, BannerKind, Bar, Pill, PillKind};
use crate::panel::Load;
use crate::{keymap, live_label, theme};

/// How long the re-run confirm is shown before it can be answered.
const ARM: Duration = Duration::from_millis(600);

/// Whether `url` is an https address on the forge's own host. A check's link is whatever the third party that posted it says, so
/// nothing else is handed to the system's opener.
fn trusted(url: &str, host: &str) -> bool {
    url.strip_prefix("https://").and_then(|rest| rest.split(['/', '?', '#']).next()).is_some_and(|found| found.eq_ignore_ascii_case(host))
}

pub(crate) enum Outcome {
    Stay,
    Back,
}

pub(crate) struct ChecksScreen {
    change: ChangeRef,
    live: bool,
    prefer: Option<String>,
    list: Load<Result<ChecksList, String>>,
    selected: Option<String>,
    log: Load<Result<LogResult, String>>,
    /// The check the log on screen belongs to.
    log_of: Option<String>,
    confirming: bool,
    /// When the re-run confirm opened: it cannot be answered until [`ARM`] has passed.
    confirm_since: Option<Instant>,
    focus_confirm: bool,
    /// Where the log should scroll to on the next frame, and where it is.
    scroll: Option<f32>,
    at: f32,
    rerun: Load<Result<String, String>>,
    /// What the last re-run said, in words.
    said: Option<Result<String, String>>,
}

impl ChecksScreen {
    pub(crate) fn open(ctx: &Context, change: ChangeRef, _title: String, prefer: Option<&str>, live: bool) -> Self {
        let mut screen = ChecksScreen {
            change,
            live,
            prefer: prefer.map(str::to_string),
            list: Load::Idle,
            selected: None,
            log: Load::Idle,
            log_of: None,
            confirming: false,
            confirm_since: None,
            focus_confirm: false,
            scroll: None,
            at: 0.0,
            rerun: Load::Idle,
            said: None,
        };
        screen.reload(ctx);
        screen
    }

    fn reload(&mut self, ctx: &Context) {
        if self.live {
            let change = self.change.clone();
            self.list = Load::start(ctx, move || engine::checks_list(&change).map_err(crate::fail));
        }
    }

    /// A screen whose checks and log have been read, for the render tests.
    #[cfg(test)]
    pub(crate) fn fixture(change: ChangeRef, list: ChecksList, selected: &str, log: LogResult) -> Self {
        let mut screen = ChecksScreen::open(&Context::default(), change, String::new(), None, false);
        screen.list = Load::Ready(Ok(list));
        screen.selected = Some(selected.to_string());
        screen.log_of = Some(selected.to_string());
        screen.log = Load::Ready(Ok(log));
        screen
    }

    #[cfg(test)]
    pub(crate) fn confirm_rerun(&mut self) {
        self.confirming = true;
        self.confirm_since = Instant::now().checked_sub(ARM * 2);
    }

    fn open_confirm(&mut self) {
        self.confirming = true;
        self.confirm_since = Some(Instant::now());
        self.focus_confirm = true;
    }

    fn armed(&self) -> bool {
        self.confirm_since.is_some_and(|since| since.elapsed() >= ARM)
    }

    fn checks(&self) -> &[Check] {
        self.list.ok().map_or(&[], |l| l.checks.as_slice())
    }

    fn chosen(&self) -> Option<&Check> {
        let id = self.selected.as_deref()?;
        self.checks().iter().find(|c| c.id == id)
    }

    /// Reads the log of the chosen check, once, when it changes.
    fn want_log(&mut self, ctx: &Context) {
        let Some(check) = self.chosen().cloned() else { return };
        if self.log_of.as_deref() == Some(check.id.as_str()) || !self.live {
            return;
        }
        self.log_of = Some(check.id.clone());
        if !check.has_log {
            self.log = Load::Idle;
            return;
        }
        let (change, id, all) = (self.change.clone(), check.id, check.state != CheckState::Failing);
        self.log = Load::start(ctx, move || engine::checks_log(&change, &id, all).map_err(crate::fail));
    }

    fn poll(&mut self, ctx: &Context) -> Option<Result<String, String>> {
        if self.list.poll() && self.selected.is_none() {
            let first_failing = self.checks().iter().find(|c| c.state == CheckState::Failing).map(|c| c.id.clone());
            let preferred = self.prefer.clone().filter(|id| self.checks().iter().any(|c| &c.id == id));
            self.selected = preferred.or(first_failing).or_else(|| self.checks().first().map(|c| c.id.clone()));
        }
        if self.log.poll() {
            // A log opens at its first failing line: that is what the reader came for.
            self.scroll = self.first_failing();
        }
        if self.rerun.poll() {
            self.confirming = false;
            self.said = self.rerun.ok().cloned().map(Ok).or_else(|| self.rerun.ready().and_then(|r| r.as_ref().err().cloned()).map(Err));
            self.rerun = Load::Idle;
            self.reload(ctx);
            self.log_of = None;
            return self.said.clone();
        }
        None
    }

    fn send_rerun(&mut self, ctx: &Context) {
        let Some(check) = self.chosen().map(|c| c.id.clone()) else { return };
        if self.rerun.busy() {
            return;
        }
        let change = self.change.clone();
        self.rerun = Load::start(ctx, move || engine::checks_rerun(&change, &check, true).map(|d| d.message).map_err(crate::fail));
    }

    /// The title bar and the three panes. Esc, or the back button, returns to the inbox.
    pub(crate) fn ui(&mut self, ui: &mut Ui, runs: &mut crate::ai::Runs, bar: &mut Bar) -> Outcome {
        let ctx = ui.ctx().clone();
        if let Some(result) = self.poll(&ctx) {
            runs.status = Some(result);
        }
        self.want_log(&ctx);
        let key = keymap::label(ui, "review.back");
        let place = format!("{}{}", self.change.forge.number_sigil(), self.change.number);
        let back = bar.left(ui, |ui| {
            let back = components::title_button(ui, "‹ Inbox", &key).clicked();
            components::breadcrumb_with(ui, true, &[self.change.repo.as_str(), place.as_str(), "Checks"]);
            back
        });
        if self.confirming && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.confirming = false;
        }
        let leave = back || (!self.confirming && keymap::pressed(ui, "review.back"));
        let rect = ui.available_rect_before_wrap();
        let p = theme::pal(ui);
        let side = vec2(theme::FILES_WIDTH + 1.0, rect.height());
        let right = if rect.width() >= 1100.0 { theme::PANEL_WIDTH } else { 0.0 };
        let list = Rect::from_min_size(rect.min, side);
        let panel = Rect::from_min_max(egui::pos2(rect.right() - right, rect.top()), rect.right_bottom());
        let centre = Rect::from_min_max(list.right_top(), egui::pos2(panel.left(), rect.bottom()));
        components::pane_at(ui, "checks-list", list, p.hover, Margin { left: 12, right: 13, top: 14, bottom: 8 }, |ui| self.list_pane(ui));
        components::seam_right(ui, list);
        components::pane_at(ui, "checks-log", centre, p.paper, Margin::ZERO, |ui| self.log_pane(ui, &ctx));
        if right > 0.0 {
            components::seam_right(ui, centre);
            components::pane_at(ui, "checks-why", panel, p.hover, Margin { left: 16, right: 16, top: 14, bottom: 8 }, |ui| self.why_pane(ui));
        }
        if leave { Outcome::Back } else { Outcome::Stay }
    }

    fn list_pane(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        let summary = match self.list.ok() {
            Some(l) => Self::passing(&l.summary),
            None => String::new(),
        };
        ui.horizontal(|ui| {
            components::label(ui, "Checks", 15.0, p.graphite);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let said = components::label(ui, summary, 12.0, p.text_tertiary);
                ui.ctx().accesskit_node_builder(said.id, |node| node.set_live(egui::accesskit::Live::Polite));
            });
        });
        ui.add_space(8.0);
        match &self.list {
            Load::Ready(Ok(_)) if self.checks().is_empty() => {
                components::note(ui, "This change has no checks.", p.text_secondary);
            }
            Load::Ready(Err(e)) => live_label(ui, format!("Could not read the checks: {e}"), p.rose_text),
            Load::Ready(Ok(_)) => {
                let rows: Vec<Check> = self.checks().to_vec();
                let mut chosen = None;
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for check in &rows {
                        let on = self.selected.as_deref() == Some(check.id.as_str());
                        if Self::check_item(ui, check, on).clicked() {
                            chosen = Some(check.id.clone());
                        }
                    }
                });
                if chosen.is_some() && chosen != self.selected {
                    self.selected = chosen;
                    self.confirming = false;
                }
            }
            _ => {
                components::note(ui, "Reading the checks…", p.text_tertiary);
            }
        }
    }

    fn passing(summary: &Checks) -> String {
        match summary.total {
            0 => String::new(),
            n => format!("{} of {n} passing", summary.passed),
        }
    }

    fn state_colour(ui: &Ui, state: CheckState) -> Color32 {
        let p = theme::pal(ui);
        match state {
            CheckState::Passing => p.moss,
            CheckState::Failing => p.rose,
            CheckState::Running => p.ember,
            CheckState::Skipped => p.haze,
        }
    }

    fn state_word(state: CheckState) -> &'static str {
        match state {
            CheckState::Passing => "passed",
            CheckState::Failing => "failed",
            CheckState::Running => "running",
            CheckState::Skipped => "skipped",
        }
    }

    /// One check: a dot, its name in the code font and how long it took. The chosen one is filled with the inverse tone.
    fn check_item(ui: &mut Ui, check: &Check, on: bool) -> egui::Response {
        let p = theme::pal(ui);
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
        let (ink, quiet) = if on { (p.on_inverse, p.on_inverse_quiet) } else { (p.graphite, p.text_tertiary) };
        if on {
            ui.painter().rect_filled(rect, 8.0, p.inverse);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 8.0, p.pressed);
        }
        ui.painter().circle_filled(egui::pos2(rect.left() + 12.0, rect.center().y), 3.5, Self::state_colour(ui, check.state));
        let font = FontId::monospace(12.5);
        let took = check.took().unwrap_or_default();
        let right = ui.painter().layout_no_wrap(took, FontId::monospace(11.5), quiet);
        ui.painter().galley(egui::pos2(rect.right() - 10.0 - right.size().x, rect.center().y - right.size().y / 2.0), right.clone(), quiet);
        let name = ui.painter().layout_no_wrap(check.name.clone(), font, ink);
        let room = rect.width() - 24.0 - right.size().x - 16.0;
        let shown = if name.size().x > room { ui.painter().layout(check.name.clone(), FontId::monospace(12.5), ink, room.max(20.0)) } else { name };
        ui.painter().galley(egui::pos2(rect.left() + 24.0, rect.center().y - shown.size().y / 2.0), shown, ink);
        components::ring(ui, &response, rect, 8.0);
        let described = format!("{}, {}{}", check.name, Self::state_word(check.state), check.took().map(|t| format!(", {t}")).unwrap_or_default());
        response.widget_info(|| egui::WidgetInfo::selected(egui::accesskit::Role::Button, true, on, described.clone()));
        response
    }

    fn log_pane(&mut self, ui: &mut Ui, ctx: &Context) {
        let p = theme::pal(ui);
        let Some(check) = self.chosen().cloned() else {
            components::padded(ui, Margin::symmetric(20, 36), |ui| match &self.list {
                Load::Ready(Err(_)) => components::empty_pane(ui, "No log to show", "The checks could not be read, so there is no log to open."),
                Load::Ready(Ok(_)) => components::empty_pane(ui, "No checks", "This change has no checks, so there is no log."),
                _ => components::empty_pane(ui, "Reading the checks", "The log appears when a check is chosen."),
            });
            return;
        };
        components::padded(ui, Margin { left: 20, right: 20, top: 12, bottom: 10 }, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                components::mono_label(ui, check.name.clone(), 15.0, p.graphite);
                let (word, colour) = match check.state {
                    CheckState::Failing => ("Failed", p.rose_text),
                    CheckState::Passing => ("Passed", p.moss_text),
                    CheckState::Running => ("Running", p.ember_text),
                    CheckState::Skipped => ("Skipped", p.text_secondary),
                };
                let took = check.took().map(|t| format!(" after {t}")).unwrap_or_default();
                components::label(ui, format!("{word}{took}"), 12.0, colour);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| self.log_actions(ui, ctx, &check));
            });
            if self.confirming {
                ui.add_space(8.0);
                let body = format!(
                    "{} starts {} again from the beginning, and the jobs that depend on it run again too. It uses CI time on the forge and its log is empty until it finishes.",
                    self.change.forge.name(),
                    check.name
                );
                let said = components::banner(ui, BannerKind::Warn, &format!("Re-run {}?", check.name), &body);
                ui.ctx().accesskit_node_builder(said.id, |node| node.set_live(egui::accesskit::Live::Polite));
            }
            if let Some(Err(e)) = &self.said {
                live_label(ui, e.clone(), p.rose_text);
            }
        });
        components::rule(ui);
        self.log_text(ui, &check);
    }

    fn log_actions(&mut self, ui: &mut Ui, ctx: &Context, check: &Check) {
        ui.spacing_mut().item_spacing.x = 8.0;
        let busy = self.rerun.busy();
        if self.confirming {
            let armed = self.armed();
            let label = format!("Re-run {}", check.name);
            let pill = Pill::new(PillKind::Primary, &label).small().enabled(!busy && armed).max_width(220.0).show(ui);
            if pill.clicked() && armed && !crate::panel::repeating(ui) {
                self.send_rerun(ctx);
            }
            if armed && self.focus_confirm {
                pill.request_focus();
                self.focus_confirm = false;
            }
            if Pill::new(PillKind::Secondary, "Cancel").small().show(ui).clicked() {
                self.confirming = false;
            }
            if !armed {
                ctx.request_repaint_after(ARM);
            }
            return;
        }
        let can = check.has_log && check.state != CheckState::Running;
        if Pill::new(PillKind::Secondary, "Re-run job").small().enabled(can && !busy).show(ui).clicked() {
            self.open_confirm();
        }
        if let Some(url) = check.url.as_deref().filter(|u| trusted(u, self.change.host_name()))
            && Pill::new(PillKind::Quiet, &format!("Open on {}", self.change.forge.name())).small().show(ui).clicked()
        {
            ctx.open_url(OpenUrl::new_tab(url));
        }
        if self.first_failing().is_some() && Pill::new(PillKind::Quiet, "Jump to first failing line").small().show(ui).clicked() {
            self.scroll = self.first_failing();
        }
    }

    /// The offset of the first line that reads like a failure, a little above it, for the jump and the first view of a log.
    fn first_failing(&self) -> Option<f32> {
        let log = &self.log.ok()?.log;
        log.lines.iter().position(|l| l.failed).map(|at| (at as f32 * 22.0 - 44.0).max(0.0))
    }

    fn log_text(&mut self, ui: &mut Ui, check: &Check) {
        let p = theme::pal(ui);
        let pad = Margin::symmetric(20, 8);
        let at = self.at;
        let mut new_at = at;
        match &self.log {
            Load::Ready(Ok(read)) => {
                let log = &read.log;
                if log.truncated {
                    let note = if log.from_cut {
                        format!(
                            "Showing the last {} lines of a longer log; the first line shown is numbered {}. The start was left out, so numbers count from where the kept text begins, not from the start of the job.",
                            log.lines.len(),
                            log.total - log.lines.len() + 1
                        )
                    } else {
                        format!("Showing the last {} of {} lines; the start of the log was left out.", log.lines.len(), log.total)
                    };
                    components::padded(ui, pad, |ui| live_label(ui, note, p.ember_text));
                }
                if log.lines.is_empty() {
                    components::padded(ui, pad, |ui| components::note(ui, "The forge sent an empty log for this job.", p.text_secondary));
                    return;
                }
                let first = log.total - log.lines.len();
                let row = 22.0;
                let height = ui.available_height();
                let max = (log.lines.len() as f32 * row - height).max(0.0);
                let mut want = self.scroll.take();
                ui.input(|i| {
                    for (key, to) in [
                        (egui::Key::PageDown, (at + height).min(max)),
                        (egui::Key::PageUp, (at - height).max(0.0)),
                        (egui::Key::Home, 0.0),
                        (egui::Key::End, max),
                    ] {
                        if i.key_pressed(key) {
                            want = Some(to);
                        }
                    }
                });
                let mut area = ScrollArea::both().auto_shrink([false, false]);
                if let Some(y) = want {
                    area = area.vertical_scroll_offset(y);
                }
                let shown = area.show_rows(ui, row, log.lines.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for line_at in range {
                        if let Some(line) = log.lines.get(line_at) {
                            Self::log_row(ui, first + line_at + 1, &line.text, line.failed, row);
                        }
                    }
                });
                new_at = shown.state.clamped_offset().y;
            }
            Load::Ready(Err(e)) => {
                components::padded(ui, pad, |ui| live_label(ui, format!("Could not read the log: {e}"), p.rose_text));
            }
            Load::Pending(_) => {
                components::padded(ui, pad, |ui| components::note(ui, "Reading the log…", p.text_tertiary));
            }
            Load::Idle if !check.has_log => {
                let why = "This check is an external status, not a job the forge keeps a log for. Open it on the forge.";
                components::padded(ui, pad, |ui| components::note(ui, why, p.text_secondary));
            }
            Load::Idle => {}
        }
        self.at = new_at;
    }

    /// One log line: its number (with a mark on a failing one, so the tint is not the only sign) and its text in the code font.
    fn log_row(ui: &mut Ui, number: usize, text: &str, failed: bool, height: f32) {
        let p = theme::pal(ui);
        let shown: String = text.chars().take(theme::MAX_LINE_CHARS).collect();
        let font = FontId::monospace(12.0);
        let galley = ui.painter().layout_no_wrap(shown, font.clone(), if failed { p.rose_text } else { p.graphite });
        let width = (64.0 + galley.size().x + 20.0).max(ui.available_width());
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        if failed {
            ui.painter().rect_filled(rect, 0.0, p.rose_bg);
        }
        let mark = if failed { format!("! {number}") } else { number.to_string() };
        let num = ui.painter().layout_no_wrap(mark, font, if failed { p.rose_text } else { p.text_tertiary });
        ui.painter().galley(egui::pos2(rect.left() + 52.0 - num.size().x, rect.center().y - num.size().y / 2.0), num, p.text_tertiary);
        ui.painter().galley(egui::pos2(rect.left() + 64.0, rect.center().y - galley.size().y / 2.0), galley, p.graphite);
        let name = format!("line {number}{}: {text}", if failed { ", failing" } else { "" });
        response.widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Label, true, name.clone()));
    }

    fn why_pane(&self, ui: &mut Ui) {
        let p = theme::pal(ui);
        components::label(ui, "Why it failed", 15.0, p.graphite);
        ui.add_space(8.0);
        components::banner(
            ui,
            BannerKind::Info,
            "No explanation yet",
            "Explaining a failure with the AI engine, and drafting a fix, are not built. The lines that read like a failure are tinted so you can find them.",
        );
        ui.add_space(8.0);
        let read = match self.log.ready() {
            Some(Ok(l)) => {
                let more = if l.log.from_cut { "at least " } else { "" };
                format!("{more}{} lines read{}.", l.log.total, if l.log.truncated { ", the last kept" } else { "" })
            }
            _ => String::new(),
        };
        let note = format!("The tint is a keyword match, so a line can be tinted that is not the failure. {read}");
        ui.add(egui::Label::new(RichText::new(note).size(11.5).color(p.text_tertiary)).wrap());
    }
}

#[cfg(test)]
mod tests {
    use super::trusted;

    #[test]
    fn only_an_https_link_on_the_forges_own_host_is_opened() {
        assert!(trusted("https://github.com/acme/api/actions/runs/1/job/2", "github.com"));
        for url in [
            "http://github.com/x",
            "https://github.com.evil.dev/x",
            "https://github.com@evil.dev/x",
            "https://ci.example.com/build/1",
            "javascript:alert(1)",
            "file:///etc/passwd",
        ] {
            assert!(!trusted(url, "github.com"), "{url}");
        }
    }
}
