//! Settings for the AI: AI engine (Figma frame 58), what the AI engine sees (101), Review agents (12, 28, 66, 30) and
//! Repositories (37, 51). Every control calls an engine command; nothing is kept in the window that the engine does not hold.

use craft::egui::text::LayoutJob;
use craft::egui::{self, Key, Margin, Rect, RichText, ScrollArea, TextEdit, TextFormat, Ui};
use pr_craft_core::Change;
use pr_craft_core::agent::{Agent, EngineSettings, RepoRef, repo_key};
use pr_craft_core::redact::{DEFAULT_NEVER_SEND, ReadRules, ReadScope};
use pr_craft_engine::ai::{EngineView, ReviewPreview};
use serde_json::{Value, json};

use crate::agents::AgentsPage;
use crate::ai::{self, Fetch, ReviewOptions, Runs};
use crate::components::{self, Pill, PillKind};
use crate::keymap;
use crate::pages::Pages;
use crate::theme;
use crate::{live_label, row_button};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Engine,
    Sees,
    Agents,
    Repos,
    Appearance,
    Accessibility,
    Keyboard,
    Sync,
    Permissions,
    About,
}

impl Tab {
    /// Each page, its name, and the registry action that opens it.
    const ALL: [(Tab, &'static str, &'static str); 10] = [
        (Tab::Engine, "AI engine", "settings.page.engine"),
        (Tab::Sees, "Privacy", "settings.page.sees"),
        (Tab::Agents, "Review agents", "settings.page.agents"),
        (Tab::Repos, "Repositories", "settings.page.repos"),
        (Tab::Appearance, "Appearance", "settings.page.appearance"),
        (Tab::Accessibility, "Accessibility", "settings.page.accessibility"),
        (Tab::Keyboard, "Keyboard", "settings.page.keyboard"),
        (Tab::Sync, "Offline and sync", "settings.page.sync"),
        (Tab::Permissions, "Permissions", "settings.page.permissions"),
        (Tab::About, "About", "settings.page.about"),
    ];
}

pub struct Settings {
    tab: Tab,
    view: Result<EngineView, String>,
    notice: Option<Result<String, String>>,
    /// The never-send pattern being typed, global rules.
    pattern: String,
    /// The daily token ceiling being typed; `None` until the page first draws it.
    ceiling: Option<String>,
    preview_at: usize,
    preview: Fetch<ReviewPreview>,
    agents: AgentsPage,
    repos: ReposPage,
    was_typing: bool,
    /// A picker's popup was open at the end of the last frame.
    popup_was_open: bool,
    /// ⌘N was pressed: a new agent opens, for the selected repository when Repositories is showing one.
    new_agent: bool,
    pages: Pages,
}

#[derive(Default)]
struct ReposPage {
    filter: String,
    selected: Option<usize>,
    /// The override being edited for the selected repository, and its pending pattern.
    rules: Option<(String, ReadRules)>,
    pattern: String,
    /// The checkouts folder and the clone folder (filed under its key) being typed; `None` until first drawn.
    checkouts: Option<String>,
    clone_text: Option<(String, String)>,
    /// The saved checkouts folder and clones, read once and again after a save.
    saved: Option<(String, std::collections::BTreeMap<String, String>)>,
    /// A folder save on its way to the engine.
    saving: Option<Fetch<String>>,
}

impl Settings {
    /// The palette closed over a text field: that field had focus before the palette opened, so the next key is a command.
    pub fn forget_typing(&mut self) {
        self.was_typing = false;
    }

    pub fn open(tab: Tab) -> Self {
        Settings {
            tab,
            view: ai::engine_get(),
            notice: None,
            pattern: String::new(),
            ceiling: None,
            preview_at: 0,
            preview: Fetch::Idle,
            agents: AgentsPage::load(),
            repos: ReposPage::default(),
            was_typing: false,
            popup_was_open: false,
            new_agent: false,
            pages: Pages::new(),
        }
    }

    /// Settings on `tab` over `view` and `agents`, reading nothing from this Mac, for the render tests.
    #[cfg(test)]
    pub(crate) fn fixture(tab: Tab, view: EngineView, agents: AgentsPage) -> Self {
        Settings {
            tab,
            view: Ok(view),
            notice: None,
            pattern: String::new(),
            ceiling: None,
            preview_at: 0,
            preview: Fetch::Idle,
            agents,
            repos: ReposPage::default(),
            was_typing: false,
            popup_was_open: false,
            new_agent: false,
            pages: Pages::new(),
        }
    }

    /// The name of the page shown, for the title bar's trail.
    /// The Review agents editor is open.
    pub fn agent_form_open(&self) -> bool {
        self.tab == Tab::Agents && self.agents.editor_open()
    }

    /// The id the command palette uses for the page on show.
    pub fn page_id(&self) -> &'static str {
        match self.tab {
            Tab::Engine => "engine",
            Tab::Sees => "sees",
            Tab::Agents => "agents",
            Tab::Repos => "repos",
            Tab::Appearance => "appearance",
            Tab::Accessibility => "accessibility",
            Tab::Keyboard => "keyboard",
            Tab::Sync => "sync",
            Tab::Permissions => "permissions",
            Tab::About => "about",
        }
    }

    pub fn page_name(&self) -> &'static str {
        Tab::ALL.iter().find(|(tab, _, _)| *tab == self.tab).map_or("", |(_, name, _)| name)
    }

    /// Opens Review agents on a new agent, with Where it runs set to one repository when given.
    pub fn new_agent(repo: Option<RepoRef>) -> Self {
        let mut settings = Settings::open(Tab::Agents);
        settings.start_agent(repo);
        settings
    }

    /// The agent editor holds changes that are not saved; replacing Settings would drop them.
    pub fn dirty(&self) -> bool {
        self.agents.dirty()
    }

    fn start_agent(&mut self, repo: Option<RepoRef>) {
        self.tab = Tab::Agents;
        let defaults = self.view.as_ref().map(|v| v.settings.clone()).unwrap_or_default();
        self.agents.new_form(&defaults, repo);
    }

    /// Draws the settings; returns true when the reviewer leaves them.
    pub fn ui(&mut self, ui: &mut Ui, changes: &[Change], runs: &mut Runs) -> bool {
        self.preview.poll();
        let leave = self.keys(ui);
        let mut close = leave;
        let p = theme::pal(ui);
        let rect = ui.available_rect_before_wrap();
        let nav = Rect::from_min_size(rect.min, egui::vec2(theme::SETTINGS_NAV_WIDTH + 1.0, rect.height()));
        let page = Rect::from_min_max(nav.right_top(), rect.right_bottom());
        components::pane_at(ui, "settings-nav", nav, p.hover, Margin { left: 8, right: 9, top: 14, bottom: 8 }, |ui| {
            close |= self.nav(ui);
        });
        components::seam_right(ui, nav);
        // The agent editor has panes of its own and fills the page.
        let full = self.tab == Tab::Agents;
        let side = if full { 0 } else { ((page.width() - theme::SETTINGS_PAGE_WIDTH) / 2.0).clamp(24.0, 56.0) as i8 };
        let top = if full { 0 } else { 28 };
        components::pane_at(ui, "settings-page", page, p.panel, Margin { left: side, right: side, top, bottom: 0 }, |ui| {
            self.page(ui, changes, runs);
        });
        self.was_typing = ui.ctx().text_edit_focused();
        // Esc closes an open picker first: it must not also leave Settings on the next frame's key reading.
        self.popup_was_open = ui.ctx().any_popup_open();
        if close && self.agents.dirty() {
            // Leaving would drop the editor: say so instead.
            self.notice = Some(Err("The agent has unsaved changes. Save the agent, or use Discard changes, then go back.".into()));
            close = false;
        }
        if std::mem::take(&mut self.new_agent) {
            self.start_agent(None);
        }
        close
    }

    /// Plain digits switch pages; ⌘N and ⌘S belong to the agent editor. Esc leaves a text field, then closes the open
    /// editor, then leaves Settings; an editor with unsaved changes is never closed or left by Esc. While the Keyboard page
    /// waits for a new shortcut, no key here acts.
    fn keys(&mut self, ui: &Ui) -> bool {
        if self.tab != Tab::Keyboard && self.pages.capturing() {
            self.pages.stop_capture(ui.ctx());
        }
        if self.pages.capturing() {
            return false;
        }
        let typing = ui.ctx().text_edit_focused();
        if keymap::consume(ui, "settings.new_agent") {
            self.new_agent = true;
        }
        let menu = self.tab == Tab::Agents && self.agents.menu_open();
        let blocked = menu || self.popup_was_open || typing || self.was_typing;
        let esc = keymap::pressed(ui, "settings.close") && !blocked;
        if esc {
            if self.agents.dirty() {
                self.notice = Some(Err("The agent has unsaved changes. Save the agent, or use Discard changes.".into()));
            } else if self.tab == Tab::Agents && self.agents.has_form() {
                self.agents.cancel();
            } else {
                return true;
            }
        }
        if typing {
            return false;
        }
        for (tab, _, id) in Tab::ALL {
            if keymap::pressed(ui, id) {
                self.tab = tab;
            }
        }
        false
    }

    /// The page list, and Back. Returns true when Back was pressed.
    fn nav(&mut self, ui: &mut Ui) -> bool {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            components::label(ui, "Settings", 13.5, theme::pal(ui).graphite);
        });
        ui.add_space(theme::SPACE_2);
        for (tab, label, id) in Tab::ALL {
            if components::nav_item(ui, self.tab == tab, label, &keymap::label(ui, id)).clicked() {
                self.tab = tab;
            }
        }
        ui.add_space(theme::SPACE_3);
        components::note(ui, "Accounts and notifications are not in this window yet.", theme::pal(ui).text_tertiary);
        ui.add_space(theme::SPACE_2);
        Pill::new(PillKind::Secondary, "‹ Back").key(ui, "settings.close").small().show(ui).clicked()
    }

    fn page(&mut self, ui: &mut Ui, changes: &[Change], runs: &mut Runs) {
        if self.tab == Tab::Agents {
            if let Some(result) = &self.notice {
                match result {
                    Ok(text) => live_label(ui, text.as_str(), theme::pal(ui).moss_text),
                    Err(error) => live_label(ui, error.as_str(), theme::pal(ui).rose_text),
                }
            }
            let defaults = self.view.as_ref().map(|v| v.settings.clone()).unwrap_or_default();
            return self.agents.ui(ui, changes, &defaults, runs);
        }
        ScrollArea::vertical().id_salt(("settings", self.tab as u8)).auto_shrink([false, false]).show(ui, |ui| {
            let widest = if self.tab == Tab::Appearance && ui.ctx().content_rect().width() >= theme::WIDE_WINDOW {
                theme::APPEARANCE_WIDE_WIDTH
            } else {
                theme::SETTINGS_PAGE_WIDTH
            };
            ui.set_max_width(ui.available_width().min(widest));
            if let Some(result) = &self.notice {
                match result {
                    Ok(text) => live_label(ui, text.as_str(), theme::pal(ui).moss_text),
                    Err(error) => live_label(ui, error.as_str(), theme::pal(ui).rose_text),
                }
            }
            match self.tab {
                Tab::Engine => self.engine_page(ui),
                Tab::Sees => self.sees_page(ui, changes),
                Tab::Agents => {
                    let defaults = self.view.as_ref().map(|v| v.settings.clone()).unwrap_or_default();
                    self.agents.ui(ui, changes, &defaults, runs);
                }
                Tab::Repos => self.repos_page(ui, changes),
                Tab::Appearance => self.pages.appearance(ui),
                Tab::Accessibility => self.pages.accessibility(ui),
                Tab::Keyboard => self.pages.keyboard(ui),
                Tab::Sync => self.pages.sync(ui),
                Tab::Permissions => self.pages.permissions(ui),
                Tab::About => self.pages.about(ui),
            }
        });
    }

    /// `ai.engine.set` with `patch`; what the engine answers is what shows.
    fn set(&mut self, patch: &Value) {
        match ai::engine_set(patch) {
            Ok(view) => {
                self.view = Ok(view);
                self.notice = Some(Ok("Saved on this Mac.".into()));
            }
            Err(error) => self.notice = Some(Err(error)),
        }
    }

    fn engine_page(&mut self, ui: &mut Ui) {
        let view = match &self.view {
            Ok(view) => view,
            Err(error) => return live_label(ui, format!("Could not read the AI engine settings: {error}"), theme::pal(ui).rose_text),
        };
        let (settings, engines) = (
            view.settings.clone(),
            view.engines.iter().map(|e| (e.engine, e.name.clone(), e.wired, e.program.clone(), e.note.clone())).collect::<Vec<_>>(),
        );
        let mut patch = serde_json::Map::new();
        components::page_title(
            ui,
            "AI engine",
            "Which engine writes summaries, runs agents and babysits, and how long a quiet run may take before it counts as stuck.",
        );
        for (engine, name, wired, program, engine_note) in engines {
            let selected = settings.engine == engine;
            let found = program.map(|p| format!("found at {p}")).unwrap_or_default();
            let line =
                if wired { "Runs the claude CLI with the login it already has." } else { "GLM models through the Z.ai API. Needs an API key." };
            let right = |ui: &mut Ui| {
                components::mono_label(ui, found.clone(), 11.5, theme::pal(ui).text_secondary);
            };
            if components::option_card(ui, selected, wired || selected, &name, line, right).clicked() && wired {
                patch.insert("engine".into(), json!(engine));
            }
            if let Some(text) = engine_note {
                live_label(ui, text, theme::pal(ui).ember_text);
            }
            ui.add_space(6.0);
        }
        if !self.view.as_ref().is_ok_and(|v| v.engines.iter().all(|e| e.wired)) {
            note(ui, "Z.ai: pasting an API key is not built, so it cannot be chosen as the default.");
        }
        section(ui, "Runs");
        stuck_rows(ui, &settings, &mut patch);
        if let Some(n) = choice(
            ui,
            "Runs at once on this Mac",
            "Counted in each PR Craft window or command-line run on its own, not across them.",
            &[1u32, 2, 4, 8],
            settings.runs_at_once,
            |n| n.to_string(),
        ) {
            patch.insert("runs_at_once".into(), json!(n));
        }
        let mut spawn = settings.spawn;
        let mut changed = false;
        changed |= pick(ui, "Extra subagents", &[0u32, 4, 8, 16], &mut spawn.extra);
        changed |= pick(ui, "Depth", &[1u32, 2, 3], &mut spawn.depth);
        changed |= pick(ui, "At once", &[2u32, 4, 8], &mut spawn.at_once);
        components::rule(ui);
        if changed {
            patch.insert("spawn".into(), json!(spawn));
        }
        components::form_row(ui, "Daily token ceiling", "Estimated input tokens per day (≈ bytes ÷ 4). Empty means no ceiling.", |ui| {
            let text = self.ceiling.get_or_insert_with(|| settings.daily_token_ceiling.map(|n| n.to_string()).unwrap_or_default());
            let field = ui.add(TextEdit::singleline(text).hint_text("No ceiling").desired_width(160.0));
            components::name_field(ui, &field, "Daily token ceiling");
            if field.lost_focus() {
                match text.trim().parse::<u64>() {
                    Ok(n) if n > 0 => patch.insert("daily_token_ceiling".into(), json!(n)),
                    _ if text.trim().is_empty() => patch.insert("daily_token_ceiling".into(), Value::Null),
                    _ => {
                        *text = settings.daily_token_ceiling.map(|n| n.to_string()).unwrap_or_default();
                        None
                    }
                };
            }
        });
        ui.add_space(theme::SPACE_2);
        note(
            ui,
            "Default spawn limits are for new agents and stored only: nothing starts subagents yet. Not built: the Claude Code usage meter, its version, and Z.ai key storage.",
        );
        if !patch.is_empty() {
            self.set(&Value::Object(patch));
        }
    }

    fn sees_page(&mut self, ui: &mut Ui, changes: &[Change]) {
        let view = match &self.view {
            Ok(view) => view,
            Err(error) => return live_label(ui, format!("Could not read the AI engine settings: {error}"), theme::pal(ui).rose_text),
        };
        let settings = view.settings.clone();
        title(ui, "What the AI engine sees");
        note(
            ui,
            "Claude Code runs on this Mac with its own login. Z.ai is not wired yet, so nothing is sent to it. These rules apply before anything leaves.",
        );
        let mut rules = settings.read.clone();
        if read_rules_ui(ui, &mut rules, &mut self.pattern) {
            self.set(&json!({ "read": rules }));
        }
        ui.add_space(theme::SPACE_2);
        ui.label(RichText::new(pr_craft_core::redact::REDACTION_NOTICE).size(theme::sz(ui).body).color(theme::pal(ui).ember_text));
        ui.add_space(theme::SPACE_3);
        section(ui, "Per repository");
        if settings.repo_read.is_empty() {
            note(ui, "No repository overrides these rules. Repositories → a repository can.");
        }
        for (key, rules) in &settings.repo_read {
            note(ui, &format!("{key} overrides this with \"{}\". Repositories → {key}.", scope_name(rules.scope)));
        }
        ui.add_space(theme::SPACE_3);
        self.preview_card(ui, changes);
    }

    /// Preview (frame 101): the exact prompt a review of a change would send, what is withheld and how many secrets were masked.
    fn preview_card(&mut self, ui: &mut Ui, changes: &[Change]) {
        section(ui, "Preview what is sent");
        note(
            ui,
            "Shows the text each run would receive, after the never-send patterns and redaction. A line over 2000 characters is cut in the view and marked; Copy has it whole. It starts nothing and costs nothing.",
        );
        if changes.is_empty() {
            note(
                ui,
                &format!("No change in your queue to preview. Open a review and press {} to preview on it.", keymap::label(ui, "review.ai_review")),
            );
            return;
        }
        self.preview_at = self.preview_at.min(changes.len() - 1);
        let selected = changes.get(self.preview_at).map(|c| c.reference()).unwrap_or_default();
        let typing = ui.ctx().text_edit_focused();
        let mut go = !typing && !self.preview.loading() && keymap::fresh(ui, "settings.preview");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("preview-change").selected_text(selected).width(theme::PICKER_WIDTH).show_ui(ui, |ui| {
                for (i, change) in changes.iter().enumerate() {
                    ui.selectable_value(&mut self.preview_at, i, change.reference());
                }
            });
            go |= ui.add_enabled(!self.preview.loading(), egui::Button::new(keymap::hint(ui, "Preview", "settings.preview"))).clicked();
        });
        if go && let Some(change) = changes.get(self.preview_at) {
            let change = change.change_ref();
            self.preview = Fetch::start(ui.ctx(), move || ai::preview(&change, &ReviewOptions::default()));
        }
        match &self.preview {
            Fetch::Idle => {}
            Fetch::Loading(_) => live_label(ui, "Building the prompts from the diff…", theme::pal(ui).text_secondary),
            Fetch::Failed(error) => live_label(ui, error.as_str(), theme::pal(ui).rose_text),
            Fetch::Ready(preview) => preview_ui(ui, preview),
        }
    }

    fn repos_page(&mut self, ui: &mut Ui, changes: &[Change]) {
        self.poll_folders();
        let (Ok(view), Some(agents)) = (&self.view, self.agents.list()) else {
            return live_label(ui, "Could not read the engine settings or the agents.", theme::pal(ui).rose_text);
        };
        let settings = view.settings.clone();
        let agents = agents.to_vec();
        let all = ai::known_repos(changes, &agents);
        title(ui, "Repositories");
        note(ui, "Which review agents run on each repository, and what the AI may read there.");
        self.checkouts_section(ui);
        ui.add(TextEdit::singleline(&mut self.repos.filter).hint_text("Filter repositories").desired_width(f32::INFINITY));
        let filter = self.repos.filter.to_lowercase();
        let shown: Vec<&RepoRef> = all.iter().filter(|r| r.repo.to_lowercase().contains(&filter)).collect();
        let down_up = self.repos_keys(ui, shown.len());
        let selected = self.repos.selected.filter(|&i| i < shown.len());
        let mut pick_repo = None;
        for (i, repo) in shown.iter().enumerate() {
            let count = agents.iter().filter(|a| a.runs_on_repo(repo.forge, &repo.repo)).count();
            let meta = match count {
                0 => "no agents".to_string(),
                1 => "1 agent".to_string(),
                n => format!("{n} agents"),
            };
            let job = row_job(ui, &repo.repo, &meta, selected == Some(i));
            let row = row_button(ui, &format!("repo-{}-{}", repo.forge.cli(), repo.repo), selected == Some(i), job);
            if row.clicked() {
                pick_repo = Some(i);
            }
            if selected == Some(i) && down_up {
                row.request_focus();
                row.scroll_to_me(None);
            }
        }
        if shown.is_empty() {
            note(ui, "No repository matches. Repositories appear from your open changes and your agents.");
        }
        if let Some(i) = pick_repo {
            self.repos.selected = Some(i);
            self.repos.rules = None;
        }
        let Some(repo) = selected.and_then(|i| shown.get(i)).map(|r| (*r).clone()) else {
            return note(ui, &format!("Select a repository with {} and {}.", keymap::label(ui, "settings.next"), keymap::label(ui, "settings.prev")));
        };
        if std::mem::take(&mut self.new_agent) {
            return self.start_agent(Some(repo));
        }
        ui.separator();
        self.repo_detail(ui, &repo, &settings, &agents, &all, changes);
    }

    /// `settings.set` with `patch` on its own thread: the engine runs git to check the folders, and a slow volume must not freeze
    /// the window. What it says shows when it answers (see [`Settings::poll_folders`]).
    fn set_folders(&mut self, ctx: &egui::Context, patch: Value, saved: &'static str) {
        if self.repos.saving.is_some() {
            return;
        }
        self.notice = None;
        self.repos.saving = Some(Fetch::start(ctx, move || pr_craft_engine::settings::set(&patch).map(|_| saved.to_string()).map_err(crate::fail)));
    }

    /// Takes in the answer of a folder save: the message shows, the typed text is replaced by what was saved, or kept on a refusal.
    fn poll_folders(&mut self) {
        let Some(mut saving) = self.repos.saving.take() else { return };
        saving.poll();
        match saving {
            Fetch::Ready(saved) => {
                self.notice = Some(Ok(saved));
                (self.repos.saved, self.repos.checkouts, self.repos.clone_text) = (None, None, None);
            }
            Fetch::Failed(error) => self.notice = Some(Err(error)),
            pending => self.repos.saving = Some(pending),
        }
    }

    fn saved_folders(&mut self) -> &(String, std::collections::BTreeMap<String, String>) {
        self.repos.saved.get_or_insert_with(|| pr_craft_engine::settings::load().map(|s| (s.checkouts_root, s.clones)).unwrap_or_default())
    }

    /// Where "Check out in a worktree" puts one folder per change.
    fn checkouts_section(&mut self, ui: &mut Ui) {
        section(ui, "Checkouts folder");
        note(ui, "Check out in a worktree makes one folder per change here, for example ~/PR Craft checkouts. Choose a folder outside every clone.");
        let current = self.saved_folders().0.clone();
        let busy = self.repos.saving.is_some();
        let text = self.repos.checkouts.get_or_insert(current);
        let mut save = false;
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(text).hint_text("~/PR Craft checkouts").desired_width(320.0));
            save = ui.add_enabled(!busy, egui::Button::new("Save")).clicked();
        });
        let typed = text.trim().to_string();
        if save {
            self.set_folders(ui.ctx(), json!({ "checkouts_root": typed }), "Checkouts folder saved.");
        }
    }

    /// The folder of the user's clone of `repo`: the only folder "Check out here" and the editors read.
    fn clone_section(&mut self, ui: &mut Ui, repo: &RepoRef, host: Option<&str>) {
        section(ui, "Local clone");
        note(
            ui,
            "The folder of your clone of this repository. PR Craft reads and switches only the folders you name here, and checks that its remote is this repository.",
        );
        let key = pr_craft_core::checkout::clone_key(repo.forge, host, &repo.repo);
        let saved = self.saved_folders().1.clone();
        let busy = self.repos.saving.is_some();
        if self.repos.clone_text.as_ref().is_none_or(|(typed_for, _)| *typed_for != key) {
            self.repos.clone_text = Some((key.clone(), saved.get(&key).cloned().unwrap_or_default()));
        }
        let Some((_, text)) = self.repos.clone_text.as_mut() else { return };
        let (mut save, mut clear) = (false, false);
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(text).hint_text("~/code/clone").desired_width(320.0));
            save = ui.add_enabled(!busy, egui::Button::new("Save")).clicked();
            clear = ui.add_enabled(saved.contains_key(&key) && !busy, egui::Button::new("Clear")).clicked();
        });
        let typed = text.trim().to_string();
        let mut clones = saved;
        if save && !typed.is_empty() {
            clones.insert(key.clone(), typed);
        } else if clear {
            clones.remove(&key);
        } else {
            return;
        }
        self.set_folders(ui.ctx(), json!({ "clones": clones }), "Local clone saved.");
    }

    /// J and K walk the repository list; returns whether they moved it this frame.
    fn repos_keys(&mut self, ui: &Ui, len: usize) -> bool {
        if ui.ctx().text_edit_focused() {
            return false;
        }
        let (down, up) = (keymap::pressed(ui, "settings.next"), keymap::pressed(ui, "settings.prev"));
        if !(down || up) {
            return false;
        }
        let next = ai::nav_step(self.repos.selected, len, down);
        let moved = next != self.repos.selected;
        self.repos.selected = next;
        self.repos.rules = None;
        moved
    }

    fn repo_detail(&mut self, ui: &mut Ui, repo: &RepoRef, settings: &EngineSettings, agents: &[Agent], all: &[RepoRef], changes: &[Change]) {
        let open = changes.iter().filter(|c| c.forge == repo.forge && c.repo.eq_ignore_ascii_case(&repo.repo)).count();
        title(ui, &repo.repo);
        note(ui, &format!("{} · {open} open {}", repo.forge.name(), if open == 1 { "change" } else { "changes" }));
        let host = changes.iter().find(|c| c.forge == repo.forge && c.repo.eq_ignore_ascii_case(&repo.repo)).and_then(|c| c.host.clone());
        self.clone_section(ui, repo, host.as_deref());
        section(ui, "Review agents here");
        let mut toggles = Vec::new();
        let mut edit = None;
        for agent in agents {
            let mut on = agent.runs_on_repo(repo.forge, &repo.repo);
            ui.horizontal(|ui| {
                let label = RichText::new(&agent.name).size(theme::sz(ui).body).color(theme::pal(ui).graphite);
                if ui.add(egui::Checkbox::new(&mut on, label)).changed() {
                    toggles.push((agent.id.clone(), ai::with_repo(&agent.runs_on, all, repo, on)));
                }
                note(
                    ui,
                    &format!("{} · {}", agent.engine.name(), if agent.scope.is_empty() { "every file".to_string() } else { agent.scope.join(" ") }),
                );
                if ui.button("Edit").clicked() {
                    edit = Some(agent.id.clone());
                }
            });
        }
        if !agents.iter().any(|a| a.runs_on_repo(repo.forge, &repo.repo)) {
            let text =
                if agents.is_empty() { "You have no review agents".to_string() } else { format!("None of your {} agents runs here", agents.len()) };
            ui.label(
                RichText::new(format!("No review agent covers this repository. {text}, so AI review here uses only the general pass."))
                    .size(theme::sz(ui).body)
                    .color(theme::pal(ui).ember_text),
            );
            if ui.button(keymap::hint(ui, &format!("New agent for {}", repo.repo), "settings.new_agent")).clicked() {
                self.start_agent(Some(repo.clone()));
            }
        }
        note(
            ui,
            "General pass for files no agent covers: it uses the default prompt of the AI engine. Switching an agent off here lists the other known repositories in its Where it runs.",
        );
        for (id, runs_on) in toggles {
            if let Err(error) = self.agents.set_runs_on(&id, &runs_on) {
                self.notice = Some(Err(error));
            }
        }
        if let Some(id) = edit {
            self.tab = Tab::Agents;
            self.agents.select_id(&id);
        }
        ui.add_space(theme::SPACE_3);
        self.repo_rules(ui, repo, settings);
    }

    fn repo_rules(&mut self, ui: &mut Ui, repo: &RepoRef, settings: &EngineSettings) {
        section(ui, "What the AI may read here");
        let key = repo_key(repo.forge, &repo.repo);
        let stored = settings.repo_read.get(&key);
        let mut overridden = stored.is_some();
        let label = RichText::new("Override the global rules for this repository").size(theme::sz(ui).body).color(theme::pal(ui).graphite);
        if ui.add(egui::Checkbox::new(&mut overridden, label)).changed() {
            let rules = overridden.then(|| settings.read.clone());
            self.save_override(repo, rules.as_ref());
            return;
        }
        let Some(stored) = stored else {
            return note(
                ui,
                &format!("Uses the global rules: {}, {} never-send patterns.", scope_name(settings.read.scope), settings.read.never_send.len()),
            );
        };
        let (_, mut rules) = self.repos.rules.take().filter(|(k, _)| *k == key).unwrap_or((key.clone(), stored.clone()));
        if read_rules_ui(ui, &mut rules, &mut self.repos.pattern) {
            self.save_override(repo, Some(&rules));
        }
        self.repos.rules = Some((key, rules));
        ui.label(RichText::new(pr_craft_core::redact::REDACTION_NOTICE).size(theme::sz(ui).body).color(theme::pal(ui).ember_text));
    }

    fn save_override(&mut self, repo: &RepoRef, rules: Option<&ReadRules>) {
        match ai::read_set(repo.forge, &repo.repo, rules) {
            Ok(view) => {
                self.view = Ok(view);
                self.repos.rules = None;
                self.notice = Some(Ok(format!("Saved for {}.", repo.repo)));
            }
            Err(error) => self.notice = Some(Err(error)),
        }
    }
}

/// "Stuck after no output for", "Long steps may stay quiet for" and "When a run looks stuck".
fn stuck_rows(ui: &mut Ui, settings: &EngineSettings, patch: &mut serde_json::Map<String, Value>) {
    let mut stuck = settings.stuck;
    let mut changed = false;
    let minutes = |secs: u64| format!("{} min", secs / 60);
    if let Some(secs) =
        choice(ui, "Stuck after no output for", "Activity flags the run and offers Kill and Retry.", &[180u64, 300, 600], stuck.quiet_secs, minutes)
    {
        stuck.quiet_secs = secs;
        // The engine refuses a long-step limit shorter than the quiet limit.
        stuck.long_step_secs = stuck.long_step_secs.max(secs);
        changed = true;
    }
    let long = "Not used yet: the engine runs with no tools, so no step counts as a long one.";
    if let Some(secs) = choice(ui, "Long steps may stay quiet for", long, &[600u64, 900, 1800], stuck.long_step_secs, minutes) {
        stuck.long_step_secs = secs.max(stuck.quiet_secs);
        changed = true;
    }
    let retry = [(false, "Tell me".to_string()), (true, "Retry once, then tell me".to_string())];
    let picked = components::form_row(ui, "When a run looks stuck", "Retrying once fixes most hung CLI calls.", |ui| {
        components::choose(ui, "When a run looks stuck", &retry, stuck.retry_once)
    });
    if let Some(retry_once) = picked.filter(|v| *v != stuck.retry_once) {
        stuck.retry_once = retry_once;
        changed = true;
    }
    if changed {
        patch.insert("stuck".into(), json!(stuck));
    }
}

/// What the AI may read: the scope, redaction and the never-send patterns (frame 101). Returns whether `rules` changed.
/// Only Diff only is built; the other scopes need a checkout PR Craft does not keep.
pub(crate) fn read_rules_ui(ui: &mut Ui, rules: &mut ReadRules, pattern: &mut String) -> bool {
    let before = rules.clone();
    section(ui, "What an agent or review may read");
    for (scope, label) in
        [(ReadScope::DiffOnly, "Diff only"), (ReadScope::DiffAndChangedFiles, "Diff and changed files"), (ReadScope::WholeCheckout, "Whole checkout")]
    {
        let selected = rules.scope == scope;
        let built = scope == ReadScope::DiffOnly;
        let text = RichText::new(if built { label.to_string() } else { format!("{label}  (not built: needs a checkout)") })
            .size(theme::sz(ui).body)
            .color(theme::pal(ui).graphite);
        if ui.add_enabled(built || selected, egui::RadioButton::new(selected, text)).clicked() {
            rules.scope = scope;
        }
    }
    if rules.scope != ReadScope::DiffOnly {
        live_label(ui, "This scope blocks AI review: PR Craft keeps no checkout to read from. Choose Diff only.", theme::pal(ui).ember_text);
    }
    let redact = RichText::new("Redact secrets").size(theme::sz(ui).body).color(theme::pal(ui).graphite);
    ui.add(egui::Checkbox::new(&mut rules.redact_secrets, redact));
    note(ui, "Strings that look like keys or tokens become [redacted]. Best effort, not a guarantee.");
    section(ui, "Never send matching files");
    note(ui, "Files matching these patterns are removed before the prompt is built.");
    let mut remove = None;
    for (i, item) in rules.never_send.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(item).monospace().size(theme::sz(ui).code).color(theme::pal(ui).graphite));
            if ui.button("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        rules.never_send.remove(i);
    }
    if rules.never_send.is_empty() {
        live_label(ui, "No never-send patterns: every file the scope allows can be sent.", theme::pal(ui).ember_text);
    }
    ui.horizontal(|ui| {
        let field = ui.add(TextEdit::singleline(pattern).hint_text("**/*.pem").desired_width(theme::PATTERN_FIELD_WIDTH));
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if (ui.button("+ Add pattern").clicked() || enter) && add_pattern(rules, pattern) {
            pattern.clear();
        }
        if ui.button("Restore default patterns").clicked() {
            rules.never_send = DEFAULT_NEVER_SEND.map(String::from).into();
        }
    });
    note(ui, "A pattern cannot start with !. Not built: include review comments, keep a log of every prompt, merge duplicate findings.");
    *rules != before
}

/// Adds a never-send pattern: trimmed, not blank, not an exclusion, not already there.
pub(crate) fn add_pattern(rules: &mut ReadRules, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern.starts_with('!') || rules.never_send.iter().any(|p| p == pattern) {
        return false;
    }
    rules.never_send.push(pattern.to_string());
    true
}

fn scope_name(scope: ReadScope) -> &'static str {
    match scope {
        ReadScope::DiffOnly => "Diff only",
        ReadScope::DiffAndChangedFiles => "Diff and changed files",
        ReadScope::WholeCheckout => "Whole checkout",
    }
}

/// The preview of `ai.review.preview`: the notice, the files no prompt carries, and each prompt in full (frame 101).
pub(crate) fn preview_ui(ui: &mut Ui, preview: &ReviewPreview) {
    note(ui, &token_line(preview));
    ui.label(RichText::new(&preview.notice).size(theme::sz(ui).body).color(theme::pal(ui).ember_text));
    if preview.prompts.is_empty() {
        note(ui, "No prompt would be sent.");
    }
    for (i, prompt) in preview.prompts.iter().enumerate() {
        let sent = if prompt.sent { "sent" } else { prompt.note.as_deref().unwrap_or("not sent") };
        let masked = match prompt.redactions {
            1 => "1 secret masked".to_string(),
            n => format!("{n} secrets masked"),
        };
        let head = format!("{} · {} · {} files · {masked} · {sent}", prompt.agent.name, prompt.engine.name(), prompt.files.len());
        egui::CollapsingHeader::new(RichText::new(head).size(theme::sz(ui).body).color(theme::pal(ui).graphite)).id_salt(("prompt", i)).show(
            ui,
            |ui| {
                if !prompt.sent {
                    return live_label(ui, prompt.note.as_deref().unwrap_or("not sent"), theme::pal(ui).ember_text);
                }
                note(ui, &format!("Files: {}", prompt.files.join(", ")));
                note(
                    ui,
                    &format!(
                        "{} characters, fingerprint {}. This is what the engine reads on standard input.",
                        prompt.prompt.chars().count(),
                        prompt.prompt_hash
                    ),
                );
                if ui.button("Copy the whole prompt").clicked() {
                    ui.ctx().copy_text(prompt.prompt.clone());
                }
                prompt_view(ui, i, &prompt.prompt);
            },
        );
    }
    if !preview.withheld.is_empty() {
        ui.label(
            RichText::new(format!("Withheld from every prompt ({})", preview.withheld.len())).size(theme::sz(ui).body).color(theme::pal(ui).graphite),
        );
        for skipped in &preview.withheld {
            note(ui, &format!("{}  ·  {}", skipped.path, skipped.reason));
        }
    }
}

/// "Sends ≈Y input tokens", and with a ceiling, how much of it is used today.
fn token_line(preview: &ReviewPreview) -> String {
    let sends = format!("Sends ≈{} input tokens", preview.estimated_tokens);
    match preview.daily_token_ceiling {
        Some(ceiling) => format!("{sends}. ≈{} of {ceiling} used today.", preview.used_today),
        None => format!("{sends}."),
    }
}

/// The first `max` characters of `line`, and how many characters follow them.
pub(crate) fn cut_line(line: &str, max: usize) -> (&str, usize) {
    match line.char_indices().nth(max) {
        Some((at, _)) => (&line[..at], line[at..].chars().count()),
        None => (line, 0),
    }
}

/// The prompt, one row per line, laid out only for the rows on screen, with a horizontal scroll for wide lines. A line
/// longer than `MAX_LINE_CHARS` is cut and says how much is not shown; the note above counts them. Copy has it whole.
fn prompt_view(ui: &mut Ui, id: usize, prompt: &str) {
    let lines: Vec<&str> = prompt.lines().collect();
    let cut = lines.iter().filter(|line| line.len() > theme::MAX_LINE_CHARS && cut_line(line, theme::MAX_LINE_CHARS).1 > 0).count();
    if cut > 0 {
        let which = if cut == 1 { "1 line is".to_string() } else { format!("{cut} lines are") };
        live_label(
            ui,
            format!(
                "{which} longer than {} characters and cut in this view, each marked. The part that is not shown is still sent: Copy has every line whole.",
                theme::MAX_LINE_CHARS
            ),
            theme::pal(ui).ember_text,
        );
    }
    ScrollArea::both().id_salt(("prompt-text", id)).max_height(theme::PROMPT_HEIGHT).auto_shrink([false, true]).show_rows(
        ui,
        theme::PROMPT_ROW_HEIGHT,
        lines.len(),
        |ui, range| {
            for line in lines.get(range).unwrap_or_default() {
                let (shown, rest) = cut_line(line, theme::MAX_LINE_CHARS);
                let text = if rest > 0 { format!("{shown}  … [{rest} more characters not shown; Copy has them]") } else { shown.to_string() };
                let label = egui::Label::new(RichText::new(text).monospace().size(theme::sz(ui).code).color(theme::pal(ui).graphite))
                    .wrap_mode(egui::TextWrapMode::Extend);
                ui.add(label);
            }
        },
    );
}

/// A row of values to pick from, as a form row with a segmented control; the current one is always among them. Returns the one
/// clicked, if it changed.
pub(crate) fn choice<T: Copy + Ord>(ui: &mut Ui, label: &str, hint: &str, options: &[T], current: T, show: impl Fn(T) -> String) -> Option<T> {
    let mut all = options.to_vec();
    if !all.contains(&current) {
        all.push(current);
        all.sort();
    }
    let shown: Vec<(T, String)> = all.into_iter().map(|value| (value, show(value))).collect();
    components::form_row(ui, label, hint, |ui| components::choose(ui, label, &shown, current)).filter(|value| *value != current)
}

/// `choice` writing into `value`; returns whether it changed.
pub(crate) fn pick(ui: &mut Ui, label: &str, options: &[u32], value: &mut u32) -> bool {
    match choice(ui, label, "", options, *value, |n| n.to_string()) {
        Some(n) => {
            *value = n;
            true
        }
        None => false,
    }
}

pub(crate) fn title(ui: &mut Ui, text: &str) {
    components::page_title(ui, text, "");
}

pub(crate) fn section(ui: &mut Ui, text: &str) {
    components::group_label(ui, text);
}

pub(crate) fn note(ui: &mut Ui, text: &str) {
    components::note_sized(ui, text, 13.0, theme::pal(ui).text_secondary);
}

/// A two-line list row: the name, then what it holds. Raised text on the selection fill keeps 4.5:1 (rule 21).
pub(crate) fn row_job(ui: &Ui, name: &str, meta: &str, selected: bool) -> LayoutJob {
    let p = theme::pal(ui);
    let (title, meta_colour) = if selected { (p.on_inverse, p.on_inverse) } else { (p.graphite, p.text_tertiary) };
    let font = egui::FontId::proportional(theme::sz(ui).body);
    let mut job = LayoutJob::default();
    job.append(name, 0.0, TextFormat::simple(font.clone(), title));
    job.append(&format!("\n{meta}"), 0.0, TextFormat::simple(font, meta_colour));
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_line_says_what_is_sent_and_what_is_used() {
        let mut preview = crate::ai::tests::prompts(&["a"]);
        preview.estimated_tokens = 1200;
        assert_eq!(token_line(&preview), "Sends ≈1200 input tokens.");
        preview.daily_token_ceiling = Some(5000);
        preview.used_today = 300;
        assert_eq!(token_line(&preview), "Sends ≈1200 input tokens. ≈300 of 5000 used today.");
    }

    #[test]
    fn a_long_prompt_line_is_cut_on_a_character_boundary_and_the_rest_is_counted() {
        assert_eq!(cut_line("short", 10), ("short", 0));
        let (shown, rest) = cut_line("ééééé", 2);
        assert_eq!((shown.chars().count(), rest), (2, 3), "counted in characters, never inside one");
        assert_eq!(shown.chars().count() + rest, "ééééé".chars().count());
    }

    #[test]
    fn a_never_send_pattern_is_trimmed_and_never_an_exclusion_or_a_repeat() {
        let mut rules = ReadRules::default();
        let before = rules.never_send.len();
        assert!(add_pattern(&mut rules, "  infra/terraform.tfvars "));
        assert_eq!(rules.never_send.last().map(String::as_str), Some("infra/terraform.tfvars"));
        for refused in ["", "   ", "!keep.txt", "infra/terraform.tfvars"] {
            assert!(!add_pattern(&mut rules, refused), "{refused:?}");
        }
        assert_eq!(rules.never_send.len(), before + 1);
    }
}
