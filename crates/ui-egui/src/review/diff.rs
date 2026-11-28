use super::{
    Act, Align, BannerKind, BarButton, BlockTarget, Cell, CodeStyle, CommitAct, Context, DiffLine, Draft, Fetch, FileDiff, FileStatus, FileView,
    Focus, FontId, Gap, Gutter, Half, HashSet, Hunk, Kind, LARGE_FILE_LINES, LAYOUT_CONTROLS, Layout, LayoutOptions, LineRef, Margin, MdBlockView,
    Pill, PillKind, Place, Pressed, Ready, Remaining, Reveal, ReviewThread, RichText, ScrollArea, Selection, StackLayer, Ui, ai, ai_card, components,
    composer_ui, context, diff_view, draft_card, drag_key, dragging, drop_block, egui, engine, engine_view, file_view, keymap, loose_threads,
    make_current, menu, stacks, theme, threads, turn, views,
};

/// Why a file shows no lines until the reader asks.
pub(super) fn fold_reason(file: &FileDiff, viewed: bool) -> String {
    let lines = file.additions + file.deletions;
    if file.binary {
        "Binary file, not shown.".to_string()
    } else if viewed {
        "Collapsed because viewed.".to_string()
    } else if file.generated {
        format!("Generated file, folded: {lines} changed lines. It does not count toward viewed.")
    } else {
        format!("Large file, folded: {lines} changed lines.")
    }
}

/// A read of lines for one gap of one file, on its way.
pub(super) struct Expanding {
    pub(super) file: usize,
    pub(super) gap: usize,
    pub(super) kind: Kind,
    pub(super) fetch: Fetch<engine::Expanded>,
}

/// A hunk's `@@` line, with how many whitespace-only changes were folded away.
pub(super) fn hunk_title(hunk: &Hunk, hidden: u32) -> String {
    match hidden {
        0 => hunk.header.clone(),
        1 => format!("{}  ·  1 whitespace-only line hidden", hunk.header),
        n => format!("{}  ·  {n} whitespace-only lines hidden", hunk.header),
    }
}

/// The keys of the expand bars, as the reader set them.
pub(super) struct ExpandKeys {
    pub(super) above: String,
    pub(super) below: String,
    pub(super) all: String,
}

impl ExpandKeys {
    pub(super) fn of(ui: &Ui) -> Self {
        ExpandKeys {
            above: keymap::label(ui, "review.expand_above"),
            below: keymap::label(ui, "review.expand_below"),
            all: keymap::label(ui, "review.expand_all"),
        }
    }
}

/// What the gaps of one file need to draw.
pub(super) struct GapDraw<'a> {
    pub(super) width: f32,
    pub(super) file: usize,
    pub(super) split: bool,
    pub(super) wrap: bool,
    pub(super) keys: &'a ExpandKeys,
    /// The hunk J and K are on, when this is the current file: its bars show the keys.
    pub(super) here: Option<usize>,
    /// Lines may follow the last hunk.
    pub(super) below_last: bool,
    pub(super) hunks: usize,
}

/// A gap: the lines read from it next to the hunk above, the bar that stands for the rest, and the lines next to the hunk below.
pub(super) fn gap_ui(ui: &mut Ui, draw: &GapDraw<'_>, gap: Option<Gap>, reveal: Option<&Reveal>, acts: &mut Vec<Act>) {
    let Some(gap) = gap else { return };
    if gap.index == draw.hunks && !draw.below_last {
        return;
    }
    let none = Reveal::default();
    let reveal = reveal.unwrap_or(&none);
    revealed_rows(ui, draw, &reveal.top);
    bar_ui(ui, draw, gap, reveal, acts);
    revealed_rows(ui, draw, &reveal.bottom);
}

/// Lines read from the file: unchanged context. The forge takes comments only on lines of the diff, so these cannot be selected.
pub(super) fn revealed_rows(ui: &mut Ui, draw: &GapDraw<'_>, lines: &[DiffLine]) {
    for line in lines {
        if draw.split {
            let cell = || Cell { line, selected: false, marks: &[] };
            components::split_row(ui, draw.width, [Some(cell()), Some(cell())], draw.wrap, false);
        } else {
            components::diff_row(ui, draw.width, line, false, false, CodeStyle { marks: &[], wrap: draw.wrap });
        }
    }
}

/// The bar for the lines still hidden in a gap, with the ways to show them.
pub(super) fn bar_ui(ui: &mut Ui, draw: &GapDraw<'_>, gap: Gap, reveal: &Reveal, acts: &mut Vec<Act>) {
    let (hidden, range, known) = match context::remaining(gap, reveal) {
        Remaining::None => return,
        Remaining::Lines { first, last } => {
            let count = last - first + 1;
            (format!("{count} hidden {}", if count == 1 { "line" } else { "lines" }), format!("lines {first}–{last}"), Some(count))
        }
        Remaining::Unknown { first } => ("Lines below the last hunk".to_string(), format!("from line {first}"), None),
    };
    let below_hunk = gap.index < draw.hunks;
    // The hunk the keys act on: the bar above it, and the bar below it, show those keys.
    let (over, under) = (draw.here == Some(gap.index), gap.index > 0 && draw.here == Some(gap.index - 1));
    let count = |kind| context::plan(gap, reveal, kind).map(|(from, to)| to.saturating_sub(from).saturating_add(1));
    let mut buttons: Vec<(String, Option<String>, Kind)> = Vec::new();
    if known.is_none_or(|n| n > context::STEP) {
        if let Some(n) = below_hunk.then(|| count(Kind::Above)).flatten() {
            buttons.push((format!("Show {n} lines above"), over.then(|| draw.keys.above.clone()), Kind::Above));
        }
        if let Some(n) = (gap.index > 0).then(|| count(Kind::Below)).flatten() {
            buttons.push((format!("Show {n} lines below"), under.then(|| draw.keys.below.clone()), Kind::Below));
        }
    }
    let all = if known.is_some() { "Expand all" } else { "Show the rest of the file" };
    buttons.push((all.to_string(), (over || under).then(|| draw.keys.all.clone()), Kind::All));
    let shown: Vec<BarButton<'_>> = buttons.iter().map(|(label, key, _)| BarButton { label, key: key.clone() }).collect();
    if let Some((_, _, kind)) = components::expand_bar(ui, draw.width, &hidden, &range, &shown).and_then(|i| buttons.get(i)) {
        acts.push(Act::Expand { file: draw.file, gap: gap.index, kind: *kind });
    }
}

/// One hunk to draw: where it is, its lines, and the options in force.
pub(super) struct HunkRows<'a> {
    pub(super) file: usize,
    pub(super) hunk: usize,
    pub(super) source: &'a Hunk,
    pub(super) layout: LayoutOptions,
    pub(super) split: bool,
}

/// What a row of a hunk shows under it: the comment box, drafts, AI drafts and threads that sit on its lines.
pub(super) struct UnderRow<'a> {
    pub(super) width: f32,
    pub(super) file: &'a FileDiff,
    pub(super) forge: pr_craft_core::Forge,
    pub(super) selection: Option<Selection>,
    pub(super) composer: &'a mut Option<String>,
    pub(super) focus_composer: &'a mut bool,
    pub(super) editing: &'a mut Option<(u64, String)>,
    pub(super) inline: &'a mut HashSet<u64>,
    pub(super) ai_inline: &'a mut HashSet<String>,
    pub(super) drafts: &'a [Draft],
    pub(super) ai: &'a [ai::Pending],
    pub(super) threads: &'a [&'a ReviewThread],
    pub(super) cards: &'a mut threads::Cards,
    pub(super) ai_current: Option<String>,
    /// The draft whose card takes focus this frame.
    pub(super) focus_draft: Option<u64>,
    pub(super) focus_ai: bool,
}

impl UnderRow<'_> {
    /// Draws what sits under the row that shows `lines` (each with its place in the hunk's own lines).
    pub(super) fn draw(&mut self, ui: &mut Ui, at: &HunkRows<'_>, lines: &[(usize, &DiffLine)], acts: &mut Vec<Act>) {
        self.composer_under(ui, at, lines, acts);
        let (threads, drafts, ai) = (self.threads, self.drafts, self.ai);
        let mut asked = Vec::new();
        for thread in threads.iter().filter(|t| t.placed() && t.line.is_some_and(|line| lines.iter().any(|(_, shown)| line.on(shown)))) {
            let overlap = drafts.iter().any(|d| d.path == thread.path && thread.line.is_some_and(|line| d.end.shown() == line.shown()));
            threads::thread_ui(ui, self.width, self.forge, thread, self.cards, overlap, &mut asked);
        }
        acts.extend(asked.into_iter().map(Act::Thread));
        for draft in drafts.iter().filter(|d| d.path == self.file.path && lines.iter().any(|(_, line)| d.end.on(line))) {
            self.inline.insert(draft.id);
            let focus = self.focus_draft == Some(draft.id);
            draft_card(ui, Place::Line(self.width), draft, self.editing, focus, acts);
        }
        // AI drafts read at this commit sit under their line, never posted until kept and submitted.
        let on_row = |p: &&ai::Pending| {
            p.placed && p.draft.finding.path == self.file.path && lines.iter().any(|(_, line)| ai::finding_at(&p.draft.finding, line))
        };
        for pending in ai.iter().filter(on_row) {
            self.ai_inline.insert(pending.draft.id.clone());
            let current = self.ai_current.as_deref() == Some(pending.draft.id.as_str());
            ai_card(ui, Some(self.width), &pending.draft, None, current, self.focus_ai && current, acts);
        }
    }

    /// The comment box sits under the last selected line.
    pub(super) fn composer_under(&mut self, ui: &mut Ui, at: &HunkRows<'_>, lines: &[(usize, &DiffLine)], acts: &mut Vec<Act>) {
        let Some(sel) = self.selection else { return };
        if self.composer.is_none() || (sel.file, sel.hunk) != (at.file, at.hunk) || !lines.iter().any(|(index, _)| *index == sel.lines().1) {
            return;
        }
        let shown = |l: usize| at.source.lines.get(l).map_or(0, |line| LineRef { old: line.old, new: line.new }.shown());
        let (lo, hi) = sel.lines();
        let mut title = if lo == hi { format!("Comment on line {}", shown(lo)) } else { format!("Comment on lines {}–{}", shown(lo), shown(hi)) };
        if lo != hi && self.forge == pr_craft_core::Forge::GitLab {
            title.push_str(&format!(". GitLab pins it to line {}; the range goes in the text.", shown(hi)));
        }
        let focus = std::mem::take(self.focus_composer);
        composer_ui(ui, Place::Line(self.width), self.composer, focus, title, acts);
    }
}

/// The rows of one hunk, unified or split, each with what sits under it.
/// The place in the hunk's visible lines that `half` of split row `row` shows: the old side of the left half, the new side of the
/// right. A removed line with no partner has no right half and an added one no left half.
pub(super) fn split_pos(row: &diff_view::SplitRow, half: Half) -> Option<usize> {
    match half {
        Half::Left => row.left,
        Half::Right => row.right,
    }
}

pub(super) fn hunk_rows(ui: &mut Ui, rows: &HunkRows<'_>, visible: &diff_view::Visible, under: &mut UnderRow<'_>, acts: &mut Vec<Act>) {
    let plain: Vec<DiffLine> = visible.lines.iter().map(|s| s.line.clone()).collect();
    let marks = if rows.layout.word_highlight { diff_view::changed_words(&plain) } else { Vec::new() };
    let marks_of = |pos: usize| marks.get(pos).map_or(&[][..], Vec::as_slice);
    let selection = under.selection;
    let selected = |index: usize| selection.is_some_and(|s| s.contains(rows.file, rows.hunk, index));
    let select =
        |ui: &Ui, index: usize| Act::Select { file: rows.file, hunk: rows.hunk, line: index, extend: ui.input(|input| input.modifiers.shift) };
    if !rows.split {
        for (pos, shown) in visible.lines.iter().enumerate() {
            let style = CodeStyle { marks: marks_of(pos), wrap: rows.layout.wrap };
            let row = components::diff_row(ui, under.width, &shown.line, selected(shown.index), true, style);
            if row.clicked() {
                acts.push(select(ui, shown.index));
            }
            let here = |extend: bool| Act::Select { file: rows.file, hunk: rows.hunk, line: shown.index, extend };
            match components::gutter_press(ui, &row) {
                // The comment button of a hovered line: a click comments on the line, a drag selects the lines it passes over.
                Some(Gutter::Click) => acts.extend([here(false), Act::OpenComposer]),
                Some(Gutter::DragStart) => {
                    acts.push(here(false));
                    ui.ctx().data_mut(|d| d.insert_temp(drag_key(), Some((rows.file, rows.hunk))));
                }
                Some(Gutter::Menu) => {
                    let at = ui.input(|i| i.pointer.interact_pos()).unwrap_or_else(|| row.rect.center());
                    acts.push(Act::LinesMenu { file: rows.file, hunk: rows.hunk, line: shown.index, at, restore: Some(row.id) });
                }
                None => {}
            }
            if dragging(ui) == Some((rows.file, rows.hunk)) && components::pointer_over(&row) {
                acts.push(here(true));
            }
            let by_key = row.has_focus() && keymap::pressed(ui, "app.context_menu");
            if row.secondary_clicked() || by_key {
                let at = if by_key { menu::below(row.rect) } else { ui.input(|i| i.pointer.interact_pos()).unwrap_or_else(|| row.rect.center()) };
                acts.push(Act::LinesMenu { file: rows.file, hunk: rows.hunk, line: shown.index, at, restore: Some(row.id) });
            }
            under.draw(ui, rows, &[(shown.index, &shown.line)], acts);
        }
        return;
    }
    for row in diff_view::split_rows(&plain) {
        let side = |pos: Option<usize>| pos.and_then(|p| visible.lines.get(p).map(|s| (p, s)));
        let (left, right) = (side(row.left), side(row.right));
        let cells = [left, right].map(|c| c.map(|(p, s)| Cell { line: &s.line, selected: selected(s.index), marks: marks_of(p) }));
        let out = components::split_row(ui, under.width, cells, rows.layout.wrap, true);
        let index_of = |half: Half| split_pos(&row, half).and_then(|p| visible.lines.get(p)).map(|s| s.index);
        let (file, hunk) = (rows.file, rows.hunk);
        let select_at = |line: usize, extend: bool| Act::Select { file, hunk, line, extend };
        if let Some(index) = out.clicked.and_then(index_of) {
            acts.push(select(ui, index));
        }
        if let Some((half, press)) = out.gutter
            && let Some(index) = index_of(half)
        {
            match press {
                Gutter::Click => acts.extend([select_at(index, false), Act::OpenComposer]),
                Gutter::DragStart => {
                    acts.push(select_at(index, false));
                    ui.ctx().data_mut(|d| d.insert_temp(drag_key(), Some((file, hunk))));
                }
                Gutter::Menu => {
                    let at = ui.input(|i| i.pointer.interact_pos()).unwrap_or_default();
                    acts.push(Act::LinesMenu { file, hunk, line: index, at, restore: None });
                }
            }
        }
        if dragging(ui) == Some((file, hunk))
            && let Some(index) = out.over.and_then(index_of)
        {
            acts.push(select_at(index, true));
        }
        if let Some((half, at, restore)) = out.menu
            && let Some(index) = index_of(half)
        {
            acts.push(Act::LinesMenu { file, hunk, line: index, at, restore: Some(restore) });
        }
        let mut here: Vec<(usize, &DiffLine)> = Vec::new();
        for (_, shown) in [left, right].into_iter().flatten() {
            if here.iter().all(|(index, _)| *index != shown.index) {
                here.push((shown.index, &shown.line));
            }
        }
        under.draw(ui, rows, &here, acts);
    }
}

/// What the whole-stack view draws besides the files.
pub(super) struct WholeView<'a> {
    pub(super) omitted: &'a [String],
    pub(super) unfolded: &'a HashSet<String>,
    /// The file and hunk J K N P last moved to, and what to focus there this frame.
    pub(super) at: (usize, usize),
    pub(super) focus: Focus,
    pub(super) forge: &'static str,
    /// The forge's page for this comparison.
    pub(super) web_url: Option<&'a str>,
    /// The width of the diff pane; rows and bands are at least this wide.
    pub(super) width: f32,
}

/// A file of the whole-stack diff: the same lines as a layer's diff, with nothing to select, comment on or mark viewed.
/// The file and hunk headers take focus so J K N P can walk them (rule 21).
pub(super) fn whole_file_ui(ui: &mut Ui, i: usize, file: &FileDiff, view: &WholeView, acts: &mut Vec<Act>) {
    let renamed = file.old_path.as_deref().map(|old| format!("{old} → ")).unwrap_or_default();
    let p = theme::pal(ui);
    let header = egui::Frame::new().fill(p.sunken).inner_margin(Margin::symmetric(16, 6)).show(ui, |ui| {
        ui.set_width(view.width - 32.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let name = RichText::new(format!("{renamed}{}", file.path)).font(FontId::monospace(theme::sz(ui).code)).color(p.graphite);
            let header = ui.add(egui::Button::new(name).frame(false));
            if view.focus == Focus::File && view.at.0 == i {
                header.request_focus();
                header.scroll_to_me(Some(Align::TOP));
            }
            components::mono_label(ui, format!("+{} −{}", file.additions, file.deletions), 11.5, p.text_secondary);
        });
    });
    components::seam_bottom(ui, header.response.rect);
    if view.omitted.contains(&file.path) {
        // Rule 15: a file the forge left out is not an empty change.
        let note = match view.web_url {
            Some(_) => format!("Too large to show here. Open on {}.", view.forge),
            None => "Too large to show here.".to_string(),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(note).size(theme::sz(ui).body).color(theme::pal(ui).ember_text));
            if let Some(url) = view.web_url
                && ui.button(format!("Open on {}", view.forge)).clicked()
            {
                acts.push(Act::OpenUrl(url.to_string()));
            }
        });
        return;
    }
    if !shows_hunks(file, false, view.unfolded) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(fold_reason(file, false)).size(theme::sz(ui).body).color(theme::pal(ui).text_tertiary));
            if !file.binary && ui.button("Show").clicked() {
                acts.push(Act::Unfold(file.path.clone()));
            }
        });
        return;
    }
    ui.spacing_mut().item_spacing.y = 0.0;
    for (h, hunk) in file.hunks.iter().enumerate() {
        let header = components::hunk_header(ui, view.width, &hunk.header);
        if view.focus == Focus::Hunk && view.at == (i, h) {
            header.request_focus();
            header.scroll_to_me(Some(Align::TOP));
        }
        for line in &hunk.lines {
            components::diff_row(ui, view.width, line, false, false, CodeStyle::default());
        }
    }
}

/// Whether a file's hunks are drawn: not binary, and not folded unless the reader unfolded it.
pub(super) fn shows_hunks(file: &FileDiff, viewed: bool, unfolded: &HashSet<String>) -> bool {
    let folded = viewed || file.generated || file.additions + file.deletions > LARGE_FILE_LINES;
    !file.binary && (!folded || unfolded.contains(&file.path))
}

/// The stop after (or before) `here`, as `(file, hunk)`. `shown` lists file indices in display order and
/// `hunk_counts` how many hunks each draws: J K step over hunks of files that draw some, N P over every shown file.
/// From a folded file (just marked viewed, say), it goes to the nearest stop in that direction.
pub(super) fn next_stop(
    shown: &[usize],
    hunk_counts: &dyn Fn(usize) -> usize,
    here: (usize, usize),
    hunks: bool,
    forward: bool,
) -> Option<(usize, usize)> {
    let mut stops = Vec::new();
    for &i in shown {
        let count = if hunks { hunk_counts(i) } else { 0 };
        if hunks && count == 0 {
            continue;
        }
        stops.extend((0..count.max(1)).map(|h| (i, h)));
    }
    let at = stops.iter().position(|&s| s == (here.0, if hunks { here.1 } else { 0 }));
    let rank = |file: usize| shown.iter().position(|&v| v == file);
    let place = rank(here.0);
    match (at, forward) {
        (None, true) => stops.iter().find(|s| rank(s.0) > place),
        (None, false) => stops.iter().rev().find(|s| rank(s.0) < place),
        (Some(p), true) => stops.get(p + 1).or(stops.get(p)),
        (Some(p), false) => stops.get(p.saturating_sub(1)),
    }
    .copied()
}

impl Ready {
    /// The keys for the diff options, more lines, and threads.
    pub(super) fn diff_keys(&self, ui: &Ui, acts: &mut Vec<Act>) {
        for (id, act) in self.layout_keys() {
            if keymap::pressed(ui, id) {
                acts.push(act);
            }
        }
        for (id, forward) in [("review.next_thread", true), ("review.prev_thread", false)] {
            if keymap::pressed(ui, id) {
                acts.push(Act::ThreadStep { forward });
            }
        }
        // Resolving and replying act once per press: a held key must not flip the thread back and forth.
        for (id, act) in [("review.resolve_thread", Act::ThreadResolveCurrent), ("review.reply_thread", Act::ThreadReplyCurrent)] {
            if keymap::fresh(ui, id) {
                acts.push(act);
            }
        }
    }

    pub(super) fn diff_pane(&mut self, ui: &mut Ui, width: f32, acts: &mut Vec<Act>) {
        let p = theme::pal(ui);
        let readonly = self.readonly();
        let controls = !readonly && !self.view.files.is_empty();
        let avail = ui.available_rect_before_wrap();
        components::padded(ui, Margin { left: 20, right: 20, top: 12, bottom: 12 }, |ui| {
            ui.set_width(ui.available_width());
            ui.scope(|ui| {
                ui.set_max_width((ui.available_width() - if controls { LAYOUT_CONTROLS } else { 0.0 }).max(160.0));
                if self.commits.shown() {
                    let mut asked = Vec::new();
                    self.commits.title(ui, &mut asked);
                    acts.extend(asked.into_iter().map(Act::Commit));
                } else if self.whole_shown() {
                    self.whole_title(ui);
                } else if self.since_shown() {
                    self.since_title(ui);
                } else {
                    let meta = &self.view.meta;
                    components::label(ui, meta.title.as_str(), 15.0, p.graphite);
                    let (adds, dels) = self.view.files.iter().fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
                    let line = format!(
                        "{} · {} → {} · +{adds} −{dels} in {} · {}",
                        meta.author,
                        meta.head_branch,
                        meta.base_branch,
                        components::files_word(self.view.files.len()),
                        self.view.change.reference()
                    );
                    components::note_sized(ui, line, 12.0, p.text_tertiary);
                }
            });
            if controls {
                self.layout_controls(ui, avail, acts);
            }
        });
        components::rule(ui);
        components::padded(ui, Margin::symmetric(20, 6), |ui| {
            let trigger = ui.horizontal(|ui| {
                let at = components::title_action(ui, &self.commits.summary(), &keymap::label(ui, "review.commits"));
                if at.clicked() {
                    acts.push(Act::Commit(CommitAct::Toggle));
                }
                at.rect
            });
            self.commits.trigger = Some(trigger.inner);
            self.stack_strip(ui, acts);
            let hint = self.hint_line(ui);
            if !hint.is_empty() {
                components::note(ui, hint, p.text_tertiary);
            }
        });
        if readonly {
            self.readonly_body(ui, width, acts);
            self.focus = Focus::None;
            return;
        }
        if self.view.stale {
            let note = "These drafts were written on an earlier commit. They post against that commit, so the forge may show them as outdated. Edit or delete them in Your review.";
            components::padded(ui, Margin::symmetric(20, 6), |ui| components::banner(ui, BannerKind::Warn, "Drafts from an earlier commit", note));
        }
        self.threads_note(ui);
        if self.view.files.is_empty() {
            components::padded(ui, Margin::symmetric(20, 6), |ui| {
                components::note_sized(ui, "This change has no file changes.", 13.0, p.text_secondary);
            });
            return;
        }
        if self.visible.is_empty() {
            components::padded(ui, Margin::symmetric(20, 6), |ui| {
                components::label(ui, "The filter hides every file", 15.0, p.graphite);
                let note = format!(
                    "{} matches none of the {} changed files. Drafts and viewed marks are not affected.",
                    self.patterns.join(" "),
                    self.view.files.len()
                );
                components::note_sized(ui, note, 13.0, p.text_secondary);
            });
            return;
        }
        let (counted, all_viewed) = {
            let counted: Vec<&FileDiff> = self.view.files.iter().filter(|f| !f.generated).collect();
            (counted.len(), !counted.is_empty() && counted.iter().all(|f| self.viewed(f)))
        };
        if !all_viewed {
            self.show_viewed = false;
        }
        if all_viewed && !self.show_viewed {
            // Completion is an empty state too (frame 108). It claims nothing the data cannot back: generated files are not counted,
            // and drafts from an earlier commit are said to be so.
            let drafts = self.view.local.drafts.len();
            let mut body = match drafts {
                0 => "You have no drafts, so Submit posts only your verdict.".to_string(),
                1 => "Your 1 draft is ready to go out as one review.".to_string(),
                n => format!("Your {n} drafts are ready to go out as one review."),
            };
            body.push_str(" A file that changes again loses its viewed mark and comes back here.");
            let generated = self.view.files.iter().filter(|f| f.generated).count();
            if generated > 0 {
                body.push_str(&format!(
                    " {} not counted.",
                    if generated == 1 { "1 generated file is".to_string() } else { format!("{generated} generated files are") }
                ));
            }
            if self.view.stale {
                body.push_str(" The drafts were written on an earlier commit.");
            }
            let title = format!("All {counted} {} viewed", if counted == 1 { "file" } else { "files" });
            let pressed = components::padded(ui, Margin::symmetric(20, 48), |ui| {
                components::empty_state(ui, &title, &body, &[("Show viewed files", ""), ("Go to Submit", "")])
            });
            match pressed {
                Some(0) => self.show_viewed = true,
                Some(_) => self.focus_submit = true,
                None => {}
            }
            self.focus = Focus::None;
            return;
        }
        ScrollArea::both().id_salt("diff").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for n in 0..self.visible.len() {
                let i = self.visible[n];
                self.file_ui(ui, i, width, acts);
                ui.add_space(theme::SPACE_3);
            }
        });
        self.focus = Focus::None;
    }

    pub(super) fn file_ui(&mut self, ui: &mut Ui, i: usize, width: f32, acts: &mut Vec<Act>) {
        let file = &self.view.files[i];
        let viewed = self.viewed(file);
        let p = theme::pal(ui);
        // Frame 79 (and every file header of the review frames): the path and its counts, then the views, a divider, Viewed and its key.
        let header = egui::Frame::new()
            .fill(p.sunken)
            .inner_margin(Margin::symmetric(16, 0))
            .show(ui, |ui| {
                // The controls take their room first; the path is cut to what is left, so they never draw over it.
                ui.allocate_ui_with_layout(egui::vec2(width - 32.0, 34.0), Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.spacing_mut().icon_spacing = 6.0;
                    components::key_cap(ui, &keymap::label(ui, "review.viewed"));
                    let mut checked = viewed;
                    let check =
                        ui.add(egui::Checkbox::new(&mut checked, RichText::new("Viewed").size(12.0 * theme::sz(ui).body / 13.5).color(p.graphite)));
                    if check.changed() {
                        acts.push(Act::ToggleViewed(i));
                    }
                    if self.focus == Focus::File && i == self.file {
                        check.request_focus();
                        check.scroll_to_me(Some(Align::TOP));
                    }
                    if views::offered(file).len() > 1 {
                        let (divider, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), egui::Sense::hover());
                        ui.painter().rect_filled(divider, 0.0, p.border);
                        self.view_switcher(ui, i, file, acts);
                    }
                    ui.add_space(12.0);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let counts = (format!("+{}", file.additions), format!("−{}", file.deletions));
                        let room =
                            ui.available_width() - 20.0 - components::mono_width(ui, &counts.0, 11.5) - components::mono_width(ui, &counts.1, 11.5);
                        let renamed = file.old_path.as_deref().map(|old| format!("{old} → ")).unwrap_or_default();
                        components::mono_label_cut(ui, format!("{renamed}{}", file.path), 12.5, p.graphite, room);
                        components::mono_label(ui, counts.0, 11.5, p.moss_text);
                        components::mono_label(ui, counts.1, 11.5, p.rose_text);
                    });
                });
            })
            .response
            .rect;
        components::seam_bottom(ui, header);
        let view = self.views.current(file);
        if view != FileView::Source && self.rendered_visible(file, viewed) {
            // A file loads when its header is near the window; the one the reader is on always does.
            let near = i == self.file || ui.is_rect_visible(header.expand2(egui::vec2(0.0, theme::PREFETCH)));
            self.rendered_body(ui, i, view, width, near, acts);
            return;
        }
        if !self.expanded(file) {
            let rendered = views::offered(file).len() > 1;
            let why = if viewed && file.binary && rendered { "Collapsed because viewed.".to_string() } else { fold_reason(file, viewed) };
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                components::label(ui, why, 12.5, p.text_tertiary);
                if (!file.binary || (viewed && rendered)) && Pill::new(PillKind::Secondary, "Show").small().show(ui).clicked() {
                    acts.push(Act::Unfold(file.path.clone()));
                }
            });
            return;
        }
        let current_hunk = (i == self.file).then_some(self.hunk);
        let (layout, split) = (self.layout.clone(), self.split_on());
        let gaps = context::gaps(&file.hunks);
        let reveals = self.reveals.get(&i).map(Vec::as_slice).unwrap_or_default();
        let keys = ExpandKeys::of(ui);
        let ai_current = self.ai_current().map(|c| c.draft.id.clone());
        let file_threads: Vec<&ReviewThread> = match &self.threads {
            Fetch::Ready(list) => list.threads.iter().filter(|t| t.path == file.path).collect(),
            _ => Vec::new(),
        };
        let mut under = UnderRow {
            width,
            file,
            forge: self.view.change.forge,
            selection: self.selection,
            composer: &mut self.composer,
            focus_composer: &mut self.focus_composer,
            editing: &mut self.editing,
            inline: &mut self.inline,
            ai_inline: &mut self.ai_inline,
            // Line numbers of stale drafts belong to another commit, so they are not placed in this diff.
            drafts: if self.view.stale { &[] } else { &self.view.local.drafts },
            ai: &self.ai_pending,
            threads: &file_threads,
            cards: &mut self.cards,
            ai_current,
            focus_draft: if self.focus_draft { self.draft_focus } else { None },
            focus_ai: self.focus_ai,
        };
        let draw = GapDraw {
            width,
            file: i,
            split,
            wrap: layout.wrap,
            keys: &keys,
            here: current_hunk,
            // An added or deleted file has no lines below its last hunk to read.
            below_last: matches!(file.status, FileStatus::Modified | FileStatus::Renamed) && !file.hunks.is_empty(),
            hunks: file.hunks.len(),
        };
        for (h, hunk) in file.hunks.iter().enumerate() {
            let writing = under.composer.is_some();
            let pinned = under.selection.filter(|s| writing && (s.file, s.hunk) == (i, h)).map(|s| s.lines().1);
            let visible = diff_view::visible(hunk, layout.hide_whitespace, pinned);
            if h > 0 {
                gap_ui(ui, &draw, gaps.get(h).copied(), reveals.get(h), acts);
            }
            let header = components::hunk_header(ui, width, &hunk_title(hunk, visible.hidden));
            if self.focus == Focus::Hunk && (i, h) == (self.file, self.hunk) {
                header.request_focus();
                header.scroll_to_me(Some(Align::TOP));
            }
            if h == 0 {
                gap_ui(ui, &draw, gaps.first().copied(), reveals.first(), acts);
            }
            let rows = HunkRows { file: i, hunk: h, source: hunk, layout: layout.clone(), split };
            hunk_rows(ui, &rows, &visible, &mut under, acts);
        }
        gap_ui(ui, &draw, gaps.get(file.hunks.len()).copied(), reveals.get(file.hunks.len()), acts);
        loose_threads(ui, &mut under, acts);
    }

    /// ⇧S switches between rendered and source; 1 to 4 pick an image compare mode.
    pub(super) fn view_keys(&self, ui: &Ui, acts: &mut Vec<Act>) {
        let Some(file) = self.view.files.get(self.file).filter(|_| self.visible.contains(&self.file)) else { return };
        let offered = views::offered(file);
        if offered.len() < 2 {
            return;
        }
        if keymap::pressed(ui, "review.toggle_source") {
            acts.push(Act::ToggleView(self.file));
        }
        let modes = [
            ("review.image_two_up", FileView::ImageTwoUp),
            ("review.image_swipe", FileView::ImageSwipe),
            ("review.image_onion", FileView::ImageOnion),
            ("review.image_difference", FileView::ImageDifference),
        ];
        for (id, view) in modes {
            if offered.contains(&view) && keymap::pressed(ui, id) {
                acts.push(Act::SetView(self.file, view));
            }
        }
    }

    /// The views `file` offers, in its header (frames 25, 26, 79); the Keys panel names the key that steps through them.
    pub(super) fn view_switcher(&self, ui: &mut Ui, i: usize, file: &FileDiff, acts: &mut Vec<Act>) {
        let offered = views::offered(file);
        let current = self.views.current(file);
        let labels: Vec<String> = offered.iter().map(|view| view.label().to_string()).collect();
        let at = offered.iter().position(|&view| view == current).unwrap_or(0);
        if let Some(picked) = components::segmented(ui, &format!("views-{i}"), "File view", &labels, at)
            && let Some(&view) = offered.get(picked)
        {
            acts.push(Act::SetView(i, view));
        }
    }

    /// A rendered view draws unless the reader folded the file (viewed, generated or large) and has not unfolded it.
    pub(super) fn rendered_visible(&self, file: &FileDiff, viewed: bool) -> bool {
        let folded = viewed || file.generated || file.additions + file.deletions > LARGE_FILE_LINES;
        !folded || self.unfolded.contains(&file.path)
    }

    /// Whether the diff draws `file` in a rendered view now, not as hunks.
    pub(super) fn drawn_rendered(&self, file: &FileDiff) -> bool {
        self.views.current(file) != FileView::Source && self.rendered_visible(file, self.viewed(file))
    }

    /// The current file shows as rendered Markdown, so J K and C work on its blocks.
    pub(super) fn rendered_current(&self) -> bool {
        self.view.files.get(self.file).is_some_and(|f| self.visible.contains(&self.file) && self.drawn_rendered(f) && self.views.walks_blocks(f))
    }

    /// Draws file `i` in its rendered `view`; Markdown blocks carry their composer and drafts under them.
    pub(super) fn rendered_body(&mut self, ui: &mut Ui, i: usize, view: FileView, width: f32, near: bool, acts: &mut Vec<Act>) {
        let file = &self.view.files[i];
        if near {
            self.views.want(ui.ctx(), &self.view.change, &self.view.meta, file, i == self.file);
        }
        let (stale, drafts) = (self.view.stale, &self.view.local.drafts);
        let mut after = |ui: &mut Ui, item: usize, block: &MdBlockView| {
            if let Some(target) = self.block
                && (target.file, target.item) == (i, item)
                && self.composer.is_some()
            {
                let focus = std::mem::take(&mut self.focus_composer);
                let title =
                    format!("Comment on this {} · it posts on line {} of the source", views::kind_name(block.block.kind), target.line.shown());
                composer_ui(ui, Place::Line(width), &mut self.composer, focus, title, acts);
            }
            let Some(anchor) = block.anchor else { return };
            // Line numbers of stale drafts belong to another commit, so they are not placed in this file.
            for draft in drafts.iter().filter(|d| !stale && d.path == file.path && d.end == anchor) {
                self.inline.insert(draft.id);
                let focus = self.focus_draft && self.draft_focus == Some(draft.id);
                draft_card(ui, Place::Line(width), draft, &mut self.editing, focus, acts);
            }
        };
        match views::show(ui, &mut self.views, file, view, width, &mut after) {
            Some(Pressed::Comment(item)) => acts.push(Act::CommentBlock { file: i, item }),
            Some(Pressed::Select(item)) => {
                // The file the reader clicked in is the one J K C work on.
                make_current(&mut self.file, &mut self.hunk, i);
                self.views.set_cursor(&file.path, item);
            }
            None => {}
        }
        if self.views.is_svg(&file.path) {
            self.svg_band(ui, i, width, acts);
        }
    }

    /// Frame 79: under an SVG's markup, the comment being written on it and its drafts, each card on its own.
    pub(super) fn svg_band(&mut self, ui: &mut Ui, i: usize, width: f32, acts: &mut Vec<Act>) {
        let file = &self.view.files[i];
        if let Some(target) = self.block.filter(|t| t.file == i)
            && self.composer.is_some()
        {
            let focus = std::mem::take(&mut self.focus_composer);
            let what = self.views.block_anchor(&file.path, target.item).map(|(_, name)| name).unwrap_or_else(|| "the rendered view".into());
            let title = format!("Comment on {what} · it posts on line {} of the source", target.line.shown());
            composer_ui(ui, Place::Band(width), &mut self.composer, focus, title, acts);
        }
        // Line numbers of stale drafts belong to another commit, so they are not placed in this file.
        for draft in self.view.local.drafts.iter().filter(|d| !self.view.stale && d.path == file.path) {
            self.inline.insert(draft.id);
            let focus = self.focus_draft && self.draft_focus == Some(draft.id);
            draft_card(ui, Place::Band(width), draft, &mut self.editing, focus, acts);
        }
    }

    /// `C` in rendered Markdown or an SVG: comment on the block or markup row the cursor is on, else the first changed one.
    pub(super) fn comment_on_cursor_block(&mut self, ctx: &Context) {
        let target = self.view.files.get(self.file).and_then(|f| self.views.comment_target(&f.path));
        match target {
            Some((item, _)) => self.open_block_composer(ctx, self.file, item),
            None => {
                self.notice = Some(format!(
                    "This file has no changed block or markup row to comment on. Click one, then press {}.",
                    keymap::label(ctx, "review.comment")
                ))
            }
        }
    }

    pub(super) fn open_block_composer(&mut self, ctx: &Context, file: usize, item: usize) {
        let Some(path) = self.view.files.get(file).map(|f| f.path.clone()) else { return };
        match self.views.block_anchor(&path, item) {
            Some((Some(line), _)) => {
                let target = BlockTarget { file, item, line };
                if self.block != Some(target) {
                    self.composer = Some(String::new());
                }
                self.selection = None;
                // The box belongs to the current file; otherwise the next frame closes it.
                make_current(&mut self.file, &mut self.hunk, file);
                self.block = Some(target);
                self.focus_composer = true;
                self.views.set_cursor(&path, item);
            }
            _ => {
                self.notice = Some(format!(
                    "The forge takes comments only on lines the diff shows, and this has none. Press {} to see the source.",
                    keymap::label(ctx, "review.toggle_source")
                ));
            }
        }
    }

    /// `C` in a source view: comment on the selected lines, else on the first line of the current hunk.
    pub(super) fn open_source_composer(&mut self) {
        // A box left open on a rendered block must not take the source comment.
        drop_block(&mut self.block, &mut self.composer);
        if self.selection.is_none()
            && let Some(file) = self.view.files.get(self.file)
            && self.expanded(file)
            && file.hunks.get(self.hunk).is_some_and(|h| !h.lines.is_empty())
        {
            self.selection = Some(Selection { file: self.file, hunk: self.hunk, anchor: 0, head: 0 });
        }
        if self.selection.is_some() {
            self.composer.get_or_insert_with(String::new);
            self.focus_composer = true;
        }
    }

    /// `review.set_view`: this file now, and every file with its extension from now on.
    pub(super) fn set_view(&mut self, i: usize, view: FileView) -> Result<(), pr_craft_engine::CommandError> {
        let Some(path) = self.view.files.get(i).map(|f| f.path.clone()) else { return Ok(()) };
        // A comment box opened on a rendered block would be hidden by another view, so it closes with it.
        if self.block.take().is_some() {
            self.composer = None;
        }
        self.views.choose(&path, view);
        self.notice = Some(format!("{path} shown as {}.", view.label()));
        self.views.remember(engine_view::set_view(&file_view::extension(&path), view)?);
        Ok(())
    }

    pub(super) fn whole_shown(&self) -> bool {
        !matches!(self.whole, Fetch::Idle)
    }

    pub(super) fn since_shown(&self) -> bool {
        !matches!(self.since, Fetch::Idle)
    }

    /// A read-only comparison replaces the diff: the whole stack or what changed since the user's review.
    pub(super) fn readonly(&self) -> bool {
        self.whole_shown() || self.since_shown() || self.commits.shown()
    }

    /// The files of the read-only comparison that has loaded, the paths the forge left out, and the forge's page for it.
    pub(super) fn compared(&self) -> Option<(&[FileDiff], &[String], Option<&str>)> {
        match (&self.commits.range, &self.whole, &self.since) {
            (Fetch::Ready(range), _, _) => Some((&range.files, &range.omitted, range.web_url.as_deref())),
            (_, Fetch::Ready(whole), _) => Some((&whole.files, &whole.omitted, whole.web_url.as_deref())),
            (_, _, Fetch::Ready(since)) => Some((&since.files, &since.omitted, since.web_url.as_deref())),
            _ => None,
        }
    }

    /// The stack's layers, bottom first; empty until they load and when this change is not stacked.
    pub(super) fn layers(&self) -> &[StackLayer] {
        match &self.layers {
            Fetch::Ready(layers) if layers.len() >= 2 => layers,
            _ => &[],
        }
    }

    pub(super) fn toggle_whole(&mut self, ctx: &Context) {
        self.focus = Focus::None;
        self.readonly_at = (0, 0);
        if self.whole_shown() {
            self.whole = Fetch::Idle;
        } else if !self.layers().is_empty() {
            self.since = Fetch::Idle;
            self.commits.range = Fetch::Idle;
            self.commits.picked = None;
            // The strip already read the layers; the diff does not list open changes again.
            let (change, layers) = (self.view.change.clone(), self.layers().to_vec());
            self.whole = Fetch::start(ctx, move || stacks::whole_of(&change, layers).map_err(crate::fail));
        }
    }

    pub(super) fn toggle_since(&mut self, ctx: &Context) {
        self.focus = Focus::None;
        self.readonly_at = (0, 0);
        if self.since_shown() {
            self.since = Fetch::Idle;
        } else {
            self.whole = Fetch::Idle;
            self.commits.range = Fetch::Idle;
            self.commits.picked = None;
            let change = self.view.change.clone();
            self.since = Fetch::start(ctx, move || turn::since_last(&change).map_err(crate::fail));
        }
    }

    /// The body of a read-only comparison: its files as the review draws them, with nothing to select or comment on.
    pub(super) fn readonly_body(&self, ui: &mut Ui, width: f32, acts: &mut Vec<Act>) {
        ui.add_space(theme::SPACE_2);
        let what = if self.commits.shown() {
            "the picked commits"
        } else if self.whole_shown() {
            "the whole stack"
        } else {
            "what changed since your review"
        };
        let Some((files, omitted, web_url)) = self.compared() else {
            match (&self.commits.range, &self.whole, &self.since) {
                (Fetch::Failed(error), _, _) | (_, Fetch::Failed(error), _) | (_, _, Fetch::Failed(error)) => {
                    crate::live_label(ui, format!("Could not load {what}: {error}"), theme::pal(ui).rose_text);
                }
                _ => crate::live_label(ui, format!("Fetching {what}…"), theme::pal(ui).text_secondary),
            }
            return;
        };
        if files.is_empty() {
            let empty = match &self.since {
                _ if self.commits.shown() => "The picked commits change no file.",
                Fetch::Ready(since) if since.commits_since == 0 => "No new commits since your review.",
                Fetch::Ready(_) => "No file changed since your review.",
                _ => "The stack has no file changes.",
            };
            ui.label(RichText::new(empty).size(theme::sz(ui).body).color(theme::pal(ui).text_secondary));
            return;
        }
        let state = WholeView {
            omitted,
            unfolded: &self.unfolded,
            at: self.readonly_at,
            focus: self.focus,
            forge: self.view.change.forge.name(),
            web_url,
            width,
        };
        ScrollArea::both().id_salt("readonly").auto_shrink([false, false]).show(ui, |ui| {
            for (i, file) in files.iter().enumerate() {
                whole_file_ui(ui, i, file, &state, acts);
                ui.add_space(theme::SPACE_3);
            }
        });
    }

    /// Steps through files (N, P) or hunks (J, K) of the visible, unfolded files, or of the read-only view when it shows.
    pub(super) fn step(&mut self, hunks: bool, forward: bool) {
        let readonly = self.readonly();
        let next = if let Some((files, ..)) = self.compared() {
            let shown: Vec<usize> = (0..files.len()).collect();
            let counts = |i: usize| files.get(i).filter(|f| shows_hunks(f, false, &self.unfolded)).map_or(0, |f| f.hunks.len());
            next_stop(&shown, &counts, self.readonly_at, hunks, forward)
        } else if readonly {
            None
        } else {
            let counts = |i: usize| self.view.files.get(i).filter(|f| self.expanded(f) && !self.drawn_rendered(f)).map_or(0, |f| f.hunks.len());
            next_stop(&self.visible, &counts, (self.file, self.hunk), hunks, forward)
        };
        if let Some(stop) = next {
            if readonly {
                self.readonly_at = stop;
            } else {
                (self.file, self.hunk) = stop;
            }
            self.focus = if hunks { Focus::Hunk } else { Focus::File };
        }
    }

    /// Reads more lines for a gap, after the read that is on its way.
    pub(super) fn queue_expand(&mut self, ctx: &Context, file: usize, gap: usize, kind: Kind) {
        self.expand_queue.push_back((file, gap, kind));
        self.next_expand(ctx);
    }

    /// ⇧K, ⇧J and ⌥E: more lines next to the current hunk.
    pub(super) fn expand_around(&mut self, ctx: &Context, kind: Kind) {
        let Some(file) = self.view.files.get(self.file).filter(|f| self.visible.contains(&self.file) && self.expanded(f)) else { return };
        if file.hunks.is_empty() || self.drawn_rendered(file) {
            return;
        }
        let (above, below) = (self.hunk, self.hunk + 1);
        let gaps: &[usize] = match kind {
            Kind::Above => &[above],
            Kind::Below => &[below],
            Kind::All => &[above, below],
        };
        if !gaps.iter().any(|&gap| self.has_more(self.file, gap, kind)) {
            self.notice = Some("There are no more lines to show around this hunk.".into());
            return;
        }
        for &gap in gaps {
            self.queue_expand(ctx, self.file, gap, kind);
        }
    }

    /// Whether reading more of gap `gap` of file `file` that way would bring lines.
    pub(super) fn has_more(&self, file: usize, gap: usize, kind: Kind) -> bool {
        let Some(f) = self.view.files.get(file) else { return false };
        // Below the last hunk there are lines to read only in a file that exists on both sides; an added or deleted file has no such lines.
        if gap >= f.hunks.len() && (f.hunks.is_empty() || !matches!(f.status, FileStatus::Modified | FileStatus::Renamed)) {
            return false;
        }
        let Some(&at) = context::gaps(&f.hunks).get(gap) else { return false };
        let reveal = self.reveals.get(&file).and_then(|r| r.get(gap)).cloned().unwrap_or_default();
        context::plan(at, &reveal, kind).is_some()
    }

    /// Starts the next queued read when none is on its way. A gap with nothing to read that way is skipped.
    pub(super) fn next_expand(&mut self, ctx: &Context) {
        while self.expanding.is_none() {
            let Some((file, gap, kind)) = self.expand_queue.pop_front() else { return };
            let Some(f) = self.view.files.get(file) else { continue };
            let Some(&at) = context::gaps(&f.hunks).get(gap) else { continue };
            let reveal = self.reveals.get(&file).and_then(|r| r.get(gap)).cloned().unwrap_or_default();
            let Some((from, to)) = context::plan(at, &reveal, kind) else { continue };
            let (change, meta, path) = (self.view.change.clone(), self.view.meta.clone(), f.path.clone());
            let fetch = Fetch::start(ctx, move || engine::expand_at(&change, &meta, &path, engine_view::Side::Head, from, to).map_err(crate::fail));
            self.expanding = Some(Expanding { file, gap, kind, fetch });
        }
    }

    /// Puts the lines a read brought into the gap they were read for, then starts the next read.
    pub(super) fn poll_expand(&mut self, ctx: &Context) {
        let Some(reading) = &mut self.expanding else { return };
        reading.fetch.poll();
        let done = match &reading.fetch {
            Fetch::Ready(read) => Ok(read.clone()),
            Fetch::Failed(error) => Err(error.clone()),
            Fetch::Idle | Fetch::Loading(_) => return,
        };
        let Some(reading) = self.expanding.take() else { return };
        match done {
            Ok(read) => self.reveal(reading.file, reading.gap, reading.kind, read),
            Err(error) => {
                self.expand_queue.clear();
                self.error = Some(format!("Could not read more lines: {error}"));
            }
        }
        self.next_expand(ctx);
    }

    pub(super) fn reveal(&mut self, file: usize, gap: usize, kind: Kind, read: engine::Expanded) {
        let Some(f) = self.view.files.get(file) else { return };
        let all = context::gaps(&f.hunks);
        let Some(&at) = all.get(gap) else { return };
        let reveals = self.reveals.entry(file).or_default();
        if reveals.len() < all.len() {
            reveals.resize(all.len(), Reveal::default());
        }
        if let Some(reveal) = reveals.get_mut(gap) {
            context::apply(at, reveal, kind, read.lines, Some(read.total));
        }
    }

    /// The current file when the filter leaves it visible.
    pub(super) fn current_file(&self) -> Option<&FileDiff> {
        self.view.files.get(self.file).filter(|_| self.visible.contains(&self.file))
    }
}

#[cfg(test)]
mod split_tests {
    use pr_craft_core::diff::parse_unified;

    use super::*;

    const DIFF: &str = "diff --git a/f.go b/f.go
--- a/f.go
+++ b/f.go
@@ -10,6 +10,6 @@ func f() {
 keep
-old one
-old two
+new one
 middle
-gone
 tail
+fresh
";

    #[test]
    fn each_half_of_a_split_row_anchors_where_the_unified_view_anchors_that_line() {
        let file = parse_unified(DIFF).remove(0);
        let lines = &file.hunks[0].lines;
        let rows = diff_view::split_rows(lines);
        // The pure mapping `hunk_rows` uses to turn a pressed half into the line that is selected and later commented on.
        let anchor = |row: usize, half| split_pos(&rows[row], half).map(|at| LineRef { old: lines[at].old, new: lines[at].new });
        let unified = |at: usize| LineRef { old: lines[at].old, new: lines[at].new };
        // A context line is the same line on both halves.
        assert_eq!(anchor(0, Half::Left), Some(unified(0)));
        assert_eq!(anchor(0, Half::Right), Some(unified(0)));
        // A replaced pair: the left half is the removed line (old number only), the right the added line (new number only).
        let (left, right) = (anchor(1, Half::Left).expect("left"), anchor(1, Half::Right).expect("right"));
        assert!(left.old.is_some() && left.new.is_none(), "{left:?}");
        assert!(right.new.is_some() && right.old.is_none(), "{right:?}");
        // A removed run longer than the added one leaves the right half of its last row empty: nothing to anchor there.
        assert!(anchor(2, Half::Left).is_some_and(|l| l.old.is_some() && l.new.is_none()));
        assert_eq!(anchor(2, Half::Right), None);
        // Removed-only anchors old, added-only anchors new.
        let removed = rows.iter().position(|r| r.right.is_none() && r.left.is_some_and(|l| lines[l].text == "gone")).expect("a removed-only row");
        assert!(anchor(removed, Half::Left).is_some_and(|l| l.old.is_some() && l.new.is_none()));
        assert_eq!(anchor(removed, Half::Right), None);
        let added = rows.iter().position(|r| r.left.is_none() && r.right.is_some_and(|l| lines[l].text == "fresh")).expect("an added-only row");
        assert_eq!(anchor(added, Half::Left), None);
        assert!(anchor(added, Half::Right).is_some_and(|l| l.new.is_some() && l.old.is_none()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn j_and_k_walk_hunks_across_files_and_n_p_walk_files() {
        // Three files with 2, 0 (folded) and 1 hunks.
        let counts = |i: usize| [2, 0, 1].get(i).copied().unwrap_or(0);
        let shown = [0, 1, 2];
        assert_eq!(next_stop(&shown, &counts, (0, 0), true, true), Some((0, 1)));
        assert_eq!(next_stop(&shown, &counts, (0, 1), true, true), Some((2, 0)));
        assert_eq!(next_stop(&shown, &counts, (2, 0), true, false), Some((0, 1)));
        assert_eq!(next_stop(&shown, &counts, (0, 1), false, true), Some((1, 0)));
        assert_eq!(next_stop(&shown, &counts, (2, 0), false, true), Some((2, 0)));
        // From the folded file, J goes to the next stop after it.
        assert_eq!(next_stop(&shown, &counts, (1, 0), true, true), Some((2, 0)));
    }
}
