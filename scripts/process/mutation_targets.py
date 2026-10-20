"""What gate/mutation mutates: the changed lines of non-test TypeScript files under src/
(the root vitest config tests only src/), as Stryker `mutate` specs (`path:start-end`),
and whether any non-test Rust file changed (cargo mutants --in-diff finds its own lines).

Usage: git diff --unified=0 <merge-base> <head> | mutation_targets.py
Prints `rust=yes|no` and `stryker=<comma-separated specs>` lines for GITHUB_OUTPUT.
"""
import re
import sys

from tier import TEST

TS = re.compile(r'^src/.*\.(ts|tsx)$')
HUNK = re.compile(r'^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@')


def targets(diff_text):
    """(rust changed?, stryker specs) from a --unified=0 diff."""
    rust, specs, path = False, [], None
    for line in diff_text.splitlines():
        if line.startswith('+++ '):
            path = line[6:] if line.startswith('+++ b/') else None
            if path and TEST.search(path):
                path = None
            rust = rust or bool(path and path.endswith('.rs'))
        elif path and TS.search(path) and not path.endswith('.d.ts'):
            hunk = HUNK.match(line)
            if hunk and hunk.group(2) != '0':
                start = int(hunk.group(1))
                specs.append(f'{path}:{start}-{start + int(hunk.group(2) or 1) - 1}')
    return rust, specs


def main():
    rust, specs = targets(sys.stdin.read())
    print(f'rust={"yes" if rust else "no"}')
    print(f'stryker={",".join(specs)}')


if __name__ == '__main__':
    main()
