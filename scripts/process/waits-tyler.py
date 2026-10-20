#!/usr/bin/env python3
"""Fail a pull request whose waits-tyler label names a category off the fixed list.

    waits-tyler.py --event <github event JSON>
    waits-tyler.py --pr <number> [--repo <owner/name>]

A hold on Tyler is a label `waits-tyler:<category>`, and the categories are the
lines of waits-tyler-categories.txt. Reversible choices (wording, UI strings,
plan options, a ship GO) get no label: General picks and the PR merges. Any
label that reads as a hold (`waits-tyler`, `waits_tyler`, `waits tyler`, any
case, any ending) must have that exact form.
Exit 0: no bad label. Exit 1: a bad label, printed one per line. Exit 2: usage
or unreadable input.
"""
import re
import sys
from pathlib import Path

import pr_labels

PREFIX = "waits-tyler:"
HOLD_LIKE = re.compile(r"\s*waits[-_ ]?tyler")
CATEGORIES = frozenset(
    Path(__file__).with_name("waits-tyler-categories.txt").read_text().split()
)


def category(label):
    """The category of a hold label, "" for a hold label in the wrong form, None for other labels."""
    name = label.lower()
    if not HOLD_LIKE.match(name):
        return None
    return name.removeprefix(PREFIX) if name.startswith(PREFIX) else ""


def is_hold(label):
    return category(label) in CATEGORIES


def check(labels):
    bad = [label for label in labels if category(label) is not None and not is_hold(label)]
    for label in bad:
        print(f"waits-tyler: '{label}' is not a hold category. Allowed: "
              + ", ".join(PREFIX + c for c in sorted(CATEGORIES)))
    if bad:
        print("waits-tyler: a reversible choice gets no label. General picks, logs it in decisions.md, and the PR merges.")
        return 1
    print(f"waits-tyler: ok ({', '.join(filter(is_hold, labels)) or 'no hold'})")
    return 0


if __name__ == "__main__":
    sys.exit(pr_labels.run_check(check, __doc__))
