//! Stacked changes: a change whose base branch is another open change's head branch sits on top of it.

use serde::Serialize;

use crate::ChangeRef;

/// One open change as stacking sees it: its branches say what it sits on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StackLayer {
    pub change: ChangeRef,
    pub title: String,
    pub author: String,
    pub head_branch: String,
    pub base_branch: String,
    pub draft: bool,
    pub url: String,
    /// The head branch lives in this repository. A fork's head is a branch of the fork: its name says nothing
    /// about this repository's branches, so it never joins a stack.
    pub head_in_repo: bool,
}

/// Where a change sits in its stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StackPlace {
    /// Number of the lowest layer; the stack's identity within a repository.
    pub bottom: u64,
    /// Number of the highest layer. Two stacks that fork from one layer share `bottom` and differ here.
    pub top: u64,
    /// Position from the bottom, starting at 0.
    pub index: usize,
    pub size: usize,
}

/// The layer `open[i]` sits on: the one open change whose in-repo head branch is its base branch.
/// `None` when there is none, or when two share that head name (the parent is then ambiguous).
fn parent_of(open: &[StackLayer], i: usize) -> Option<usize> {
    let base = &open[i].base_branch;
    let mut parents = open.iter().enumerate().filter(|(_, l)| l.head_in_repo && l.head_branch == *base);
    match (parents.next(), parents.next()) {
        (Some((parent, _)), None) => Some(parent),
        _ => None,
    }
}

/// The stack that holds change `number`, bottom first. `open` is one repository's open changes.
/// Empty when `number` is not among them; one layer when it is not stacked.
pub fn stack_of(open: &[StackLayer], number: u64) -> Vec<StackLayer> {
    let Some(start) = open.iter().position(|l| l.change.number == number) else { return Vec::new() };
    let mut seen = vec![start];
    let mut below = Vec::new();
    let mut cur = start;
    while let Some(next) = parent_of(open, cur).filter(|n| !seen.contains(n)) {
        seen.push(next);
        below.push(next);
        cur = next;
    }
    let mut above = Vec::new();
    cur = start;
    loop {
        let mut children = (0..open.len()).filter(|&i| !seen.contains(&i) && parent_of(open, i) == Some(cur));
        let (Some(next), None) = (children.next(), children.next()) else { break };
        seen.push(next);
        above.push(next);
        cur = next;
    }
    below.reverse();
    below.into_iter().chain([start]).chain(above).map(|i| open[i].clone()).collect()
}

/// Where change `number` sits, or `None` when it is not in a stack of two or more.
pub fn place_of(open: &[StackLayer], number: u64) -> Option<StackPlace> {
    let stack = stack_of(open, number);
    let index = stack.iter().position(|l| l.change.number == number)?;
    let (bottom, top) = (stack.first()?.change.number, stack.last()?.change.number);
    (stack.len() >= 2).then_some(StackPlace { bottom, top, index, size: stack.len() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Forge;

    /// Invented layers shaped like `gh pr list` rows: `(number, head, base)`.
    fn layers(rows: &[(u64, &str, &str)]) -> Vec<StackLayer> {
        rows.iter()
            .map(|&(number, head, base)| StackLayer {
                change: ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number, host: None },
                title: format!("Change {number}"),
                author: "ada".into(),
                head_branch: head.into(),
                base_branch: base.into(),
                draft: false,
                url: format!("https://github.com/acme/api/pull/{number}"),
                head_in_repo: true,
            })
            .collect()
    }

    fn numbers(stack: &[StackLayer]) -> Vec<u64> {
        stack.iter().map(|l| l.change.number).collect()
    }

    #[test]
    fn a_three_layer_chain_is_found_from_any_layer() {
        // Listed out of order on purpose: the walk follows branches, not list order.
        let open = layers(&[(12, "c", "b"), (10, "a", "main"), (11, "b", "a")]);
        for number in [10, 11, 12] {
            assert_eq!(numbers(&stack_of(&open, number)), [10, 11, 12], "from {number}");
        }
    }

    #[test]
    fn unrelated_changes_do_not_join() {
        let open = layers(&[(1, "a", "main"), (2, "b", "main"), (3, "c", "b")]);
        assert_eq!(numbers(&stack_of(&open, 1)), [1]);
        assert_eq!(numbers(&stack_of(&open, 3)), [2, 3]);
        assert!(stack_of(&open, 99).is_empty());
    }

    #[test]
    fn a_fork_stops_the_walk_up() {
        let open = layers(&[(1, "a", "main"), (2, "b", "a"), (3, "c", "a"), (4, "d", "b")]);
        assert_eq!(numbers(&stack_of(&open, 1)), [1]);
        assert_eq!(numbers(&stack_of(&open, 2)), [1, 2, 4]);
        assert_eq!(numbers(&stack_of(&open, 3)), [1, 3]);
    }

    /// Shaped like public-apis/public-apis: one fork PR whose head is called `master`, and in-repo PRs that
    /// target `master`. Marks the fork rows from `fork_numbers`.
    fn with_forks(mut open: Vec<StackLayer>, fork_numbers: &[u64]) -> Vec<StackLayer> {
        for layer in open.iter_mut().filter(|l| fork_numbers.contains(&l.change.number)) {
            layer.head_in_repo = false;
        }
        open
    }

    #[test]
    fn a_fork_head_named_like_a_base_does_not_join_a_stack() {
        let open = with_forks(layers(&[(9525, "master", "master"), (9526, "add-x", "master"), (9527, "add-y", "master")]), &[9525]);
        for number in [9525, 9526, 9527] {
            assert_eq!(numbers(&stack_of(&open, number)), [number], "from {number}");
            assert_eq!(place_of(&open, number), None);
        }
    }

    #[test]
    fn a_fork_change_is_the_top_of_a_stack_never_a_middle() {
        // 2 is a fork PR on top of in-repo `a`; 3 targets 2's head branch name, which belongs to the fork.
        let open = with_forks(layers(&[(1, "a", "main"), (2, "b", "a"), (3, "c", "b")]), &[2]);
        assert_eq!(numbers(&stack_of(&open, 2)), [1, 2]);
        assert_eq!(numbers(&stack_of(&open, 3)), [3]);
    }

    #[test]
    fn two_in_repo_heads_with_one_name_are_ambiguous_below() {
        let open = layers(&[(1, "a", "main"), (2, "a", "other"), (3, "c", "a")]);
        for number in [1, 2, 3] {
            assert_eq!(numbers(&stack_of(&open, number)), [number], "from {number}");
        }
    }

    #[test]
    fn sibling_stacks_share_a_bottom_and_differ_in_top() {
        // uv-like: 2 and 3 both sit on 1, so each sees the other's bottom.
        let open = layers(&[(1, "a", "main"), (2, "b", "a"), (3, "c", "a")]);
        let (two, three) = (place_of(&open, 2), place_of(&open, 3));
        assert_eq!((two.map(|p| p.bottom), three.map(|p| p.bottom)), (Some(1), Some(1)));
        assert_eq!((two.map(|p| p.top), three.map(|p| p.top)), (Some(2), Some(3)));
    }

    #[test]
    fn only_a_stack_of_two_or_more_has_a_place() {
        let open = layers(&[(1, "a", "main"), (2, "b", "a"), (3, "c", "b"), (9, "z", "main")]);
        assert_eq!(place_of(&open, 3), Some(StackPlace { bottom: 1, top: 3, index: 2, size: 3 }));
        assert_eq!(place_of(&open, 1), Some(StackPlace { bottom: 1, top: 3, index: 0, size: 3 }));
        assert_eq!(place_of(&open, 9), None);
        assert_eq!(place_of(&open, 50), None);
    }

    #[test]
    fn a_cycle_terminates() {
        let open = layers(&[(1, "a", "b"), (2, "b", "a")]);
        assert_eq!(numbers(&stack_of(&open, 1)), [2, 1]);
        let own = layers(&[(1, "a", "a")]);
        assert_eq!(numbers(&stack_of(&own, 1)), [1]);
    }
}
