#!/usr/bin/env python3
"""Count the open PRs of one MonoCode instance that use a slot of its 4-open-PR cap.

    pr-cap.py --instance <1|2> [--repo <owner/name>]

The cap is per instance. A PR belongs to the instance its `instance:<n>` label
names (instance-label.py checks every PR has one). Every open PR on the repo
counts, stacked PRs and drafts too, except a PR held on Tyler (a valid
waits-tyler:<category> label) and a PR labelled process-fix. A PR with an
off-list waits-tyler label still counts: that label fails waits-tyler.py and
is no hold.
Exit 0: a slot is free. Exit 1: the cap is full, so no new builder starts.
Exit 2: gh failed or its output cannot be read.
"""
import argparse
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pr_labels

CAP = 4
EXEMPT = "process-fix"

_spec = importlib.util.spec_from_file_location("waits_tyler", Path(__file__).with_name("waits-tyler.py"))
waits_tyler = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(waits_tyler)


def exemptions(pr):
    """The labels that keep a PR out of the cap."""
    return [name for name in (label["name"] for label in pr["labels"]) if name == EXEMPT or waits_tyler.is_hold(name)]


def split(prs, instance):
    """(PRs of the instance that use a cap slot, PRs of the instance that do not)."""
    counted, exempt = [], []
    for pr in prs:
        if f"instance:{instance}" in (label["name"] for label in pr["labels"]):
            (exempt if exemptions(pr) else counted).append(pr)
    return counted, exempt


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--instance", required=True, choices=["1", "2"])
    parser.add_argument("--repo", default=pr_labels.REPO)
    args = parser.parse_args()
    try:
        prs = json.loads(subprocess.run(
            ["gh", "pr", "list", "-R", args.repo, "--state", "open", "--limit", "500",
             "--json", "number,title,baseRefName,labels"],
            check=True, capture_output=True, text=True,
        ).stdout)
        counted, exempt = split(prs, args.instance)
    except (OSError, KeyError, TypeError, ValueError, subprocess.CalledProcessError) as err:
        print(f"pr-cap: cannot read the open PRs: {err}", file=sys.stderr)
        return 2
    for pr in counted:
        print(f"#{pr['number']} -> {pr['baseRefName']}  {pr['title']}")
    for pr in exempt:
        print(f"#{pr['number']} -> {pr['baseRefName']}  {pr['title']} (not counted: {', '.join(exemptions(pr))})")
    full = len(counted) >= CAP
    print(f"pr-cap: instance {args.instance}: {len(counted)} of {CAP} slots used, {len(exempt)} not counted. "
          + ("Cap full: no new builder." if full else "A slot is free."))
    return 1 if full else 0


if __name__ == "__main__":
    sys.exit(main())
