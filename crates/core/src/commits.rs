//! The commits of a change and which of them the reader picked to review. A pick is always one unbroken run, because a
//! diff between two commits has no meaning for commits skipped in the middle. No I/O.

use serde::{Deserialize, Serialize};

use crate::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    /// The first line of the message.
    pub title: String,
    pub author: String,
    pub at: Option<DateTime<Utc>>,
    /// The first parent: what the commit's own changes are against.
    pub parent: Option<String>,
    /// More than one parent. Its changes are against its first parent, so a range holding it also holds what it merged in.
    #[serde(default)]
    pub merge: bool,
}

impl Commit {
    pub fn short(&self) -> &str {
        self.sha.get(..7).unwrap_or(&self.sha)
    }
}

/// A run of commits, as positions in the list (oldest first), both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picked {
    pub first: usize,
    pub last: usize,
}

impl Picked {
    pub fn one(i: usize) -> Self {
        Picked { first: i, last: i }
    }

    pub fn contains(self, i: usize) -> bool {
        (self.first..=self.last).contains(&i)
    }

    pub fn count(self) -> usize {
        self.last - self.first + 1
    }
}

/// The pick after the reader clicked commit `i`. A plain click ticks it, or unticks it; a tick away from the run grows the
/// run to reach it, taking the commits between. With `shift` and an `anchor` (the commit last clicked plainly), it picks
/// everything from the anchor to `i`.
pub fn click(picked: Option<Picked>, i: usize, shift: bool, anchor: Option<usize>) -> Option<Picked> {
    if let (true, Some(a)) = (shift, anchor) {
        return Some(Picked { first: a.min(i), last: a.max(i) });
    }
    let Some(run) = picked else { return Some(Picked::one(i)) };
    if i < run.first {
        Some(Picked { first: i, ..run })
    } else if i > run.last {
        Some(Picked { last: i, ..run })
    } else if run.count() == 1 {
        None
    } else if i == run.first {
        Some(Picked { first: i + 1, ..run })
    } else if i == run.last {
        Some(Picked { last: i - 1, ..run })
    } else {
        Some(Picked::one(i))
    }
}

/// One commit forward or back. From "all changes" it goes to the first commit (forward) or the last (back); from a run, one
/// past its end in that direction. It stops at the ends of the list.
pub fn step(picked: Option<Picked>, count: usize, forward: bool) -> Option<Picked> {
    let top = count.checked_sub(1)?;
    Some(Picked::one(match (picked, forward) {
        (None, true) => 0,
        (None, false) => top,
        (Some(p), true) => (p.last + 1).min(top),
        (Some(p), false) => p.first.saturating_sub(1),
    }))
}

/// The commits after `reviewed` (a full or abbreviated id), or `None` when it is not in the list or is the last one.
pub fn since(commits: &[Commit], reviewed: &str) -> Option<Picked> {
    let at = commits.iter().position(|c| !reviewed.is_empty() && (c.sha.starts_with(reviewed) || reviewed.starts_with(&c.sha)))?;
    (at + 1 < commits.len()).then_some(Picked { first: at + 1, last: commits.len() - 1 })
}

/// What a pick compares: the first commit's parent against the last commit. Fails for a first commit with no parent.
pub fn bounds(commits: &[Commit], picked: Picked) -> Result<(&str, &str), String> {
    let first = commits.get(picked.first).ok_or("that commit is not in the list")?;
    let last = commits.get(picked.last).ok_or("that commit is not in the list")?;
    let parent = first.parent.as_deref().ok_or_else(|| format!("{} has no parent, so there is nothing to compare it against", first.short()))?;
    Ok((parent, last.sha.as_str()))
}

/// The merge commits inside a pick, by position. A range with one compares first parents, so it also holds the merged-in work.
pub fn merges(commits: &[Commit], picked: Picked) -> Vec<usize> {
    (picked.first..=picked.last).filter(|&i| commits.get(i).is_some_and(|c| c.merge)).collect()
}

/// The longest run of `picked` that holds no merge commit; `None` when every commit of it is one. The earliest wins a tie.
pub fn without_merges(commits: &[Commit], picked: Picked) -> Option<Picked> {
    let mut best: Option<Picked> = None;
    let mut start: Option<usize> = None;
    for i in picked.first..=picked.last + 1 {
        let plain = i <= picked.last && commits.get(i).is_some_and(|c| !c.merge);
        match (plain, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                let run = Picked { first: s, last: i - 1 };
                if best.is_none_or(|b| run.count() > b.count()) {
                    best = Some(run);
                }
                start = None;
            }
            _ => {}
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(sha: &str, parent: Option<&str>) -> Commit {
        Commit { sha: sha.into(), title: String::new(), author: String::new(), at: None, parent: parent.map(String::from), merge: false }
    }

    #[test]
    fn a_pick_is_always_one_unbroken_run() {
        let mut picked = click(None, 3, false, None);
        assert_eq!(picked, Some(Picked::one(3)));
        picked = click(picked, 5, false, None);
        assert_eq!(picked, Some(Picked { first: 3, last: 5 }), "a tick away from the run takes the commits between");
        picked = click(picked, 3, false, None);
        assert_eq!(picked, Some(Picked { first: 4, last: 5 }), "unticking an end shortens it");
        picked = click(picked, 5, false, None);
        picked = click(picked, 4, false, None);
        assert_eq!(picked, None, "unticking the only commit clears the pick");
    }

    #[test]
    fn shift_click_picks_everything_from_the_commit_last_clicked() {
        assert_eq!(click(Some(Picked::one(4)), 1, true, Some(4)), Some(Picked { first: 1, last: 4 }));
        assert_eq!(click(None, 2, true, None), Some(Picked::one(2)), "with no anchor it is a plain click");
    }

    #[test]
    fn stepping_goes_one_commit_and_stops_at_the_ends() {
        assert_eq!(step(None, 4, true), Some(Picked::one(0)));
        assert_eq!(step(None, 4, false), Some(Picked::one(3)));
        assert_eq!(step(Some(Picked::one(3)), 4, true), Some(Picked::one(3)));
        assert_eq!(step(Some(Picked::one(0)), 4, false), Some(Picked::one(0)));
        assert_eq!(step(Some(Picked { first: 1, last: 2 }), 4, true), Some(Picked::one(3)));
        assert_eq!(step(None, 0, true), None, "no commits, nothing to step to");
    }

    #[test]
    fn since_a_review_is_the_commits_after_the_reviewed_one() {
        let list = [commit("aaaa111", None), commit("bbbb222", Some("aaaa111")), commit("cccc333", Some("bbbb222"))];
        assert_eq!(since(&list, "bbbb222"), Some(Picked::one(2)));
        assert_eq!(since(&list, "bbbb"), Some(Picked::one(2)), "an abbreviated id still finds it");
        assert_eq!(since(&list, "cccc333"), None, "nothing came after the last commit");
        assert_eq!(since(&list, "gone"), None, "a rewritten history has no such commit");
        assert_eq!(since(&list, ""), None);
    }

    #[test]
    fn a_pick_compares_the_first_commits_parent_with_the_last_commit() {
        let list = [commit("aaaa111", None), commit("bbbb222", Some("aaaa111")), commit("cccc333", Some("bbbb222"))];
        assert_eq!(bounds(&list, Picked { first: 1, last: 2 }), Ok(("aaaa111", "cccc333")));
        assert!(bounds(&list, Picked::one(0)).is_err(), "a first commit without a parent");
        assert!(bounds(&list, Picked::one(9)).is_err());
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    #[test]
    fn a_pick_with_a_merge_is_named_and_can_be_cut_down_to_its_longest_merge_free_run() {
        let mut list: Vec<Commit> = (0..6)
            .map(|i| Commit { sha: format!("c{i}"), title: String::new(), author: String::new(), at: None, parent: None, merge: false })
            .collect();
        list[3].merge = true;
        let all = Picked { first: 0, last: 5 };
        assert_eq!(merges(&list, all), [3]);
        assert_eq!(without_merges(&list, all), Some(Picked { first: 0, last: 2 }), "the earlier of two runs of three");
        assert_eq!(without_merges(&list, Picked { first: 3, last: 3 }), None, "nothing left to compare");
        assert!(merges(&list, Picked { first: 4, last: 5 }).is_empty());
    }
}
