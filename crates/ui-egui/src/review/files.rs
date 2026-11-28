use super::{
    Act, Align, Context, Editor, FileDiff, Layout, LayoutOptions, LineRef, LocalAct, Margin, Pill, PillKind, Ready, ScrollArea, Selection, TextEdit,
    Ui, components, egui, engine, keymap, menu, theme,
};

/// The folder of a path: everything before the last `/`, empty for a file at the root.
pub(super) fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

pub(super) fn drag_key() -> egui::Id {
    egui::Id::NULL.with("gutter-drag")
}

/// The file and hunk a drag from the gutter button began in, while the button is still held.
pub(super) fn dragging(ui: &Ui) -> Option<(usize, usize)> {
    ui.ctx().data(|d| d.get_temp::<Option<(usize, usize)>>(drag_key())).flatten()
}

/// Whether a folder name cannot be written into the Files filter: it splits on spaces and commas, `*` and `?` are globs and a
/// leading `!` excludes.
pub(crate) fn filter_unfit(dir: &str) -> bool {
    dir.contains([' ', ',', '*', '?']) || dir.starts_with('!')
}

impl Ready {
    pub(super) fn files_pane(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let p = theme::pal(ui);
        if self.readonly() {
            let title = if self.commits.shown() {
                "Changes in the picked commits"
            } else if self.whole_shown() {
                "Changes in the stack"
            } else {
                "Changes since your review"
            };
            components::label(ui, title, 13.5, p.graphite);
            if let Some((files, omitted, _)) = self.compared() {
                components::label(ui, components::files_word(files.len()), 12.0, p.text_tertiary);
                ui.add_space(theme::SPACE_2);
                let most = files.iter().map(|f| f.additions + f.deletions).max().unwrap_or(0);
                ScrollArea::vertical().id_salt("readonly-files").show(ui, |ui| {
                    let base = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
                    for file in files {
                        let left_out = if omitted.contains(&file.path) { "  ·  too large" } else { "" };
                        // Two files with one name show their folder too, so the rows tell apart.
                        let twins = files.iter().filter(|other| base(&other.path) == base(&file.path)).count() > 1;
                        let name = if twins { file.path.as_str().to_string() } else { base(&file.path) };
                        components::files_changed_row(ui, &format!("{name}{left_out}"), file.additions, file.deletions, most)
                            .on_hover_text(&file.path);
                    }
                });
            }
            return;
        }
        let counted: Vec<&FileDiff> = self.view.files.iter().filter(|f| !f.generated).collect();
        let viewed = counted.iter().filter(|f| self.viewed(f)).count();
        components::padded(ui, Margin::symmetric(8, 0), |ui| {
            ui.horizontal(|ui| {
                components::label(ui, "Changes", 13.5, p.graphite);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    components::label(ui, format!("{viewed} of {} viewed", counted.len()), 12.0, p.text_tertiary);
                });
            });
        });
        ui.add_space(theme::SPACE_2);
        let field = ui.add(TextEdit::singleline(&mut self.filter_text).hint_text("Filter: internal/** !*_test.go").desired_width(f32::INFINITY));
        if field.changed() {
            acts.push(Act::FilterChanged);
        }
        ui.add_space(4.0);
        let save = format!("Save for {}", self.view.change.repo);
        if Pill::new(PillKind::Secondary, &save).small().max_width(ui.available_width()).show(ui).clicked() {
            acts.push(Act::SaveFilter);
        }
        if !self.patterns.is_empty() {
            let hidden = self.view.files.len() - self.visible.len();
            components::note(
                ui,
                format!("{} of {} · {hidden} hidden", self.visible.len(), components::files_word(self.view.files.len())),
                p.text_secondary,
            );
        }
        ui.add_space(theme::SPACE_2);
        ScrollArea::vertical().id_salt("files").show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let mut folder = None;
            for &i in &self.visible {
                let file = &self.view.files[i];
                let (dir, name) = file.path.rsplit_once('/').unwrap_or(("", &file.path));
                if folder != Some(dir) {
                    folder = Some(dir);
                    if !dir.is_empty() {
                        ui.add_space(4.0);
                        let (done, total) = self.folder_counts(dir);
                        let folded = self.folded.contains(dir);
                        let head = components::folder_row(ui, dir, folded, done, total);
                        if head.clicked() {
                            acts.push(if folded { Act::UnfoldFolder(dir.to_string()) } else { Act::FoldFolder(dir.to_string()) });
                        }
                        let by_key = head.has_focus() && keymap::pressed(ui, "app.context_menu");
                        if head.secondary_clicked() || by_key {
                            let at = if by_key {
                                menu::below(head.rect)
                            } else {
                                ui.input(|input| input.pointer.interact_pos()).unwrap_or_else(|| head.rect.center())
                            };
                            acts.push(Act::FolderMenu { dir: dir.to_string(), at, restore: Some(head.id) });
                        }
                    }
                }
                if !dir.is_empty() && self.folded.contains(dir) {
                    continue;
                }
                if components::review_file_row(ui, name, i == self.file, self.viewed(file), file.additions, file.deletions).clicked() {
                    acts.push(Act::GoFile(i));
                }
            }
        });
    }

    /// How many of the visible files of folder `dir` are viewed, and how many there are.
    pub(super) fn folder_counts(&self, dir: &str) -> (usize, usize) {
        let files: Vec<&FileDiff> = self.visible.iter().filter_map(|&i| self.view.files.get(i)).filter(|f| parent_dir(&f.path) == dir).collect();
        (files.iter().filter(|f| self.viewed(f)).count(), files.len())
    }

    /// ← → ⇧V ⌥← and ⌥⌘C act on the folder of the current file.
    pub(super) fn folder_keys(&self, ui: &Ui, acts: &mut Vec<Act>) {
        let Some(file) = self.current_file() else { return };
        let dir = parent_dir(&file.path).to_string();
        if keymap::pressed(ui, "review.fold_all") {
            acts.push(Act::FoldAll);
        }
        if dir.is_empty() {
            return;
        }
        if keymap::pressed(ui, "review.fold_folder") {
            acts.push(Act::FoldFolder(dir.clone()));
        }
        if keymap::pressed(ui, "review.unfold_folder") {
            acts.push(Act::UnfoldFolder(dir.clone()));
        }
        if keymap::pressed(ui, "review.viewed_folder") {
            acts.push(Act::ViewedFolder(dir.clone()));
        }
        if keymap::pressed(ui, "review.copy_path") {
            acts.push(Act::CopyText { text: dir, said: "Copied the folder's path.".into() });
        }
    }

    /// What marking folder `dir` does: the mark to set (viewed, unless every visible file of it already is) and the files whose
    /// mark changes, each with its hash.
    pub(super) fn folder_marks(&self, dir: &str) -> (bool, Vec<(String, u64)>) {
        let files: Vec<(&FileDiff, bool)> =
            self.visible.iter().filter_map(|&i| self.view.files.get(i)).filter(|f| parent_dir(&f.path) == dir).map(|f| (f, self.viewed(f))).collect();
        let want = !files.iter().all(|(_, viewed)| *viewed);
        (want, files.into_iter().filter(|(_, viewed)| *viewed != want).map(|(f, _)| (f.path.clone(), f.hash)).collect())
    }

    /// Marks the visible files of `dir` viewed, or all unviewed when every one already is.
    pub(super) fn mark_folder(&mut self, dir: &str) -> Result<(), pr_craft_engine::CommandError> {
        let (want, changing) = self.folder_marks(dir);
        for (path, hash) in changing {
            let local = engine::mark_viewed(&self.view.change, &path, want.then_some(hash))?;
            self.set_local(local);
        }
        Ok(())
    }

    /// The text of the selected lines and how many there are.
    pub(super) fn selected_lines(&self) -> Option<(String, usize)> {
        let sel = self.selection?;
        let (lo, hi) = sel.lines();
        let lines = self.view.files.get(sel.file)?.hunks.get(sel.hunk)?.lines.get(lo..=hi)?;
        Some((lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n"), lines.len()))
    }

    /// The menu of a line of the diff (frame 103), or of the selection when the line is inside it. Every item repeats a key or
    /// a button; Suggest a change is not built (it has no key, so S stays the whole-stack key), Ask Claude Code on a selection
    /// and Copy link to lines have no command, and Show the whole file is not built.
    pub(super) fn open_lines_menu(&mut self, ctx: &Context, file: usize, hunk: usize, line: usize, at: egui::Pos2, restore: Option<egui::Id>) {
        if !self.selection.is_some_and(|s| s.contains(file, hunk, line)) {
            self.block = None;
            self.selection = Some(Selection { file, hunk, anchor: line, head: line });
        }
        let Some(sel) = self.selection else { return };
        // Every item acts on the file the menu names, so the current file follows the selection it was opened on.
        (self.file, self.hunk) = (sel.file, sel.hunk);
        let Some(diff) = self.view.files.get(sel.file) else { return };
        let Some(source) = diff.hunks.get(sel.hunk) else { return };
        let (lo, hi) = sel.lines();
        let shown = |l: usize| source.lines.get(l).map_or(0, |x| LineRef { old: x.old, new: x.new }.shown());
        let name = diff.path.rsplit('/').next().unwrap_or(&diff.path);
        let (header, comment) = if lo == hi {
            (format!("Line {} · {name}", shown(lo)), "Comment on this line")
        } else {
            (format!("Lines {}–{} · {name}", shown(lo), shown(hi)), "Comment on these lines")
        };
        let key = |id: &str| keymap::label(ctx, id);
        // The same two paths as ⌘O: with the popover closed it opens the popover; open, it opens the editor at this line.
        let code = match (self.open_target(), self.local.open) {
            (Some(_), false) => menu::Item::new("locally", "Open locally…").key(key("review.open_locally")),
            (Some(target), true) => menu::Item::new("vscode", format!("Open {name}:{} in VS Code", target.line)).key(key("review.open_locally")),
            (None, _) => menu::Item::new("vscode", "Open in VS Code").key(key("review.open_locally")).unavailable("This line is not in the checkout"),
        };
        let viewed = self.viewed(diff);
        use menu::Entry::{Item, Rule};
        menu::open(
            ctx,
            menu::Spec {
                owner: "lines",
                target: String::new(),
                header: Some(header),
                at,
                restore,
                entries: vec![
                    Item(menu::Item::new("comment", comment).key(key("review.comment"))),
                    Item(menu::Item::new("suggest", "Suggest a change").unavailable("Not built yet")),
                    Rule,
                    Item(menu::Item::new("copy_lines", if lo == hi { "Copy line" } else { "Copy lines" }).key("⌘C")),
                    Item(menu::Item::new("markdown", "Copy review as Markdown")),
                    Rule,
                    Item(code),
                    Rule,
                    Item(
                        menu::Item::new("whitespace", "Hide whitespace changes")
                            .key(key("review.hide_whitespace"))
                            .checked(self.layout.hide_whitespace),
                    ),
                    Item(menu::Item::new("viewed", if viewed { "Mark file unviewed" } else { "Mark file viewed" }).key(key("review.viewed"))),
                ],
            },
        );
    }

    /// The menu of a folder of the Files list (frame 105). Review this folder with AI, Open in VS Code and Reveal in Finder are
    /// not here: the AI sheet has no folder scope, and the editors open the checkout, not a folder of it.
    pub(super) fn open_folder_menu(&mut self, ctx: &Context, dir: String, at: egui::Pos2, restore: Option<egui::Id>) {
        let (done, total) = self.folder_counts(&dir);
        let folded = self.folded.contains(&dir);
        // ⇧V, ← → and ⌥⌘C act on the folder of the current file, so a hint is shown only on that folder's own menu.
        let current = self.view.files.get(self.file).is_some_and(|f| parent_dir(&f.path) == dir);
        let key = |id: &str| if current { keymap::label(ctx, id) } else { String::new() };
        let unfit = filter_unfit(&dir).then_some("The folder name has characters the filter cannot hold");
        let only = |item: menu::Item| match unfit {
            Some(why) => item.unavailable(why),
            None => item,
        };
        use menu::Entry::{Item, Rule};
        menu::open(
            ctx,
            menu::Spec {
                owner: "folder",
                header: Some(format!("{dir} · {}", components::files_word(total))),
                target: dir,
                at,
                restore,
                entries: vec![
                    Item(
                        menu::Item::new("viewed", if done == total { "Mark folder unviewed" } else { "Mark folder viewed" })
                            .key(key("review.viewed_folder")),
                    ),
                    Item(if folded {
                        menu::Item::new("fold", "Open folder").key(key("review.unfold_folder"))
                    } else {
                        menu::Item::new("fold", "Collapse folder").key(key("review.fold_folder"))
                    }),
                    Item(menu::Item::new("fold_all", "Collapse all").key(keymap::label(ctx, "review.fold_all"))),
                    Rule,
                    Item(only(menu::Item::new("only", "Show only this folder"))),
                    Item(only(menu::Item::new("hide", "Hide this folder"))),
                    Rule,
                    Item(menu::Item::new("copy", "Copy path").key(key("review.copy_path"))),
                ],
            },
        );
    }

    /// Turns the choice made in an open menu into the acts of the key or button it repeats. Also ends a drag from the gutter
    /// button once the pointer button is up.
    pub(super) fn menu_acts(&mut self, ctx: &Context, acts: &mut Vec<Act>) {
        if !ctx.input(|i| i.pointer.primary_down()) {
            ctx.data_mut(|d| d.insert_temp(drag_key(), None::<(usize, usize)>));
        }
        if let Some(choice) = menu::take(ctx, "lines") {
            let now = self.layout.clone();
            let act = match choice.item {
                "comment" => Some(Act::OpenComposer),
                "copy_lines" => Some(Act::CopyLines),
                "markdown" => Some(Act::CopyMarkdown),
                // Decided when the menu opened: the item that was drawn is the one that runs.
                "vscode" => Some(Act::Local(LocalAct::Editor(Editor::VsCode))),
                "locally" => Some(Act::Local(LocalAct::Toggle)),
                "whitespace" => Some(Act::SetLayout(LayoutOptions { hide_whitespace: !now.hide_whitespace, ..now.clone() })),
                "viewed" => Some(Act::ToggleViewed(self.file)),
                _ => None,
            };
            acts.extend(act);
        }
        if let Some(choice) = menu::take(ctx, "folder") {
            let dir = choice.target;
            let act = match choice.item {
                "viewed" => Some(Act::ViewedFolder(dir)),
                "fold" if self.folded.contains(&dir) => Some(Act::UnfoldFolder(dir)),
                "fold" => Some(Act::FoldFolder(dir)),
                "fold_all" => Some(Act::FoldAll),
                "only" => Some(Act::FilterTo(format!("{dir}/**"))),
                "hide" => Some(Act::FilterTo(format!("{} !{dir}/**", self.filter_text.trim()).trim().to_string())),
                "copy" => Some(Act::CopyText { text: dir, said: "Copied the folder's path.".into() }),
                _ => None,
            };
            acts.extend(act);
        }
        if let Some(choice) = menu::take(ctx, "thread") {
            self.cards.current = Some(choice.target);
            let act = match choice.item {
                "reply" => Some(Act::ThreadReplyCurrent),
                "resolve" => Some(Act::ThreadResolveCurrent),
                "next" => Some(Act::ThreadStep { forward: true }),
                "prev" => Some(Act::ThreadStep { forward: false }),
                "markdown" => Some(Act::CopyMarkdown),
                "forge" => Some(Act::OpenOnForge),
                _ => None,
            };
            acts.extend(act);
        }
    }
}
