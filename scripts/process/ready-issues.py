#!/usr/bin/env python3
"""Print the ADE issues whose blockers have all merged and that no builder works on.

    ready-issues.py

Reads three sources:
- Linear: the open ADE issues and their blockedBy relations (GraphQL API; the
  key from LINEAR_API_KEY, else the Linear MCP header in ~/.omp/agent/mcp.json).
- GitHub: the PRs on tyler/main (gh). A blocker has merged when its Linear
  state is Done or Canceled, or when it has no open sub-issue and every PR
  linked to it is merged and at least one is. A PR is linked by a Linear
  attachment, its title prefix `ADE-<n>` or an `ade-<n>` in its branch. PRs
  closed without a merge do not count. A ticket built in several PRs counts as
  merged once every PR opened so far has merged.
- MonoCode: the busy sessions (`app sessions.list`) and the branch of each
  session's worktree (`app worktrees.list`). A session is a builder of an issue
  when its title or its branch names the ticket id. The CLI is MONOCODE_CLI,
  else the first app bundle that exists.
Only Todo issues with no linked PR yet are candidates. "Ready since" is the
latest merge time of the issue's blockers, so an issue with no blockers has
none and never sets exit 1.
Exit 0: no issue has waited too long. Exit 1: an issue with blockers has been
ready for more than 60 minutes with no builder. Exit 2: a source failed.
"""
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
BASE = "tyler/main"
MAX_WAIT = timedelta(minutes=60)
MONOCODE_CLIS = [
    "/Applications/MonoCode 2.app/Contents/MacOS/monocode",
    "/Applications/MonoCode.app/Contents/MacOS/monocode",
]
ISSUES_QUERY = """
query($after: String) {
  issues(first: 100, after: $after, filter: {team: {key: {eq: "ADE"}}, state: {type: {nin: ["completed", "canceled"]}}}) {
    pageInfo { hasNextPage endCursor }
    nodes {
      identifier title state { type } attachments { nodes { url } }
      inverseRelations { nodes { type issue {
        identifier state { type } completedAt canceledAt attachments { nodes { url } }
        children { nodes { state { type } } }
      } } }
    }
  }
}"""
PR_URL = re.compile(rf"github\.com/{REPO}/pull/(\d+)$", re.IGNORECASE)
TITLE_TICKET = re.compile(r"^\s*(ADE-\d+)\b", re.IGNORECASE)
TICKET = re.compile(r"\b(ADE-\d+)\b", re.IGNORECASE)


@dataclass(frozen=True)
class Ready:
    id: str
    title: str
    since: datetime | None


def parse_time(text):
    return datetime.fromisoformat(text.replace("Z", "+00:00"))


def tickets_in(text):
    return {match.upper() for match in TICKET.findall(text or "")}


def linked_prs(prs):
    """A function from an issue to its PRs on tyler/main, leaving out PRs closed without a merge."""
    by_number = {pr["number"]: pr for pr in prs}
    by_ticket = {}
    for pr in prs:
        title = TITLE_TICKET.match(pr["title"])
        for ticket in tickets_in(pr["headRefName"]) | ({title[1].upper()} if title else set()):
            by_ticket.setdefault(ticket, {})[pr["number"]] = pr

    def live(issue):
        linked = dict(by_ticket.get(issue["identifier"], {}))
        for attachment in issue["attachments"]["nodes"]:
            url = PR_URL.search(attachment["url"])
            if url and int(url[1]) in by_number:
                linked[int(url[1])] = by_number[int(url[1])]
        return [pr for pr in linked.values() if pr["state"] != "CLOSED"]

    return live


def merged_at(issue, live_prs):
    """When a blocker merged, or None while it has not."""
    pr_times = [parse_time(pr["mergedAt"]) for pr in live_prs if pr["state"] == "MERGED"]
    state = issue["state"]["type"]
    if state == "canceled":
        return parse_time(issue["canceledAt"])
    if state == "completed":
        return max(pr_times, default=parse_time(issue["completedAt"]))
    open_child = any(child["state"]["type"] not in ("completed", "canceled") for child in issue["children"]["nodes"])
    if live_prs and len(pr_times) == len(live_prs) and not open_child:
        return max(pr_times)
    return None


def builder_tickets(sessions, worktrees):
    """Ticket ids named by a busy session's title or by its worktree's branch."""
    branch_of = {sid: tree.get("branch") or "" for tree in worktrees for sid in tree["sessionIds"]}
    named = set()
    for session in sessions:
        if session["busy"] and not session["archived"]:
            named |= tickets_in(session["title"]) | tickets_in(branch_of.get(session["id"]))
    return named


def ready_issues(issues, prs, sessions, worktrees):
    """Todo issues with no PR yet, whose blockers have all merged and that no busy session builds."""
    live = linked_prs(prs)
    building = builder_tickets(sessions, worktrees)
    ready = []
    for issue in issues:
        if issue["state"]["type"] != "unstarted" or issue["identifier"] in building or live(issue):
            continue
        blockers = [r["issue"] for r in issue["inverseRelations"]["nodes"] if r["type"] == "blocks"]
        times = [merged_at(blocker, live(blocker)) for blocker in blockers]
        if None not in times:
            ready.append(Ready(issue["identifier"], issue["title"], max(times, default=None)))
    return sorted(ready, key=lambda r: (r.since is None, r.since or datetime.min, r.id))


def report(ready, now):
    """Output lines and the exit code."""
    lines, late = [], 0
    for issue in ready:
        if issue.since is None:
            lines.append(f"{issue.id}  no blockers                {issue.title}")
            continue
        waited = now - issue.since
        late += waited > MAX_WAIT
        when = issue.since.astimezone().strftime("%Y-%m-%d %H:%M")
        lines.append(f"{issue.id}  ready since {when} ({int(waited.total_seconds() // 60)} min)  {issue.title}")
    lines.append(f"ready-issues: {len(ready)} ready with no builder, {late} waiting more than 60 min.")
    return lines, 1 if late else 0


def linear_key():
    key = os.environ.get("LINEAR_API_KEY")
    if not key:
        config = json.loads((Path.home() / ".omp/agent/mcp.json").read_text())
        key = config["mcpServers"]["linear"]["headers"]["Authorization"]
    key = key.removeprefix("Bearer ").strip()
    return key if key.startswith("lin_api_") else f"Bearer {key}"


def linear_issues():
    issues, after, key = [], None, linear_key()
    while True:
        request = urllib.request.Request(
            "https://api.linear.app/graphql",
            data=json.dumps({"query": ISSUES_QUERY, "variables": {"after": after}}).encode(),
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


def monocode(action):
    cli = os.environ.get("MONOCODE_CLI") or next(filter(os.path.exists, MONOCODE_CLIS), None)
    if cli is None:
        raise FileNotFoundError("no MonoCode CLI: set MONOCODE_CLI")
    answer = json.loads(subprocess.run([cli, "app", action, "--json", "{}"], capture_output=True, text=True).stdout)
    if not answer.get("ok"):
        raise RuntimeError(f"monocode app {action}: {answer.get('error')}")
    return answer["result"]


def main():
    try:
        issues = linear_issues()
        prs = json.loads(subprocess.run(
            ["gh", "pr", "list", "-R", REPO, "--base", BASE, "--state", "all", "--limit", "1000",
             "--json", "number,title,headRefName,state,mergedAt"],
            check=True, capture_output=True, text=True,
        ).stdout)
        sessions = monocode("sessions.list")["sessions"]
        worktrees = monocode("worktrees.list")["worktrees"]
    except (OSError, KeyError, RuntimeError, ValueError, subprocess.CalledProcessError) as err:
        print(f"ready-issues: {err}", file=sys.stderr)
        return 2
    lines, code = report(ready_issues(issues, prs, sessions, worktrees), datetime.now(timezone.utc))
    print("\n".join(lines))
    return code


if __name__ == "__main__":
    sys.exit(main())
