//! Share commands: the model and the SVG of an infographic for a review, a release note or a stack (PRODUCT.md
//! "Infographic"). The PNG comes from `pr-craft-share-render`, which the engine does not link: `share.save` runs in the
//! CLI and the app, `share.copy` only in the app.

use pr_craft_core::share::{Big, Include, Layer, LayerState, ShareKind, ShareModel, ShareSize, Stat, clean, svg};
use pr_craft_core::stack::StackLayer;
use pr_craft_core::{ChangeRef, DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::review::{self, ReviewView};
use crate::{CommandError, stack};

/// Characters kept of a title or the sharer's own line.
const TITLE_CHARS: usize = 300;
const SUMMARY_CHARS: usize = 400;

/// What the sharer chose beyond the template.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Look {
    #[serde(default)]
    pub include: Include,
    /// One line of the sharer's own words, shown under the headline.
    #[serde(default)]
    pub summary: Option<String>,
}

/// `share.render`'s params: the change to read, and the image to make of it.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub kind: ShareKind,
    pub change: ChangeRef,
    pub size: ShareSize,
    #[serde(flatten)]
    pub look: Look,
}

/// `share.render`'s answer.
#[derive(Debug, Clone, Serialize)]
pub struct Rendered {
    pub model: ShareModel,
    pub size: ShareSize,
    pub width: u32,
    pub height: u32,
    pub svg: String,
}

/// What a model is built from: the open review, or a stack's layers.
pub enum Data<'a> {
    Review(&'a ReviewView),
    Stack(&'a ChangeRef, &'a [StackLayer]),
}

/// `share.render`: reads the change on the forge and builds its image. Blocks on the CLI.
pub fn render(request: &Request) -> Result<Rendered, CommandError> {
    let now = Utc::now();
    let model = match request.kind {
        ShareKind::StackProgress => {
            let layers = stack::list(&request.change)?;
            model(request.kind, &Data::Stack(&request.change, &layers), &request.look, now)?
        }
        ShareKind::ReviewSummary | ShareKind::ReleaseNote => model(request.kind, &Data::Review(&review::open(&request.change)?), &request.look, now)?,
    };
    Ok(rendered(model, request.size))
}

/// The SVG of `model` at `size`.
pub fn rendered(model: ShareModel, size: ShareSize) -> Rendered {
    let (width, height) = size.pixels();
    Rendered { svg: svg(&model, size), model, size, width, height }
}

/// The model of `kind` from `data`. Fails when `data` is not what the kind reads, or a stack has fewer than two layers.
pub fn model(kind: ShareKind, data: &Data, look: &Look, now: DateTime<Utc>) -> Result<ShareModel, CommandError> {
    let summary = look.summary.as_deref().map(|s| cap(s, SUMMARY_CHARS)).filter(|s| !s.is_empty());
    match (kind, data) {
        (ShareKind::ReviewSummary, Data::Review(view)) => Ok(review_summary(view, look.include, summary, now)),
        (ShareKind::ReleaseNote, Data::Review(view)) => Ok(release_note(view, look.include, summary, now)),
        (ShareKind::StackProgress, Data::Stack(change, layers)) => stack_progress(change, layers, look.include, summary, now),
        _ => Err(CommandError::Invalid(format!("{} is not made from this data.", kind.word()))),
    }
}

fn cap(text: &str, chars: usize) -> String {
    let text = clean(text);
    if text.chars().count() <= chars { text } else { text.chars().take(chars - 1).collect::<String>() + "…" }
}

fn eyebrow(kind: ShareKind, now: DateTime<Utc>) -> String {
    format!("{} · {}", kind.word(), now.with_timezone(&Local).format("%b %-d"))
}

fn reference(change: &ChangeRef, include: Include) -> String {
    if include.repo_name { change.reference() } else { format!("{}{}", change.forge.number_sigil(), change.number) }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 { one.to_string() } else { many.to_string() }
}

/// Lines added, lines removed and files, from the counts of every file. No path or line text is read.
fn totals(view: &ReviewView) -> (u64, u64, u64) {
    let (adds, dels) = view.files.iter().fold((0, 0), |(a, d), f| (a + u64::from(f.additions), d + u64::from(f.deletions)));
    (adds, dels, view.files.len() as u64)
}

fn lines_stat(adds: u64, dels: u64, files: u64) -> Stat {
    Stat { label: "Diff".into(), value: format!("+{adds}  −{dels}"), note: format!("{files} {}", plural(files, "file", "files")) }
}

fn review_summary(view: &ReviewView, include: Include, summary: Option<String>, now: DateTime<Utc>) -> ShareModel {
    let (adds, dels, files) = totals(view);
    let mut stats = Vec::new();
    if include.diff_stats {
        stats.push(lines_stat(adds, dels, files));
    }
    let drafts = view.local.drafts.len() as u64;
    // The big number is a diff number, so Diff stats off swaps it for the one count that is not.
    let big = if include.diff_stats {
        Big { value: files, caption: plural(files, "file changed", "files changed") }
    } else {
        Big { value: drafts, caption: plural(drafts, "draft comment, not posted", "draft comments, not posted") }
    };
    stats.push(Stat { label: "Draft comments".into(), value: drafts.to_string(), note: "not posted".into() });
    stats.push(Stat { label: "Verdict to post".into(), value: pending_verdict(view), note: "not posted".into() });
    ShareModel {
        kind: ShareKind::ReviewSummary,
        eyebrow: eyebrow(ShareKind::ReviewSummary, now),
        headline: cap(&view.meta.title, TITLE_CHARS),
        reference: reference(&view.change, include),
        summary,
        big,
        stats,
        people: if include.people { vec![cap(&view.meta.author, 80)] } else { Vec::new() },
        layers: Vec::new(),
        left_out: vec![
            "Checks and approvals are not read yet, so the image does not show them.".into(),
            "A verdict already posted on the forge is not read: the verdict shown is only the one waiting on this Mac.".into(),
        ],
    }
}

/// The verdict waiting to post from this Mac. A review with nothing written has none pending: `Comment` is only the
/// default then.
fn pending_verdict(view: &ReviewView) -> String {
    if view.local.is_empty() { "None pending".into() } else { view.local.verdict.word().into() }
}

fn release_note(view: &ReviewView, include: Include, summary: Option<String>, now: DateTime<Utc>) -> ShareModel {
    let (adds, dels, files) = totals(view);
    let stats = if include.diff_stats {
        vec![
            Stat { label: "Lines added".into(), value: format!("+{adds}"), note: String::new() },
            Stat { label: "Lines removed".into(), value: format!("−{dels}"), note: String::new() },
            Stat { label: "Files".into(), value: files.to_string(), note: String::new() },
        ]
    } else {
        Vec::new()
    };
    ShareModel {
        kind: ShareKind::ReleaseNote,
        eyebrow: eyebrow(ShareKind::ReleaseNote, now),
        headline: cap(&view.meta.title, TITLE_CHARS),
        reference: reference(&view.change, include),
        summary,
        big: if include.diff_stats {
            Big { value: adds + dels, caption: plural(adds + dels, "line changed", "lines changed") }
        } else {
            Big { value: 1, caption: "change".into() }
        },
        stats,
        people: if include.people { vec![cap(&view.meta.author, 80)] } else { Vec::new() },
        layers: Vec::new(),
        left_out: vec!["Checks and the merge date are not read yet, so the image does not show them.".into()],
    }
}

fn stack_progress(
    change: &ChangeRef,
    layers: &[StackLayer],
    include: Include,
    summary: Option<String>,
    now: DateTime<Utc>,
) -> Result<ShareModel, CommandError> {
    if layers.len() < 2 {
        return Err(CommandError::Invalid(format!("{} is not in a stack, so there is no stack progress to share.", change.reference())));
    }
    let sigil = change.forge.number_sigil();
    let rows: Vec<Layer> = layers
        .iter()
        .map(|l| Layer {
            title: cap(&l.title, TITLE_CHARS),
            number: format!("{sigil}{}", l.change.number),
            state: if l.draft { LayerState::Draft } else { LayerState::InReview },
            current: l.change.number == change.number,
        })
        .collect();
    let mut people: Vec<String> = Vec::new();
    for layer in layers {
        let name = cap(&layer.author, 80);
        if !people.contains(&name) {
            people.push(name);
        }
    }
    let headline = layers.iter().find(|l| l.change.number == change.number).map_or_else(|| change.reference(), |l| cap(&l.title, TITLE_CHARS));
    // The stack layout has no room for a summary line: say so rather than drop it silently.
    let mut left_out = vec![
        "Merged layers are not shown: the forge lists open changes only.".to_string(),
        "Checks and approvals are not read yet, so the image does not show them.".to_string(),
    ];
    if summary.is_some() {
        left_out.push("The summary line is not shown on Stack progress.".into());
    }
    Ok(ShareModel {
        kind: ShareKind::StackProgress,
        eyebrow: eyebrow(ShareKind::StackProgress, now),
        headline,
        reference: reference(change, include),
        summary: None,
        big: Big { value: layers.len() as u64, caption: "layers in the stack".into() },
        stats: Vec::new(),
        people: if include.people { people } else { Vec::new() },
        layers: rows,
        left_out,
    })
}

#[cfg(test)]
mod tests {
    use pr_craft_core::diff::parse_unified;
    use pr_craft_core::review::{Anchor, Draft, LineRef, LocalReview};
    use pr_craft_core::{DateTime, Forge};
    use pr_craft_forge::review::ChangeMeta;

    use super::*;

    const DIFF: &str = "diff --git a/src/secret_module.rs b/src/secret_module.rs\n--- a/src/secret_module.rs\n+++ b/src/secret_module.rs\n@@ -1,2 +1,3 @@ fn hidden_context()\n let kept = SENTINEL_CONTEXT;\n-let old = SENTINEL_REMOVED;\n+let new = SENTINEL_ADDED;\n+let more = 1;\n";

    fn view() -> ReviewView {
        let change = ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 482, host: None };
        let meta = ChangeMeta {
            title: "Retry webhooks".into(),
            author: "ada".into(),
            url: "https://github.com/acme/api/pull/482".into(),
            head_branch: "retry".into(),
            base_branch: "main".into(),
            head_sha: "abc1234".into(),
            base_sha: None,
            diff_refs: None,
        };
        let draft = Draft {
            id: 1,
            path: "src/secret_module.rs".into(),
            old_path: None,
            start: None,
            end: LineRef { old: None, new: Some(2) },
            body: "SENTINEL_COMMENT".into(),
            on_rendered: false,
        };
        let second = Draft { id: 2, body: "SENTINEL_COMMENT_TWO".into(), ..draft.clone() };
        let local = LocalReview { drafts: vec![draft, second], body: "SENTINEL_BODY".into(), ..LocalReview::default() };
        ReviewView {
            change,
            meta,
            anchor: Anchor { head_sha: "abc1234".into(), base_sha: None, start_sha: None },
            stale: false,
            files: parse_unified(DIFF),
            local,
            filter: Vec::new(),
            layout: Default::default(),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_763_100_000, 0).unwrap_or_default()
    }

    fn layer(number: u64, draft: bool) -> StackLayer {
        StackLayer {
            change: ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number, host: None },
            title: format!("Layer {number}"),
            author: "ada".into(),
            head_branch: format!("l{number}"),
            base_branch: "main".into(),
            draft,
            url: String::new(),
            head_in_repo: true,
        }
    }

    #[test]
    fn no_image_carries_code_comments_or_paths() {
        let view = view();
        let stack = [layer(481, false), layer(482, true)];
        let look = Look { include: Include { repo_name: true, ..Include::default() }, summary: None };
        for (kind, data) in [
            (ShareKind::ReviewSummary, Data::Review(&view)),
            (ShareKind::ReleaseNote, Data::Review(&view)),
            (ShareKind::StackProgress, Data::Stack(&view.change, &stack)),
        ] {
            let Ok(model) = model(kind, &data, &look, now()) else { panic!("{kind:?} has no model") };
            let rendered = rendered(model, ShareSize::Wide1200x630);
            let everything = format!("{}{}", serde_json::to_string(&rendered.model).unwrap_or_default(), rendered.svg);
            for secret in ["SENTINEL_", "secret_module", "hidden_context", "@@"] {
                assert!(!everything.contains(secret), "{kind:?} carries {secret}");
            }
        }
    }

    #[test]
    fn numbers_come_from_the_counts_of_the_diff() {
        let model = review_summary(&view(), Include::default(), None, now());
        assert_eq!(model.big.value, 1);
        assert!(model.stats.iter().any(|s| s.value.contains("+2") && s.value.contains("−1")));
        assert!(model.stats.iter().any(|s| s.label == "Draft comments" && s.value == "2"));
    }

    #[test]
    fn the_repository_name_is_left_out_unless_asked_for() {
        let view = view();
        let hidden = review_summary(&view, Include::default(), None, now());
        let shown = review_summary(&view, Include { repo_name: true, ..Include::default() }, None, now());
        assert!(!hidden.reference.contains("acme"));
        assert!(shown.reference.contains("acme/api"));
    }

    #[test]
    fn a_change_outside_a_stack_has_no_stack_image() {
        let view = view();
        let one = [layer(482, false)];
        assert!(model(ShareKind::StackProgress, &Data::Stack(&view.change, &one), &Look::default(), now()).is_err());
        assert!(model(ShareKind::StackProgress, &Data::Review(&view), &Look::default(), now()).is_err());
    }

    #[test]
    fn a_stack_marks_this_change_and_each_layers_state() {
        let view = view();
        let stack = [layer(481, false), layer(482, true)];
        let Ok(model) = model(ShareKind::StackProgress, &Data::Stack(&view.change, &stack), &Look::default(), now()) else { panic!("no model") };
        assert_eq!(model.layers.iter().map(|l| (l.current, l.state)).collect::<Vec<_>>(), [(false, LayerState::InReview), (true, LayerState::Draft)]);
        assert_eq!(model.headline, "Layer 482");
    }

    fn verdict_stat(model: &ShareModel) -> Option<&Stat> {
        model.stats.iter().find(|s| s.label == "Verdict to post")
    }

    #[test]
    fn a_verdict_is_unposted_and_none_is_pending_while_nothing_is_written() {
        let mut view = view();
        view.local = pr_craft_core::review::LocalReview::default();
        let model = review_summary(&view, Include::default(), None, now());
        assert_eq!(verdict_stat(&model).map(|s| (s.value.as_str(), s.note.as_str())), Some(("None pending", "not posted")));
        assert!(model.left_out.iter().any(|l| l.contains("already posted on the forge")));
        view.local.verdict = pr_craft_core::review::Verdict::Approve;
        let model = review_summary(&view, Include::default(), None, now());
        assert_eq!(verdict_stat(&model).map(|s| (s.value.as_str(), s.note.as_str())), Some(("Approve", "not posted")));
    }

    #[test]
    fn turning_diff_stats_off_leaves_no_diff_number_on_the_image() {
        let view = view();
        let off = Include { diff_stats: false, ..Include::default() };
        let summary = review_summary(&view, off, None, now());
        assert_eq!(summary.big.value, 2, "the two draft comments, not the one file");
        assert_eq!(review_summary(&view, Include::default(), None, now()).big.value, 1, "with Diff stats on it is the one file");
        assert!(summary.stats.iter().all(|s| s.label != "Diff"));
        let note = release_note(&view, off, None, now());
        assert!(note.stats.is_empty());
        assert!(!note.big.caption.contains("line"), "{}", note.big.caption);
    }

    #[test]
    fn a_summary_line_the_stack_image_cannot_hold_is_disclosed_not_dropped() {
        let view = view();
        let stack = [layer(481, false), layer(482, true)];
        let build = |summary: Option<String>| {
            let Ok(model) = stack_progress(&view.change, &stack, Include::default(), summary, now()) else { panic!("no model") };
            model
        };
        let with = build(Some("my words".into()));
        assert!(with.summary.is_none());
        assert!(with.left_out.iter().any(|l| l.contains("summary line")));
        assert!(!build(None).left_out.iter().any(|l| l.contains("summary line")));
        assert!(with.left_out.iter().any(|l| l.contains("Checks and approvals")));
    }
}
