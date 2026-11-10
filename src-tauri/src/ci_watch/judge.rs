//! What one read of a PR's CI means for a watch: wait, or end with a wake.
//! Pure: the poll reads GitHub and the store, this decides.

use serde::Deserialize;

use crate::fs::{parse_github_pr_checks, GitHubPrCheck};

/// A watch gives up this long after it was registered.
pub(crate) const GIVE_UP_MS: i64 = 6 * 60 * 60_000;

/// One read of a PR: its state, its head and the checks on that head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub state: PrState,
    pub head: String,
    pub checks: Vec<GitHubPrCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrState {
    Open,
    Merged,
    Closed,
}

/// `gh pr view --json state,headRefOid,statusCheckRollup` as a `Snapshot`.
pub(crate) fn parse(json: &str) -> Result<Snapshot, String> {
    #[derive(Deserialize)]
    struct Row {
        #[serde(default)]
        state: String,
    }
    let row: Row = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let state = match row.state.as_str() {
        "OPEN" => PrState::Open,
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        other => return Err(format!("Unknown pull request state {other:?}")),
    };
    let checks = parse_github_pr_checks(json)?;
    Ok(Snapshot {
        state,
        head: checks.head_oid,
        checks: checks.checks,
    })
}

/// The stored part of a watch that judging reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Watch {
    pub id: String,
    pub repo: String,
    pub pr: i64,
    pub sha: String,
    pub required: Vec<String>,
    pub created_wall_ms: i64,
    /// The finished set the last read saw, not yet confirmed by a second.
    pub settled: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Finished,
    Superseded,
    Merged,
    Closed,
    GaveUp,
    /// GitHub has no such PR or repository.
    Failed,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Outcome::Finished => "finished",
            Outcome::Superseded => "superseded",
            Outcome::Merged => "merged",
            Outcome::Closed => "closed",
            Outcome::GaveUp => "gave_up",
            Outcome::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Judged {
    /// Read again later; `settled` is the finished set to confirm, if any.
    Wait {
        settled: Option<String>,
    },
    End {
        outcome: Outcome,
        text: String,
    },
}

/// What read `read` at wall time `now` means for `watch`. A finished set
/// ends the watch only when the next read finds the same set (the debounce:
/// a run that starts late joins the set first). A read that failed keeps
/// what the watch had.
pub(crate) fn judge(watch: &Watch, read: &Result<Snapshot, String>, now: i64) -> Judged {
    let given_up = now - watch.created_wall_ms >= GIVE_UP_MS;
    let snapshot = match read {
        Err(error) if error.contains("Could not resolve to a") => {
            return end(
                watch,
                Outcome::Failed,
                format!("GitHub has no such PR: {error}"),
            );
        }
        Err(error) if given_up => {
            return end(
                watch,
                Outcome::GaveUp,
                format!("Gave up after 6 h. The last read of CI failed: {error}"),
            );
        }
        Err(_) => {
            return Judged::Wait {
                settled: watch.settled.clone(),
            }
        }
        Ok(snapshot) => snapshot,
    };
    let set = latest(&snapshot.checks);
    let on_sha = snapshot.head == watch.sha;
    let closed = match snapshot.state {
        PrState::Open => None,
        PrState::Merged => Some((Outcome::Merged, "The PR was merged.")),
        PrState::Closed => Some((Outcome::Closed, "The PR was closed without a merge.")),
    };
    if let Some((outcome, said)) = closed {
        let body = if on_sha {
            format!("{said}\n{}", report(&set, &watch.required))
        } else {
            format!("{said} Its head is {}.", snapshot.head)
        };
        return end(watch, outcome, body);
    }
    if !on_sha {
        return end(
            watch,
            Outcome::Superseded,
            format!(
                "Superseded by {}: the PR head moved, so CI on {} will not be reported.",
                snapshot.head, watch.sha
            ),
        );
    }
    let finished = !set.is_empty() && set.iter().all(|check| check.state != "pending");
    if finished {
        let seen = fingerprint(&set);
        if watch.settled.as_deref() == Some(seen.as_str()) {
            return end(watch, Outcome::Finished, report(&set, &watch.required));
        }
        if !given_up {
            return Judged::Wait {
                settled: Some(seen),
            };
        }
    }
    if given_up {
        return end(
            watch,
            Outcome::GaveUp,
            format!(
                "Gave up after 6 h: CI had not finished.\n{}",
                report(&set, &watch.required)
            ),
        );
    }
    Judged::Wait { settled: None }
}

fn end(watch: &Watch, outcome: Outcome, body: String) -> Judged {
    let text = format!(
        "<ci_watch id=\"{}\" repo=\"{}\" pr=\"{}\" sha=\"{}\" outcome=\"{}\">\nPR #{} at {}: {}\n</ci_watch>",
        watch.id,
        watch.repo,
        watch.pr,
        watch.sha,
        outcome.as_str(),
        watch.pr,
        watch.sha,
        body
    );
    Judged::End { outcome, text }
}

/// One result per check: a re-run's newer attempt replaces the older one.
/// GitHub can list both attempts of a job; the later start wins, and on a
/// tie the one listed last.
fn latest(checks: &[GitHubPrCheck]) -> Vec<&GitHubPrCheck> {
    let mut kept: Vec<&GitHubPrCheck> = Vec::new();
    for check in checks {
        match kept
            .iter_mut()
            .find(|seen| seen.workflow == check.workflow && seen.name == check.name)
        {
            Some(seen) if check.started_at >= seen.started_at => *seen = check,
            Some(_) => {}
            None => kept.push(check),
        }
    }
    kept
}

/// The set as one string, to tell whether two reads saw the same finished set.
fn fingerprint(set: &[&GitHubPrCheck]) -> String {
    let mut lines: Vec<String> = set
        .iter()
        .map(|check| {
            format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                check.workflow,
                check.name,
                check.state,
                check.url.as_deref().unwrap_or_default()
            )
        })
        .collect();
    lines.sort();
    lines.join("\n")
}

/// The run id in a check's Actions URL (`.../actions/runs/<id>/job/<id>`).
fn run_id(check: &GitHubPrCheck) -> Option<&str> {
    let url = check.url.as_deref()?;
    let (_, rest) = url.split_once("/actions/runs/")?;
    let id = rest.split('/').next()?;
    (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then_some(id)
}

fn word(state: &str) -> &str {
    match state {
        "pass" => "success",
        "fail" => "failure",
        "cancel" => "cancelled",
        "skipping" => "skipped",
        other => other,
    }
}

fn named(check: &GitHubPrCheck) -> String {
    let label = if check.workflow.is_empty() {
        check.name.clone()
    } else {
        format!("{} / {}", check.workflow, check.name)
    };
    match run_id(check) {
        Some(run) => format!("{label} (run {run})"),
        None => label,
    }
}

/// A required check by its name; a job named `ci (fast)` counts as `ci`.
fn required_check<'a>(set: &[&'a GitHubPrCheck], name: &str) -> Option<&'a GitHubPrCheck> {
    let variant = format!("{name} (");
    set.iter()
        .find(|check| check.name == name)
        .or_else(|| set.iter().find(|check| check.name.starts_with(&variant)))
        .copied()
}

/// The wake's body: each required check's conclusion, then every failed,
/// cancelled, skipped and unfinished job by name and run id. Cancelled is
/// listed apart from failed.
fn report(set: &[&GitHubPrCheck], required: &[String]) -> String {
    let count = |state: &str| set.iter().filter(|check| check.state == state).count();
    let mut lines = vec![format!(
        "{} checks: {} passed, {} failed, {} cancelled, {} skipped, {} not finished.",
        set.len(),
        count("pass"),
        count("fail"),
        count("cancel"),
        count("skipping"),
        count("pending")
    )];
    lines.push("Required:".into());
    for name in required {
        let found = required_check(set, name);
        let line = match found {
            Some(check) if check.name == *name => format!("  {name}: {}", word(&check.state)),
            Some(check) => format!("  {name}: {} (as {})", word(&check.state), check.name),
            None => format!("  {name}: not reported"),
        };
        lines.push(match found.and_then(run_id) {
            Some(run) => format!("{line}, run {run}"),
            None => line,
        });
    }
    for (state, title) in [
        ("fail", "Failed"),
        ("cancel", "Cancelled"),
        ("skipping", "Skipped"),
        ("pending", "Not finished"),
        ("unknown", "Unknown"),
    ] {
        let jobs: Vec<String> = set
            .iter()
            .filter(|check| check.state == state)
            .map(|check| format!("  {}", named(check)))
            .collect();
        if !jobs.is_empty() {
            lines.push(format!("{title}:"));
            lines.extend(jobs);
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0f4432e0d46670e4afed982a0d2a8d0ca13dbe34";
    const NEW_SHA: &str = "1111111111111111111111111111111111111111";

    fn check(name: &str, state: &str, run: u32, started: &str) -> GitHubPrCheck {
        GitHubPrCheck {
            name: name.into(),
            workflow: "CI".into(),
            state: state.into(),
            url: Some(format!(
                "https://github.com/acme/web/actions/runs/{run}/job/{}",
                run + 1
            )),
            started_at: Some(started.into()),
            completed_at: None,
        }
    }

    fn watch(settled: Option<String>) -> Watch {
        Watch {
            id: "cw_t".into(),
            repo: "acme/web".into(),
            pr: 7,
            sha: SHA.into(),
            required: vec!["gate/lint".into(), "ci".into(), "review/agent".into()],
            created_wall_ms: 0,
            settled,
        }
    }

    fn open(checks: Vec<GitHubPrCheck>) -> Result<Snapshot, String> {
        Ok(Snapshot {
            state: PrState::Open,
            head: SHA.into(),
            checks,
        })
    }

    /// Two reads a minute apart, as the poll makes them.
    fn twice(read: &Result<Snapshot, String>) -> Judged {
        let Judged::Wait { settled } = judge(&watch(None), read, 0) else {
            panic!("the first read of a finished set ended the watch");
        };
        assert!(settled.is_some(), "a finished set is not held to confirm");
        judge(&watch(settled), read, 60_000)
    }

    fn ended(judged: Judged) -> (Outcome, String) {
        match judged {
            Judged::End { outcome, text } => (outcome, text),
            Judged::Wait { .. } => panic!("the watch did not end"),
        }
    }

    #[test]
    fn parse_reads_state_head_and_checks() {
        let snapshot = parse(
            r#"{"state":"MERGED","headRefOid":"abc","statusCheckRollup":[
                {"__typename":"CheckRun","name":"ci","workflowName":"CI","status":"COMPLETED","conclusion":"CANCELLED"}]}"#,
        )
        .unwrap();
        assert_eq!(snapshot.state, PrState::Merged);
        assert_eq!(snapshot.head, "abc");
        assert_eq!(snapshot.checks[0].state, "cancel");
        assert_eq!(
            parse(r#"{"state":"CLOSED","headRefOid":"abc"}"#)
                .unwrap()
                .state,
            PrState::Closed
        );
        assert_eq!(
            parse(r#"{"state":"OPEN","headRefOid":"abc"}"#)
                .unwrap()
                .state,
            PrState::Open
        );
        assert!(parse(r#"{"state":"DRAFT","headRefOid":"abc"}"#).is_err());
    }

    #[test]
    fn a_finished_set_wakes_on_the_second_same_read() {
        let read = open(vec![
            check("gate/lint", "pass", 10, "2026-11-10T01:00:00Z"),
            check("ci", "fail", 20, "2026-11-10T01:00:00Z"),
        ]);
        let (outcome, text) = ended(twice(&read));
        assert_eq!(outcome, Outcome::Finished);
        assert_eq!(
            text,
            format!(
                "<ci_watch id=\"cw_t\" repo=\"acme/web\" pr=\"7\" sha=\"{SHA}\" outcome=\"finished\">\n\
                 PR #7 at {SHA}: 2 checks: 1 passed, 1 failed, 0 cancelled, 0 skipped, 0 not finished.\n\
                 Required:\n  gate/lint: success, run 10\n  ci: failure, run 20\n  review/agent: not reported\n\
                 Failed:\n  CI / ci (run 20)\n</ci_watch>"
            )
        );
    }

    #[test]
    fn a_set_that_changes_between_reads_waits_again() {
        let first = open(vec![check("ci", "pass", 20, "a")]);
        let Judged::Wait { settled } = judge(&watch(None), &first, 0) else {
            panic!("ended at once");
        };
        let grown = open(vec![
            check("ci", "pass", 20, "a"),
            check("e2e-linux", "pass", 30, "a"),
        ]);
        let Judged::Wait { settled: next } = judge(&watch(settled.clone()), &grown, 60_000) else {
            panic!("a set that grew between reads woke");
        };
        assert_ne!(next, settled);
        assert!(matches!(
            judge(&watch(next), &grown, 120_000),
            Judged::End {
                outcome: Outcome::Finished,
                ..
            }
        ));
    }

    #[test]
    fn a_running_job_holds_the_wake() {
        let read = open(vec![
            check("ci", "pass", 20, "a"),
            check("e2e-linux", "pending", 30, "a"),
        ]);
        let settled = Some(fingerprint(&latest(&open_checks(&read))));
        assert_eq!(
            judge(&watch(settled), &read, 60_000),
            Judged::Wait { settled: None }
        );
        assert_eq!(
            judge(&watch(None), &open(vec![]), 0),
            Judged::Wait { settled: None },
            "no checks yet is not a finished set"
        );
    }

    fn open_checks(read: &Result<Snapshot, String>) -> Vec<GitHubPrCheck> {
        read.as_ref().unwrap().checks.clone()
    }

    #[test]
    fn cancelled_and_skipped_are_listed_apart_from_failed() {
        let read = open(vec![
            check("ci", "pass", 20, "a"),
            check("stop (gate/lint)", "cancel", 40, "a"),
            check("e2e-macos-1", "skipping", 50, "a"),
        ]);
        let (_, text) = ended(twice(&read));
        assert!(
            text.contains("1 passed, 0 failed, 1 cancelled, 1 skipped"),
            "{text}"
        );
        assert!(!text.contains("Failed:"), "{text}");
        assert!(
            text.contains("Cancelled:\n  CI / stop (gate/lint) (run 40)"),
            "{text}"
        );
        assert!(
            text.contains("Skipped:\n  CI / e2e-macos-1 (run 50)"),
            "{text}"
        );
    }

    #[test]
    fn a_rerun_replaces_the_old_attempt() {
        let read = open(vec![
            check("ci", "fail", 20, "2026-11-10T01:00:00Z"),
            check("ci", "pass", 21, "2026-11-10T02:00:00Z"),
            check("gate/lint", "pass", 10, "2026-11-10T02:00:00Z"),
            check("gate/lint", "fail", 9, "2026-11-10T01:00:00Z"),
        ]);
        let (_, text) = ended(twice(&read));
        assert!(text.contains("2 checks: 2 passed, 0 failed"), "{text}");
        assert!(text.contains("  ci: success, run 21"), "{text}");
        assert!(text.contains("  gate/lint: success, run 10"), "{text}");
        let running = open(vec![
            check("ci", "pass", 20, "2026-11-10T01:00:00Z"),
            check("ci", "pending", 21, "2026-11-10T02:00:00Z"),
        ]);
        assert_eq!(
            judge(&watch(None), &running, 0),
            Judged::Wait { settled: None },
            "a re-run in progress is not finished"
        );
    }

    #[test]
    fn a_required_check_matches_its_named_variant() {
        let read = open(vec![
            check("ci (fast)", "pass", 20, "a"),
            check("gate/lint", "pass", 10, "a"),
        ]);
        let (_, text) = ended(twice(&read));
        assert!(
            text.contains("  ci: success (as ci (fast)), run 20"),
            "{text}"
        );
    }

    #[test]
    fn a_status_context_has_no_run_id() {
        let mut review = check("review/agent", "pass", 0, "a");
        review.workflow = String::new();
        review.url = Some("https://example.com/review".into());
        let read = open(vec![review, check("ci", "pass", 20, "a")]);
        let (_, text) = ended(twice(&read));
        assert!(text.contains("  review/agent: success\n"), "{text}");
    }

    #[test]
    fn a_moved_head_wakes_superseded() {
        let read = Ok(Snapshot {
            state: PrState::Open,
            head: NEW_SHA.into(),
            checks: vec![check("ci", "pending", 20, "a")],
        });
        let (outcome, text) = ended(judge(&watch(None), &read, 0));
        assert_eq!(outcome, Outcome::Superseded);
        assert!(text.contains(&format!("Superseded by {NEW_SHA}")), "{text}");
    }

    #[test]
    fn a_merged_or_closed_pr_ends_and_says_which() {
        for (state, outcome, said) in [
            (PrState::Merged, Outcome::Merged, "The PR was merged."),
            (
                PrState::Closed,
                Outcome::Closed,
                "The PR was closed without a merge.",
            ),
        ] {
            let read = Ok(Snapshot {
                state,
                head: SHA.into(),
                checks: vec![check("ci", "pass", 20, "a")],
            });
            let (got, text) = ended(judge(&watch(None), &read, 0));
            assert_eq!(got, outcome);
            assert!(text.contains(&format!("{said}\n1 checks")), "{text}");
            assert!(text.contains(&format!("outcome=\"{}\"", outcome.as_str())));
            let moved = Ok(Snapshot {
                state,
                head: NEW_SHA.into(),
                checks: vec![],
            });
            let (got, text) = ended(judge(&watch(None), &moved, 0));
            assert_eq!(got, outcome, "merge or close wins over a moved head");
            assert!(
                text.contains(&format!("{said} Its head is {NEW_SHA}.")),
                "{text}"
            );
        }
    }

    #[test]
    fn six_hours_gives_up() {
        let running = open(vec![check("ci", "pending", 20, "a")]);
        assert_eq!(
            judge(&watch(None), &running, GIVE_UP_MS - 1),
            Judged::Wait { settled: None }
        );
        let (outcome, text) = ended(judge(&watch(None), &running, GIVE_UP_MS));
        assert_eq!(outcome, Outcome::GaveUp);
        assert!(text.contains("CI had not finished"), "{text}");
        assert!(text.contains("Not finished:\n  CI / ci (run 20)"), "{text}");
        let finished_once = open(vec![check("ci", "pass", 20, "a")]);
        assert_eq!(
            text_outcome(judge(&watch(None), &finished_once, GIVE_UP_MS)),
            Outcome::GaveUp,
            "at 6 h an unconfirmed set does not wait another read"
        );
        let failed: Result<Snapshot, String> = Err("HTTP 502".into());
        assert_eq!(
            judge(&watch(Some("x".into())), &failed, GIVE_UP_MS - 1),
            Judged::Wait {
                settled: Some("x".into())
            },
            "a failed read keeps the set it was confirming"
        );
        let (outcome, text) = ended(judge(&watch(None), &failed, GIVE_UP_MS));
        assert_eq!(outcome, Outcome::GaveUp);
        assert!(
            text.contains("The last read of CI failed: HTTP 502"),
            "{text}"
        );
    }

    fn text_outcome(judged: Judged) -> Outcome {
        ended(judged).0
    }

    #[test]
    fn an_unknown_pr_fails_at_once() {
        let read = Err("GraphQL: Could not resolve to a PullRequest with the number of 9.".into());
        let (outcome, text) = ended(judge(&watch(None), &read, 0));
        assert_eq!(outcome, Outcome::Failed);
        assert!(text.contains("GitHub has no such PR"), "{text}");
    }

    #[test]
    fn run_id_reads_only_actions_urls() {
        let mut job = check("ci", "pass", 123, "a");
        assert_eq!(run_id(&job), Some("123"));
        job.url = Some("https://github.com/acme/web/actions/runs/abc/job/1".into());
        assert_eq!(run_id(&job), None);
        job.url = None;
        assert_eq!(run_id(&job), None);
    }
}
