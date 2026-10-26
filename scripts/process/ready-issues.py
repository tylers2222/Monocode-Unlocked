#!/usr/bin/env python3
"""Print the Todo ADE issues of one MonoCode instance with no builder as START NOW or WAIT.

    ready-issues.py --instance <1|2>

An issue belongs to the instance its Linear label `instance:<n>` names. A Todo
issue without exactly one instance label is listed under "unlabelled".

Code complete. A ticket PR body has the line `Ticket-part: <n>/<N>`. When a PR
merged into tyler/main has a title that starts with `ADE-<id>` and a body line
with n == N, the script adds the Linear label `code-complete` to that ticket.
Adding the label again changes nothing, and other labels stay.

Blockers. A blocker (a Linear blockedBy relation) unblocks when its state is
Done, Canceled or Duplicate, or it has `code-complete`, and no builder works on
it. It unblocked at the earliest of: the history entry of its closed state, the
merge of its last part, the history entry that added `code-complete`, else its
updatedAt.

START NOW and WAIT. A Todo issue starts now when every blocker unblocked, it
has a Files list, and no open PR of the instance or worktree of another ticket
changes a file on that list. Otherwise it waits, and the lines under it name
each blocker, each overlapping PR or worktree with its files, or "no Files
list". The Files list is the `Files.` line of the issue's section in the
Change R plan (PLAN), else a `Files:` line in the issue description. Its
paths are the backtick-quoted words with a `/` or a `.`, or, with nothing
quoted, a comma list of such words (a list with prose in it has none). A path
whose last part has no `.` covers the directory, `*` matches within one
directory, and `a/{b,c}/{d,e}` lists every combination. A worktree
counts when it exists, its branch names a ticket (the builder rule below) and
it shares history with tyler/main; scratch and review checkouts do not count,
and work with a PR counts through the PR. It changes the files that differ
from its merge base with tyler/main (the
remote-tracking ref, not fetched here), committed or not, plus its untracked
files. A PR or worktree whose branch names the issue is its own. START NOW
lists Change R issues (those in PLAN) first, then Urgent and High, then the
rest, each by ticket number. An issue is ready since its last blocker
unblocked, or, with no blockers, since it entered Todo (history, else createdAt).

Builders. A session builds ticket ADE-<id> when it is busy, not archived, and
the branch the app recorded for it matches `(^|/)ade-<id>(-|$)` (any case,
the exact id with no leading zero). The sessions are the monocode project's,
from `monocode app sessions.list`. The project is $MONOCODE_PROJECT, else the
main checkout of the git repo that holds this script (also when it runs from a
worktree). The CLI answers for the app its environment points at, whichever
binary runs it, so the reply names that app (`appIdentifier`) and the script
exits 2 unless it is the app of --instance.
Run the script in a session or Mono of that instance. The other instance is not
read: the lead accepted this one-instance scope on 2026-10-23 (decisions.md,
ADE-84 #268 r1 m2), because the CLI answers only for its own app and D1 denies
direct database reads. A closed or code-complete blocker that a builder of the
other instance still works on counts as unblocked. A reply from an app built
before #268 has no `appIdentifier` and exits 2. Titles are never matched. Busy
sessions on `main` or with no branch are listed as "unattributed".

--dry-run prints `would label ADE-<n>` for each code-complete label it would
add, and writes nothing.

The Linear key is LINEAR_API_KEY, else the Linear MCP header in
~/.omp/agent/mcp.json. Exit 0: nothing to do. Exit 1: a START NOW issue has
been ready for more than 60 minutes, an issue with no unmerged blocker has no
Files list, or a Todo issue is unlabelled.
Exit 2: a source failed or answered something unexpected.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path

REPO = "tylers2222/Monocode-Unlocked"
MAX_WAIT = timedelta(minutes=60)
CLOSED = ("completed", "canceled", "duplicate")
CODE_COMPLETE = "code-complete"
INSTANCE_APPS = {"1": "com.monocode.desktop", "2": "com.monocode.second"}
APP_CLI = "/Applications/MonoCode.app/Contents/MacOS/monocode"
PLAN = Path.home() / "MonoCode/gigas/monocode-general-2/projects/change-r/plan-v2.md"
REPO_DIR = Path(__file__).resolve().parent
BASE = "refs/remotes/tyler/tyler/main"
HIGH = (1, 2)
PLAN_TICKET = re.compile(r"^- ([REDF]\d+): (ADE-\d+)$", re.MULTILINE)
FILES_LINE = re.compile(r"^Files[.:] (.+)$", re.MULTILINE)
PATH = re.compile(r"\S*[/.]\S*")
NO_FILES = "no Files list"
BRACES = re.compile(r"\{([^{}]*)\}")
BRANCH_TICKET = re.compile(r"(?:^|/)ade-([1-9]\d*)(?=-|$)", re.IGNORECASE)
TITLE_TICKET = re.compile(r"ADE-(\d+)\b")
TICKET_PART = re.compile(r"^Ticket-part: (\d+)/(\d+)\r?$", re.MULTILINE)
ISSUES_QUERY = """
query($filter: IssueFilter, $after: String) {
  issues(first: 50, after: $after, filter: $filter) {
    pageInfo { hasNextPage endCursor }
    nodes {
      id identifier title description priority createdAt updatedAt state { type } labels { nodes { name } }
      history(first: 100) { nodes { createdAt toState { type } addedLabels { name } } }
      inverseRelations { nodes { type issue { identifier } } }
    }
  }
}"""


@dataclass(frozen=True)
class Todo:
    id: str
    title: str
    since: datetime | None
    waits: tuple[str, ...]


@dataclass(frozen=True)
class Claim:
    """The files an open PR or a worktree changes."""
    owner: str
    branch: str
    files: frozenset[str]


@dataclass(frozen=True)
class FinalPart:
    pr: int
    merged_at: datetime


def parse_time(text):
    return datetime.fromisoformat(text.replace("Z", "+00:00"))


def label_names(issue):
    return {label["name"] for label in issue["labels"]["nodes"]}


def last_event(issue, happened):
    times = [parse_time(event["createdAt"]) for event in issue["history"]["nodes"] if happened(event)]
    return max(times, default=None)


def entered_state(issue):
    return last_event(issue, lambda event: (event["toState"] or {}).get("type") == issue["state"]["type"])


def final_parts(prs):
    """Ticket id -> the last merged PR whose title starts with the ticket id and whose body says part N of N."""
    finals = {}
    for pr in prs:
        title, part = TITLE_TICKET.match(pr["title"]), TICKET_PART.search(pr["body"] or "")
        if title and part and 1 <= int(part[1]) == int(part[2]):
            final = FinalPart(pr["number"], parse_time(pr["mergedAt"]))
            ticket = f"ADE-{int(title[1])}"
            finals[ticket] = max(finals.get(ticket, final), final, key=lambda f: f.merged_at)
    return finals


def to_label(issues, finals):
    """The fetched issues that have a final part merged but no code-complete label yet."""
    return [issue for issue in issues if issue["identifier"] in finals and CODE_COMPLETE not in label_names(issue)]


def unblocked_at(blocker, final):
    """When a blocker stopped blocking, or None while it still blocks."""
    times = []
    if blocker["state"]["type"] in CLOSED:
        times.append(entered_state(blocker))
    if final:
        times.append(final.merged_at)
    if CODE_COMPLETE in label_names(blocker):
        times.append(last_event(blocker, lambda event: CODE_COMPLETE in {l["name"] for l in event["addedLabels"] or []}))
    if not times:
        return None
    return min(filter(None, times), default=parse_time(blocker["updatedAt"]))


def busy(sessions):
    return [session for session in sessions if session["busy"] and not session["archived"]]


def branch_tickets(branch):
    return {f"ADE-{n}" for n in BRANCH_TICKET.findall(branch or "")}


def builder_tickets(sessions):
    return {ticket for session in busy(sessions) for ticket in branch_tickets(session["branch"])}


def number(ident):
    return int(ident.split("-")[1])


def unattributed(sessions):
    """Busy sessions on main or with no branch."""
    return [session for session in busy(sessions) if (session["branch"] or "main") == "main"]


def expand(path):
    """`a/{b,c}/{d,e}` -> a/b/d, a/b/e, a/c/d, a/c/e."""
    braces = BRACES.search(path)
    if not braces:
        return [path]
    head, tail = path[:braces.start()], path[braces.end():]
    return [full for part in braces[1].split(",") for full in expand(head + part + tail)]


def file_patterns(line):
    """The paths on a Files line: its backtick-quoted paths, or with nothing quoted, a comma list that holds
    only paths (a list with prose in it gives none). A path whose last part has no `.` is a directory."""
    line = re.sub(r"\s*,\s*", ",", line)
    quoted = line.split("`")[1::2]
    if quoted:
        paths = [span for span in quoted if PATH.fullmatch(span)]
    else:
        paths = [item.strip().rstrip(".") for item in re.split(r",(?![^{]*\})", line)]
        if not all(PATH.fullmatch(path) for path in paths):
            return ()
    expanded = [full for path in paths for full in expand(path)]
    return tuple(path if path.endswith("/") or "." in path.rsplit("/", 1)[-1] else path + "/" for path in expanded)


def plan_files(text):
    """Change R issue -> the patterns on the Files. line of its plan section, empty when it has none."""
    sections = {}
    for section in re.split(r"^#+ ", text, flags=re.MULTILINE)[1:]:
        line = FILES_LINE.search(section)
        sections[section.split(".", 1)[0]] = file_patterns(line[1]) if line else ()
    return {ticket: sections.get(key, ()) for key, ticket in PLAN_TICKET.findall(text)}


def ticket_files(issue, plan):
    line = FILES_LINE.search(issue["description"] or "")
    return plan.get(issue["identifier"]) or (file_patterns(line[1]) if line else ())


def covers(pattern, path):
    """A pattern ending in / covers its directory; * matches within one path segment."""
    if pattern.endswith("/"):
        return path.startswith(pattern)
    return re.fullmatch(re.escape(pattern).replace(r"\*", "[^/]*"), path) is not None


def overlaps(ticket, patterns, claims):
    """`<owner>: <files>` for each claim of another ticket that changes a file on the list."""
    found = []
    for claim in claims:
        if ticket in branch_tickets(claim.branch):
            continue
        files = sorted(path for path in claim.files if any(covers(pattern, path) for pattern in patterns))
        if files:
            found.append(f"{claim.owner}: {', '.join(files)}")
    return sorted(found)


def ready_issues(issues, sessions, finals, instance, plan, claims):
    """(Todo issues of the instance with no builder, START NOW first; Todo issues without exactly one instance label)."""
    by_id = {issue["identifier"]: issue for issue in issues}
    building = builder_tickets(sessions)
    todo, unlabelled = [], []
    for issue in issues:
        ident = issue["identifier"]
        if issue["state"]["type"] != "unstarted":
            continue
        instances = {name for name in label_names(issue) if name.startswith("instance:")}
        if len(instances) != 1:
            unlabelled.append(issue)
            continue
        if instances != {f"instance:{instance}"} or ident in building:
            continue
        blockers = sorted((r["issue"]["identifier"] for r in issue["inverseRelations"]["nodes"] if r["type"] == "blocks"), key=number)
        times = {b: None if b in building else unblocked_at(by_id[b], finals.get(b)) for b in blockers}
        blocked = [f"blocker {b}" for b, time in times.items() if time is None]
        since = None
        if not blocked:
            since = max(times.values()) if times else entered_state(issue) or parse_time(issue["createdAt"])
        patterns = ticket_files(issue, plan)
        waits = blocked + (overlaps(ident, patterns, claims) if patterns else [NO_FILES])
        rank = (bool(waits), ident not in plan, issue["priority"] not in HIGH, number(ident))
        todo.append((rank, Todo(ident, issue["title"], since, tuple(waits))))
    return [item for _, item in sorted(todo, key=lambda pair: pair[0])], unlabelled


def report(todo, unlabelled, unattributed_sessions, now):
    """Output lines and the exit code."""
    start = [issue for issue in todo if not issue.waits]
    no_files = [issue for issue in todo if issue.since and issue.waits == (NO_FILES,)]
    late = sum(now - issue.since > MAX_WAIT for issue in start)

    def ready(issue):
        when = issue.since.astimezone().strftime("%Y-%m-%d %H:%M")
        return f"{issue.id}  ready since {when} ({int((now - issue.since).total_seconds() // 60)} min)  {issue.title}"

    lines = ["START NOW (blockers merged, Files list, no file overlap):", *map(ready, start), "WAIT:"]
    for issue in todo:
        if issue.waits:
            lines += [f"{issue.id}  {issue.title}", *(f"    {reason}" for reason in issue.waits)]
    if no_files:
        lines.append("no Files list (blockers merged; add a Files: line of paths to the description):")
        lines += map(ready, no_files)
    if unlabelled:
        lines.append("unlabelled (a Todo issue needs exactly one of instance:1, instance:2):")
        lines += [f"{issue['identifier']}  {issue['title']}" for issue in unlabelled]
    if unattributed_sessions:
        lines.append("unattributed (busy on main or no branch; a builder works on a branch named ade-<n>):")
        lines += [f"instance {s['instance']}  {s['id'][-8:]}  {s['branch'] or '<no branch>'}  {s['title']}"
                  for s in unattributed_sessions]
    lines.append(f"ready-issues: {len(start)} start now, {late} waiting more than 60 min, {len(no_files)} with no Files list, "
                 f"{len(todo) - len(start)} wait, {len(unlabelled)} unlabelled, {len(unattributed_sessions)} unattributed.")
    return lines, 1 if late or no_files or unlabelled else 0


def linear_key():
    key = os.environ.get("LINEAR_API_KEY")
    if not key:
        config = json.loads((Path.home() / ".omp/agent/mcp.json").read_text())
        key = config["mcpServers"]["linear"]["headers"]["Authorization"]
    key = key.removeprefix("Bearer ").strip()
    return key if key.startswith("lin_api_") else f"Bearer {key}"


def linear(key, query, variables):
    request = urllib.request.Request(
        "https://api.linear.app/graphql",
        data=json.dumps({"query": query, "variables": variables}).encode(),
        headers={"Authorization": key, "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        body = json.load(response)
    if "errors" in body:
        raise RuntimeError(f"Linear: {body['errors'][0]['message']}")
    return body["data"]


def linear_issues(key, issue_filter):
    issues, after = [], None
    while True:
        page = linear(key, ISSUES_QUERY, {"filter": issue_filter, "after": after})["issues"]
        issues += page["nodes"]
        if not page["pageInfo"]["hasNextPage"]:
            return issues
        after = page["pageInfo"]["endCursor"]


def open_issues_and_blockers(key):
    """The open ADE issues, plus the closed issues that block one of them."""
    team = {"key": {"eq": "ADE"}}
    issues = linear_issues(key, {"team": team, "state": {"type": {"nin": list(CLOSED)}}})
    known = {issue["identifier"] for issue in issues}
    missing = sorted({number(r["issue"]["identifier"]) for issue in issues
                      for r in issue["inverseRelations"]["nodes"]
                      if r["type"] == "blocks" and r["issue"]["identifier"] not in known})
    return issues + (linear_issues(key, {"team": team, "number": {"in": missing}}) if missing else [])


def add_code_complete(key, issues):
    label = linear(key, '{ issueLabels(filter: {name: {eq: "code-complete"}, team: {key: {eq: "ADE"}}}) { nodes { id } } }', {})
    label_id = label["issueLabels"]["nodes"][0]["id"]
    for issue in issues:
        result = linear(key, "mutation($id: String!, $label: String!) { issueAddLabel(id: $id, labelId: $label) { success } }",
                        {"id": issue["id"], "label": label_id})
        if not result["issueAddLabel"]["success"]:
            raise RuntimeError(f"Linear: could not label {issue['identifier']} {CODE_COMPLETE}")
        issue["labels"]["nodes"].append({"name": CODE_COMPLETE})


def gh_prs(*flags):
    return json.loads(subprocess.run(["gh", "pr", "list", "-R", REPO, *flags], check=True, capture_output=True, text=True).stdout)


def merged_prs():
    return gh_prs("--base", "tyler/main", "--state", "merged", "--limit", "1000", "--json", "number,title,body,mergedAt")


def open_prs(instance):
    """The instance's open PRs. gh lists at most 100 files of a PR, so a longer list is read from the API page by page."""
    prs = gh_prs("--state", "open", "--label", f"instance:{instance}", "--limit", "500", "--json", "number,headRefName,files,changedFiles")
    for pr in prs:
        if len(pr["files"]) < pr["changedFiles"]:
            out = subprocess.run(["gh", "api", "--paginate", f"repos/{REPO}/pulls/{pr['number']}/files", "--jq", ".[].filename"],
                                 check=True, capture_output=True, text=True).stdout
            pr["files"] = [{"path": path} for path in out.splitlines()]
    return prs


def pr_claims(prs):
    return [Claim(f"PR #{pr['number']}", pr["headRefName"], frozenset(f["path"] for f in pr["files"])) for pr in prs]


def git(cwd, *args):
    return subprocess.run(["git", "-C", cwd, *args], check=True, capture_output=True, text=True).stdout


def worktree_claims(repo, pr_branches):
    """Each existing ticket worktree's files that differ from its merge base with tyler/main, committed or not, and its untracked files.
    A worktree whose branch has an open PR counts through the PR."""
    claims = []
    for entry in git(repo, "worktree", "list", "--porcelain").strip().split("\n\n"):
        fields = {key: value for key, _, value in (line.partition(" ") for line in entry.splitlines())}
        path, branch = fields["worktree"], fields.get("branch", "").removeprefix("refs/heads/")
        if not branch_tickets(branch) or branch in pr_branches or not Path(path).is_dir():
            continue
        merge_base = subprocess.run(["git", "-C", path, "merge-base", BASE, "HEAD"], capture_output=True, text=True)
        if merge_base.returncode == 1:
            continue
        merge_base.check_returncode()
        out = git(path, "diff", "-z", "--name-only", merge_base.stdout.strip()) + git(path, "ls-files", "-z", "--others", "--exclude-standard")
        claims.append(Claim(f"worktree {branch}", branch, frozenset(filter(None, out.split("\0")))))
    return claims


def sessions_list(payload):
    run = subprocess.run([APP_CLI, "app", "sessions.list", "--json", json.dumps(payload)],
                         capture_output=True, text=True, timeout=60)
    return json.loads(run.stdout)


def monocode_project():
    """$MONOCODE_PROJECT, else the main checkout of the git repo that holds this script, from a worktree too."""
    if os.environ.get("MONOCODE_PROJECT"):
        return os.environ["MONOCODE_PROJECT"]
    common_dir = subprocess.run(
        ["git", "-C", str(Path(__file__).resolve().parent), "rev-parse", "--path-format=absolute", "--git-common-dir"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    return str(Path(common_dir).parent)


def app_sessions(instance):
    """The monocode project's sessions, from `sessions.list` in the app of this instance."""
    project = monocode_project()
    reply = sessions_list({"project": project})
    # A Mono names the project; a session lists its own and may not name one.
    if reply.get("error") == "project is only for a Mono":
        reply = sessions_list({})
    if not reply.get("ok"):
        raise RuntimeError(f"app sessions.list: {reply.get('error')}")
    result = reply["result"]
    if "appIdentifier" not in result:
        raise RuntimeError("app sessions.list has no appIdentifier: the app was built before #268 and sends no branch")
    if result["appIdentifier"] != INSTANCE_APPS[instance]:
        raise RuntimeError(f"app sessions.list was answered by {result['appIdentifier']}, not instance {instance} "
                           f"({INSTANCE_APPS[instance]}); run it in a session of instance {instance}")
    if os.path.realpath(result["cwd"]) != os.path.realpath(project):
        raise RuntimeError(f"app sessions.list listed {result['cwd']}, not {project}")
    return [{"id": s["id"], "title": s["title"], "branch": s["branch"], "archived": s["archived"],
             "busy": s["busy"], "instance": instance} for s in result["sessions"]]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--instance", required=True, choices=list(INSTANCE_APPS))
    parser.add_argument("--dry-run", action="store_true", help="print the labels it would add; write nothing")
    args = parser.parse_args()
    try:
        sessions = app_sessions(args.instance)
        key = linear_key()
        issues = open_issues_and_blockers(key)
        finals = final_parts(merged_prs())
        plan = plan_files(PLAN.read_text())
        prs = open_prs(args.instance)
        claims = pr_claims(prs) + worktree_claims(REPO_DIR, {pr["headRefName"] for pr in prs})
        labelled = to_label(issues, finals)
        if labelled and not args.dry_run:
            add_code_complete(key, labelled)
        todo, unlabelled = ready_issues(issues, sessions, finals, args.instance, plan, claims)
        lines, code = report(todo, unlabelled, unattributed(sessions), datetime.now(timezone.utc))
    except Exception as err:
        print(f"ready-issues: {type(err).__name__}: {err}", file=sys.stderr)
        return 2
    for issue in labelled:
        if args.dry_run:
            print(f"would label {issue['identifier']} {CODE_COMPLETE}")
        else:
            print(f"{issue['identifier']}  labelled {CODE_COMPLETE} (PR #{finals[issue['identifier']].pr} is its last part)")
    print("\n".join(lines))
    return code


if __name__ == "__main__":
    sys.exit(main())
