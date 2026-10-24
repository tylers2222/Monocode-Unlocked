#!/usr/bin/env python3
"""Fail a pull request that does not have exactly one instance label.

    instance-label.py --event <github event JSON>
    instance-label.py --pr <number> [--repo <owner/name>]

Every PR names the MonoCode instance that owns it with one label, `instance:1`
or `instance:2`, because pr-cap.py counts the open-PR cap per instance.
Exit 0: exactly one valid instance label. Exit 1: none, more than one, or an
`instance:` label with another value. Exit 2: usage or unreadable input.
"""
import sys

import pr_labels

INSTANCES = ("instance:1", "instance:2")


def check(labels):
    found = [label for label in labels if label.lower().startswith("instance:")]
    if len(found) == 1 and found[0] in INSTANCES:
        print(f"instance-label: ok ({found[0]})")
        return 0
    print(f"instance-label: a PR needs exactly one of {', '.join(INSTANCES)}; it has "
          + (", ".join(found) or "none"))
    return 1


if __name__ == "__main__":
    sys.exit(pr_labels.run_check(check, __doc__))
