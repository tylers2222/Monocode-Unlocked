"""Review tier of a pull request: the riskiest code it touches.

T3: gates, state paths (store, DB, IPC, hooks) or concurrency code. T1: docs, config
and tests only, or pure model/script code with tests. T2: everything else. Agent
rules, prompts and help text inside Rust raw strings are behaviour, so they are T2
at least and never T1 (PR #116). The rules come from the 2026-10-20 process review,
section 4b.

Usage: tier.py <pr-number> [--apply]
Prints the tier and its reasons. With GITHUB_OUTPUT set, writes `tier` and `nt`
(non-test lines added). --apply sets the label tier:T<n> and removes any other
tier label. GATE_REPO overrides the repository.
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
GATE_PATH = re.compile(r'^\.github/workflows/gate|^scripts/gate/|integrity|heavy-lock')
T3_PATH = re.compile(r'session_store|mono_messages|mono_gate|ask_gate|asks\.rs|active_clock|migrat|control_cli|/store/|Store\.ts'
                     r'|(?<![Cc]argo\.)(?<!-)lock\.|scheduler|/hooks/|/use[A-Z]\w*\.tsx?$')
PURE_DIR = re.compile(r'/model/|^scripts/')
NOT_CODE_LINE = re.compile(r'^\s*(//|/\*|\*|#|"|\'|`|$)')
T3_CODE = re.compile(r'\b(useState|useEffect|useLayoutEffect|useReducer|useRef|useSyncExternalStore|useCallback|useMemo)\b'
                     r'|\binvoke\(|\blisten\(|#\[tauri::command\]|rusqlite|\.execute\(|CREATE TABLE|ALTER TABLE|BEGIN IMMEDIATE'
                     r'|Mutex|RwLock|tokio::spawn|thread::spawn|AtomicU|setTimeout|setInterval|flock|clock_gettime')
RAW_OPEN = re.compile(r'(?<!\w)b?r(#+)"')
TIER_LABEL = re.compile(r'^tier:T[123]$')

Line = collections.namedtuple('Line', 'kind text raw')  # kind: '+', '-', ' ' or '@'


def raw_string_lines(source):
    """1-based numbers of the lines of a Rust file that are text of a multi-line raw
    string r#"..."#: they start inside one, or open one that ends on a later line."""
    inside, closer = set(), None
    for no, line in enumerate(source.splitlines(), 1):
        if closer:
            inside.add(no)
        pos = 0
        while True:
            if closer is None:
                opened = RAW_OPEN.search(line, pos)
                if not opened:
                    break
                closer, pos = '"' + opened.group(1), opened.end()
            end = line.find(closer, pos)
            if end < 0:
                break
            closer, pos = None, end + len(closer)
        if closer:
            inside.add(no)
    return inside


def parse_diff(text, raw):
    """Per file, the hunk lines in order. raw maps a path to the raw-string line
    numbers of its old and new versions; a line in one of them is flagged raw."""
    files, cur, old_no, new_no = collections.defaultdict(list), None, 0, 0
    for ln in text.splitlines():
        if ln.startswith('+++ '):
            cur = ln[6:] if ln.startswith('+++ b/') else None
        elif ln.startswith(('--- ', 'diff --git', 'index ')):
            continue
        elif ln.startswith('@@'):
            if cur:
                old_no, new_no = (int(n) for n in re.match(r'@@ -(\d+)(?:,\d+)? \+(\d+)', ln).groups())
                files[cur].append(Line('@', ln.split('@@')[-1], False))  # enclosing function name git prints
        elif cur and ln[:1] in '+- ':
            old_raw, new_raw = raw.get(cur, (set(), set()))
            kind = ln[:1]
            flag = old_no in old_raw if kind == '-' else new_no in new_raw
            files[cur].append(Line(kind, ln[1:], flag))
            old_no += kind != '+'
            new_no += kind != '-'
    return files


def tier(files, diff_text, raw=None):
    """files: [{'path', 'additions'}] as the GitHub API lists them. raw: per Rust
    path, (old, new) raw_string_lines of its two versions. Returns (tier, reasons)."""
    lines = parse_diff(diff_text, raw or {})
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
    rules_text = [p for p in code if any(l.raw for l in changed(p))]
    if rules_text:
        return 'T2', ['agent rules or help text in raw strings: ' + ', '.join(rules_text[:3])]
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


def raw_sets(repo, pr, files):
    """raw_string_lines of each Rust file at the merge base and at the head."""
    head, base = gh('api', f'repos/{repo}/pulls/{pr}', '--jq', '.head.sha + " " + .base.sha').split()
    merge_base = gh('api', f'repos/{repo}/compare/{base}...{head}', '--jq', '.merge_base_commit.sha').strip()
    show = lambda path, ref: gh('api', '-H', 'Accept: application/vnd.github.raw',
                                f'repos/{repo}/contents/{quote(path)}?ref={ref}', check=False)
    return {f['path']: (raw_string_lines(show(f['path'], merge_base)), raw_string_lines(show(f['path'], head)))
            for f in files if f['path'].endswith('.rs')}


def apply_label(repo, pr, t):
    labels = gh('api', f'repos/{repo}/issues/{pr}/labels', '--jq', '.[].name').splitlines()
    for old in labels:
        if TIER_LABEL.match(old) and old != f'tier:{t}':
            gh('api', '-X', 'DELETE', f'repos/{repo}/issues/{pr}/labels/{quote(old)}')
    if f'tier:{t}' not in labels:
        gh('api', '-X', 'POST', f'repos/{repo}/issues/{pr}/labels', '-f', f'labels[]=tier:{t}')


def main(argv):
    if len(argv) not in (2, 3) or not argv[1].isdigit() or argv[2:] not in ([], ['--apply']):
        sys.exit('usage: tier.py <pr-number> [--apply]')
    pr, repo = argv[1], os.environ.get('GATE_REPO', 'tylers2222/Monocode-Unlocked')
    files = [json.loads(l) for l in gh('api', '--paginate', f'repos/{repo}/pulls/{pr}/files',
                                        '--jq', '.[] | {path: .filename, additions}').splitlines()]
    t, why = tier(files, gh('pr', 'diff', pr, '-R', repo), raw_sets(repo, pr, files))
    nt = non_test_lines(files)
    print(f'tier: {t}, {nt} non-test lines added')
    for w in why:
        print(f'tier:   {w}')
    if os.environ.get('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a') as out:
            out.write(f'tier={t}\nnt={nt}\n')
    if '--apply' in argv:
        apply_label(repo, pr, t)


if __name__ == '__main__':
    main(sys.argv)
