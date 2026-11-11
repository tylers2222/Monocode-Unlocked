#!/usr/bin/env python3
"""The narrow first pass of mutation.sh rust, and what it must hand to the full pass.

  mutation_narrow.py scope <crate-src-dir>   mutant names (cargo mutants --list) on stdin;
      prints `narrow <nextest filterset>` or `full <reason>`.
  mutation_narrow.py retry <mutants.out>     prints one `^name$` regex per mutant the full
      pass must test again: missed, timed out, or caught with no failing test in its log.
  mutation_narrow.py packages                cargo mutants --list --json on stdin; prints
      each mutated package, one per line (cargo-mutants builds each with --package).
  mutation_narrow.py merge <mutants.out> <full mutants.out>
      writes the final caught/missed/timeout/unviable lists into the first directory.

The narrow pass is only a speed-up. A mutant counts as caught there only when its nextest
log shows a failing test; every other mutant goes to the full pass (cargo test on the whole
package, the old run), whose verdict is final. So a wrong narrowing costs time, never a
verdict: it cannot turn a caught mutant into a missed one.

The narrow set: the top-level modules of the mutated files, plus every file that names one
of them (its tests reach their code: the scenario tests name `alarms`). A mutated file
outside the crate's src dir, the crate root, or a file the mod tree does not reach: full.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

MOD_DECL = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;")
INLINE_MOD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*\{")
PATH_ATTR = re.compile(r'^\s*#\[path\s*=\s*"([^"]+)"\]')
ATTR = re.compile(r"^\s*#\[")
# nextest's per-test failure lines (FAIL, a signal, ABORT, LEAK-FAIL). Not TIMEOUT: a test
# that nextest's slow-timeout terminate-after killed is a hang, which the old single
# cargo-mutants run reported as a Timeout (red), not a catch. Such a mutant goes to pass 2,
# whose cargo test verdict is final (#618 review r1 m2; a PR can add .config/nextest.toml).
NEXTEST_FAIL = re.compile(r"^\s*(?:FAIL|SIG[A-Z]+|ABORT|LEAK-FAIL)\s+\[", re.M)
# Terminal colour codes (CSI sequences). dtolnay/rust-toolchain sets CARGO_TERM_COLOR=always
# on GitHub, so nextest wraps FAIL in them; unstripped, no caught mutant was proven and
# every one re-ran in pass 2 (#574, run 42951620743: the 110-minute timeout, ADE-317).
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
ROOTS = ("lib.rs", "main.rs")


def mod_tree(src: Path) -> dict[str, str]:
    """Each file the crate's mod declarations reach (relative to src) -> its module path."""
    found: dict[str, str] = {}
    todo = [(src / r, "") for r in ROOTS if (src / r).is_file()]
    while todo:
        file, path = todo.pop()
        rel = file.relative_to(src).as_posix()
        if rel in found:
            continue
        found[rel] = path
        mod_rs = file.name in ROOTS or file.name == "mod.rs"
        child_dir = file.parent if mod_rs else file.parent / file.stem
        attr_path = None
        for line in file.read_text(errors="replace").splitlines():
            m = PATH_ATTR.match(line)
            if m:
                attr_path = m.group(1)
                continue
            if ATTR.match(line):
                continue
            m = MOD_DECL.match(line)
            if m:
                name = m.group(1)
                if attr_path:
                    cands = [file.parent / attr_path]
                else:
                    cands = [child_dir / f"{name}.rs", child_dir / name / "mod.rs"]
                child = next((c.resolve() for c in cands if c.is_file()), None)
                if child is not None and src.resolve() in child.parents:
                    todo.append((src / child.relative_to(src.resolve()), f"{path}::{name}".lstrip(":")))
            attr_path = None
    return found


def scope(src: Path, names: list[str]) -> str:
    files = sorted({n.split(":", 1)[0] for n in names if n.strip()})
    if not files:
        return "full no mutants on this shard"
    tree = mod_tree(src)
    prefix = src.as_posix().rstrip("/") + "/"
    tops: set[str] = set()
    for f in files:
        if not f.startswith(prefix):
            return f"full {f} is outside {src}"
        mod = tree.get(f[len(prefix):])
        if mod is None:
            return f"full {f} is not in the mod tree from {src}/lib.rs"
        if mod == "":
            return f"full {f} is the crate root"
        tops.add(mod.split("::", 1)[0])
    word = re.compile(r"\b(?:" + "|".join(sorted(tops)) + r")\b")
    prefixes = set(tops)
    for rel, mod in tree.items():
        if mod.split("::", 1)[0] in tops:
            continue
        text = (src / rel).read_text(errors="replace")
        body = "\n".join(l for l in text.splitlines() if not MOD_DECL.match(l))
        if not word.search(body):
            continue
        if mod:
            prefixes.add(mod)
        else:
            # The crate root: its own inline modules (its tests), not every module under it.
            prefixes.update(m.group(1) for m in map(INLINE_MOD.match, text.splitlines()) if m)
    return "narrow test(/^(?:" + "|".join(sorted(prefixes)) + ")::/)"


def outcomes(out: Path) -> list[dict]:
    return [o for o in json.loads((out / "outcomes.json").read_text())["outcomes"]
            if isinstance(o["scenario"], dict) and "Mutant" in o["scenario"]]


def proven_caught(out: Path, o: dict) -> bool:
    if o["summary"] != "CaughtMutant":
        return False
    log = out / o["log_path"]
    return log.is_file() and bool(NEXTEST_FAIL.search(ANSI.sub("", log.read_text(errors="replace"))))


def rust_regex_escape(s: str) -> str:
    return re.sub(r"([\\.+*?()|\[\]{}^$#&~-])", r"\\\1", s)


def retry(out: Path) -> list[str]:
    rows = outcomes(out)
    again = [o for o in rows if o["summary"] != "Unviable" and not proven_caught(out, o)]
    caught = sum(o["summary"] == "CaughtMutant" for o in rows)
    # On stderr, so a pass 1 that proves nothing shows in the job log (ADE-317).
    print(f"mutation: pass 1 proved {caught - sum(o['summary'] == 'CaughtMutant' for o in again)}"
          f" of {caught} caught mutants by a failing test", file=sys.stderr)
    return ["^" + rust_regex_escape(o["scenario"]["Mutant"]["name"]) + "$" for o in again]


def merge(out: Path, full: Path | None) -> None:
    lists: dict[str, list[str]] = {"caught": [], "missed": [], "timeout": [], "unviable": []}
    kind = {"CaughtMutant": "caught", "MissedMutant": "missed", "Timeout": "timeout", "Unviable": "unviable"}
    retried = set()
    if full is not None:
        for o in outcomes(full):
            name = o["scenario"]["Mutant"]["name"]
            retried.add(name)
            lists[kind[o["summary"]]].append(name)
    for o in outcomes(out):
        name = o["scenario"]["Mutant"]["name"]
        if name in retried:
            continue
        k = kind[o["summary"]]
        if k in ("caught", "unviable") and (k == "unviable" or proven_caught(out, o)):
            lists[k].append(name)
        else:
            # Neither proven nor retried: the full pass did not run it. Never report it caught.
            lists["missed"].append(name)
    for k, v in lists.items():
        (out / f"{k}.txt").write_text("".join(f"{n}\n" for n in sorted(v)))


def packages(listing: str) -> list[str]:
    return sorted({m["package"] for m in json.loads(listing or "[]")})


def main(argv: list[str]) -> int:
    if len(argv) >= 3 and argv[1] == "scope":
        print(scope(Path(argv[2]), sys.stdin.read().splitlines()))
    elif len(argv) == 2 and argv[1] == "packages":
        print("\n".join(packages(sys.stdin.read())))
    elif len(argv) == 3 and argv[1] == "retry":
        print("\n".join(retry(Path(argv[2]))))
    elif len(argv) in (3, 4) and argv[1] == "merge":
        merge(Path(argv[2]), Path(argv[3]) if len(argv) == 4 else None)
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
