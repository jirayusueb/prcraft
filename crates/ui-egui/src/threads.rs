//! Review threads drawn in the diff (Figma frame 90): the conversation under its line with a reply field and Resolve thread,
//! a resolved thread folded to one line, and an outdated thread with "Show original ›". A reply and a resolve write to the
//! forge at once; they are not part of the review that Submit posts, and the buttons say so.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use craft::egui::{self, Align, Context, Id, Pos2, TextEdit, Ui};
use pr_craft_core::Forge;
use pr_craft_core::thread::ReviewThread;

use crate::components::{self, BannerKind, Pill, PillKind, TagKind};
use crate::{keymap, menu, theme};

/// What a thread card asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThreadAct {
    /// Post the typed reply now.
    Reply(String),
    /// ⌘↵ in this thread's reply field: send it like the button does, asking first when a draft sits on the line.
    Send(String),
    /// Resolve the thread, or reopen it when `true` is the thread's current state.
    Resolve(String),
    /// Open or fold a resolved or outdated thread.
    Toggle(String),
    /// The review holds a draft on this thread's line: ask before posting the reply now.
    AskFirst(String),
    CancelConfirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Resolve,
    Reply,
}

/// What the cards of one review remember between frames.
#[derive(Default)]
pub(crate) struct Cards {
    /// The reply being typed, by thread id.
    pub replies: HashMap<String, String>,
    /// Resolved and outdated threads the reader opened.
    pub open: HashSet<String>,
    /// The thread ⇧R and ⌥R act on: the one stepped to with T.
    pub current: Option<String>,
    /// The control that takes focus on this frame only.
    pub focus: Option<(String, Focus)>,
    /// The thread whose reply waits on the reader's go-ahead.
    pub confirm: Option<String>,
    /// The thread a write is on its way for; its buttons wait.
    pub busy: Option<String>,
    /// The thread whose reply field had focus when last drawn; ⌘↵ there sends the reply, not the review.
    pub typing: Option<String>,
}

/// `invoicer.go:44`, or the file alone when the thread has no line in this diff.
pub(crate) fn place(thread: &ReviewThread) -> String {
    let name = thread.path.rsplit('/').next().unwrap_or(&thread.path);
    match (thread.line, thread.original.line) {
        (Some(line), _) => format!("{name}:{}", line.shown()),
        (None, Some(original)) => format!("{name}:{original}"),
        (None, None) => name.to_string(),
    }
}

/// The words on a folded strip: who wrote first, and the start of what they said.
fn strip_text(thread: &ReviewThread) -> String {
    let first = thread.comments.first().map_or("Someone", |c| c.author.as_str());
    let said = thread.first_line();
    if thread.outdated {
        let at = match (thread.original.line, thread.original.commit.as_deref()) {
            (Some(line), Some(commit)) => format!(" on line {line} of {}", commit.get(..7).unwrap_or(commit)),
            (Some(line), None) => format!(" on line {line}"),
            _ => String::new(),
        };
        format!("{first}{at}: “{said}”")
    } else {
        let by = thread.resolved_by.as_deref().map(|who| format!(" by {who}")).unwrap_or_default();
        format!("{first}: “{said}”  ·  resolved{by}")
    }
}

/// One thread, folded or open. `overlap` says the review holds a draft on this thread's line.
pub(crate) fn thread_ui(ui: &mut Ui, width: f32, forge: Forge, thread: &ReviewThread, cards: &mut Cards, overlap: bool, acts: &mut Vec<ThreadAct>) {
    let open = cards.open.contains(&thread.id) || (cards.current.as_deref() == Some(thread.id.as_str()) && cards.focus.is_some());
    // Right-click, or the context-menu key while this is the thread T stepped to (and no field has the keys).
    let by_key = cards.current.as_deref() == Some(thread.id.as_str()) && !ui.ctx().text_edit_focused() && keymap::pressed(ui, "app.context_menu");
    if (thread.resolved || thread.outdated) && !open {
        let (chip, action) = if thread.outdated { ("Outdated", "Show original ›") } else { ("Resolved", "Show thread ›") };
        let strip = components::strip_row(ui, width, chip, &strip_text(thread), action);
        if strip.clicked() {
            acts.push(ThreadAct::Toggle(thread.id.clone()));
        }
        if strip.secondary_clicked() || by_key {
            let at = if by_key { menu::below(strip.rect) } else { pointer_or(ui, strip.rect) };
            open_menu(ui.ctx(), forge, thread, at, Some(strip.id));
        }
        return;
    }
    let p = theme::pal(ui);
    let band = components::card_band(ui, width, p.border_strong, |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            head(ui, thread, cards, acts);
            if thread.outdated {
                original(ui, thread);
            }
            let now = Utc::now();
            for comment in &thread.comments {
                let when = comment.at.map(|at| crate::age(now, at)).unwrap_or_default();
                components::comment_row(ui, &comment.author, &when, &comment.body);
            }
            if thread.comments_total as usize > thread.comments.len() {
                let more = thread.comments_total as usize - thread.comments.len();
                components::note(ui, format!("{more} more comments are on the forge."), p.text_tertiary);
            }
            footer(ui, thread, cards, overlap, acts);
        });
    });
    // The card holds buttons and fields of its own, so it only listens for the right button (no widget over them).
    if (ui.input(|i| i.pointer.secondary_clicked()) && ui.rect_contains_pointer(band)) || by_key {
        let at = if by_key { menu::below(band) } else { pointer_or(ui, band) };
        open_menu(ui.ctx(), forge, thread, at, None);
    }
}

fn pointer_or(ui: &Ui, rect: egui::Rect) -> Pos2 {
    ui.input(|i| i.pointer.interact_pos()).unwrap_or_else(|| rect.center())
}

/// The menu of a thread card (frame 111): every item is a key the thread screen has already. Copy link is not here: the forge's
/// answer for a thread carries no address of its own, so there is nothing to copy.
fn open_menu(ctx: &Context, forge: Forge, thread: &ReviewThread, at: Pos2, restore: Option<Id>) {
    let key = |id: &str| keymap::label(ctx, id);
    let mut reply = menu::Item::new("reply", "Reply").key(key("review.reply_thread"));
    if !thread.can_reply {
        reply = reply.unavailable("The forge does not let you reply here");
    }
    let toggle = if thread.resolved { "Reopen thread" } else { "Resolve thread" };
    let mut resolve = menu::Item::new("resolve", toggle).key(key("review.resolve_thread"));
    if !thread.can_resolve {
        resolve = resolve.unavailable("The forge does not let you resolve this thread");
    }
    use menu::Entry::{Item, Rule};
    menu::open(
        ctx,
        menu::Spec {
            owner: "thread",
            target: thread.id.clone(),
            header: Some(format!("Thread on {}", place(thread))),
            at,
            restore,
            entries: vec![
                Item(reply),
                Item(resolve),
                Rule,
                Item(menu::Item::new("next", "Next thread").key(key("review.next_thread"))),
                Item(menu::Item::new("prev", "Previous thread").key(key("review.prev_thread"))),
                Rule,
                Item(menu::Item::new("markdown", "Copy review as Markdown")),
                Item(menu::Item::new("forge", format!("Open on {}", forge.name()))),
            ],
        },
    );
}

/// The top line of an open thread: where it is, how long, and its state.
fn head(ui: &mut Ui, thread: &ReviewThread, cards: &Cards, acts: &mut Vec<ThreadAct>) {
    let p = theme::pal(ui);
    ui.horizontal(|ui| {
        components::mono_label(ui, format!("{} · {}", place(thread), thread.comment_count()), 11.5, p.text_secondary);
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            let (kind, word) = match (thread.resolved, thread.outdated) {
                (true, _) => (TagKind::Neutral, "Resolved"),
                (false, true) => (TagKind::Neutral, "Outdated"),
                (false, false) => (TagKind::OnTheWay, "Open"),
            };
            components::tag(ui, kind, word);
            let folds = (thread.resolved || thread.outdated) && cards.open.contains(&thread.id);
            if folds && Pill::new(PillKind::Quiet, "Fold").small().show(ui).clicked() {
                acts.push(ThreadAct::Toggle(thread.id.clone()));
            }
        });
    });
}

/// What an outdated thread was written on: its commit and line, and the code the forge kept with it.
fn original(ui: &mut Ui, thread: &ReviewThread) {
    let p = theme::pal(ui);
    let from = match (thread.original.line, thread.original.commit.as_deref()) {
        (Some(line), Some(commit)) => format!("Written on line {line} of {}. The lines have changed since.", commit.get(..7).unwrap_or(commit)),
        _ => "Written on lines that have changed since.".to_string(),
    };
    components::note(ui, from, p.text_tertiary);
    if !thread.original.code.is_empty() {
        egui::Frame::new().fill(p.sunken).corner_radius(6).inner_margin(8).show(ui, |ui| {
            for line in &thread.original.code {
                components::mono_label(ui, line, 11.5, p.graphite);
            }
        });
    }
}

/// The reply field, Reply now, and Resolve thread.
fn footer(ui: &mut Ui, thread: &ReviewThread, cards: &mut Cards, overlap: bool, acts: &mut Vec<ThreadAct>) {
    let p = theme::pal(ui);
    let busy = cards.busy.as_deref() == Some(thread.id.as_str());
    let confirming = cards.confirm.as_deref() == Some(thread.id.as_str());
    let focus = cards.focus.as_ref().filter(|(id, _)| *id == thread.id).map(|(_, what)| *what);
    let text = cards.replies.entry(thread.id.clone()).or_default();
    let sendable = !text.trim().is_empty() && !busy && thread.can_reply;
    if thread.can_reply {
        let field = ui.add(TextEdit::multiline(text).desired_rows(1).desired_width(f32::INFINITY).hint_text("Reply…"));
        components::name_field(ui, &field, &format!("Reply to {}", place(thread)));
        if field.has_focus() {
            cards.typing = Some(thread.id.clone());
        } else if cards.typing.as_deref() == Some(thread.id.as_str()) {
            cards.typing = None;
        }
        if focus == Some(Focus::Reply) {
            field.request_focus();
            field.scroll_to_me(Some(Align::Center));
        }
    } else {
        components::note(ui, "You cannot reply to this thread.", p.text_tertiary);
    }
    if confirming {
        let note = "You have a draft on this line. It posts with your review. This reply posts now, on its own, and cannot wait for Submit.";
        components::banner(ui, BannerKind::Warn, "Reply now?", note);
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        if thread.can_reply {
            let label = if busy { "Posting…" } else { "Reply now" };
            if Pill::new(PillKind::Primary, label).small().enabled(sendable).show(ui).clicked() {
                acts.push(if overlap && !confirming { ThreadAct::AskFirst(thread.id.clone()) } else { ThreadAct::Reply(thread.id.clone()) });
            }
            if confirming && Pill::new(PillKind::Quiet, "Cancel").small().show(ui).clicked() {
                acts.push(ThreadAct::CancelConfirm);
            }
        }
        let label = if thread.resolved { "Reopen thread" } else { "Resolve thread" };
        let button = Pill::new(PillKind::Secondary, label).small().key(ui, "review.resolve_thread").enabled(thread.can_resolve && !busy).show(ui);
        if focus == Some(Focus::Resolve) {
            button.request_focus();
            button.scroll_to_me(Some(Align::Center));
        }
        if button.clicked() {
            acts.push(ThreadAct::Resolve(thread.id.clone()));
        }
    });
    components::note(ui, "Reply now and Resolve thread post to the forge at once. They do not wait for Submit review.", p.text_tertiary);
}
