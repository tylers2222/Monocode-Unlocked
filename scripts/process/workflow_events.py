"""gate/integrity: only .github/workflows/gate-integrity.yml may use pull_request_target.

The repository's Actions policy allows that event for that file only, and a
pull_request_target workflow that checks out or runs PR code is the "pwn request"
pattern. For each other workflow file the PR changes, its head version fails when:
its on: names pull_request_target (read by workflow_on.on_events); its on: cannot be
read; its on: block holds a backslash, a quoted key, an alias, an anchor or a tag (YAML
can spell or pull in the event that way); or its text names the event outside a comment.

Usage: workflow_events.py <base-rev> <head-rev>
Reads git objects only. Prints one ::error:: line per file and exits 1, or prints
"workflow-events: ok" and exits 0.
"""
import re
import subprocess
import sys

from workflow_on import ON, on_events

ALLOWED = '.github/workflows/gate-integrity.yml'
WORKFLOW = re.compile(r'^\.github/workflows/[^/]+\.ya?ml$')
COMMENT = re.compile(r'(^|\s)#.*$', re.M)
QUOTED = re.compile(r'''"[^"\n]*"|'[^'\n]*\'''')
QUOTED_KEY = re.compile(r'''^\s+(?:-\s+)?["'][^"'\n]*["']\s*:''', re.M)
NODE_PROPERTY = re.compile(r'(?:^|[\s\[{,:])[*&!]')
EVENT = re.compile(r'\bpull_request_target\b')


def on_block(text):
    """The on: line and the indented lines under it, comments removed."""
    lines = COMMENT.sub('', text).splitlines()
    start = next((i for i, line in enumerate(lines) if ON.match(line)), None)
    if start is None:
        return ''
    end = next((j for j in range(start + 1, len(lines)) if lines[j] and not lines[j][0].isspace()), len(lines))
    return '\n'.join(lines[start:end])


def problem(text):
    """Why a workflow other than gate-integrity.yml fails, or None."""
    events = on_events(text)
    if events is None:
        return 'its on: is in a form the gate cannot read'
    block = on_block(text)
    if '\\' in block or QUOTED_KEY.search(block) or NODE_PROPERTY.search(QUOTED.sub('', block)):
        return 'its on: has an escape, a quoted key, an alias, an anchor or a tag'
    if 'pull_request_target' in events or EVENT.search(COMMENT.sub('', text)):
        return 'it uses pull_request_target'
    return None


def violations(files):
    """files: {path: head text} for the files the PR changes. (path, reason) for each
    workflow file other than ALLOWED that fails, sorted by path."""
    found = ((path, problem(text)) for path, text in sorted(files.items()) if path != ALLOWED and WORKFLOW.match(path))
    return [(path, reason) for path, reason in found if reason]


def git(*args):
    return subprocess.run(['git', *args], check=True, capture_output=True, text=True).stdout


def main(argv):
    if len(argv) != 3:
        sys.exit('usage: workflow_events.py <base-rev> <head-rev>')
    base, head = argv[1:]
    merge_base = git('merge-base', base, head).strip()
    paths = git('diff', '--name-only', '--no-renames', '--diff-filter=d', merge_base, head, '--', '.github/workflows').split()
    bad = violations({path: git('show', f'{head}:{path}') for path in paths})
    for path, reason in bad:
        print(f'::error file={path}::{path}: {reason}; only {ALLOWED} may use pull_request_target')
    print('workflow-events: ' + (f'{len(bad)} file(s) fail' if bad else 'ok'))
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
