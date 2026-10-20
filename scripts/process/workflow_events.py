"""gate/integrity: only .github/workflows/gate-integrity.yml may use pull_request_target.

The repository's Actions policy allows that event for that file only, and a
pull_request_target workflow that checks out or runs PR code is the "pwn request"
pattern. This fails a PR whose head version of any other workflow file it changes
names the event outside a comment.

Usage: workflow_events.py <base-rev> <head-rev>
Reads git objects only. Prints one ::error:: line per file and exits 1, or prints
"workflow-events: ok" and exits 0.
"""
import re
import subprocess
import sys

ALLOWED = '.github/workflows/gate-integrity.yml'
WORKFLOW = re.compile(r'^\.github/workflows/[^/]+\.ya?ml$')
EVENT = re.compile(r'\bpull_request_target\b')
COMMENT = re.compile(r'(^|\s)#.*$', re.M)


def violations(files):
    """files: {path: head text} for the files the PR changes. The workflow files other than
    ALLOWED that name pull_request_target outside a comment, sorted."""
    return sorted(path for path, text in files.items()
                  if path != ALLOWED and WORKFLOW.match(path) and EVENT.search(COMMENT.sub('', text)))


def git(*args):
    return subprocess.run(['git', *args], check=True, capture_output=True, text=True).stdout


def main(argv):
    if len(argv) != 3:
        sys.exit('usage: workflow_events.py <base-rev> <head-rev>')
    base, head = argv[1:]
    merge_base = git('merge-base', base, head).strip()
    paths = git('diff', '--name-only', '--no-renames', '--diff-filter=d', merge_base, head, '--', '.github/workflows').split()
    bad = violations({path: git('show', f'{head}:{path}') for path in paths})
    for path in bad:
        print(f'::error file={path}::{path} uses pull_request_target; only {ALLOWED} may use it')
    print('workflow-events: ' + (f'{len(bad)} file(s) use pull_request_target' if bad else 'ok'))
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
