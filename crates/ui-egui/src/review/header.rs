use super::{
    Act, Align, BannerKind, Bar, ChangeRef, Context, DateTime, Fetch, Kind, LAYOUT_CONTROLS, Layout, LayoutOptions, LocalAct, Ready, Rect, RichText,
    ScrollArea, SinceLast, Ui, UiBuilder, Utc, Verdict, card, components, diff_view, egui, engine, keymap, theme, views,
};

/// Back and the trail of a change on the left of the title bar. True when Back was clicked.
pub(super) fn back_trail(ui: &mut Ui, bar: &Bar, change: &ChangeRef) -> bool {
    let key = keymap::label(ui, "review.back");
    let place = format!("{}{}", change.forge.number_sigil(), change.number);
    // The change number is always whole: when the strip is short, "PR Craft" goes first, then the repository.
    let room = bar.room();
    let width = |brand: bool, repo: bool| {
        let (gap, sep) = (8.0, components::text_width(ui, "/", 13.0));
        let back = components::text_width(ui, "‹ Inbox", 12.5) + 20.0;
        let mut total = back + gap + 14.0;
        let mut after_first = false;
        let mut add = |text_width: f32| {
            total += gap + if after_first { sep + gap } else { 0.0 } + text_width;
            after_first = true;
        };
        if brand {
            add(components::text_width(ui, "PR Craft", 13.0));
        }
        if repo {
            add(components::text_width(ui, &change.repo, 13.0));
        }
        add(components::text_width(ui, &place, 13.0));
        total
    };
    let (brand, repo) = [(true, true), (false, true), (false, false)].into_iter().find(|&(b, r)| width(b, r) <= room).unwrap_or((false, false));
    bar.left(ui, |ui| {
        let back = components::title_button(ui, "‹ Inbox", &key).clicked();
        let trail: Vec<&str> = if repo { vec![change.repo.as_str(), place.as_str()] } else { vec![place.as_str()] };
        components::breadcrumb_with(ui, brand, &trail);
        back
    })
}

/// What a verdict means, under its name.
pub(super) fn verdict_note(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Comment => "Feedback without a verdict",
        Verdict::Approve => "Looks good to merge",
        Verdict::RequestChanges => "Blocks merge until resolved",
    }
}

/// The "Since your review" summary: your review and its age, then what is compared and how big it is.
pub(super) fn since_line(since: &SinceLast, now: DateTime<Utc>) -> String {
    let (adds, dels) = since.files.iter().fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
    let short = |sha: &str| sha.get(..7).unwrap_or(sha).to_string();
    let when = since.review.submitted_at.map(|at| format!(" {} ago", crate::age(now, at))).unwrap_or_default();
    let size = format!("+{adds} −{dels} in {}", components::files_word(since.files.len()));
    let you = since.review.state.you();
    if since.rewritten {
        return format!("{you}{when} · its commit {} was force-pushed away · whole change, {size}", short(&since.from));
    }
    let commits = if since.commits_since == 1 { "1 commit".to_string() } else { format!("{} commits", since.commits_since) };
    format!("{you}{when} · {commits} since, {} → {} · {size}", short(&since.from), short(&since.to))
}

impl Ready {
    /// The review's part of the title bar: back and the trail on the left, the review's actions on the right.
    pub(super) fn title_bar(&self, ui: &mut Ui, bar: &mut Bar, acts: &mut Vec<Act>) {
        let change = &self.view.change;
        bar.right(ui, |ui| {
            if components::title_action(ui, &format!("Open on {}", change.forge.name()), "").clicked() {
                acts.push(Act::OpenOnForge(None));
            }
            let open = components::title_action(ui, "Open", &keymap::label(ui, "review.open_locally"));
            self.local.trigger.set(Some(open.rect));
            if open.clicked() {
                acts.push(Act::Local(LocalAct::Toggle));
            }
            if components::title_action(ui, "Share image", &keymap::label(ui, "review.share")).clicked() {
                acts.push(Act::OpenShare);
            }
            if components::title_action(ui, "AI review", &keymap::label(ui, "review.ai_review")).clicked() {
                acts.push(Act::OpenSheet);
            }
            if self.from_back {
                let text = if self.since_shown() { "All changes" } else { "Since your review" };
                if components::title_action(ui, text, &keymap::label(ui, "review.since_review")).clicked() {
                    acts.push(Act::ToggleSince);
                }
            }
        });
        if back_trail(ui, bar, change) {
            acts.push(Act::Back);
        }
    }

    /// The line of keys under the title in a read-only view; the Keys panel lists the rest.
    pub(super) fn hint_line(&self, ui: &Ui) -> String {
        let k = |id: &str| keymap::label(ui, id);
        let (hunks, files) = (
            format!("{} {} hunk", k("review.next_hunk"), k("review.prev_hunk")),
            format!("{} {} file", k("review.next_file"), k("review.prev_file")),
        );
        if self.commits.shown() {
            format!("{hunks} · {files} · {} returns to all changes", k("review.back"))
        } else if self.whole_shown() {
            format!("{hunks} · {files} · {} or {} back to this layer", k("review.whole_stack"), k("review.back"))
        } else if self.since_shown() {
            format!("{hunks} · {files} · {} or {} back to all changes", k("review.since_review"), k("review.back"))
        } else {
            String::new()
        }
    }

    /// The rows of the Keys panel: what the keys do on this screen, with the caps the reader set.
    pub(super) fn key_rows(&self, ui: &Ui) -> Vec<(Vec<String>, String)> {
        let caps = |ids: &[&str]| ids.iter().map(|id| keymap::label(ui, id)).collect::<Vec<_>>();
        let mut rows = vec![
            (caps(&["review.next_hunk", "review.prev_hunk"]), "Next or previous hunk".to_string()),
            (caps(&["review.next_file", "review.prev_file"]), "Next or previous file".to_string()),
        ];
        // A read-only view has nothing to comment on, keep or mark viewed; its own hint says how to leave it.
        if self.readonly() {
            return rows;
        }
        rows.push((caps(&["review.comment"]), "Comment on line".to_string()));
        rows.push((caps(&["review.viewed"]), "Mark file viewed".to_string()));
        rows.push((caps(&["review.layout", "review.diff_options"]), "Unified or split, diff options".to_string()));
        if matches!(&self.threads, Fetch::Ready(list) if !list.threads.is_empty()) {
            rows.push((caps(&["review.next_thread", "review.prev_thread"]), "Next or previous thread".to_string()));
            rows.push((caps(&["review.resolve_thread", "review.reply_thread"]), "Resolve or reply to the thread".to_string()));
        }
        rows.push((caps(&["review.expand_above", "review.expand_below"]), "Show more lines around the hunk".to_string()));
        if !self.ai_pending.is_empty() {
            rows.push((caps(&["review.keep_ai"]), "Keep suggestion".to_string()));
            rows.push((caps(&["review.keep_ai_edit"]), "Keep suggestion and edit".to_string()));
            rows.push((caps(&["review.discard_ai"]), "Discard suggestion".to_string()));
            rows.push((caps(&["review.next_ai", "review.prev_ai"]), "Next or previous suggestion".to_string()));
        }
        if !self.layers().is_empty() {
            rows.push((caps(&["review.whole_stack"]), "Whole stack".to_string()));
        }
        if self.from_back {
            rows.push((caps(&["review.since_review"]), "Since your review".to_string()));
        }
        if let Some(file) = self.view.files.get(self.file).filter(|_| self.visible.contains(&self.file))
            && views::offered(file).len() > 1
        {
            rows.push((caps(&["review.toggle_source"]), "Rendered or source".to_string()));
        }
        rows
    }

    /// The layers of this change's stack, bottom first. Each layer is reviewed on its own diff: its base is the layer below.
    pub(super) fn stack_strip(&self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let layers = match &self.layers {
            Fetch::Idle | Fetch::Loading(_) => {
                ui.label(RichText::new("Checking whether this change is stacked…").size(theme::sz(ui).body).color(theme::pal(ui).text_tertiary));
                return;
            }
            // Rule 15: a failed lookup is not "not stacked".
            Fetch::Failed(error) => {
                let note = format!("Could not check whether this change is stacked: {error}");
                crate::live_label(ui, note, theme::pal(ui).text_secondary);
                return;
            }
            Fetch::Ready(_) => self.layers(),
        };
        if layers.is_empty() {
            return;
        }
        card(ui, |ui| {
            ui.label(
                RichText::new(format!("Stack · {} layers, bottom first", layers.len())).size(theme::sz(ui).body).color(theme::pal(ui).text_secondary),
            );
            ScrollArea::vertical().id_salt("stack").max_height(theme::STACK_STRIP_HEIGHT).show(ui, |ui| {
                for layer in layers {
                    let draft = if layer.draft { "  ·  Draft" } else { "" };
                    let text = format!("{}{}  {}{draft}", layer.change.forge.number_sigil(), layer.change.number, layer.title);
                    if layer.change == self.view.change {
                        let here = RichText::new(format!("{text}  ·  This layer")).size(theme::sz(ui).body).color(theme::pal(ui).graphite);
                        ui.label(here.background_color(theme::pal(ui).ember_subtle));
                    } else {
                        let button = egui::Button::new(RichText::new(text).size(theme::sz(ui).body).color(theme::pal(ui).graphite)).frame(false);
                        if ui.add(button).on_hover_text(format!("Review {} on its own diff", layer.change.reference())).clicked() {
                            acts.push(Act::OpenLayer(layer.change.clone(), layer.title.clone()));
                        }
                    }
                }
            });
        });
    }

    pub(super) fn whole_title(&self, ui: &mut Ui) {
        let p = theme::pal(ui);
        components::label(ui, format!("Whole stack: {} layers", self.layers().len()), 15.0, p.graphite);
        if let Fetch::Ready(whole) = &self.whole
            && let (Some(bottom), Some(top)) = (whole.layers.first(), whole.layers.last())
        {
            let (adds, dels) = whole.files.iter().fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
            let line = format!("{} → {} · +{adds} −{dels} in {}", top.head_branch, bottom.base_branch, components::files_word(whole.files.len()));
            components::note_sized(ui, line, 12.0, p.text_tertiary);
        }
        let note = "Read-only. Comments go on a layer: pick one in the stack above, then comment on its own diff.";
        components::note_sized(ui, note, 12.0, p.text_tertiary);
        if let Fetch::Ready(whole) = &self.whole
            && whole.truncated
        {
            let note = format!("{} left out some files because they are too large; they are marked below.", self.view.change.forge.name());
            components::note_sized(ui, note, 12.0, p.ember_text);
        }
    }

    pub(super) fn since_title(&self, ui: &mut Ui) {
        let p = theme::pal(ui);
        components::label(ui, "Since your review", 15.0, p.graphite);
        if let Fetch::Ready(since) = &self.since {
            components::note_sized(ui, since_line(since, Utc::now()), 12.0, p.text_tertiary);
            if since.rewritten {
                // Rule 15: this is not "only what is new".
                let text = "The commit you reviewed was force-pushed away, so what is new cannot be told apart. This shows everything in the change since its branch meets the base.";
                let banner = components::banner(ui, BannerKind::Warn, "Your reviewed commit is gone", text);
                ui.ctx().accesskit_node_builder(banner.id, |node| node.set_live(egui::accesskit::Live::Polite));
            }
        }
        let note = format!(
            "Read-only. To comment, press {} or {} for all changes.",
            keymap::label(ui, "review.since_review"),
            keymap::label(ui, "review.back")
        );
        components::note_sized(ui, note, 12.0, p.text_tertiary);
        if let Fetch::Ready(since) = &self.since
            && since.truncated
        {
            let note = format!("{} left out some files because they are too large; they are marked below.", self.view.change.forge.name());
            components::note_sized(ui, note, 12.0, p.ember_text);
        }
    }

    /// Split view is on and the window has room for it.
    pub(super) fn split_on(&self) -> bool {
        self.layout.split && diff_view::split_allowed(self.window)
    }

    /// The keys that change the diff options or read more lines, with what each does now.
    pub(super) fn layout_keys(&self) -> [(&'static str, Act); 8] {
        let now = self.layout.clone();
        [
            ("review.layout", Act::SetLayout(LayoutOptions { split: !self.split_on(), ..now.clone() })),
            ("review.hide_whitespace", Act::SetLayout(LayoutOptions { hide_whitespace: !now.hide_whitespace, ..now.clone() })),
            ("review.word_highlight", Act::SetLayout(LayoutOptions { word_highlight: !now.word_highlight, ..now.clone() })),
            ("review.wrap", Act::SetLayout(LayoutOptions { wrap: !now.wrap, ..now.clone() })),
            ("review.diff_options", Act::ToggleOptions),
            ("review.expand_above", Act::ExpandAround(Kind::Above)),
            ("review.expand_below", Act::ExpandAround(Kind::Below)),
            ("review.expand_all", Act::ExpandAround(Kind::All)),
        ]
    }

    /// `review.set_layout`: saves the options for this repository and draws with them. Split is refused when the window is
    /// too narrow, and the reader is told.
    pub(super) fn set_layout(&mut self, mut wanted: LayoutOptions) -> Result<(), pr_craft_engine::CommandError> {
        if wanted.split && !diff_view::split_allowed(self.window) {
            wanted.split = false;
            self.notice = Some(format!(
                "Split view needs a window at least {} px wide; this one is {} px. The diff stays unified.",
                diff_view::MIN_SPLIT_WINDOW as u32,
                self.window as u32
            ));
        }
        self.layout = engine::set_layout(self.view.change.forge, self.view.change.host.as_deref(), &self.view.change.repo, wanted)?;
        Ok(())
    }

    /// The Unified/Split switch and the Diff options control, at the right of the diff header (frame 91).
    pub(super) fn layout_controls(&mut self, ui: &mut Ui, header: Rect, acts: &mut Vec<Act>) {
        let rect = Rect::from_min_size(egui::pos2(header.right() - 20.0 - LAYOUT_CONTROLS, header.top() + 12.0), egui::vec2(LAYOUT_CONTROLS, 26.0));
        let now = self.layout.clone();
        let at = usize::from(self.split_on());
        let mut trigger = None;
        ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::right_to_left(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let both = ["Unified".to_string(), "Split".to_string()];
            if let Some(picked) = components::segmented(ui, "layout", "Diff layout", &both, at) {
                acts.push(Act::SetLayout(LayoutOptions { split: picked == 1, ..now.clone() }));
            }
            let options = components::title_action(ui, "Diff options", &keymap::label(ui, "review.diff_options"));
            trigger = Some(options.rect);
            if options.clicked() {
                acts.push(Act::ToggleOptions);
            }
        });
        self.options_trigger = trigger;
    }

    /// The Diff options popover (frame 91), under the layout switch: the layout, whitespace, words and wrapping, saved for
    /// the repository.
    pub(super) fn options_popover(&mut self, ctx: &Context, diff: Rect, acts: &mut Vec<Act>) {
        if !self.options_open || self.readonly() {
            return;
        }
        let p = theme::pal(ctx);
        let now = self.layout.clone();
        let whitespace = self.view.files.get(self.file).map_or(0, |f| f.hunks.iter().map(|h| diff_view::visible(h, true, None).hidden).sum::<u32>());
        let repo = self.view.change.repo.clone();
        let split_at = usize::from(self.split_on());
        let area = egui::Area::new(egui::Id::NULL.with("diff-options"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(egui::pos2(diff.right() - 16.0, diff.top() + 64.0))
            .show(ctx, |ui| {
                components::card_frame(ui).show(ui, |ui| {
                    ui.set_width(300.0);
                    components::label(ui, "Diff options", 13.5, p.graphite);
                    ui.add_space(theme::SPACE_2);
                    ui.horizontal(|ui| {
                        components::label(ui, "Layout", 12.5, p.text_secondary);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let both = ["Unified".to_string(), "Split".to_string()];
                            if let Some(picked) = components::segmented(ui, "options-layout", "Diff layout", &both, split_at) {
                                acts.push(Act::SetLayout(LayoutOptions { split: picked == 1, ..now.clone() }));
                            }
                        });
                    });
                    let found = format!("{whitespace} whitespace-only {} in this file", if whitespace == 1 { "line" } else { "lines" });
                    let rows = [
                        (
                            "Hide whitespace changes",
                            found.as_str(),
                            now.hide_whitespace,
                            LayoutOptions { hide_whitespace: !now.hide_whitespace, ..now.clone() },
                        ),
                        ("Highlight changed words", "", now.word_highlight, LayoutOptions { word_highlight: !now.word_highlight, ..now.clone() }),
                        ("Wrap long lines", "", now.wrap, LayoutOptions { wrap: !now.wrap, ..now.clone() }),
                    ];
                    for (title, note, on, next) in rows {
                        ui.add_space(theme::SPACE_2);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                components::label(ui, title, 12.5, p.graphite);
                                if !note.is_empty() {
                                    components::note(ui, note, p.text_tertiary);
                                }
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if components::toggle(ui, on, title).is_some() {
                                    acts.push(Act::SetLayout(next));
                                }
                            });
                        });
                    }
                    ui.add_space(theme::SPACE_2);
                    let note = format!("Saved for {repo}. Split needs a window {} px wide or more.", diff_view::MIN_SPLIT_WINDOW as u32);
                    components::note(ui, note, p.text_tertiary);
                });
            });
        // A press anywhere else closes it; a press on the control that opened it is that control's to handle.
        let away = ctx
            .input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|at| !self.options_trigger.is_some_and(|t| t.contains(at))));
        if away && !area.response.contains_pointer() {
            acts.push(Act::ToggleOptions);
        }
    }
}

#[cfg(test)]
mod since_tests {
    use pr_craft_core::turn::{MyReview, ReviewState};

    use super::*;

    // An invented result shaped like `review.since_last`.
    fn since(rewritten: bool) -> SinceLast {
        SinceLast {
            change: ChangeRef { forge: pr_craft_core::Forge::GitHub, repo: "acme/api".into(), number: 1, host: None },
            review: MyReview { state: ReviewState::ChangesRequested, submitted_at: None, commit: Some("aaaaaaaaaa".into()) },
            from: "aaaaaaaaaa".into(),
            to: "bbbbbbbbbb".into(),
            commits_since: 7,
            files: Vec::new(),
            omitted: Vec::new(),
            truncated: false,
            rewritten,
            web_url: None,
        }
    }

    #[test]
    fn a_rewritten_history_title_does_not_claim_a_count_of_new_commits_or_a_range() {
        let (normal, rewritten) = (since_line(&since(false), Utc::now()), since_line(&since(true), Utc::now()));
        assert_ne!(normal, rewritten);
        assert!(normal.contains('7') && normal.contains("bbbbbbb"), "{normal}");
        assert!(!rewritten.contains('7'), "no count of new commits: {rewritten}");
        assert!(!rewritten.contains("bbbbbbb"), "no from-to range: {rewritten}");
    }
}
