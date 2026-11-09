#!/usr/bin/env python3
"""Rank the tests that fail and then pass, from job data and a few job logs.

This records only. It retries nothing, hides nothing and changes no check.

    python3 scripts/process/flaky_tests.py --store DIR --cache DIR --out report.md
    (--offline reads only what is on disk; otherwise at most --budget GET calls)

--cache directories (ci-metrics format, or raw jobs responses) are only read.
New job data goes to DIR/jobs and logs to DIR/logs in --store.

Evidence for one failed job, strongest first:
  same ref   a later attempt of the same run passed, or a new run passed on the
             same head and base. The merge commit comes from the checkout line
             of both logs when both are read; else GitHub's rule that a rerun
             keeps GITHUB_SHA is the evidence, and the report says "by rule".
  repeat     a scenario repeated inside the job failed in some repeats and
             passed in others.
  new ref    a new run on the same head passed, and the base differs. Main moved
             under it, so main may hold the fix. "ref unread" means no log told.
  later head a later push passed. A code fix looks the same, so it is not flaky
             evidence and is only shown.

Only jobs with same-ref or new-run evidence get a log fetched. Other failed jobs
count only if their log is already on disk; those counts are scaled to all failed
jobs of their kind and are labelled as estimates. A job with more than
MASS_FAILURE failed tests is counted on its own and not per test: something broke
the whole run, and those names would bury the real list.
"""
import argparse
import collections
import glob
import hashlib
import json
import os
import re
import subprocess
import time
from datetime import datetime

REPO = "tylers2222/Monocode-Unlocked"
SKIP_WORKFLOWS = {"gate-integrity", "gate-integrity-rerun", "cancel-closed-pr-runs", "Fork sync"}
MASS_FAILURE = 10
WATCH_ROWS = 20

ROLLUP_STEP = re.compile(r"^Every (job|needed job) ")
TEST_STEP = re.compile(r"^Run (the scenarios|the suites|npm (run )?test|cargo test|npx vitest|node --test|python3 -m unittest)")
SETUP_STEP = re.compile(r"install|set up|setup|download|checkout|cache|\bapt\b|docker|image|toolchain|^Run actions/", re.I)
NETWORK = re.compile(r"curl: \(\d+\)|HTTP error 5\d\d|\b50[234] (Bad Gateway|Service|Gateway)|error: 50[234]|ECONNRESET|ETIMEDOUT"
                     r"|Could not resolve host|TLS handshake|Temporary failure resolving|npm ERR! network", re.I)
LOG_LINE = re.compile(r"^\S+Z\s?")
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
MERGE = re.compile(r"Merge ([0-9a-f]{40}) into ([0-9a-f]{40})")
SECTION = re.compile(r"^== scenarios: (.+?) #(\d+)\s*$")
ANY_SECTION = re.compile(r"^== ")
EVAL_ERROR = re.compile(r"^\[giga-eval [\d:]+\] ERROR: (?:timed out after \d+ s: )?(.+)$")
EVAL_COUNT = re.compile(r"^\[giga-eval [\d:]+\] .+: (\d+)/(\d+) passed\s*$")
SCENARIO_FAIL = re.compile(r"^\s+FAIL (.+)$")
PW_HEAD = re.compile(r"^\s*\d+ (failed|flaky)\s*$")
PW_TEST = re.compile(r"^\s*\[(\w+)\] › (.+?):\d+:\d+ › (.+?)\s*$")
CARGO = re.compile(r"^test (\S+) \.\.\. FAILED\s*$")
VITEST = re.compile(r"^ FAIL  (.+?)(?: \[ .+ \])?\s*$")
UNITTEST = re.compile(r"^(?:FAIL|ERROR): (\S+) \((.+?)\)")
NODE = re.compile(r"^✖ (?!failing tests)(.+?) \(\d")


def clean_scenario_text(text):
    text = UUID.sub("*", text)
    if text.startswith("instance exited"):
        return "instance exited"
    match = re.match(r"(.+):\s*[^:]*, expected\b", text)
    return (match.group(1) if match else text).strip()[:120]


def parse_log(text):
    """Failed test names, repeats and the merge ref in one job's log."""
    out = {"ref": None, "tests": set(), "network": None, "repeats": {}}
    runs = collections.defaultdict(set)   # scenario -> repeat numbers seen
    failed = collections.defaultdict(lambda: collections.defaultdict(set))  # scenario -> test -> repeats
    section, number, in_summary = None, None, False
    for raw in text.splitlines():
        line = ANSI.sub("", LOG_LINE.sub("", raw))
        merge = MERGE.search(line)
        if merge and not out["ref"]:
            out["ref"] = merge.group(1, 2)
        if not out["network"]:
            hit = NETWORK.search(line)
            out["network"] = hit.group(0) if hit else None
        head = SECTION.match(line)
        if head:
            section, number = head.group(1), int(head.group(2))
            runs[section].add(number)
            continue
        if ANY_SECTION.match(line):
            section = None
        if section:
            hit = EVAL_ERROR.match(line) or SCENARIO_FAIL.match(line)
            if hit:
                failed[section][clean_scenario_text(hit.group(1))].add(number)
                continue
            count = EVAL_COUNT.match(line)
            if count and int(count.group(1)) < int(count.group(2)) and not any(number in r for r in failed[section].values()):
                failed[section]["failed, no detail in the log"].add(number)
        if PW_HEAD.match(line):
            in_summary = True
            continue
        if in_summary:
            pw = PW_TEST.match(line)
            if pw:
                out["tests"].add("browser: %s › %s" % (pw.group(2).replace("tests/browser/", ""), pw.group(3)))
                continue
            in_summary = False
        for rule, make in ((CARGO, lambda m: "cargo: " + m.group(1)),
                           (VITEST, lambda m: "vitest: " + m.group(1)),
                           (UNITTEST, lambda m: "unittest: %s.%s" % (m.group(2), m.group(1))),
                           (NODE, lambda m: "node: " + m.group(1))):
            hit = rule.match(line)
            if hit:
                out["tests"].add(make(hit))
                break
    for scenario, tests in failed.items():
        bad = set().union(*tests.values()) if tests else set()
        clean = len(runs[scenario] - bad)
        for test, numbers in tests.items():
            name = "scenario %s: %s" % (scenario, test)
            out["tests"].add(name)
            if clean and numbers:
                out["repeats"][name] = (len(numbers), len(runs[scenario]))
    return out


def classify_step(names):
    """test, setup, rollup or check, from the failed steps of a job."""
    if not names:
        return "check"
    if any(ROLLUP_STEP.search(n) for n in names):
        return "rollup"
    if any(TEST_STEP.search(n) for n in names):
        return "test"
    if any(SETUP_STEP.search(n) for n in names):
        return "setup"
    return "check"


def failed_steps(job):
    return [s["name"] for s in job.get("steps") or [] if s.get("conclusion") == "failure"]


def minutes(job):
    if not job.get("started_at") or not job.get("completed_at"):
        return 0.0
    fmt = "%Y-%m-%dT%H:%M:%SZ"
    delta = datetime.strptime(job["completed_at"], fmt) - datetime.strptime(job["started_at"], fmt)
    return max(delta.total_seconds() / 60, 0.0)


class Source:
    """Disk cache in the ci-metrics format, plus GET calls up to a budget."""

    def __init__(self, caches, store, budget, offline, repo=REPO, runner=subprocess.run):
        self.store_jobs, self.logs = os.path.join(store, "jobs"), os.path.join(store, "logs")
        self.budget, self.offline, self.repo = budget, offline, repo
        self.runner, self.calls, self.stopped = runner, 0, None
        self.jobs, self.runs, self.keys = {}, {}, set()
        os.makedirs(self.store_jobs, exist_ok=True)
        os.makedirs(self.logs, exist_ok=True)
        for directory in [*caches, self.store_jobs]:
            self.load(directory)

    def load(self, directory):
        for path in glob.glob(os.path.join(directory, "*.json")):
            with open(path) as f:
                doc = json.load(f)
            body = doc.get("body", doc)
            self.keys.add(doc.get("key"))
            if not isinstance(body, dict):
                continue
            for run in body.get("workflow_runs", []):
                self.runs[run["id"]] = run
            for job in body.get("jobs", []):
                self.jobs[job["id"]] = job

    def get(self, path):
        """One GET, or None when the budget is spent or GitHub refuses."""
        if self.offline or self.stopped or self.calls >= self.budget:
            return None
        self.calls += 1
        done = self.runner(["gh", "api", path], capture_output=True, text=True)
        if done.returncode == 0:
            return done.stdout
        err = (done.stderr or "").lower()
        if "403" in err or "429" in err or "rate limit" in err:
            self.stopped = "GitHub refused: " + err.strip()[:120]
        return None

    def fetch_run_jobs(self, run):
        path = "repos/%s/actions/runs/%d/jobs?filter=all&per_page=100&page=1" % (self.repo, run["id"])
        key = "%s#attempt=%d" % (path, run["run_attempt"])
        if key in self.keys:
            return
        text = self.get(path)
        if text is None:
            return
        body = json.loads(text)
        name = hashlib.sha256(key.encode()).hexdigest()[:32] + ".json"
        with open(os.path.join(self.store_jobs, name), "w") as f:
            json.dump({"key": key, "fetched": int(time.time()), "body": body}, f)
        self.keys.add(key)
        for job in body["jobs"]:
            self.jobs[job["id"]] = job

    def has_all_attempts(self, run):
        """True when job data reaches back to attempt 1 and up to the latest."""
        seen = {j.get("run_attempt", 1) for j in self.jobs.values() if j["run_id"] == run["id"]}
        return 1 in seen and run.get("run_attempt", 1) in seen

    def log(self, job_id, fetch=False):
        path = os.path.join(self.logs, "%d.log" % job_id)
        if not os.path.exists(path) and fetch:
            text = self.get("repos/%s/actions/jobs/%d/logs" % (self.repo, job_id))
            if text is not None:
                with open(path, "w") as f:
                    f.write(text)
        if os.path.exists(path):
            with open(path, errors="replace") as f:
                return parse_log(f.read())
        return None


def runs_worth_reading(runs):
    """Runs that were rerun, and runs on a head where one run failed and another passed."""
    by_head = collections.defaultdict(list)
    for run in runs.values():
        if run.get("name") not in SKIP_WORKFLOWS and run.get("status") == "completed":
            by_head[(run["name"], run["head_sha"])].append(run)
    picked = []
    for group in by_head.values():
        ends = {run.get("conclusion") for run in group}
        for run in group:
            mixed = {"failure", "success"} <= ends and run.get("conclusion") in ends & {"failure", "success"}
            if run.get("run_attempt", 1) > 1 or mixed:
                picked.append(run)
    return sorted(picked, key=lambda r: r["id"])


def find_failures(jobs, since=None, until=None):
    """Every failed job in the window, with the job that passed after it."""
    passes = collections.defaultdict(list)
    for job in jobs.values():
        if job.get("conclusion") == "success" and job.get("workflow_name") not in SKIP_WORKFLOWS:
            passes[(job.get("workflow_name"), job["name"], job.get("head_branch"))].append(job)
    found, seen = [], set()
    for job in sorted(jobs.values(), key=lambda j: j["id"]):
        when = job.get("started_at") or ""
        if job.get("conclusion") != "failure" or job.get("workflow_name") in SKIP_WORKFLOWS:
            continue
        # A rerun of other jobs lists this one again under the new attempt, unchanged.
        once = (job["run_id"], job["name"], when)
        if once in seen:
            continue
        seen.add(once)
        if (since and when < since) or (until and when > until):
            continue
        later = [p for p in passes[(job.get("workflow_name"), job["name"], job.get("head_branch"))]
                 if (p.get("started_at") or "") > when]
        rerun = [p for p in later if p["run_id"] == job["run_id"]]
        new_run = [p for p in later if p["run_id"] != job["run_id"] and p["head_sha"] == job["head_sha"]]
        later_head = [p for p in later if p["head_sha"] != job["head_sha"]]
        found.append({"job": job, "class": classify_step(failed_steps(job)), "minutes": minutes(job),
                      "rerun": min(rerun, key=lambda p: p["started_at"]) if rerun else None,
                      "new_run": min(new_run, key=lambda p: p["started_at"]) if new_run else None,
                      "later_head": bool(later_head)})
    return found


def ref_verdict(failure, source, fetch=True):
    """same_ref, same_ref_by_rule, new_ref, new_ref_unread, later_head or none."""
    pass_job = failure["rerun"] or failure["new_run"]
    if not pass_job:
        return "later_head" if failure["later_head"] else "none"
    ours = source.log(failure["job"]["id"], fetch=fetch)
    theirs = source.log(pass_job["id"], fetch=fetch)
    if ours and theirs and ours["ref"] and theirs["ref"]:
        return "same_ref" if ours["ref"] == theirs["ref"] else "new_ref"
    return "same_ref_by_rule" if failure["rerun"] else "new_ref_unread"


def group_of(job):
    return (job.get("workflow_name"), job["name"].split(" (")[0].rstrip("0123456789-"), tuple(failed_steps(job)))


def rank(failures, source, fetch=True):
    """Test rows, setup rows and counts."""
    tests = collections.defaultdict(lambda: {
        "same_ref": 0, "by_rule": 0, "new_ref": 0, "unread": 0, "repeat": 0, "repeat_runs": [0, 0], "later_head": 0,
        "none": 0, "jobs": {}, "branches": set(), "where": collections.Counter(), "examples": [], "sampled": {}})
    setup = collections.defaultdict(lambda: {"jobs": 0, "minutes": 0.0, "same_ref": 0, "new_ref": 0, "examples": []})
    counts = collections.Counter()
    no_name, mass = [], []
    known, read = collections.Counter(), collections.Counter()
    for fail in failures:
        job = fail["job"]
        counts[fail["class"]] += 1
        counts[fail["class"] + "_minutes"] += fail["minutes"]
        paired = bool(fail["rerun"] or fail["new_run"])
        verdict = ref_verdict(fail, source, fetch=fetch and fail["class"] == "test")
        log = source.log(job["id"], fetch=fetch and paired and fail["class"] == "setup")
        if fail["class"] == "setup" or (fail["class"] == "test" and log and not log["tests"] and log["network"]):
            step = (failed_steps(job) or ["(no step)"])[0]
            label = re.sub(r"\s*\(.*$", "", step) + (" [%s]" % log["network"] if log and log["network"] else "")
            row = setup[label]
            row["jobs"] += 1
            row["minutes"] += fail["minutes"]
            row["same_ref"] += int(bool(fail["rerun"]))
            row["new_ref"] += int(bool(not fail["rerun"] and fail["new_run"]))
            row["examples"].append(job["run_id"])
            continue
        if fail["class"] != "test":
            continue
        if not paired:
            known[group_of(job)] += 1
            if log:
                read[group_of(job)] += 1
        if not log:
            if paired:
                no_name.append(job)
            continue
        if not log["tests"]:
            no_name.append(job)
        if len(log["tests"]) > MASS_FAILURE:
            mass.append((job, len(log["tests"]), paired))
            continue
        for name in log["tests"]:
            row = tests[name]
            key = {"same_ref": "same_ref", "same_ref_by_rule": "by_rule", "new_ref": "new_ref",
                   "new_ref_unread": "unread", "later_head": "later_head", "none": "none"}[verdict]
            row[key] += 1
            if name in log["repeats"]:
                row["repeat"] += 1
                row["repeat_runs"][0] += log["repeats"][name][0]
                row["repeat_runs"][1] += log["repeats"][name][1]
            row["jobs"][job["id"]] = fail["minutes"]
            row["branches"].add(job.get("head_branch"))
            row["where"][job["name"]] += 1
            strong = key in ("same_ref", "by_rule") or name in log["repeats"]
            row["examples"].append((0 if strong else 1, job["run_id"], job.get("run_attempt", 1)))
            if not paired:
                row["sampled"][job["id"]] = group_of(job)
    for row in tests.values():
        row["est_jobs"], row["est_minutes"], row["estimate"] = 0.0, 0.0, False
        for job_id, mins in row["jobs"].items():
            group = row["sampled"].get(job_id)
            scale = known[group] / max(read[group], 1) if group else 1.0
            row["estimate"] = row["estimate"] or scale > 1.0
            row["est_jobs"] += scale
            row["est_minutes"] += mins * scale
    return {"tests": tests, "setup": setup, "counts": counts, "no_name": no_name, "mass": mass, "known": known, "read": read}


def strong(row):
    return row["same_ref"] + row["by_rule"] + row["repeat"]


def ordered(tests):
    return sorted(tests.items(), key=lambda kv: (-strong(kv[1]), -kv[1]["est_minutes"], kv[0]))


def cost(row):
    mark = "~" if row["estimate"] else ""
    return "%s%.0f" % (mark, row["est_jobs"]), "%s%.0f" % (mark, row["est_minutes"])


def example_ids(row, n=3):
    picked, seen = [], set()
    for _, run, attempt in sorted(row["examples"]):
        if run not in seen:
            seen.add(run)
            picked.append("%d#%d" % (run, attempt))
    return " ".join(picked[:n])


def test_row(i, name, row):
    jobs, mins = cost(row)
    repeat = "%d (%d of %d repeats)" % (row["repeat"], *row["repeat_runs"]) if row["repeat"] else "0"
    return "| %d | `%s` | %d | %s | %s | %d | %d | %d | %s | %s | %d | %s |" % (
        i, name.replace("|", "\\|"), len(row["jobs"]), jobs, mins, row["same_ref"], row["by_rule"], row["new_ref"] + row["unread"],
        repeat, row["later_head"], len(row["branches"]), example_ids(row))


HEAD = ("| # | test | count | jobs lost | job-min | same ref, logs read | same ref, by rule | new ref | repeat in job "
        "| later head only | branches | example run#attempt |\n|---|---|---|---|---|---|---|---|---|---|---|---|")


def report(ranked, source, candidates, window):
    tests, setup, counts = ranked["tests"], ranked["setup"], ranked["counts"]
    no_name, mass, known, read = ranked["no_name"], ranked["mass"], ranked["known"], ranked["read"]
    created = sorted(r["created_at"] for r in source.runs.values() if r.get("created_at"))
    lines = ["# Flaky test record: %s" % source.repo, "",
             "Failed jobs: %s. Rerun evidence comes from runs listed for %s. Outside that, a failure shows only in a log already on disk." % (
                 window, "%s to %s" % (created[0], created[-1]) if created else "no window"),
             "Records only: nothing here retries a job or hides a failure.",
             "A `~` is an estimate: logged failures scaled to all failed jobs of that kind. Counts without `~` are measured.",
             "A test sharing a job with another test counts that whole job, so rows do not add up to a total.", ""]
    proven = [(n, r) for n, r in ordered(tests) if strong(r)]
    weak = [(n, r) for n, r in ordered(tests) if not strong(r) and (r["new_ref"] + r["unread"])]
    watch = [(n, r) for n, r in ordered(tests) if not strong(r) and not (r["new_ref"] + r["unread"]) and len(r["branches"]) >= 2]
    for title, rows in (("## Ranked: passed on the same ref, or inside the job (flaky)", proven),
                        ("## Passed only on a new ref (main moved; may be a fix on main)", weak),
                        ("## Failed on 2 or more branches, no rerun evidence (not proven flaky, may be real)", watch)):
        lines += [title, ""]
        shown = rows[:WATCH_ROWS]
        lines += [HEAD] + [test_row(i + 1, n, r) for i, (n, r) in enumerate(shown)] if rows else ["None."]
        if len(rows) > len(shown):
            lines.append("\n%d more rows are not shown." % (len(rows) - len(shown)))
        lines.append("")
    one = len(tests) - len(proven) - len(weak) - len(watch)
    lines += ["Left out of the lists: %d tests that failed on one branch with no rerun evidence (treated as real)." % one, ""]
    if candidates:
        lines += ["## Candidates asked about", "", "| pattern | matches |", "|---|---|"]
        for pattern in candidates:
            hits = [(n, r) for n, r in ordered(tests) if re.search(pattern, n)]
            text = "; ".join("`%s`: count %d, same ref %d, by rule %d, new ref %d, repeat %d, later head %d, branches %d" % (
                n, len(r["jobs"]), r["same_ref"], r["by_rule"], r["new_ref"] + r["unread"], r["repeat"], r["later_head"], len(r["branches"]))
                for n, r in hits) or "no failure in the logs read"
            lines.append("| `%s` | %s |" % (pattern, text.replace("|", "\\|")))
        lines.append("")
    lines += ["## Setup failures (not in the test ranking)", "",
              "From the failed step of each job. The bracket is a line from the log, where a log was read.", "",
              "| step | jobs | job-min | passed on rerun | passed in a new run | example runs |", "|---|---|---|---|---|---|"]
    for label, row in sorted(setup.items(), key=lambda kv: -kv[1]["minutes"]):
        lines.append("| %s | %d | %.0f | %d | %d | %s |" % (label, row["jobs"], row["minutes"], row["same_ref"], row["new_ref"],
                                                             " ".join(str(r) for r in row["examples"][:3])))
    lines += ["", "## Jobs with more than %d failed tests (not per test)" % MASS_FAILURE, ""]
    lines += ["| run#attempt | job | failed tests | passed after a rerun or new run |", "|---|---|---|---|"] if mass else ["None."]
    for job, n, paired in mass:
        lines.append("| %d#%d | %s | %d | %s |" % (job["run_id"], job.get("run_attempt", 1), job["name"], n, "yes" if paired else "no"))
    lines += ["", "## Coverage", "", "| failed jobs | jobs | job-min |", "|---|---|---|"]
    for cls, text in (("test", "in a test step"), ("setup", "in a setup step"),
                      ("rollup", "rollup job (`ci`, `gate/mutation`), not a cause"), ("check", "other checks: lint, mutation, build")):
        lines.append("| %s | %d | %.0f |" % (text, counts[cls], counts[cls + "_minutes"]))
    lines += ["", "- Job data read for %d of %d runs that were rerun, or on a head with both a failed and a passed run." % (
        sum(1 for r in runs_worth_reading(source.runs) if source.has_all_attempts(r)),
        len(runs_worth_reading(source.runs))),
        "- Failed test-step jobs with no rerun evidence: %d of %d logs read (a sample, so scaled counts are estimates)." % (
            sum(read.values()), sum(known.values())),
        "- Test-step jobs with a log and no test name, or rerun evidence and no log: %d. Examples: %s." % (
            len(no_name), " ".join(str(j["run_id"]) for j in no_name[:5]) or "none"),
        "- GET calls this run: %d.%s" % (source.calls, " Stopped: " + source.stopped if source.stopped else ""),
        "- Only reruns and repeats show a flake, so this is a lower bound and not a flake rate. Runs nobody reran show nothing.",
        "- Job-minutes are the failed job's run time, not a person's time."]
    return "\n".join(lines) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cache", action="append", default=[], help="directory of job data to read, never written; can repeat")
    ap.add_argument("--store", required=True, help="writable directory: jobs/ and logs/ (<job id>.log)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--budget", type=int, default=150, help="hard cap on GET calls in one run")
    ap.add_argument("--offline", action="store_true", help="read the disk only")
    ap.add_argument("--since", help="started at or after this UTC time")
    ap.add_argument("--until", help="started at or before this UTC time")
    ap.add_argument("--candidate", action="append", default=[], help="regex; report what the logs say about it")
    args = ap.parse_args()
    source = Source(args.cache, args.store, args.budget, args.offline)
    for run in runs_worth_reading(source.runs):
        if not source.has_all_attempts(run):
            source.fetch_run_jobs(run)
    failures = find_failures(source.jobs, args.since, args.until)
    ranked = rank(failures, source, fetch=not args.offline)
    starts = sorted(f["job"]["started_at"] for f in failures)
    window = "%s to %s" % (args.since or starts[0], args.until or starts[-1]) if starts else "no failed jobs"
    with open(args.out, "w") as f:
        f.write(report(ranked, source, args.candidate, window))
    print("wrote %s: %d tests, %d GET calls%s" % (args.out, len(ranked["tests"]), source.calls, ", stopped" if source.stopped else ""))


if __name__ == "__main__":
    main()
