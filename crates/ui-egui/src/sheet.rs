//! The AI review sheet (Figma frames 29, 54, 81): which agents run, what each file goes to, the options the engine
//! offers, a preview of exactly what is sent, and Start. Start is allowed only after a preview of the same options, and
//! sends that preview's hashes, so a change that moved since is refused by the engine.

use craft::egui::{self, Align, Align2, Context, Layout, Margin, RichText, ScrollArea, Ui};
use pr_craft_core::ChangeRef;
use pr_craft_core::agent::Agent;
use pr_craft_engine::ai::{Plan, ReviewPreview};

use crate::ai::{self, Fetch, Grid, ReviewOptions, Runs, StartGate};
use crate::components::{self, Pill, PillKind};
use crate::keymap;
use crate::live_label;
use crate::review::card;
use crate::settings::{note, preview_ui, section};
use crate::theme;

/// Rows of the coverage grid drawn; the sheet says when files are not shown.
const GRID_ROWS: usize = 200;

pub(crate) enum Event {
    None,
    Close,
    /// "Create an agent" (frame 81).
    CreateAgent,
}

pub(crate) struct AiSheet {
    change: ChangeRef,
    options: ReviewOptions,
    /// Every agent that runs on this repository, ticked or not: the plan lists only the ticked ones.
    agents: Result<Vec<Agent>, String>,
    plan: Fetch<Plan>,
    plan_for: ReviewOptions,
    preview: Fetch<ReviewPreview>,
    preview_for: ReviewOptions,
    gate: Option<StartGate>,
    starting: Fetch<Vec<String>>,
    /// The agent row J and K are on.
    cursor: Option<usize>,
    focus_cursor: bool,
    /// How many files the reviewer marked viewed, for the Skip option's label.
    viewed: usize,
    /// ⌘↵ with no preview shows the preview instead of starting; this says why.
    notice: Option<String>,
}

impl AiSheet {
    pub(crate) fn open(ctx: &Context, change: ChangeRef, viewed: usize) -> Self {
        let options = ReviewOptions::default();
        let (plan_change, plan_options) = (change.clone(), options.clone());
        AiSheet {
            agents: ai::agents_on(&change),
            plan: Fetch::start(ctx, move || ai::plan(&plan_change, &plan_options)),
            plan_for: options.clone(),
            preview: Fetch::Idle,
            preview_for: options.clone(),
            gate: None,
            starting: Fetch::Idle,
            cursor: None,
            focus_cursor: false,
            viewed,
            notice: None,
            change,
            options,
        }
    }

    /// A sheet over `plan` and `agents` that has read nothing from this Mac, for the render tests.
    #[cfg(test)]
    pub(crate) fn fixture(change: ChangeRef, plan: Plan, agents: Vec<Agent>) -> Self {
        let options = ReviewOptions::default();
        AiSheet {
            change,
            agents: Ok(agents),
            plan: Fetch::Ready(plan),
            plan_for: options.clone(),
            preview: Fetch::Idle,
            preview_for: options.clone(),
            gate: None,
            starting: Fetch::Idle,
            cursor: None,
            focus_cursor: false,
            viewed: 2,
            notice: None,
            options,
        }
    }

    /// What Start would do now: the plan matches the options, it is not blocked, and the preview matches them too.
    fn can_start(&self) -> bool {
        let Fetch::Ready(plan) = &self.plan else { return false };
        plan.blocked.is_none()
            && self.plan_for == self.options
            && ai::run_count(plan) > 0
            && self.gate.as_ref().is_some_and(|g| g.matches(&self.options))
            && !self.starting.loading()
    }

    fn start_preview(&mut self, ctx: &Context) {
        if self.preview.loading() {
            return;
        }
        let (change, options) = (self.change.clone(), self.options.clone());
        self.reset_for_preview();
        self.notice = None;
        self.preview = Fetch::start(ctx, move || ai::preview(&change, &options));
    }

    /// What a new preview throws away: the gate of the last one and the refusal of the last start.
    fn reset_for_preview(&mut self) {
        self.preview_for = self.options.clone();
        self.gate = None;
        self.starting = Fetch::Idle;
    }

    fn start(&mut self, ctx: &Context) {
        let Some(gate) = self.gate.as_ref().filter(|_| self.can_start()) else { return };
        let (change, options, hashes) = (self.change.clone(), self.options.clone(), gate.hashes());
        self.starting = Fetch::start(ctx, move || ai::start(&change, &options, Some(hashes)));
    }

    /// An option changed: plan again, and what was previewed no longer matches.
    fn options_changed(&mut self, ctx: &Context) {
        if self.options == self.plan_for {
            return;
        }
        let (change, options) = (self.change.clone(), self.options.clone());
        self.plan_for = options.clone();
        self.plan = Fetch::start(ctx, move || ai::plan(&change, &options));
        self.preview = Fetch::Idle;
        self.gate = None;
    }

    fn poll(&mut self) {
        self.plan.poll();
        let was_loading = self.preview.loading();
        self.preview.poll();
        if was_loading && let Fetch::Ready(preview) = &self.preview {
            self.gate = Some(StartGate::of(preview, &self.preview_for));
        }
        // A start the engine refused because the change moved leaves nothing the reviewer saw: preview again, once.
        let was_starting = self.starting.loading();
        self.starting.poll();
        if was_starting && matches!(&self.starting, Fetch::Failed(error) if error.contains("moved since")) {
            self.preview = Fetch::Idle;
            self.gate = None;
        }
    }

    /// Draws the sheet over the review. ⌘↵ starts, P previews, Esc closes, J and K walk the agents.
    pub(crate) fn ui(&mut self, ctx: &Context, runs: &mut Runs) -> Event {
        self.poll();
        if let Fetch::Ready(ids) = &self.starting {
            runs.status = Some(Ok(format!("Started {} runs. Findings arrive as AI drafts; nothing posts.", ids.len())));
            runs.refresh_now();
            return Event::Close;
        }
        let mut event = self.keys(ctx);
        let height = (ctx.content_rect().height() - theme::SHEET_MARGIN).max(theme::SHEET_MIN_HEIGHT);
        let frame = egui::Frame::window(&ctx.global_style()).fill(theme::pal(ctx).raised).corner_radius(10).inner_margin(0);
        egui::Window::new(format!("Review {} with AI", self.change.reference()))
            .title_bar(false)
            .frame(frame)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, theme::SHEET_TOP])
            .fixed_size([theme::SHEET_WIDTH, height])
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let engine = match &self.plan {
                    Fetch::Ready(plan) => plan.general_engine.name(),
                    _ => "",
                };
                components::padded(ui, Margin { left: 20, right: 20, top: 16, bottom: 4 }, |ui| {
                    ui.horizontal(|ui| {
                        components::label(ui, format!("Review {} with AI", self.change.reference()), 16.0, theme::pal(ui).graphite);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            components::label(ui, engine, 12.5, theme::pal(ui).text_secondary);
                        });
                    });
                });
                ScrollArea::vertical().max_height(height - 104.0).auto_shrink([false, false]).show(ui, |ui| {
                    components::padded(ui, Margin::symmetric(20, 4), |ui| self.body(ui, &mut event));
                });
                components::rule(ui);
                components::padded(ui, Margin::symmetric(20, 12), |ui| self.footer(ui, &mut event));
            });
        self.options_changed(ctx);
        event
    }

    fn keys(&mut self, ctx: &Context) -> Event {
        if keymap::consume(ctx, "sheet.close") {
            return Event::Close;
        }
        if keymap::consume(ctx, "sheet.start") {
            if self.can_start() {
                self.start(ctx);
            } else if self.gate.as_ref().is_none_or(|g| !g.matches(&self.options)) {
                self.start_preview(ctx);
                self.notice =
                    Some(format!("Preview first: this is exactly what would be sent. Press {} again to start.", keymap::label(ctx, "sheet.start")));
            }
        }
        // Letters belong to a text field with focus; none is on the sheet, but the review under it may still hold one.
        if ctx.text_edit_focused() {
            return Event::None;
        }
        if keymap::consume(ctx, "sheet.preview") {
            self.start_preview(ctx);
        }
        let step = (keymap::consume(ctx, "sheet.next"), keymap::consume(ctx, "sheet.prev"));
        if step.0 || step.1 {
            let len = self.agents.as_ref().map_or(0, Vec::len);
            self.cursor = ai::nav_step(self.cursor, len, step.0);
            self.focus_cursor = true;
        }
        Event::None
    }

    fn body(&mut self, ui: &mut Ui, event: &mut Event) {
        note(
            ui,
            "Runs every agent whose scope matches this change, plus a general pass for anything left over. Findings come back as drafts in the diff. Nothing posts on its own.",
        );
        match &self.plan {
            Fetch::Idle | Fetch::Loading(_) => return live_label(ui, "Reading the change and its diff…", theme::pal(ui).text_secondary),
            Fetch::Failed(error) => return live_label(ui, format!("Could not plan the review: {error}"), theme::pal(ui).rose_text),
            Fetch::Ready(_) => {}
        }
        let Fetch::Ready(plan) = &self.plan else { return };
        let plan_stale = self.plan_for != self.options;
        if plan_stale {
            live_label(ui, "Planning again with your options…", theme::pal(ui).text_secondary);
        }
        if let Some(blocked) = &plan.blocked {
            live_label(ui, format!("This review cannot start: {blocked}"), theme::pal(ui).rose_text);
        }
        for text in &plan.notes {
            ui.label(RichText::new(text).size(theme::sz(ui).body).color(theme::pal(ui).ember_text));
        }
        ui.add_space(theme::SPACE_2);
        let plan = PlanView::of(plan);
        self.agent_rows(ui, &plan, event);
        ui.add_space(theme::SPACE_2);
        coverage_ui(ui, &plan);
        ui.add_space(theme::SPACE_2);
        self.option_rows(ui, &plan);
        self.preview_section(ui, &plan);
    }

    fn agent_rows(&mut self, ui: &mut Ui, plan: &PlanView, event: &mut Event) {
        section(ui, "Agents");
        let agents = match &self.agents {
            Ok(agents) => agents.clone(),
            Err(error) => return live_label(ui, format!("Could not read the agents: {error}"), theme::pal(ui).rose_text),
        };
        if agents.is_empty() {
            card(ui, |ui| {
                section(ui, "No review agents yet");
                note(ui, "The general pass reviews every file with the default prompt. An agent adds a focused prompt, skills and a file scope.");
                if ui.button("Create an agent").clicked() {
                    *event = Event::CreateAgent;
                }
            });
        }
        let mut toggled = None;
        for (i, agent) in agents.iter().enumerate() {
            let ticked = self.options.agents.as_ref().is_none_or(|ids| ids.contains(&agent.id));
            let planned = plan.agents.iter().find(|(id, _)| *id == agent.id).map(|(_, files)| *files);
            let detail = match (ticked, planned) {
                (false, _) => "Left out: its files go to the general pass".to_string(),
                (true, Some(0)) => format!("Skipped: outside {}", scope_text(agent)),
                (true, Some(files)) => components::files_word(files),
                (true, None) => "Planning…".to_string(),
            };
            let response = components::check_row(ui, ticked, &agent.name, &scope_text(agent), &format!("{detail}  ·  {}", agent.engine.name()));
            if self.cursor == Some(i) && self.focus_cursor {
                response.request_focus();
                response.scroll_to_me(None);
            }
            if response.clicked() {
                toggled = Some((i, !ticked));
            }
        }
        self.focus_cursor = false;
        if let Some((i, on)) = toggled {
            self.cursor = Some(i);
            self.toggle(&agents, i, on);
        }
        let general = if plan.general == 0 { "Nothing left over".to_string() } else { components::files_word(plan.general) };
        components::included_row(ui, "General pass", "files no agent covers", &format!("{general}  ·  {}", plan.general_engine));
    }

    /// Ticking agents sets the list the engine takes; all ticked is the default, `None`.
    fn toggle(&mut self, agents: &[Agent], i: usize, on: bool) {
        let mut ids: Vec<String> =
            agents.iter().filter(|a| self.options.agents.as_ref().is_none_or(|ids| ids.contains(&a.id))).map(|a| a.id.clone()).collect();
        let Some(id) = agents.get(i).map(|a| a.id.clone()) else { return };
        ids.retain(|x| *x != id);
        if on {
            ids.push(id);
        }
        self.options.agents = (ids.len() != agents.len()).then_some(ids);
    }

    fn option_rows(&mut self, ui: &mut Ui, plan: &PlanView) {
        let hint = match (self.options.skip_viewed, self.viewed) {
            (true, _) => format!("{} would be skipped", plan.skipped_viewed),
            (false, n) => format!("{n} marked"),
        };
        if let Some(on) = components::form_row(ui, "Skip files I marked viewed", &hint, |ui| {
            components::toggle(ui, self.options.skip_viewed, "Skip files I marked viewed")
        }) {
            self.options.skip_viewed = on;
        }
        components::rule(ui);
        ui.add_space(4.0);
        note(ui, "Merging duplicate findings into one summary is not built.");
        let read = format!(
            "May read: {}  ·  {} never-send patterns  ·  secret redaction {}",
            plan.scope,
            plan.never_send,
            if plan.redact { "on" } else { "off" }
        );
        note(ui, &read);
        ui.label(RichText::new(&plan.notice).size(theme::sz(ui).body).color(theme::pal(ui).ember_text));
    }

    fn preview_section(&mut self, ui: &mut Ui, _plan: &PlanView) {
        ui.add_space(theme::SPACE_2);
        section(ui, "Preview");
        let busy = self.preview.loading();
        if Pill::new(PillKind::Secondary, "Preview what is sent").key(ui, "sheet.preview").small().enabled(!busy).show(ui).clicked() {
            self.start_preview(ui.ctx());
        }
        if let Some(text) = &self.notice {
            live_label(ui, text.as_str(), theme::pal(ui).text_secondary);
        }
        match &self.preview {
            Fetch::Idle => note(ui, "Start unlocks once you have previewed these options."),
            Fetch::Loading(_) => live_label(ui, "Building the prompts…", theme::pal(ui).text_secondary),
            Fetch::Failed(error) => live_label(ui, error.as_str(), theme::pal(ui).rose_text),
            Fetch::Ready(preview) => preview_ui(ui, preview),
        }
    }

    fn footer(&mut self, ui: &mut Ui, event: &mut Event) {
        if let Fetch::Failed(error) = &self.starting {
            live_label(ui, error.as_str(), theme::pal(ui).rose_text);
        }
        if self.starting.loading() {
            live_label(ui, "Starting the runs…", theme::pal(ui).text_secondary);
        }
        let ready = self.can_start();
        let mut summary = match &self.plan {
            Fetch::Ready(plan) => {
                let runs = ai::run_count(plan);
                format!("{runs} {}  ·  no time estimate", if runs == 1 { "run" } else { "runs" })
            }
            _ => String::new(),
        };
        if !ready && !self.starting.loading() {
            summary.push_str(&format!("  ·  {} previews first", keymap::label(ui, "sheet.start")));
        }
        ui.horizontal(|ui| {
            components::mono_label(ui, summary, 11.5, theme::pal(ui).text_tertiary);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if Pill::new(PillKind::Primary, "Start AI review").key(ui, "sheet.start").enabled(ready).show(ui).clicked() {
                    self.start(ui.ctx());
                }
                if Pill::new(PillKind::Quiet, "Cancel").key(ui, "sheet.close").show(ui).clicked() {
                    *event = Event::Close;
                }
            });
        });
    }
}

/// The scope of an agent as one line.
fn scope_text(agent: &Agent) -> String {
    if agent.scope.is_empty() { "every file".to_string() } else { agent.scope.join(" ") }
}

/// What the sheet draws from a plan, copied out so the sheet can change itself while drawing.
struct PlanView {
    /// Each planned agent and how many files it reads.
    agents: Vec<(String, usize)>,
    general: usize,
    general_engine: &'static str,
    grid: Grid,
    skipped: Vec<pr_craft_core::run::Skipped>,
    skipped_viewed: usize,
    scope: &'static str,
    never_send: usize,
    redact: bool,
    notice: String,
}

impl PlanView {
    fn of(plan: &Plan) -> Self {
        PlanView {
            agents: plan.agents.iter().map(|a| (a.id.clone(), a.files.len())).collect(),
            general: plan.general.len(),
            general_engine: plan.general_engine.name(),
            grid: Grid::of(plan),
            skipped: plan.skipped.clone(),
            skipped_viewed: plan.coverage.skipped_viewed.len(),
            scope: match plan.read.scope {
                pr_craft_core::redact::ReadScope::DiffOnly => "diff only",
                pr_craft_core::redact::ReadScope::DiffAndChangedFiles => "diff and changed files",
                pr_craft_core::redact::ReadScope::WholeCheckout => "whole checkout",
            },
            never_send: plan.read.never_send.len(),
            redact: plan.read.redact_secrets,
            notice: plan.redaction_notice.clone(),
        }
    }
}

/// The files × agents grid (frames 29 and 54), then the files the AI will not read.
fn coverage_ui(ui: &mut Ui, plan: &PlanView) {
    let p = theme::pal(ui);
    let total = plan.grid.rows.len();
    let covered = total - plan.grid.uncovered();
    ui.horizontal(|ui| {
        components::group_label(ui, "Coverage");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            components::label(ui, format!("{covered} of {} covered", components::files_word(total)), 12.0, p.moss_text);
        });
    });
    if total > 0 {
        ScrollArea::vertical().id_salt("coverage").max_height(theme::PROMPT_HEIGHT).auto_shrink([false, true]).show(ui, |ui| {
            let rows: Vec<(String, Vec<bool>)> = plan.grid.rows.iter().take(GRID_ROWS).map(|(path, row)| (path.clone(), row.clone())).collect();
            let hover = |n: usize| {
                let Some((_, row)) = rows.get(n) else { return String::new() };
                let readers: Vec<&str> = plan.grid.columns.iter().zip(row).filter(|&(_, &read)| read).map(|(name, _)| name.as_str()).collect();
                let merged = if readers.len() > 1 { " More than one column reads it; duplicate findings are not merged." } else { "" };
                format!("Read by: {}.{merged}", readers.join(", "))
            };
            components::coverage_table(ui, &plan.grid.columns, &rows, hover);
        });
        if total > GRID_ROWS {
            note(ui, &format!("Showing the first {GRID_ROWS} of {total} files."));
        }
    }
    if !plan.skipped.is_empty() {
        section(ui, "Not read");
        for skipped in &plan.skipped {
            note(ui, &format!("{}  ·  {}", skipped.path, skipped.reason));
        }
    }
}

#[cfg(test)]
mod tests {
    use craft::egui::Context;
    use pr_craft_core::agent::{Engine, RunsOn, SpawnLimits, Trigger};

    use super::*;
    use crate::ai::tests::{change, drawn, plan, prompts};

    fn agent(id: &str) -> Agent {
        Agent {
            id: id.into(),
            name: id.to_uppercase(),
            description: String::new(),
            prompt: "Review.".into(),
            skills: Vec::new(),
            engine: Engine::ClaudeCode,
            scope: vec!["**/*.go".into()],
            triggers: vec![Trigger::Manual],
            spawn: SpawnLimits::default(),
            runs_on: RunsOn::All,
        }
    }

    fn sheet(plan: Plan, agents: Vec<Agent>) -> AiSheet {
        let options = ReviewOptions::default();
        AiSheet {
            change: change(1),
            agents: Ok(agents),
            plan: Fetch::Ready(plan),
            plan_for: options.clone(),
            preview: Fetch::Idle,
            preview_for: options.clone(),
            gate: None,
            starting: Fetch::Idle,
            cursor: None,
            focus_cursor: false,
            viewed: 2,
            notice: None,
            options,
        }
    }

    fn draw(sheet: &mut AiSheet) -> String {
        let mut runs = Runs::default();
        drawn(&Context::default(), |ui| {
            let _ = sheet.ui(ui.ctx(), &mut runs);
        })
    }

    #[test]
    fn the_sheet_shows_the_grid_and_keeps_start_locked_until_a_preview() {
        let p = plan(&[("go", &["a.go"])], &["a.go", "README.md"], &["README.md"]);
        let mut s = sheet(p, vec![agent("go")]);
        let text = draw(&mut s);
        assert!(text.contains("README.md"), "every planned file is in the grid:\n{text}");
        assert!(text.contains("GO"), "the agent has a column:\n{text}");
        assert!(!s.can_start(), "no preview yet");
        s.preview = Fetch::Ready(prompts(&["aaaa"]));
        s.gate = Some(StartGate::of(&prompts(&["aaaa"]), &s.options));
        assert!(s.can_start(), "a preview of these options unlocks Start");
    }

    #[test]
    fn start_needs_a_preview_of_the_options_on_screen_and_an_unblocked_plan() {
        let p = plan(&[("go", &["a.go"])], &["a.go"], &[]);
        let mut s = sheet(p, vec![agent("go")]);
        let preview = prompts(&["aaaa"]);
        s.gate = Some(StartGate::of(&preview, &s.options));
        s.preview = Fetch::Ready(preview);
        assert!(s.can_start());
        s.options.skip_viewed = true;
        assert!(!s.can_start(), "an option changed since the preview");
        s.plan_for = s.options.clone();
        assert!(!s.can_start(), "and the preview does not match it");
        s.options.skip_viewed = false;
        s.plan_for = s.options.clone();
        assert!(s.can_start());
        if let Fetch::Ready(plan) = &mut s.plan {
            plan.blocked = Some("this repository reads whole checkouts".into());
        }
        assert!(!s.can_start(), "a blocked plan never starts");
        assert!(draw(&mut s).contains("This review cannot start: this repository reads whole checkouts"));
    }

    #[test]
    fn ticking_agents_keeps_the_unticked_ones_listed_and_all_ticked_is_the_default() {
        let agents = vec![agent("go"), agent("sql"), agent("docs")];
        let mut s = sheet(plan(&[], &[], &[]), agents.clone());
        s.toggle(&agents, 1, false);
        assert_eq!(s.options.agents, Some(vec!["go".to_string(), "docs".to_string()]));
        s.toggle(&agents, 0, false);
        assert_eq!(s.options.agents, Some(vec!["docs".to_string()]));
        s.toggle(&agents, 1, true);
        s.toggle(&agents, 0, true);
        assert_eq!(s.options.agents, None, "everything ticked again is the default, not a list");
    }

    #[test]
    fn with_no_agents_the_sheet_still_plans_the_general_pass_over_every_file() {
        let p = plan(&[], &["a.md", "b.md"], &["a.md", "b.md"]);
        assert_eq!(ai::run_count(&p), 1);
        let mut s = sheet(p, Vec::new());
        let text = draw(&mut s);
        assert!(text.contains("a.md") && text.contains("b.md"), "every file is in the grid:\n{text}");
    }

    #[test]
    fn a_start_refused_because_the_change_moved_unlocks_again_with_a_new_preview() {
        let p = plan(&[("go", &["a.go"])], &["a.go"], &[]);
        let mut s = sheet(p, vec![agent("go")]);
        let preview = prompts(&["aaaa"]);
        s.gate = Some(StartGate::of(&preview, &s.options));
        s.preview = Fetch::Ready(preview);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Err("the change moved since the preview; preview again".to_string())).expect("send");
        s.starting = Fetch::Loading(rx);
        s.poll();
        assert!(!s.can_start(), "the old preview is thrown away once");
        s.reset_for_preview();
        let fresh = prompts(&["bbbb"]);
        s.gate = Some(StartGate::of(&fresh, &s.options));
        s.poll();
        assert!(s.can_start(), "a new preview is not wiped by the earlier refusal");
    }
}
