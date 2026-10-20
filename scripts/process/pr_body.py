#!/usr/bin/env python3
"""gate/integrity: the PR body must state the size, the self-pass and the file caps.

    pr_body.py <pr-number> --nt <non-test lines added, from scripts/process/tier.py>

Prints one "pr-body: missing <item>: <hint>" line per missing item and exits 1,
or prints "pr-body: ok" and exits 0. A PR under 100 non-test lines that touches
a file of another open PR also gets a ::warning:: line, because a change that
small costs less to review inside that PR. The warning never changes the exit code.
Reads GitHub with gh only; GATE_REPO names the repo.
"""
import argparse
import json
import os
import re
import subprocess
import sys

TINY_NT = 100
ADE_TITLE = re.compile(r"ADE-\d+")
TICKET_PART = re.compile(r"Ticket-part: (\d+)/(\d+)")
SIZE_COUNTS = re.compile(r"\+\s?\d+\s*/?\s*[-\u2212]\s?\d+")
HINTS = {
    "size": 'add a line like "Size: +120/-30 in 4 files, 90 non-test lines"',
    "self-pass": 'add a line like "Self-pass: I ran thermo-nuclear and deslop on this diff"',
    "file-caps": 'add a line like "File caps: MilestoneTimeline.tsx 451 lines (cap 600); new files 264 and 122"',
    "ticket-part": 'an ADE ticket PR needs the exact line "Ticket-part: <n>/<N>" (this PR is part n of N, 1 <= n <= N)',
}


def lint_body(body: str, title: str = "") -> list[str]:
    """Names of the missing items, in the order size, self-pass, file-caps. Matching ignores case.
    A format lint: it checks that each item is stated, not that it is true.

    size: a line that starts with "Size" once leading #, -, * and spaces are removed,
    where its section (to the next heading, for a heading) or that line and the next 3
    non-empty lines (for any other line) hold a +N/-N and "non-test". self-pass: one line
    names deslop and thermo-nuclear (or thermonuclear). file-caps: one line holds "file caps".
    ticket-part: only for a title that starts with ADE-<digits>, the exact line
    "Ticket-part: <n>/<N>" with integers 1 <= n <= N.
    """
    lower = body.lower()
    lines = lower.splitlines()
    present = {
        "size": _states_size(lines),
        "self-pass": any(
            "deslop" in line and ("thermo-nuclear" in line or "thermonuclear" in line) for line in lines
        ),
        "file-caps": "file caps" in lower,
    }
    if ADE_TITLE.match(title):
        present["ticket-part"] = any(_ticket_part_ok(line) for line in body.splitlines())
    return [item for item, ok in present.items() if not ok]


def _ticket_part_ok(line: str) -> bool:
    match = TICKET_PART.fullmatch(line.strip())
    return bool(match) and 1 <= int(match[1]) <= int(match[2])


def _states_size(lines: list[str]) -> bool:
    lines = [line for line in lines if line.strip()]
    for i, line in enumerate(lines):
        if not line.lstrip("#-* ").startswith("size"):
            continue
        if line.startswith("#"):
            end = next((j for j in range(i + 1, len(lines)) if lines[j].startswith("#")), len(lines))
        else:
            end = i + 4
        window = "\n".join(lines[i:end])
        if SIZE_COUNTS.search(window) and "non-test" in window:
            return True
    return False


def own_stack(number: int, open_prs: list[dict]) -> set[int]:
    """This PR and the open PRs stacked under or over it (base branch = another's head branch)."""
    by_head = {pr["headRefName"]: pr for pr in open_prs}
    stack, todo = set(), [pr for pr in open_prs if pr["number"] == number]
    while todo:
        pr = todo.pop()
        if pr["number"] in stack:
            continue
        stack.add(pr["number"])
        todo += [by_head[pr["baseRefName"]]] if pr["baseRefName"] in by_head else []
        todo += [other for other in open_prs if other["baseRefName"] == pr["headRefName"]]
    return stack


def tiny_overlaps(nt: int, files: set[str], open_prs: list[dict], number: int) -> list[tuple[int, list[str]]]:
    """(PR number, sorted shared paths) for each open PR outside this PR's stack that shares
    a path, by PR number. Empty when the PR has TINY_NT or more non-test lines."""
    if nt >= TINY_NT:
        return []
    stack = own_stack(number, open_prs) | {number}
    overlaps = []
    for pr in open_prs:
        shared = files.intersection(f["path"] for f in pr["files"])
        if shared and pr["number"] not in stack:
            overlaps.append((pr["number"], sorted(shared)))
    return sorted(overlaps)


def _command_text(text: str) -> str:
    """Escapes text for a workflow command, as @actions/core does, so a path cannot end the line."""
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def decide(
    body: str, files: set[str], open_prs: list[dict] | None, nt: int | None, number: int, title: str = ""
) -> tuple[list[str], int]:
    """The output lines and the exit code. Only a missing item fails; the overlap warning never does."""
    missing = lint_body(body, title)
    lines = [f"pr-body: missing {item}: {HINTS[item]}" for item in missing] or ["pr-body: ok"]
    if nt is not None and open_prs is not None:
        for other, paths in tiny_overlaps(nt, files, open_prs, number):
            lines.append(
                f"::warning::tiny PR ({nt} non-test lines) touches the same files as open PR #{other}: "
                f"{_command_text(', '.join(paths))}. Fold it into that PR or the next PR on these files."
            )
    return lines, 1 if missing else 0


def _gh_json(*args: str):
    return json.loads(subprocess.run(["gh", *args], check=True, stdout=subprocess.PIPE, text=True).stdout)


def main() -> int:
    parser = argparse.ArgumentParser(description="Check a PR body for the gate/integrity items.")
    parser.add_argument("number", type=int, help="pull request number")
    parser.add_argument("--nt", type=int, help="non-test lines added, from scripts/process/tier.py; no overlap check without it")
    args = parser.parse_args()
    repo = os.environ.get("GATE_REPO") or "tylers2222/Monocode-Unlocked"
    try:
        pr = _gh_json("pr", "view", str(args.number), "-R", repo, "--json", "body,files,title")
        open_prs = None
        if args.nt is not None and args.nt < TINY_NT:
            fields = "number,files,baseRefName,headRefName"
            open_prs = _gh_json("pr", "list", "-R", repo, "--state", "open", "--limit", "100", "--json", fields)
    except subprocess.CalledProcessError as err:
        print(f"pr-body: {' '.join(err.cmd)} exited {err.returncode}", file=sys.stderr)
        return 2
    files = {f["path"] for f in pr["files"]}
    lines, code = decide(pr["body"] or "", files, open_prs, args.nt, args.number, pr.get("title", ""))
    print("\n".join(lines))
    return code


if __name__ == "__main__":
    sys.exit(main())
