//! `review.commits` and `review.range`: the commits of a change and the diff of a run of them.

use pr_craft_core::commits::Commit;
use pr_craft_core::diff::{self, FileDiff};
use pr_craft_core::turn::{ReviewState, assess};
use pr_craft_core::{ChangeRef, DateTime, Utc};
use pr_craft_forge::turn as turn_forge;
use pr_craft_forge::{commits as forge, stack};
use serde::Serialize;

use crate::CommandError;

/// The user's latest review of a change.
#[derive(Debug, Clone, Serialize)]
pub struct Reviewed {
    /// The commit the review was written on.
    pub commit: String,
    pub state: ReviewState,
    pub submitted_at: Option<DateTime<Utc>>,
}

/// `review.commits`: the commits of a change, oldest first, and the one the user last reviewed.
#[derive(Debug, Clone, Serialize)]
pub struct CommitList {
    pub change: ChangeRef,
    pub commits: Vec<Commit>,
    /// Every commit was read.
    pub complete: bool,
    /// `None` when the user has not reviewed the change, or `reviewed_error` says why it is unknown.
    pub reviewed: Option<Reviewed>,
    pub reviewed_error: Option<String>,
}

/// The commits of `change` with the user's latest review. A failure reading the review leaves `reviewed` empty and says
/// why in `reviewed_error`; it never fails the list. Blocks on the CLI.
pub fn list(change: &ChangeRef) -> Result<CommitList, CommandError> {
    let (read, reviewed) = std::thread::scope(|scope| {
        let reviewed = scope.spawn(|| reviewed(change));
        (forge::list(change), reviewed.join().unwrap_or_else(|_| Err("the lookup stopped".into())))
    });
    let read = read.map_err(CommandError::Forge)?;
    let (reviewed, reviewed_error) = match reviewed {
        Ok(reviewed) => (reviewed, None),
        Err(error) => (None, Some(error)),
    };
    Ok(CommitList { change: change.clone(), commits: read.commits, complete: read.complete, reviewed, reviewed_error })
}

fn reviewed(change: &ChangeRef) -> Result<Option<Reviewed>, String> {
    let me = turn_forge::my_login(change.forge, change.host_name())?;
    let Some(snapshot) = turn_forge::snapshot(change, &me)? else { return Ok(None) };
    let review = assess(&snapshot).review;
    let commit = review.commit.ok_or_else(|| format!("{} holds no commit your review was written on", change.forge.name()))?;
    Ok(Some(Reviewed { commit, state: review.state, submitted_at: review.submitted_at }))
}

/// `review.range`: the diff of a run of commits.
#[derive(Debug, Clone, Serialize)]
pub struct RangeView {
    pub change: ChangeRef,
    pub from: String,
    pub to: String,
    pub files: Vec<FileDiff>,
    /// Paths in `files` the forge sent without lines because they are too large; they show no hunks.
    pub omitted: Vec<String>,
    pub truncated: bool,
    pub web_url: Option<String>,
}

/// What changed from commit `from` (the parent of the first picked commit) to `to` (the last picked one). Blocks on the CLI.
pub fn range(change: &ChangeRef, from: &str, to: &str) -> Result<RangeView, CommandError> {
    let (from, to) = (from.trim(), to.trim());
    if from.is_empty() || to.is_empty() {
        return Err(CommandError::Invalid("A range needs a commit to start from and a commit to end on.".into()));
    }
    let compare = stack::compare_diff(change, from, to).map_err(CommandError::Forge)?;
    Ok(RangeView {
        change: change.clone(),
        from: from.to_string(),
        to: to.to_string(),
        files: diff::parse_unified(&compare.text),
        truncated: !compare.omitted.is_empty(),
        omitted: compare.omitted,
        web_url: compare.web_url,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{CommandError, run};

    #[test]
    fn the_thread_and_commit_commands_exist_and_take_params_before_touching_a_forge() {
        for id in ["thread.list", "thread.reply", "thread.resolve", "review.commits", "review.range"] {
            assert!(matches!(run(id, &json!({})), Err(CommandError::Params(_))), "{id} answers bad params with Params, not Unknown");
        }
    }

    #[test]
    fn a_range_without_both_ends_is_refused_before_any_forge_call() {
        let change = pr_craft_core::ChangeRef { forge: pr_craft_core::Forge::GitHub, repo: "acme/api".into(), number: 1, host: None };
        assert!(matches!(super::range(&change, "", "abc"), Err(CommandError::Invalid(_))));
        assert!(matches!(super::range(&change, "abc", " "), Err(CommandError::Invalid(_))));
    }
}
