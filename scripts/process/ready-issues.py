#!/usr/bin/env python3
"""Print the Todo ADE issues of one MonoCode instance that can start now and have no builder.

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
updatedAt. An issue is ready since its last blocker unblocked, or, with no
blockers, since it entered Todo (history, else createdAt).

Builders. A session builds ticket ADE-<id> when it is busy, not archived, and
the branch the app recorded for it matches `(^|/)ade-<id>(-|$)` (any case,
the exact id with no leading zero). Both app databases are read the same way,
read-only: a session is busy when its id is in that app's in_flight_sessions
table. The app CLI is not used, because it answers for whichever instance the
environment points at. Titles are never matched. Busy sessions on `main` or
with no branch are listed as "unattributed".

--dry-run prints `would label ADE-<n>` for each code-complete label it would
add, and writes nothing.

The Linear key is LINEAR_API_KEY, else the Linear MCP header in
~/.omp/agent/mcp.json. Exit 0: nothing to do. Exit 1: an issue has been ready
for more than 60 minutes with no builder, or a Todo issue is unlabelled.
Exit 2: a source failed or answered something unexpected.
"""
import argparse
import json
import os
import re
import sqlite3
import subprocess
import sys
import urllib.request
from contextlib import closing
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path

REPO = "tylers2222/Monocode-Unlocked"
MAX_WAIT = timedelta(minutes=60)
CLOSED = ("completed", "canceled", "duplicate")
CODE_COMPLETE = "code-complete"
INSTANCES = ("1", "2")
APP_DBS = {
    "1": Path.home() / "Library/Application Support/com.monocode.desktop/monocode.db",
    "2": Path.home() / "Library/Application Support/com.monocode.second/monocode.db",
}
BRANCH_TICKET = re.compile(r"(?:^|/)ade-([1-9]\d*)(?=-|$)", re.IGNORECASE)
TITLE_TICKET = re.compile(r"ADE-(\d+)\b")
TICKET_PART = re.compile(r"^Ticket-part: (\d+)/(\d+)\r?$", re.MULTILINE)
ISSUES_QUERY = """
query($filter: IssueFilter, $after: String) {
  issues(first: 50, after: $after, filter: $filter) {
    pageInfo { hasNextPage endCursor }
    nodes {
      id identifier title createdAt updatedAt state { type } labels { nodes { name } }
      history(first: 100) { nodes { createdAt toState { type } addedLabels { name } } }
      inverseRelations { nodes { type issue { identifier } } }
    }
  }
}"""


@dataclass(frozen=True)
class Ready:
    id: str
    title: str
    since: datetime


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


def builder_tickets(sessions):
    return {f"ADE-{n}" for session in busy(sessions) for n in BRANCH_TICKET.findall(session["branch"] or "")}


def unattributed(sessions):
    """Busy sessions on main or with no branch."""
    return [session for session in busy(sessions) if (session["branch"] or "main") == "main"]


def ready_issues(issues, sessions, finals, instance):
    """(ready issues of the instance with no builder, Todo issues without exactly one instance label)."""
    by_id = {issue["identifier"]: issue for issue in issues}
    building = builder_tickets(sessions)
    ready, unlabelled = [], []
    for issue in issues:
        if issue["state"]["type"] != "unstarted":
            continue
        instances = {name for name in label_names(issue) if name.startswith("instance:")}
        if len(instances) != 1:
            unlabelled.append(issue)
            continue
        if instances != {f"instance:{instance}"} or issue["identifier"] in building:
            continue
        blockers = [r["issue"]["identifier"] for r in issue["inverseRelations"]["nodes"] if r["type"] == "blocks"]
        times = [None if b in building else unblocked_at(by_id[b], finals.get(b)) for b in blockers]
        if None in times:
            continue
        since = max(times) if times else entered_state(issue) or parse_time(issue["createdAt"])
        ready.append(Ready(issue["identifier"], issue["title"], since))
    return sorted(ready, key=lambda r: (r.since, r.id)), unlabelled


def report(ready, unlabelled, unattributed_sessions, now):
    """Output lines and the exit code."""
    lines, late = [], 0
    for issue in ready:
        waited = now - issue.since
        late += waited > MAX_WAIT
        when = issue.since.astimezone().strftime("%Y-%m-%d %H:%M")
        lines.append(f"{issue.id}  ready since {when} ({int(waited.total_seconds() // 60)} min)  {issue.title}")
    if unlabelled:
        lines.append("unlabelled (a Todo issue needs exactly one of instance:1, instance:2):")
        lines += [f"{issue['identifier']}  {issue['title']}" for issue in unlabelled]
    if unattributed_sessions:
        lines.append("unattributed (busy on main or no branch; a builder works on a branch named ade-<n>):")
        lines += [f"instance {s['instance']}  {s['id'][-8:]}  {s['branch'] or '<no branch>'}  {s['title']}"
                  for s in unattributed_sessions]
    lines.append(f"ready-issues: {len(ready)} ready with no builder, {late} waiting more than 60 min, "
                 f"{len(unlabelled)} unlabelled, {len(unattributed_sessions)} unattributed.")
    return lines, 1 if late or unlabelled else 0


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
    missing = sorted({int(r["issue"]["identifier"].split("-")[1]) for issue in issues
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


def merged_prs():
    return json.loads(subprocess.run(
        ["gh", "pr", "list", "-R", REPO, "--base", "tyler/main", "--state", "merged", "--limit", "1000",
         "--json", "number,title,body,mergedAt"],
        check=True, capture_output=True, text=True,
    ).stdout)


def connect_ro(path):
    return sqlite3.connect(f"{Path(path).as_uri()}?mode=ro", uri=True)


def read_db(path):
    """(session rows, in-flight session ids) of one app database, opened read-only."""
    with closing(connect_ro(path)) as db:
        db.row_factory = sqlite3.Row
        rows = [dict(row) for row in db.execute("SELECT id, title, branch, worktree_cwd, archived FROM sessions")]
        in_flight = {row[0] for row in db.execute("SELECT session_id FROM in_flight_sessions")}
    return rows, in_flight


def app_sessions(dbs):
    """Sessions of both instances; busy when the app lists the id in its own in_flight_sessions."""
    sessions = []
    for name, path in dbs.items():
        rows, in_flight = read_db(path)
        sessions += [{**row, "instance": name, "busy": row["id"] in in_flight} for row in rows]
    return sessions


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--instance", required=True, choices=INSTANCES)
    parser.add_argument("--dry-run", action="store_true", help="print the labels it would add; write nothing")
    args = parser.parse_args()
    try:
        sessions = app_sessions(APP_DBS)
        key = linear_key()
        issues = open_issues_and_blockers(key)
        finals = final_parts(merged_prs())
        labelled = to_label(issues, finals)
        if labelled and not args.dry_run:
            add_code_complete(key, labelled)
        ready, unlabelled = ready_issues(issues, sessions, finals, args.instance)
        lines, code = report(ready, unlabelled, unattributed(sessions), datetime.now(timezone.utc))
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
