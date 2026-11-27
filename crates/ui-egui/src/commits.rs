//! Pick commits to review (Figma frame 95): the popover under the header's subline, and the read-only title of the diff of the
//! commits picked. Comments still attach to the change's latest commit, so the view of picked commits cannot be commented on.

use chrono::Utc;
use craft::egui::{self, Align, Context, Pos2, Rect, ScrollArea, Ui};
use pr_craft_core::commits::{self as pick, Picked};
use pr_craft_engine::commits::{CommitList, RangeView};

use crate::ai::Fetch;
use crate::components;
use crate::keymap;
use crate::theme;

/// What the commit controls ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitAct {
    Toggle,
    /// "All changes": the whole diff again.
    All,
    /// "Since your review".
    Since,
    /// Cut the pick down to its longest run without a merge commit.
    DropMerges,
    Pick {
        index: usize,
        shift: bool,
    },
    Step {
        forward: bool,
    },
}

/// The commit picker of one review.
pub(crate) struct Commits {
    pub list: Fetch<CommitList>,
    pub open: bool,
    pub picked: Option<Picked>,
    /// The commit last clicked without shift: a shift-click picks from here.
    pub anchor: Option<usize>,
    /// The diff of the picked commits; `Idle` while the whole change shows.
    pub range: Fetch<RangeView>,
    /// Where the Commits control was drawn, so a press on it does not count as "outside" the popover.
    pub trigger: Option<Rect>,
}

impl Commits {
    pub(crate) fn new() -> Self {
        Commits { list: Fetch::Idle, open: false, picked: None, anchor: None, range: Fetch::Idle, trigger: None }
    }

    pub(crate) fn shown(&self) -> bool {
        !matches!(self.range, Fetch::Idle)
    }

    /// The list when it has been read.
    pub(crate) fn ready(&self) -> Option<&CommitList> {
        match &self.list {
            Fetch::Ready(list) => Some(list),
            _ => None,
        }
    }

    /// The words of the Commits control: how many of the commits are being read.
    pub(crate) fn summary(&self) -> String {
        match (self.ready(), self.picked) {
            (Some(list), Some(p)) => format!("{} of {} commits", p.count(), list.commits.len()),
            _ => "Commits".to_string(),
        }
    }

    /// The popover: all changes, since the user's review, then one box per commit, newest first.
    pub(crate) fn popover(&self, ctx: &Context, at: Pos2, totals: (u32, u32), acts: &mut Vec<CommitAct>) -> Rect {
        let p = theme::pal(ctx);
        let area = egui::Area::new(egui::Id::NULL.with("commits-popover")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
            components::card_frame(ui).show(ui, |ui| {
                ui.set_width(440.0);
                ui.horizontal(|ui| {
                    components::label(ui, "Commits to review", 13.5, p.graphite);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        components::note(ui, "Shift-click for a range", p.text_tertiary);
                    });
                });
                ui.add_space(theme::SPACE_2);
                self.body(ui, totals, acts);
                ui.add_space(theme::SPACE_2);
                let steps = format!("{} {}", keymap::label(ui, "review.prev_commit"), keymap::label(ui, "review.next_commit"));
                components::note(ui, format!("Comments still attach to the latest commit. {steps} step one commit."), p.text_tertiary);
            });
        });
        area.response.rect
    }

    fn body(&self, ui: &mut Ui, totals: (u32, u32), acts: &mut Vec<CommitAct>) {
        let p = theme::pal(ui);
        let list = match &self.list {
            Fetch::Idle | Fetch::Loading(_) => {
                components::note(ui, "Reading the commits…", p.text_secondary);
                return;
            }
            Fetch::Failed(error) => {
                components::note(ui, format!("Could not read the commits: {error}"), p.rose_text);
                return;
            }
            Fetch::Ready(list) => list,
        };
        let all = format!("{} commits · +{} −{}", list.commits.len(), totals.0, totals.1);
        if components::range_row(ui, self.picked.is_none(), "All changes", &all).clicked() {
            acts.push(CommitAct::All);
        }
        self.since_row(ui, list, acts);
        components::rule(ui);
        ui.add_space(4.0);
        let now = Utc::now();
        let shift = ui.input(|i| i.modifiers.shift);
        ScrollArea::vertical().id_salt("commit-list").max_height(280.0).show(ui, |ui| {
            for (index, commit) in list.commits.iter().enumerate().rev() {
                let age = commit.at.map(|at| crate::age(now, at)).unwrap_or_default();
                let ticked = self.picked.is_some_and(|picked| picked.contains(index));
                if components::commit_row(
                    ui,
                    ticked,
                    commit.short(),
                    &if commit.merge { format!("{} (merge)", commit.title) } else { commit.title.clone() },
                    &age,
                )
                .clicked()
                {
                    acts.push(CommitAct::Pick { index, shift });
                }
            }
        });
        if !list.complete {
            let note = format!("Only {} commits were read; the forge holds more.", list.commits.len());
            components::note(ui, note, theme::pal(ui).ember_text);
        }
    }

    /// "Since your review", when the user has a review and commits came after it; else why there is none.
    fn since_row(&self, ui: &mut Ui, list: &CommitList, acts: &mut Vec<CommitAct>) {
        let p = theme::pal(ui);
        if let Some(reviewed) = &list.reviewed {
            match pick::since(&list.commits, &reviewed.commit) {
                Some(range) => {
                    let note = format!("{} commits · after {}", range.count(), reviewed.state.you().to_lowercase());
                    if components::range_row(ui, self.picked == Some(range), "Since your review", &note).clicked() {
                        acts.push(CommitAct::Since);
                    }
                }
                None => {
                    let note = "Since your review: no commit came after it, or it was force-pushed away.";
                    components::note(ui, note, p.text_tertiary);
                }
            }
        } else if let Some(error) = &list.reviewed_error {
            components::note(ui, format!("Your last review could not be read: {error}"), p.text_tertiary);
        }
    }

    /// A merge commit in the pick: its changes are against its first parent, so the range also holds the work it merged in.
    fn merge_note(&self, ui: &mut Ui, acts: &mut Vec<CommitAct>) {
        let (Some(list), Some(picked)) = (self.ready(), self.picked) else { return };
        let merges = pick::merges(&list.commits, picked);
        if merges.is_empty() {
            return;
        }
        let named: Vec<&str> = merges.iter().filter_map(|&i| list.commits.get(i)).map(|c| c.short()).collect();
        let text = format!("{} in this pick is a merge, so the diff also holds what it merged in from another branch.", named.join(", "));
        components::note_sized(ui, text, 12.0, theme::pal(ui).ember_text);
        if pick::without_merges(&list.commits, picked).is_some()
            && components::Pill::new(components::PillKind::Secondary, "Leave out merge commits").small().show(ui).clicked()
        {
            acts.push(CommitAct::DropMerges);
        }
    }

    /// The title of the diff of the picked commits.
    pub(crate) fn title(&self, ui: &mut Ui, acts: &mut Vec<CommitAct>) {
        let p = theme::pal(ui);
        components::label(ui, "Picked commits", 15.0, p.graphite);
        if let (Fetch::Ready(range), Some(list), Some(picked)) = (&self.range, self.ready(), self.picked) {
            let (adds, dels) = range.files.iter().fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
            let short = |sha: &str| sha.get(..7).unwrap_or(sha).to_string();
            let line = format!(
                "{} of {} commits · {} → {} · +{adds} −{dels} in {}",
                picked.count(),
                list.commits.len(),
                short(&range.from),
                short(&range.to),
                components::files_word(range.files.len())
            );
            components::note_sized(ui, line, 12.0, p.text_tertiary);
        }
        let note = format!(
            "Read-only. Comments attach to the latest commit, so comment on all changes. {} returns to all changes; {} {} step one commit.",
            keymap::label(ui, "review.back"),
            keymap::label(ui, "review.prev_commit"),
            keymap::label(ui, "review.next_commit"),
        );
        components::note_sized(ui, note, 12.0, p.text_tertiary);
        self.merge_note(ui, acts);
        if let Fetch::Ready(range) = &self.range
            && range.truncated
        {
            components::note_sized(ui, "The forge left out some files because they are too large; they are marked below.", 12.0, p.ember_text);
        }
    }
}
