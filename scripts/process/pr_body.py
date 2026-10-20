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
SIZE_COUNTS = re.compile(r"\+\s?\d+\s*/?\s*[-\u2212]\s?\d+")
HINTS = {
    "size": 'add a line like "Size: +120/-30 in 4 files, 90 non-test lines"',
    "self-pass": 'add a line like "Self-pass: I ran thermo-nuclear and deslop on this diff"',
    "file-caps": 'add a line like "File caps: MilestoneTimeline.tsx 451 lines (cap 600); new files 264 and 122"',
}


def lint_body(body: str) -> list[str]:
    """Names of the missing items, in the order size, self-pass, file-caps. Matching ignores case.

    size: a line that starts with "Size" once leading #, -, * and spaces are
    removed, where that line and the next 3 non-empty lines hold a +N/-N and
    "non-test". self-pass: one line names deslop and thermo-nuclear (or
    thermonuclear). file-caps: one line holds "file caps".
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
    return [item for item, ok in present.items() if not ok]


def _states_size(lines: list[str]) -> bool:
    lines = [line for line in lines if line.strip()]
    for i, line in enumerate(lines):
        if line.lstrip("#-* ").startswith("size"):
            window = "\n".join(lines[i : i + 4])
            if SIZE_COUNTS.search(window) and "non-test" in window:
                return True
    return False


def tiny_overlaps(nt: int, files: set[str], open_prs: list[dict], number: int) -> list[tuple[int, list[str]]]:
    """(PR number, sorted shared paths) for each other open PR that shares a path, by PR number.

    Empty when the PR has TINY_NT or more non-test lines.
    """
    if nt >= TINY_NT:
        return []
    overlaps = []
    for pr in open_prs:
        shared = files.intersection(f["path"] for f in pr["files"])
        if shared and pr["number"] != number:
            overlaps.append((pr["number"], sorted(shared)))
    return sorted(overlaps)


def _gh_json(*args: str):
    return json.loads(subprocess.run(["gh", *args], check=True, stdout=subprocess.PIPE, text=True).stdout)


def main() -> int:
    parser = argparse.ArgumentParser(description="Check a PR body for the gate/integrity items.")
    parser.add_argument("number", type=int, help="pull request number")
    parser.add_argument("--nt", type=int, required=True, help="non-test lines added, from scripts/process/tier.py")
    args = parser.parse_args()
    repo = os.environ.get("GATE_REPO") or "tylers2222/Monocode-Unlocked"
    try:
        pr = _gh_json("pr", "view", str(args.number), "-R", repo, "--json", "body,files")
        open_prs = _gh_json("pr", "list", "-R", repo, "--state", "open", "--limit", "100", "--json", "number,files")
    except subprocess.CalledProcessError as err:
        print(f"pr-body: {' '.join(err.cmd)} exited {err.returncode}", file=sys.stderr)
        return 2

    missing = lint_body(pr["body"])
    for item in missing:
        print(f"pr-body: missing {item}: {HINTS[item]}")
    if not missing:
        print("pr-body: ok")
    files = {f["path"] for f in pr["files"]}
    for other, paths in tiny_overlaps(args.nt, files, open_prs, args.number):
        print(
            f"::warning::tiny PR ({args.nt} non-test lines) touches the same files as open PR #{other}: "
            f"{', '.join(paths)}. Fold it into that PR or the next PR on these files."
        )
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
