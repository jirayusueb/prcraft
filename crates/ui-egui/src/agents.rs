//! Settings → Review agents (Figma frames 12, 28, 66 and 30): the list, the editor with its slash menu, and what saving
//! an agent does to the changes waiting for review.

use craft::egui::{self, Align, Key, Layout, Margin, Modifiers, Rect, RichText, ScrollArea, TextEdit, Ui};
use pr_craft_core::agent::{Agent, Engine, EngineSettings, RepoRef, RunsOn};
use pr_craft_core::{Change, ChangeRef};
use pr_craft_engine::ai::Preview;

use crate::ai::{self, AgentForm, Fetch, Runs};
use crate::components::{self, Pill, PillKind};
use crate::keymap;
use crate::review::card;
use crate::settings::{note, pick};
use crate::theme;
use crate::{live_label, selectable};

pub struct AgentsPage {
    agents: Result<Vec<Agent>, String>,
    /// Index in the list of the agent being edited; `None` with a form open is a new agent.
    selected: Option<usize>,
    form: Option<Editor>,
    status: Option<Result<String, String>>,
}

struct Editor {
    id: Option<String>,
    form: AgentForm,
    /// The form as last loaded or saved, to tell unsaved changes.
    clean: AgentForm,
    skill: String,
    slash: Slash,
    saved: Option<Saved>,
    confirm_delete: bool,
}

#[derive(Default)]
struct Slash {
    selected: usize,
    /// The word start of a menu the writer closed with Esc.
    dismissed: Option<usize>,
    open: bool,
}

/// What saving did (frame 30): the agent, and the changes in the inbox its scope matches.
struct Saved {
    preview: Fetch<Preview>,
}

/// What the editor asks of the page after a frame.
enum Do {
    Save,
    Cancel,
    Delete,
    RunNow(Vec<(ChangeRef, Vec<String>)>),
    DismissSaved,
}

impl Editor {
    fn new(form: AgentForm, id: Option<String>) -> Self {
        Editor { id, clean: form.clone(), form, skill: String::new(), slash: Slash::default(), saved: None, confirm_delete: false }
    }

    fn dirty(&self) -> bool {
        self.form != self.clean
    }
}

impl AgentsPage {
    pub fn load() -> Self {
        AgentsPage { agents: ai::agents(), selected: None, form: None, status: None }
    }

    /// A page that holds `agents` and reads nothing from this Mac, for the render tests.
    #[cfg(test)]
    pub(crate) fn fixture(agents: Vec<Agent>) -> Self {
        AgentsPage { agents: Ok(agents), selected: None, form: None, status: None }
    }

    /// The page with a new agent open and `name` and `description` typed, for the render tests.
    #[cfg(test)]
    pub(crate) fn fixture_new_form(&mut self, name: &str, description: &str) {
        self.new_form(&EngineSettings::default(), None);
        if let Some(editor) = &mut self.form {
            editor.form.name = name.into();
            editor.form.description = description.into();
            editor.form.prompt = "You review Go changes for concurrency bugs.\nFor each finding, cite the file and line, say what can go wrong,\nand propose the smallest fix. Skip style issues.\n\nChange under review: {pr_title}\nFiles in your scope: {files}".into();
            editor.form.skills = vec!["go-concurrency".into(), "repo-conventions".into()];
            editor.form.include = "**/*.go".into();
            editor.form.exclude = "*_test.go\nvendor/**".into();
            editor.clean = editor.form.clone();
        }
    }

    pub fn list(&self) -> Option<&[Agent]> {
        self.agents.as_deref().ok()
    }

    pub fn has_form(&self) -> bool {
        self.form.is_some()
    }

    /// The slash menu is open and takes Esc itself.
    pub fn menu_open(&self) -> bool {
        self.form.as_ref().is_some_and(|f| f.slash.open)
    }

    pub fn cancel(&mut self) {
        self.form = None;
    }

    /// The agent editor is open.
    pub(crate) fn editor_open(&self) -> bool {
        self.form.is_some()
    }

    /// The open editor holds changes that are not saved.
    pub fn dirty(&self) -> bool {
        self.form.as_ref().is_some_and(Editor::dirty)
    }

    pub fn new_form(&mut self, defaults: &EngineSettings, repo: Option<RepoRef>) {
        if !self.may_replace() {
            return;
        }
        self.selected = None;
        self.form = Some(Editor::new(AgentForm::new(defaults.engine, defaults.spawn, repo), None));
    }

    pub fn select_id(&mut self, id: &str) {
        let found = self.list().and_then(|list| list.iter().position(|a| a.id == id));
        if let Some(i) = found {
            self.select(i);
        }
    }

    fn select(&mut self, i: usize) {
        if !self.may_replace() {
            return;
        }
        let Some(agent) = self.list().and_then(|list| list.get(i)).cloned() else { return };
        self.selected = Some(i);
        self.form = Some(Editor::new(AgentForm::from_agent(&agent), Some(agent.id)));
    }

    /// Whether the open editor may be replaced: one with unsaved changes stays, and the reviewer is told why.
    fn may_replace(&mut self) -> bool {
        if self.dirty() {
            self.status = Some(Err("The agent has unsaved changes. Save the agent, or use Discard changes first.".into()));
            return false;
        }
        true
    }

    fn reload(&mut self) {
        self.agents = ai::agents();
    }

    /// Where `id` runs, changed from Settings → Repositories.
    pub fn set_runs_on(&mut self, id: &str, runs_on: &RunsOn) -> Result<(), String> {
        // The open editor would save the old place back, and closing it would drop what was typed.
        if self.form.as_ref().is_some_and(|f| f.id.as_deref() == Some(id) && f.dirty()) {
            return Err("This agent has unsaved changes in Review agents. Save or discard them first.".into());
        }
        ai::agent_set_runs_on(id, runs_on)?;
        self.reload();
        if self.form.as_ref().is_some_and(|f| f.id.as_deref() == Some(id)) {
            self.form = None;
            self.selected = None;
        }
        Ok(())
    }

    pub fn ui(&mut self, ui: &mut Ui, changes: &[Change], defaults: &EngineSettings, runs: &mut Runs) {
        if let Some(saved) = self.form.as_mut().and_then(|f| f.saved.as_mut()) {
            saved.preview.poll();
        }
        let list = match &self.agents {
            Ok(list) => list.clone(),
            Err(error) => return live_label(ui, format!("Could not read the agents: {error}"), theme::pal(ui).rose_text),
        };
        let known = ai::known_repos(changes, &list);
        let typing = ui.ctx().text_edit_focused();
        // ⌘⌫ is "delete to line start" in a text field, so it is taken only when no field has focus, and a held key
        // does not press it again: two quick taps or a hold must not arm and then delete.
        let fresh_delete = keymap::fresh(ui, "settings.delete_agent");
        let (save_key, taken) = (keymap::consume(ui, "settings.save_agent"), !typing && keymap::consume(ui, "settings.delete_agent"));
        let delete_key = taken && fresh_delete;
        let moved = self.keys(ui, list.len());
        let mut act = None;
        let p = theme::pal(ui);
        let rect = ui.available_rect_before_wrap();
        let list_rect = Rect::from_min_size(rect.min, egui::vec2(theme::AGENT_LIST_WIDTH + 1.0, rect.height()));
        let editor_rect = Rect::from_min_max(list_rect.right_top(), rect.right_bottom());
        components::pane_at(ui, "agent-list", list_rect, p.hover, Margin { left: 8, right: 9, top: 14, bottom: 8 }, |ui| {
            self.list_pane(ui, &list, defaults, moved);
        });
        components::seam_right(ui, list_rect);
        components::pane_at(ui, "agent-editor", editor_rect, p.panel, Margin { left: 40, right: 40, top: 24, bottom: 0 }, |ui| {
            match &mut self.form {
                Some(editor) => act = editor_ui(ui, editor, &known, (save_key, delete_key)),
                None => note(
                    ui,
                    &format!(
                        "Select an agent with {} and {}, or press {} for a new one.",
                        keymap::label(ui, "settings.next"),
                        keymap::label(ui, "settings.prev"),
                        keymap::label(ui, "settings.new_agent")
                    ),
                ),
            }
        });
        if let Some(act) = act {
            self.apply(ui.ctx(), act, runs);
        }
        if let Some(result) = &self.status {
            match result {
                Ok(text) => live_label(ui, text.as_str(), theme::pal(ui).moss_text),
                Err(error) => live_label(ui, error.as_str(), theme::pal(ui).rose_text),
            }
        }
    }

    /// J and K walk the list unless the editor holds unsaved changes. Returns whether they moved the selection.
    fn keys(&mut self, ui: &Ui, len: usize) -> bool {
        if ui.ctx().text_edit_focused() || self.form.as_ref().is_some_and(Editor::dirty) {
            return false;
        }
        let (down, up) = (keymap::pressed(ui, "settings.next"), keymap::pressed(ui, "settings.prev"));
        if !(down || up) {
            return false;
        }
        let next = ai::nav_step(self.selected, len, down);
        if let Some(i) = next {
            self.select(i);
        }
        true
    }

    fn list_pane(&mut self, ui: &mut Ui, list: &[Agent], defaults: &EngineSettings, moved: bool) {
        ui.spacing_mut().item_spacing.y = 2.0;
        components::padded(ui, Margin::symmetric(8, 0), |ui| {
            ui.horizontal(|ui| {
                components::label(ui, "Review agents", 13.5, theme::pal(ui).graphite);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Pill::new(PillKind::Secondary, "New").key(ui, "settings.new_agent").small().show(ui).clicked() {
                        self.new_form(defaults, None);
                    }
                });
            });
        });
        ui.add_space(theme::SPACE_2);
        if self.form.as_ref().is_some_and(|f| f.id.is_none()) {
            components::list_row(ui, "agent-new", self.selected.is_none(), "New agent", "Unsaved draft", "");
        }
        let mut clicked = None;
        for (i, agent) in list.iter().enumerate() {
            let skills = match agent.skills.len() {
                1 => "1 skill".to_string(),
                n => format!("{n} skills"),
            };
            let selected = self.selected == Some(i);
            let row = components::list_row(
                ui,
                &format!("agent-{}", agent.id),
                selected,
                &agent.name,
                &agent.description,
                &format!("{} · {skills}", agent.engine.name()),
            );
            if row.clicked() {
                clicked = Some(i);
            }
            if selected && moved {
                row.request_focus();
                row.scroll_to_me(None);
            }
        }
        if list.is_empty() {
            note(
                ui,
                "No review agents yet. The general pass reviews every file with the default prompt; an agent adds a focused prompt and a file scope.",
            );
        }
        if let Some(i) = clicked {
            self.select(i);
        }
        ui.add_space(theme::SPACE_2);
        components::note(
            ui,
            format!(
                "Agents run when you start AI review ({}). Findings arrive as AI drafts and post nothing.",
                keymap::label(ui, "review.ai_review")
            ),
            theme::pal(ui).text_tertiary,
        );
    }

    fn apply(&mut self, ctx: &egui::Context, act: Do, runs: &mut Runs) {
        match act {
            Do::Cancel => self.form = None,
            Do::DismissSaved => {
                if let Some(editor) = &mut self.form {
                    editor.saved = None;
                }
            }
            Do::Save => self.save(ctx),
            Do::Delete => self.delete(),
            Do::RunNow(targets) => {
                // What the confirm says comes from the saved agent, which is what the start runs.
                let saved = self.form.as_ref().and_then(|f| f.id.as_deref()).and_then(|id| self.list()?.iter().find(|a| a.id == id)).cloned();
                match (saved, self.dirty()) {
                    (Some(agent), false) => {
                        runs.ask(ai::PaidStart::agent_on(&agent, targets));
                        if let Some(editor) = &mut self.form {
                            editor.saved = None;
                        }
                    }
                    (Some(_), true) => {
                        self.status = Some(Err("The agent has unsaved changes. Save it first: Run on these runs the saved agent.".into()))
                    }
                    (None, _) => self.status = Some(Err("That agent is not saved, so there is nothing to run.".into())),
                }
            }
        }
    }

    fn save(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.form else { return };
        if let Some(problem) = editor.form.problem() {
            self.status = Some(Err(problem.to_string()));
            return;
        }
        match ai::agent_save(editor.id.as_deref(), &editor.form) {
            Ok(agent) => {
                editor.id = Some(agent.id.clone());
                editor.clean = editor.form.clone();
                let id = agent.id.clone();
                editor.saved = Some(Saved { preview: Fetch::start(ctx, move || ai::scope_preview(&id)) });
                self.status = Some(Ok(format!("Saved {}.", agent.name)));
                self.reload();
                self.selected = self.list().and_then(|list| list.iter().position(|a| a.id == agent.id));
            }
            Err(error) => self.status = Some(Err(error)),
        }
    }

    fn delete(&mut self) {
        let Some(id) = self.form.as_ref().and_then(|f| f.id.clone()) else { return };
        match ai::agent_delete(&id) {
            Ok(()) => {
                self.status = Some(Ok("Agent deleted. The drafts of its past runs stay.".into()));
                self.form = None;
                self.selected = None;
                self.reload();
            }
            Err(error) => self.status = Some(Err(error)),
        }
    }
}

/// The editor form (frames 12, 28 and 30). Returns what the reviewer asked for. The footer stays at the foot of the pane.
fn editor_ui(ui: &mut Ui, editor: &mut Editor, known: &[RepoRef], (save_key, delete_key): (bool, bool)) -> Option<Do> {
    let mut act = None;
    let foot = 80.0;
    let body_height = (ui.available_height() - foot).max(120.0);
    ScrollArea::vertical().id_salt("agent-form").max_height(body_height).auto_shrink([false, false]).show(ui, |ui| {
        ui.set_max_width(ui.available_width().min(theme::AGENT_FORM_WIDTH));
        let name = if editor.id.is_some() { editor.form.name.clone() } else { "New review agent".to_string() };
        components::page_title(ui, &name, "");
        if editor.dirty() {
            live_label(ui, "Unsaved changes", theme::pal(ui).ember_text);
        }
        identity(ui, &mut editor.form);
        prompt_field(ui, editor);
        skills(ui, editor);
        scope_fields(ui, &mut editor.form);
        runs_fields(ui, &mut editor.form);
        where_it_runs(ui, &mut editor.form, known);
        if let Some(problem) = editor.form.problem() {
            ui.add_space(theme::SPACE_2);
            note(ui, problem);
        }
        act = saved_card(ui, editor);
        ui.add_space(theme::SPACE_3);
    });
    components::rule(ui);
    ui.add_space(8.0);
    components::note_sized(ui, "Stored on this Mac. Not shared with the repository.", 12.0, theme::pal(ui).text_secondary);
    ui.horizontal(|ui| {
        ui.set_min_height(foot - 31.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if Pill::new(PillKind::Primary, "Save agent").key(ui, "settings.save_agent").show(ui).clicked() || save_key {
                act = Some(Do::Save);
            }
            if editor.id.is_some() {
                if editor.confirm_delete {
                    if Pill::new(PillKind::Danger, "Really delete").key(ui, "settings.delete_agent").show(ui).clicked() || delete_key {
                        act = Some(Do::Delete);
                    }
                    if Pill::new(PillKind::Secondary, "Keep the agent").show(ui).clicked() {
                        editor.confirm_delete = false;
                    }
                } else if Pill::new(PillKind::Secondary, "Delete agent").key(ui, "settings.delete_agent").show(ui).clicked() || delete_key {
                    editor.confirm_delete = true;
                }
            }
            let (text, key) = if editor.dirty() { ("Discard changes", None) } else { ("Close", Some("settings.close")) };
            let pill = Pill::new(PillKind::Quiet, text);
            if (if let Some(key) = key { pill.key(ui, key) } else { pill }).show(ui).clicked() {
                act = Some(Do::Cancel);
            }
        });
    });
    act
}

/// A small caption over a field of the editor.
fn field(ui: &mut Ui, text: &str) {
    ui.add_space(theme::SPACE_2);
    components::label(ui, text, 12.0, theme::pal(ui).text_secondary);
    ui.add_space(3.0);
}

fn identity(ui: &mut Ui, form: &mut AgentForm) {
    ui.columns(2, |cols| {
        field(&mut cols[0], "Name");
        let name = cols[0].add(TextEdit::singleline(&mut form.name).desired_width(f32::INFINITY).hint_text("Concurrency reviewer"));
        components::name_field(&cols[0], &name, "Name");
        field(&mut cols[1], "Engine");
        let all: Vec<(Engine, String)> = Engine::ALL.iter().map(|e| (*e, e.name().to_string())).collect();
        if let Some(engine) = components::choose(&mut cols[1], "Engine", &all, form.engine) {
            form.engine = engine;
        }
    });
    if !form.engine.wired() {
        live_label(ui, "Z.ai is not wired yet: a run of this agent fails at once, with that reason.", theme::pal(ui).ember_text);
    }
    field(ui, "Description");
    let description = ui.add(TextEdit::singleline(&mut form.description).desired_width(f32::INFINITY));
    components::name_field(ui, &description, "Description");
}

/// The prompt, with the slash menu: `/` at the start of a word offers the variables, and what follows filters them.
fn prompt_field(ui: &mut Ui, editor: &mut Editor) {
    field(ui, "Prompt");
    let (ctx, id) = (ui.ctx().clone(), ui.make_persistent_id("agent-prompt"));
    let cursor = TextEdit::load_state(&ctx, id).and_then(|s| s.cursor.char_range()).map(|r| r.primary.index.0);
    let focused = ctx.memory(|m| m.has_focus(id));
    let query = cursor.filter(|_| focused).and_then(|c| ai::slash_query(&editor.form.prompt, c));
    let matches = query.as_ref().map(|(_, q)| ai::slash_matches(q)).unwrap_or_default();
    if query.is_none() {
        editor.slash.dismissed = None;
    }
    let open = query.as_ref().is_some_and(|(start, _)| editor.slash.dismissed != Some(*start)) && !matches.is_empty();
    editor.slash.open = open;
    let mut choose = None;
    if open {
        let n = matches.len();
        let slash = &mut editor.slash;
        slash.selected = slash.selected.min(n - 1);
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                slash.selected = (slash.selected + 1) % n;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                slash.selected = (slash.selected + n - 1) % n;
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                choose = Some(slash.selected);
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                slash.dismissed = query.as_ref().map(|(start, _)| *start);
            }
        });
    }
    ui.add(
        TextEdit::multiline(&mut editor.form.prompt)
            .id(id)
            .font(egui::TextStyle::Monospace)
            .desired_rows(10)
            .desired_width(f32::INFINITY)
            .hint_text("You review Go changes for concurrency bugs. …"),
    );
    if open {
        card(ui, |ui| {
            for (i, item) in matches.iter().enumerate() {
                let selected = i == editor.slash.selected;
                let colour = if selected { theme::pal(ui).on_inverse } else { theme::pal(ui).graphite };
                let text = RichText::new(format!("{}   {}   {}", item.label, item.insert, item.detail)).size(theme::sz(ui).body).color(colour);
                if crate::add_selectable(ui, selected, selectable(ui, selected, text)).clicked() {
                    choose = Some(i);
                }
            }
            note(ui, "↑ ↓ move · Tab or ↵ insert · Esc close");
        });
    }
    if let (Some(i), Some((start, _)), Some(cursor)) = (choose, &query, cursor)
        && let Some(item) = matches.get(i)
    {
        let at = ai::apply_slash(&mut editor.form.prompt, *start, cursor, item);
        if let Some(mut state) = TextEdit::load_state(&ctx, id) {
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(at))));
            state.store(&ctx, id);
        }
        ctx.memory_mut(|m| m.request_focus(id));
        ctx.request_repaint();
    }
    note(
        ui,
        "Variables: {pr_title} {files} {diff} {base_branch}. Without {diff} the diff is added at the end. Type / for them. Skills, files, output formats and subagents are not built.",
    );
}

fn skills(ui: &mut Ui, editor: &mut Editor) {
    field(ui, "Skills");
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        for (i, skill) in editor.form.skills.iter().enumerate() {
            components::mono_label(ui, skill, 12.0, theme::pal(ui).graphite);
            if Pill::new(PillKind::Quiet, &format!("Remove {skill}")).small().show(ui).clicked() {
                remove = Some(i);
            }
        }
    });
    if let Some(i) = remove {
        editor.form.skills.remove(i);
    }
    ui.horizontal(|ui| {
        let field =
            ui.add(TextEdit::singleline(&mut editor.skill).hint_text("~/.claude/skills/go-concurrency").desired_width(theme::SKILL_FIELD_WIDTH));
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if (Pill::new(PillKind::Secondary, "Add skill").small().show(ui).clicked() || enter) && !editor.skill.trim().is_empty() {
            editor.form.skills.push(editor.skill.trim().to_string());
            editor.skill.clear();
        }
    });
    components::note(ui, "Stored on this Mac and counted, but their text is not put in the prompt yet.", theme::pal(ui).text_tertiary);
}

fn scope_fields(ui: &mut Ui, form: &mut AgentForm) {
    field(ui, "Files in scope");
    components::note(ui, "One pattern per line. Empty includes every file.", theme::pal(ui).text_tertiary);
    let (ink, size) = (theme::pal(ui).text_secondary, theme::sz(ui).body);
    ui.columns(2, |cols| {
        cols[0].label(RichText::new("Include").size(size).color(ink));
        let include = cols[0].add(TextEdit::multiline(&mut form.include).desired_rows(3).desired_width(f32::INFINITY).hint_text("**/*.go"));
        components::name_field(&cols[0], &include, "Include");
        cols[1].label(RichText::new("Exclude").size(size).color(ink));
        let exclude = cols[1].add(TextEdit::multiline(&mut form.exclude).desired_rows(3).desired_width(f32::INFINITY).hint_text("*_test.go"));
        components::name_field(&cols[1], &exclude, "Exclude");
    });
}

fn runs_fields(ui: &mut Ui, form: &mut AgentForm) {
    field(ui, "Runs");
    for (on_request, label) in [(false, "Only when I ask"), (true, "When review is requested")] {
        let text = RichText::new(label).size(theme::sz(ui).body).color(theme::pal(ui).graphite);
        if ui.add(egui::RadioButton::new(form.on_request == on_request, text)).clicked() {
            form.on_request = on_request;
        }
    }
    ui.add_enabled(false, egui::RadioButton::new(false, "On new commits  (not built)"));
    note(
        ui,
        &format!(
            "Stored only: nothing starts a run on a review request yet. Start agents from AI review ({}).",
            keymap::label(ui, "review.ai_review")
        ),
    );
    note(
        ui,
        "Splitting the work into subagents is not built: the engine runs with no tools, so it cannot start any. These limits are stored for when it can.",
    );
    pick(ui, "Extra subagents", &[0u32, 4, 8, 16], &mut form.spawn.extra);
    pick(ui, "Depth", &[1u32, 2, 3], &mut form.spawn.depth);
    pick(ui, "At once", &[2u32, 4, 8], &mut form.spawn.at_once);
}

/// Where it runs (frame 30): every repository, or the ones ticked.
fn where_it_runs(ui: &mut Ui, form: &mut AgentForm, known: &[RepoRef]) {
    field(ui, "Where it runs");
    for (all, label) in [(true, "All repos"), (false, "Chosen")] {
        let text = RichText::new(label).size(theme::sz(ui).body).color(theme::pal(ui).graphite);
        if ui.add(egui::RadioButton::new(form.all_repos == all, text)).clicked() {
            form.all_repos = all;
        }
    }
    if form.all_repos {
        return;
    }
    // A repository the agent names stays listed even when no change of it is open.
    let mut listed: Vec<RepoRef> = known.to_vec();
    for repo in &form.repos {
        if !listed.contains(repo) {
            listed.push(repo.clone());
        }
    }
    for repo in listed {
        let mut on = form.repos.contains(&repo);
        let label = RichText::new(format!("{}  ·  {}", repo.repo, repo.forge.name())).size(theme::sz(ui).body).color(theme::pal(ui).graphite);
        if ui.add(egui::Checkbox::new(&mut on, label)).changed() {
            form.repos.retain(|r| *r != repo);
            if on {
                form.repos.push(repo.clone());
            }
        }
    }
    note(ui, "Off here means it is left unticked in AI review for that repository.");
}

/// What saving found (frame 30): the changes waiting for you that the scope matches, and the two ways on.
fn saved_card(ui: &mut Ui, editor: &Editor) -> Option<Do> {
    let saved = editor.saved.as_ref()?;
    let mut act = None;
    ui.add_space(theme::SPACE_2);
    card(ui, |ui| match &saved.preview {
        Fetch::Idle | Fetch::Loading(_) => {
            live_label(ui, "Saved. Matching its scope against the changes waiting for you…", theme::pal(ui).text_secondary)
        }
        Fetch::Failed(error) => {
            live_label(ui, format!("Saved. Could not preview its scope: {error}"), theme::pal(ui).rose_text);
            if ui.button(keymap::hint(ui, "Close", "settings.close")).clicked() {
                act = Some(Do::DismissSaved);
            }
        }
        Fetch::Ready(preview) => act = preview_rows(ui, preview, editor.dirty()),
    });
    act
}

fn preview_rows(ui: &mut Ui, preview: &Preview, dirty: bool) -> Option<Do> {
    use pr_craft_core::agent::Outcome;
    let runs: Vec<_> = preview.rows.iter().filter(|r| matches!(r.outcome, Outcome::Runs { .. })).collect();
    live_label(ui, format!("Saved. It matches {} of the {} changes waiting for you.", runs.len(), preview.rows.len()), theme::pal(ui).moss_text);
    for unavailable in &preview.unavailable {
        live_label(
            ui,
            format!("{} could not be read, so its changes are not listed: {}", unavailable.forge.name(), unavailable.reason),
            theme::pal(ui).ember_text,
        );
    }
    for row in &preview.rows {
        match &row.outcome {
            Outcome::Runs { files } => note(ui, &format!("{}  {}  ·  {} files", row.title, row.change.reference(), files.len())),
            Outcome::Skipped { reason } => note(ui, &format!("Skipped: {}  ·  {reason}", row.change.reference())),
        }
    }
    note(
        ui,
        "It never starts on its own: triggers are stored only. Run on these asks you first; it reads only this agent's files, with no General review.",
    );
    let mut act = None;
    ui.horizontal(|ui| {
        let label = format!("Run on these {} now", runs.len());
        if ui.add_enabled(!runs.is_empty() && !dirty, egui::Button::new(label)).clicked() {
            let targets = preview.rows.iter().filter_map(|r| match &r.outcome {
                Outcome::Runs { files } => Some((r.change.clone(), files.clone())),
                Outcome::Skipped { .. } => None,
            });
            act = Some(Do::RunNow(targets.collect()));
        }
        if ui.button("Only new requests").clicked() {
            act = Some(Do::DismissSaved);
        }
    });
    if dirty {
        note(ui, "The form has changes that are not saved. Run on these runs the saved agent: save first.");
    }
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_with_form() -> AgentsPage {
        let form = AgentForm::new(Engine::ClaudeCode, pr_craft_core::agent::SpawnLimits::default(), None);
        AgentsPage { agents: Ok(Vec::new()), selected: None, form: Some(Editor::new(form, None)), status: None }
    }

    #[test]
    fn an_editor_with_unsaved_changes_is_not_replaced_by_a_new_agent_or_another_agent() {
        let mut page = page_with_form();
        let defaults = EngineSettings::default();
        page.new_form(&defaults, None);
        assert!(!page.dirty(), "a clean editor may be replaced");
        if let Some(editor) = &mut page.form {
            editor.form.prompt = "Review the locks.".into();
        }
        assert!(page.dirty());
        let kept = page.form.as_ref().map(|f| f.form.prompt.clone());
        page.new_form(&defaults, None);
        page.select(0);
        assert_eq!(page.form.as_ref().map(|f| f.form.prompt.clone()), kept, "what was typed is still there");
        assert!(matches!(page.status, Some(Err(_))), "and the reviewer is told why");
        page.cancel();
        assert!(!page.dirty() && !page.has_form());
    }

    #[test]
    fn run_on_these_confirms_the_saved_agent_and_never_an_unsaved_form() {
        use pr_craft_core::agent::{SpawnLimits, Trigger};
        let saved = Agent {
            id: "go".into(),
            name: "Go".into(),
            description: String::new(),
            prompt: "Review.".into(),
            skills: Vec::new(),
            engine: Engine::ClaudeCode,
            scope: Vec::new(),
            triggers: vec![Trigger::Manual],
            spawn: SpawnLimits::default(),
            runs_on: RunsOn::All,
        };
        let mut page = AgentsPage { agents: Ok(vec![saved.clone()]), selected: Some(0), form: None, status: None };
        page.form = Some(Editor::new(AgentForm::from_agent(&saved), Some("go".into())));
        let (ctx, mut runs) = (egui::Context::default(), Runs::default());
        if let Some(editor) = &mut page.form {
            editor.form.engine = Engine::ZaiGlm;
        }
        page.apply(&ctx, Do::RunNow(Vec::new()), &mut runs);
        assert!(runs.confirm.is_none() && matches!(page.status, Some(Err(_))), "unsaved changes: nothing to confirm");
        if let Some(editor) = &mut page.form {
            editor.form.engine = Engine::ClaudeCode;
        }
        page.apply(&ctx, Do::RunNow(Vec::new()), &mut runs);
        let paid = runs.confirm.expect("a clean form confirms");
        assert_eq!(paid.engine, saved.engine, "the confirm says what the saved agent will use");
        assert_eq!(paid.options.agents, Some(vec!["go".to_string()]));
    }
}
