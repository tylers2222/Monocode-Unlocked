"""Review tier of a pull request: the riskiest code it touches.

T3: gates (including scripts/process/, the gate's own logic), state paths (store,
DB, IPC, hooks) or concurrency code. T1: docs, config and tests only, or pure
model/script code with tests. T2: everything else. Agent rules, prompts and help
text are behaviour, so a change in a pure-folder source file that can hold a
multi-line string (a JS/TS backtick, any Rust string), or a changed line of string
text, is T2 at least and never T1 (PR #116). The rules come from the 2026-10-20 process review, section 4b.

Usage: tier.py <pr-number> [--apply]
Prints the tier and its reasons. --apply makes tier:T<n> the PR's only tier label.
GATE_REPO overrides the repository.
"""
import collections
import json
import os
import re
import subprocess
import sys
from urllib.parse import quote

TEST = re.compile(r'(^|/)(tests?|__tests__|e2e|fixtures?|scenario|evals?)/|\.(test|spec)\.|tests\.rs$|_test\.|\.snap$')
DOC = re.compile(r'\.(md|txt)$|^docs/|CHANGELOG')
CONFIG = re.compile(r'(package(-lock)?\.json|Cargo\.lock|\.ya?ml|\.toml|\.json|\.eslintrc.*|\.npmrc|tsconfig.*)$')
GATE_PATH = re.compile(r'^\.github/workflows/gate|^scripts/gate/|^scripts/process/|integrity|heavy-lock')
T3_PATH = re.compile(r'session_store|mono_messages|mono_gate|ask_gate|asks\.rs|active_clock|migrat|control_cli|/store/|Store\.ts'
                     r'|(?<![Cc]argo\.)(?<!-)lock\.|scheduler|/hooks/|/use[A-Z]\w*\.tsx?$')
PURE_DIR = re.compile(r'/model/|^scripts/')
NOT_CODE_LINE = re.compile(r'^\s*(//|/\*|\*|#|"|\'|`|$)')
T3_CODE = re.compile(r'\b(useState|useEffect|useLayoutEffect|useReducer|useRef|useSyncExternalStore|useCallback|useMemo)\b'
                     r'|\binvoke\(|\blisten\(|#\[tauri::command\]|rusqlite|\.execute\(|CREATE TABLE|ALTER TABLE|BEGIN IMMEDIATE'
                     r'|Mutex|RwLock|tokio::spawn|thread::spawn|AtomicU|setTimeout|setInterval|flock|clock_gettime')
# A source file that can hold a multi-line string: a JS/TS file with a backtick anywhere
# (a template literal), a Rust file with any string (a plain Rust string can span lines too).
# Any changed line in such a file may be prompt text; no scan decides which, so this can
# only raise the tier.
STRING_OPENER = {'.rs': re.compile(r'"'), '.ts': re.compile(r'`')}
STRING_FILE = re.compile(r'\.(rs|[cm]?[jt]sx?)$')
# A changed line that is string text: it starts with a quote, holds a raw string, or
# holds a literal of 40+ characters, the length of a sentence of prose.
STRING_LINE = re.compile(r'^\s*(["\'`]|b?r#+")|(?<!\w)b?r#+"|"[^"]{40,}"|\'[^\']{40,}\'|`[^`]{40,}`')
TIER_LABEL = re.compile(r'^tier:T[123]$')

Line = collections.namedtuple('Line', 'kind text')  # kind: '+', '-', ' ' or '@'


def can_hold_multi_line_string(source, path):
    return bool(STRING_OPENER['.rs' if path.endswith('.rs') else '.ts'].search(source))


def parse_diff(text):
    """Per file, the hunk lines in order."""
    files, cur = collections.defaultdict(list), None
    for ln in text.splitlines():
        if ln.startswith('+++ '):
            cur = ln[6:] if ln.startswith('+++ b/') else None
        elif ln.startswith(('--- ', 'diff --git', 'index ')):
            continue
        elif ln.startswith('@@'):
            if cur:
                files[cur].append(Line('@', ln.split('@@')[-1]))  # enclosing function name git prints
        elif cur and ln[:1] in '+- ':
            files[cur].append(Line(ln[:1], ln[1:]))
    return files


def tier(files, diff_text, string_files=frozenset()):
    """files: [{'path', 'additions'}] as the GitHub API lists them. diff_text: the PR
    diff; empty or None when GitHub gives none (then the strictest tier). string_files:
    the pure-folder source paths whose merge-base or head version can hold a multi-line
    string (can_hold_multi_line_string). Returns (tier, reasons)."""
    if not diff_text:
        return 'T3', ['no diff from GitHub']
    lines = parse_diff(diff_text)
    changed = lambda p: [l for l in lines.get(p, []) if l.kind in '+-']
    prod = [f['path'] for f in files if not TEST.search(f['path']) and not DOC.search(f['path'])]
    why = []
    for p in prod:
        if CONFIG.search(p) and not GATE_PATH.search(p):
            continue
        if GATE_PATH.search(p):
            why.append(f'gate:{p}')
        elif T3_PATH.search(p) and any(not NOT_CODE_LINE.match(l.text) for l in changed(p)):
            why.append(f'path:{p}')
        else:
            m = T3_CODE.search('\n'.join(l.text for l in lines.get(p, [])))
            if m:
                why.append(f'code:{p}:{m.group(0)}')
    if why:
        return 'T3', why
    code = [p for p in prod if not CONFIG.search(p)]
    rules_text = [p for p in code if PURE_DIR.search(p) and STRING_FILE.search(p) and changed(p)
                  and (p in string_files or any(STRING_LINE.search(l.text) for l in changed(p)))]
    if rules_text:
        return 'T2', ['string text (agent rules, prompts, help) in: ' + ', '.join(rules_text[:3])]
    tests_touched = any(TEST.search(f['path']) for f in files)
    if not code:
        return 'T1', ['docs/config/tests only']
    if tests_touched and all(PURE_DIR.search(p) and not p.endswith('.tsx') for p in code):
        return 'T1', ['pure model/script code with tests: ' + ', '.join(code[:3])]
    return 'T2', ['app logic: ' + ', '.join(code[:3])]


def non_test_lines(files):
    return sum(f['additions'] for f in files if not TEST.search(f['path']) and not DOC.search(f['path']))


def gh(*args, check=True):
    run = subprocess.run(['gh', *args], capture_output=True, text=True)
    if check and run.returncode:
        sys.exit(f'tier: gh {args[0]} {args[1] if len(args) > 1 else ""} failed: {run.stderr.strip()}')
    return run.stdout if run.returncode == 0 else ''


def string_files(repo, pr, files):
    """The pure-folder source files whose merge-base or head version can hold a
    multi-line string: only there can string text change the tier."""
    head, base = gh('api', f'repos/{repo}/pulls/{pr}', '--jq', '.head.sha + " " + .base.sha').split()
    merge_base = gh('api', f'repos/{repo}/compare/{base}...{head}', '--jq', '.merge_base_commit.sha').strip()
    def show(path, ref):
        # '' when the file does not exist at ref; None when GitHub could not answer.
        run = subprocess.run(['gh', 'api', '-H', 'Accept: application/vnd.github.raw',
                              f'repos/{repo}/contents/{quote(path)}?ref={ref}'], capture_output=True, text=True)
        return run.stdout if run.returncode == 0 else '' if 'HTTP 404' in run.stderr else None

    return {p for p in (f['path'] for f in files) if PURE_DIR.search(p) and STRING_FILE.search(p)
            and any(text is None or can_hold_multi_line_string(text, p) for text in (show(p, merge_base), show(p, head)))}


def label_changes(labels, t):
    """(tier labels to remove, labels to add) so that tier:<t> is the PR's only tier label."""
    want = f'tier:{t}'
    return [l for l in labels if TIER_LABEL.match(l) and l != want], [] if want in labels else [want]


def main(argv):
    if len(argv) not in (2, 3) or not argv[1].isdigit() or argv[2:] not in ([], ['--apply']):
        sys.exit('usage: tier.py <pr-number> [--apply]')
    pr, repo = argv[1], os.environ.get('GATE_REPO', 'tylers2222/Monocode-Unlocked')
    files = [json.loads(l) for l in gh('api', '--paginate', f'repos/{repo}/pulls/{pr}/files',
                                        '--jq', '.[] | {path: .filename, additions}').splitlines()]
    diff = gh('pr', 'diff', pr, '-R', repo, check=False)
    t, why = tier(files, diff, string_files(repo, pr, files) if diff else frozenset())
    print(f'tier: {t}, {non_test_lines(files)} non-test lines added')
    for w in why:
        print(f'tier:   {w}')
    if '--apply' in argv:
        remove, add = label_changes(gh('api', f'repos/{repo}/issues/{pr}/labels', '--jq', '.[].name').splitlines(), t)
        for old in remove:
            gh('api', '-X', 'DELETE', f'repos/{repo}/issues/{pr}/labels/{quote(old)}')
        for new in add:
            gh('api', '-X', 'POST', f'repos/{repo}/issues/{pr}/labels', '-f', f'labels[]={new}')


if __name__ == '__main__':
    main(sys.argv)
