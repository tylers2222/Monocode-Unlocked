#!/usr/bin/env python3
"""Count the open PRs on tyler/main that use a slot of the 4-open-PR cap.

    pr-cap.py [--repo <owner/name>]

A PR with a valid waits-tyler:<category> label waits on Tyler and uses no
slot. A PR with an off-list waits-tyler label still uses one: that label fails
waits-tyler.py and is no hold.
Exit 0: a slot is free. Exit 1: the cap is full, so no new builder starts.
Exit 2: gh failed.
"""
import argparse
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

CAP = 4
BASE = "tyler/main"

_spec = importlib.util.spec_from_file_location("waits_tyler", Path(__file__).with_name("waits-tyler.py"))
waits_tyler = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(waits_tyler)


def split(prs):
    """(PRs that use a cap slot, PRs held on Tyler)."""
    counted, held = [], []
    for pr in prs:
        (held if any(waits_tyler.is_hold(label["name"]) for label in pr["labels"]) else counted).append(pr)
    return counted, held


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=waits_tyler.REPO)
    args = parser.parse_args()
    try:
        out = subprocess.run(
            ["gh", "pr", "list", "-R", args.repo, "--base", BASE, "--state", "open",
             "--limit", "200", "--json", "number,title,labels"],
            check=True, capture_output=True, text=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as err:
        print(f"pr-cap: gh pr list failed: {err}", file=sys.stderr)
        return 2
    counted, held = split(json.loads(out))
    for pr in counted:
        print(f"#{pr['number']} {pr['title']}")
    for pr in held:
        holds = ", ".join(label["name"] for label in pr["labels"] if waits_tyler.is_hold(label["name"]))
        print(f"#{pr['number']} {pr['title']} (not counted: {holds})")
    full = len(counted) >= CAP
    print(f"pr-cap: {len(counted)} of {CAP} slots used, {len(held)} held on Tyler. "
          + ("Cap full: no new builder." if full else "A slot is free."))
    return 1 if full else 0


if __name__ == "__main__":
    sys.exit(main())
