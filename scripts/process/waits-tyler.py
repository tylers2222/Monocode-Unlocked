#!/usr/bin/env python3
"""Fail a pull request whose waits-tyler label names a category off the fixed list.

    waits-tyler.py --event <github event JSON>
    waits-tyler.py --pr <number> [--repo <owner/name>]

A hold on Tyler is a label `waits-tyler:<category>`, and the categories are the
lines of waits-tyler-categories.txt. Reversible choices (wording, UI strings,
plan options, a ship GO) get no label: General picks and the PR merges.
Exit 0: no bad label. Exit 1: a bad label, printed one per line. Exit 2: usage
or unreadable input.
"""
import argparse
import json
import subprocess
import sys
from pathlib import Path

PREFIX = "waits-tyler"
CATEGORIES = frozenset(
    Path(__file__).with_name("waits-tyler-categories.txt").read_text().split()
)
REPO = "tylers2222/Monocode-Unlocked"


def category(label):
    """The category of a hold label, "" for a hold label without one, None for other labels."""
    name = label.lower()
    if name != PREFIX and not name.startswith(PREFIX + ":"):
        return None
    return name[len(PREFIX) + 1 :]


def is_hold(label):
    return category(label) in CATEGORIES


def bad_holds(labels):
    return [label for label in labels if category(label) is not None and not is_hold(label)]


def pr_labels(args):
    if args.event:
        event = json.loads(Path(args.event).read_text())
        return [label["name"] for label in event["pull_request"]["labels"]]
    out = subprocess.run(
        ["gh", "pr", "view", str(args.pr), "-R", args.repo, "--json", "labels"],
        check=True, capture_output=True, text=True,
    ).stdout
    return [label["name"] for label in json.loads(out)["labels"]]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--event", help="GitHub event JSON (GITHUB_EVENT_PATH)")
    source.add_argument("--pr", type=int, help="PR number, read with gh")
    parser.add_argument("--repo", default=REPO)
    args = parser.parse_args()
    try:
        labels = pr_labels(args)
    except (OSError, KeyError, TypeError, json.JSONDecodeError, subprocess.CalledProcessError) as err:
        print(f"waits-tyler: cannot read the PR labels: {err}", file=sys.stderr)
        return 2
    bad = bad_holds(labels)
    for label in bad:
        print(f"waits-tyler: '{label}' is not a hold category. Allowed: "
              + ", ".join(f"{PREFIX}:{c}" for c in sorted(CATEGORIES)))
    if bad:
        print("waits-tyler: a reversible choice gets no label. General picks, logs it in decisions.md, and the PR merges.")
        return 1
    print(f"waits-tyler: ok ({', '.join(filter(is_hold, labels)) or 'no hold'})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
