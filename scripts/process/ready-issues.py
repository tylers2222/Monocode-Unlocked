#!/usr/bin/env python3
"""Print the Todo ADE issues of one MonoCode instance that can start now and have no builder.

    ready-issues.py --instance <1|2>

An issue belongs to the instance its Linear label `instance:<n>` names. A Todo
issue without exactly one instance label is listed under "unlabelled".
A blocker (a Linear blockedBy relation) unblocks when its state is Done,
Canceled or Duplicate, or when it has the label `code-complete`, which the
Orchestrator sets when the last planned PR of a ticket merges. It unblocked at
the history entry that put it in that state or added that label (the earlier
of the two), else at its updatedAt. A blocker that has a builder never
unblocks, whatever its state or labels.
An issue is ready when every blocker has unblocked. It is ready since the last
of those times, or, with no blockers, since it entered Todo (history, else
createdAt). A session builds an issue only when it is busy, not archived, and
its title starts with "omp · Linear ADE-<n>", read from `app sessions.list` of
the instance's CLI (MONOCODE_CLI, else the instance's app bundle). Busy
sessions whose title names no ticket are listed as "unattributed", so a
builder with a bad title is visible; they do not change the exit code.
The Linear key is LINEAR_API_KEY, else the Linear MCP header in
~/.omp/agent/mcp.json.
Exit 0: nothing to do. Exit 1: an issue has been ready for more than 60
minutes with no builder, or a Todo issue is unlabelled. Exit 2: a source failed
or answered something unexpected.
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

MAX_WAIT = timedelta(minutes=60)
CLOSED = ("completed", "canceled", "duplicate")
CODE_COMPLETE = "code-complete"
INSTANCE_CLIS = {
    "1": "/Applications/MonoCode.app/Contents/MacOS/monocode",
    "2": "/Applications/MonoCode 2.app/Contents/MacOS/monocode",
}
BUILDER = re.compile(r"omp · Linear (ADE-\d+)\b")
TICKET = re.compile(r"\bADE-\d+\b", re.IGNORECASE)
ISSUES_QUERY = """
query($filter: IssueFilter, $after: String) {
  issues(first: 50, after: $after, filter: $filter) {
    pageInfo { hasNextPage endCursor }
    nodes {
      identifier title createdAt updatedAt state { type } labels { nodes { name } }
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


def parse_time(text):
    return datetime.fromisoformat(text.replace("Z", "+00:00"))


def label_names(issue):
    return {label["name"] for label in issue["labels"]["nodes"]}


def last_event(issue, happened):
    times = [parse_time(event["createdAt"]) for event in issue["history"]["nodes"] if happened(event)]
    return max(times, default=None)


def entered_state(issue):
    return last_event(issue, lambda event: (event["toState"] or {}).get("type") == issue["state"]["type"])


def unblocked_at(blocker):
    """When a blocker stopped blocking, or None while it still blocks."""
    times = []
    if blocker["state"]["type"] in CLOSED:
        times.append(entered_state(blocker))
    if CODE_COMPLETE in label_names(blocker):
        times.append(last_event(blocker, lambda event: CODE_COMPLETE in {l["name"] for l in event["addedLabels"] or []}))
    if not times:
        return None
    return min(filter(None, times), default=parse_time(blocker["updatedAt"]))


def busy(sessions):
    return [session for session in sessions if session["busy"] and not session["archived"]]


def builder_tickets(sessions):
    return {match[1] for session in busy(sessions) if (match := BUILDER.match(session["title"]))}


def unattributed(sessions):
    """Busy sessions whose title names no ticket."""
    return [session for session in busy(sessions) if not TICKET.search(session["title"])]


def ready_issues(issues, sessions, instance):
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
        times = [None if blocker in building else unblocked_at(by_id[blocker]) for blocker in blockers]
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
        lines.append("unattributed (busy, the title names no ticket; a builder must start \"Linear ADE-<n>\"):")
        lines += [f"{session['id'][-8:]}  {session['title']}" for session in unattributed_sessions]
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


def linear_issues(key, issue_filter):
    issues, after = [], None
    while True:
        request = urllib.request.Request(
            "https://api.linear.app/graphql",
            data=json.dumps({"query": ISSUES_QUERY, "variables": {"filter": issue_filter, "after": after}}).encode(),
            headers={"Authorization": key, "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=30) as response:
            body = json.load(response)
        if "errors" in body:
            raise RuntimeError(f"Linear: {body['errors'][0]['message']}")
        page = body["data"]["issues"]
        issues += page["nodes"]
        if not page["pageInfo"]["hasNextPage"]:
            return issues
        after = page["pageInfo"]["endCursor"]


def open_issues_and_blockers():
    """The open ADE issues, plus the closed issues that block one of them."""
    key, team = linear_key(), {"key": {"eq": "ADE"}}
    issues = linear_issues(key, {"team": team, "state": {"type": {"nin": list(CLOSED)}}})
    known = {issue["identifier"] for issue in issues}
    missing = sorted({int(r["issue"]["identifier"].split("-")[1]) for issue in issues
                      for r in issue["inverseRelations"]["nodes"]
                      if r["type"] == "blocks" and r["issue"]["identifier"] not in known})
    return issues + (linear_issues(key, {"team": team, "number": {"in": missing}}) if missing else [])


def sessions_of(instance):
    cli = os.environ.get("MONOCODE_CLI") or INSTANCE_CLIS[instance]
    answer = json.loads(subprocess.run([cli, "app", "sessions.list", "--json", "{}"], capture_output=True, text=True).stdout)
    if not answer.get("ok"):
        raise RuntimeError(f"monocode app sessions.list: {answer.get('error')}")
    return answer["result"]["sessions"]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--instance", required=True, choices=sorted(INSTANCE_CLIS))
    args = parser.parse_args()
    try:
        sessions = sessions_of(args.instance)
        issues = open_issues_and_blockers()
        ready, unlabelled = ready_issues(issues, sessions, args.instance)
        lines, code = report(ready, unlabelled, unattributed(sessions), datetime.now(timezone.utc))
    except Exception as err:
        print(f"ready-issues: {type(err).__name__}: {err}", file=sys.stderr)
        return 2
    print("\n".join(lines))
    return code


if __name__ == "__main__":
    sys.exit(main())
