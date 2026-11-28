//! GitHub GraphQL quota safety: a member's polling spends at most [`HOURLY_BUDGET`] points an hour and never runs the bucket dry
//! (`quota.status`). Each poll's GraphQL calls report their `rateLimit` here; the UI stretches its interval and pauses a host from it.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use pr_craft_core::{DateTime, Forge, Local, TimeDelta, Utc};
use pr_craft_forge::{FailKind, ForgeError, RateLimit};
use serde::Serialize;

/// The points an hour one member's polling may spend (a quarter of GitHub's 5,000, shared with every other tool).
pub const HOURLY_BUDGET: u32 = 1250;

/// Polls whose cost is remembered; the largest one sets the pace, so one cheap poll does not hide an expensive one.
const KEPT: usize = 3;

#[derive(Default)]
struct Budget {
    /// Points spent by the poll in progress.
    open: u32,
    /// Cost of the last [`KEPT`] finished polls, newest last.
    polls: VecDeque<u32>,
    latest: Option<(u32, DateTime<Utc>)>,
}

static BUDGETS: Mutex<Option<HashMap<String, Budget>>> = Mutex::new(None);

fn with<T>(act: impl FnOnce(&mut HashMap<String, Budget>) -> T) -> T {
    act(BUDGETS.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(HashMap::new))
}

/// How long a rate-limited GitLab host is left alone when the server names no `Retry-After`.
const DEFAULT_RETRY_SECS: i64 = 60;
const MOST_RETRY_SECS: i64 = 3600;

static PAUSES: Mutex<BTreeMap<String, DateTime<Utc>>> = Mutex::new(BTreeMap::new());

/// Notes that `host` answered `error`. A GitLab host that rate-limited is paused until its `Retry-After` (seconds), else 60 s.
pub fn note_failure(forge: Forge, host: &str, error: &ForgeError, now: DateTime<Utc>) {
    if forge == Forge::GitLab && error.kind == FailKind::RateLimit {
        // A server names the wait; an hour is the most this build holds a host for.
        let wait = error.retry_after().map_or(DEFAULT_RETRY_SECS, |secs| i64::try_from(secs).unwrap_or(i64::MAX)).min(MOST_RETRY_SECS);
        let until = now + TimeDelta::seconds(wait);
        PAUSES.lock().unwrap_or_else(PoisonError::into_inner).insert(host.to_string(), until);
    }
}

/// When `host`'s rate-limit pause ends, while it still holds at `now`.
pub fn paused_until(host: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    PAUSES.lock().unwrap_or_else(PoisonError::into_inner).get(host).copied().filter(|until| now < *until)
}

/// The error a read of a paused host reports instead of asking it.
pub fn paused_error(host: &str, until: DateTime<Utc>) -> ForgeError {
    let at = until.with_timezone(&Local).format("%H:%M");
    ForgeError::new(FailKind::RateLimit, format!("{host} is paused until {at} after a rate limit"))
}

/// Runs one host's `read` unless the host is paused, and pauses it when the read ends in a rate limit.
pub fn guarded<T>(forge: Forge, host: &str, read: impl FnOnce() -> Result<T, ForgeError>) -> Result<T, ForgeError> {
    if let Some(until) = paused_until(host, Utc::now()) {
        return Err(paused_error(host, until));
    }
    read().inspect_err(|error| note_failure(forge, host, error, Utc::now()))
}

/// Adds one GraphQL call of the poll in progress on `host`.
pub fn record(host: &str, rate: RateLimit) {
    with(|all| {
        let budget = all.entry(host.to_string()).or_default();
        budget.open += rate.cost;
        budget.latest = Some((rate.remaining, rate.reset_at));
    });
}

/// Closes the poll in progress: what it spent becomes the newest of the last [`KEPT`]. Hosts that spent nothing are left alone.
pub fn end_poll() {
    with(|all| {
        for budget in all.values_mut().filter(|b| b.open > 0) {
            if budget.polls.len() == KEPT {
                budget.polls.pop_front();
            }
            budget.polls.push_back(std::mem::take(&mut budget.open));
        }
    });
}

/// The smallest interval, at least `chosen`, at which `cost` points a poll stay within [`HOURLY_BUDGET`].
fn stretched(cost: u32, chosen: u32) -> u32 {
    chosen.max((u64::from(cost) * 60).div_ceil(u64::from(HOURLY_BUDGET)) as u32)
}

/// What the guard does on one GitHub host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostQuota {
    pub host: String,
    /// Points the newest finished poll spent.
    pub last_cost: u32,
    /// Points an hour at `effective_minutes`, from the dearest of the last three polls.
    pub projected_per_hour: u32,
    pub remaining: Option<u32>,
    pub reset_at: Option<DateTime<Utc>>,
    pub effective_minutes: u32,
    /// Set while fewer than [`HOURLY_BUDGET`] points remain before `reset_at`.
    pub paused_until: Option<DateTime<Utc>>,
}

fn host_quota(host: &str, budget: &Budget, chosen: u32, now: DateTime<Utc>) -> HostQuota {
    let dearest = budget.polls.iter().copied().max().unwrap_or(0);
    let effective = stretched(dearest, chosen);
    let paused_until = budget.latest.filter(|(left, reset)| *left < HOURLY_BUDGET && now < *reset).map(|(_, reset)| reset);
    HostQuota {
        host: host.to_string(),
        last_cost: budget.polls.back().copied().unwrap_or(0),
        projected_per_hour: (u64::from(dearest) * 60 / u64::from(effective)) as u32,
        remaining: budget.latest.map(|(left, _)| left),
        reset_at: budget.latest.map(|(_, reset)| reset),
        effective_minutes: effective,
        paused_until,
    }
}

/// `quota.status`: every GitHub host that has polled, by name, with the interval it needs when the reader chose `chosen_minutes`.
pub fn status(chosen_minutes: u32, now: DateTime<Utc>) -> Vec<HostQuota> {
    let mut hosts: Vec<_> = with(|all| all.iter().map(|(host, budget)| host_quota(host, budget, chosen_minutes, now)).collect());
    hosts.sort_by(|a, b| a.host.cmp(&b.host));
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u32) -> DateTime<Utc> {
        format!("2026-11-28T{hour:02}:00:00Z").parse().expect("a time")
    }

    fn budget(polls: &[u32], remaining: u32, reset: DateTime<Utc>) -> Budget {
        Budget { open: 0, polls: polls.iter().copied().collect(), latest: Some((remaining, reset)) }
    }

    #[test]
    fn an_expensive_poll_stretches_the_interval_and_a_cheap_one_keeps_it() {
        assert_eq!(stretched(57, 1), 3, "57 x 60 / 3 = 1140 points an hour");
        assert_eq!(stretched(10, 5), 5);
        assert_eq!(stretched(0, 1), 1);
    }

    #[test]
    fn the_dearest_of_the_last_three_polls_sets_the_pace() {
        let status = host_quota("github.com", &budget(&[200, 10, 10], 5000, at(10)), 1, at(9));
        assert_eq!((status.last_cost, status.effective_minutes, status.projected_per_hour), (10, 10, 1200));
    }

    #[test]
    fn a_host_is_paused_while_under_the_budget_before_its_reset_and_not_after() {
        let low = budget(&[10], 1249, at(10));
        assert_eq!(host_quota("h", &low, 5, at(9)).paused_until, Some(at(10)));
        assert_eq!(host_quota("h", &low, 5, at(10)).paused_until, None);
        assert_eq!(host_quota("h", &budget(&[10], 1250, at(10)), 5, at(9)).paused_until, None);
    }

    #[test]
    fn polls_are_recorded_summed_and_closed_into_the_last_three() {
        let rate = |cost| RateLimit { cost, remaining: 4000, reset_at: at(10) };
        for cost in [20, 30] {
            record("quota-test.example", rate(cost));
        }
        end_poll();
        for cost in [1, 2, 3] {
            record("quota-test.example", rate(cost));
            end_poll();
        }
        let ours = status(1, at(9)).into_iter().find(|h| h.host == "quota-test.example").expect("recorded");
        assert_eq!((ours.last_cost, ours.effective_minutes, ours.remaining), (3, 1, Some(4000)), "50 fell out of the last three");
    }

    #[test]
    fn a_gitlab_429_pauses_its_host_until_retry_after_and_not_after() {
        let limited = |text: &str| ForgeError::new(FailKind::RateLimit, text);
        let now = at(9);
        note_failure(Forge::GitLab, "pause-a.example", &limited("glab: 429 Too Many Requests\nRetry-After: 90"), now);
        note_failure(Forge::GitLab, "pause-b.example", &limited("glab: 429 Too Many Requests"), now);
        note_failure(Forge::GitHub, "pause-c.example", &limited("gh: 429 Too Many Requests"), now);
        note_failure(Forge::GitLab, "pause-d.example", &ForgeError::new(FailKind::Network, "no such host"), now);
        let (ninety, sixty) = (now + TimeDelta::seconds(90), now + TimeDelta::seconds(60));
        assert_eq!(paused_until("pause-a.example", now), Some(ninety));
        assert_eq!(paused_until("pause-a.example", ninety - TimeDelta::seconds(1)), Some(ninety));
        assert_eq!(paused_until("pause-a.example", ninety), None, "paused until Retry-After, not after");
        assert_eq!(paused_until("pause-b.example", now), Some(sixty), "no Retry-After: 60 s");
        assert_eq!(paused_until("pause-c.example", now), None, "GitHub has its own guard");
        assert_eq!(paused_until("pause-d.example", now), None, "only a rate limit pauses");
    }
}
