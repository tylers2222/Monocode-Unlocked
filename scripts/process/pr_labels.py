"""The command line and input shared by the PR label checks.

A check reads the labels of one PR from the GitHub event JSON (--event, for
gate/integrity) or with gh (--pr). It returns 0 when the labels pass and 1 when
they fail; input that cannot be read exits 2.
"""
import argparse
import json
import subprocess
import sys
from pathlib import Path

REPO = "tylers2222/Monocode-Unlocked"


def read_labels(args):
    if args.event:
        pull_request = json.loads(Path(args.event).read_text())["pull_request"]
    else:
        pull_request = json.loads(subprocess.run(
            ["gh", "pr", "view", str(args.pr), "-R", args.repo, "--json", "labels"],
            check=True, capture_output=True, text=True,
        ).stdout)
    labels = [label["name"] for label in pull_request["labels"]]
    if not all(isinstance(name, str) for name in labels):
        raise TypeError("a label name is not a string")
    return labels


def run_check(check, doc):
    """Parse the command line, read the labels and return the check's exit code."""
    parser = argparse.ArgumentParser(description=doc.splitlines()[0])
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--event", help="GitHub event JSON (GITHUB_EVENT_PATH)")
    source.add_argument("--pr", type=int, help="PR number, read with gh")
    parser.add_argument("--repo", default=REPO)
    args = parser.parse_args()
    try:
        labels = read_labels(args)
    except (OSError, KeyError, TypeError, ValueError, subprocess.CalledProcessError) as err:
        print(f"{Path(sys.argv[0]).stem}: cannot read the PR labels: {err}", file=sys.stderr)
        return 2
    return check(labels)
