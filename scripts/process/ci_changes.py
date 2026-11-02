#!/usr/bin/env python3
"""ci-changes: which path-gated CI jobs a change runs, from ci-map.toml beside this script.

Usage:
  ci_changes.py github <event> <base> <head>
      For the `changes` jobs of ci.yml and e2e.yml. Prints one `<job>=true|false` line per
      job in the map (for GITHUB_OUTPUT) and the reason and each changed file on stderr.
      Every job runs for any event but pull_request, when `git diff <base> <head>` fails or
      is empty, and when a changed file is in the map's `all` list or matches no glob.
  ci_changes.py files
      Reads repo-relative paths from stdin and prints `<path><TAB><class>` for each: `all`,
      `unmatched`, `none`, or the comma-separated jobs it runs. box-ci.sh reads this.

A map that cannot be read is an error (exit 1): the workflows then run every job, and the
red `changes` job keeps the PR from merging until the map is fixed.
"""
import re
import subprocess
import sys
import tomllib
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path

MAP = Path(__file__).with_name("ci-map.toml")
ALL, UNMATCHED = "all", "unmatched"


def glob_re(glob: str) -> re.Pattern[str]:
    """A glob as a regex for re.fullmatch: `*` and `?` stay inside one folder, `**` crosses
    folders, and `**/` also matches no folder at all."""
    parts = re.split(r"(\*\*/|\*\*|\*|\?)", glob)
    tokens = {"**/": "(?:.*/)?", "**": ".*", "*": "[^/]*", "?": "[^/]"}
    return re.compile("".join(tokens.get(part, re.escape(part)) for part in parts))


@dataclass(frozen=True)
class CiMap:
    all: tuple[re.Pattern[str], ...]
    none: tuple[re.Pattern[str], ...]
    jobs: Mapping[str, tuple[re.Pattern[str], ...]]


def parse(text: str) -> CiMap:
    data = tomllib.loads(text)
    if set(data) != {"all", "none", "jobs"}:
        raise ValueError(f"ci-map.toml needs exactly the keys all, none and [jobs]; it has {sorted(data)}")

    def globs(where: str, value: object) -> tuple[re.Pattern[str], ...]:
        if not isinstance(value, list) or not value or not all(isinstance(g, str) and g for g in value):
            raise ValueError(f"ci-map.toml: {where} must be a non-empty list of globs")
        return tuple(glob_re(g) for g in value)

    if not isinstance(data["jobs"], dict) or not data["jobs"]:
        raise ValueError("ci-map.toml: [jobs] must name at least one job")
    return CiMap(globs("all", data["all"]), globs("none", data["none"]),
                 {job: globs(f"jobs.{job}", value) for job, value in data["jobs"].items()})


def classify(ci_map: CiMap, path: str) -> str | frozenset[str]:
    """ALL or UNMATCHED when the path runs every job, else the jobs it runs (empty for a
    path only `none` holds). A job glob wins over `none`; `all` wins over both."""
    if any(p.fullmatch(path) for p in ci_map.all):
        return ALL
    jobs = frozenset(job for job, patterns in ci_map.jobs.items() if any(p.fullmatch(path) for p in patterns))
    if jobs or any(p.fullmatch(path) for p in ci_map.none):
        return jobs
    return UNMATCHED


def select(ci_map: CiMap, event: str, changed: Iterable[str] | None) -> tuple[dict[str, bool], str]:
    """Each job's run decision and the reason. changed is None when the diff failed."""
    run_all = {job: True for job in ci_map.jobs}
    if event != "pull_request":
        return run_all, f"{event or 'no'} event: every job runs"
    paths = [path for path in changed or () if path]
    if not paths:
        return run_all, "no diff could be read (failed or empty): every job runs"
    run = {job: False for job in ci_map.jobs}
    for path in paths:
        kind = classify(ci_map, path)
        if kind in (ALL, UNMATCHED):
            why = "is in the map's all list" if kind == ALL else "matches no glob in the map"
            return run_all, f"{path} {why}: every job runs"
        for job in kind:
            run[job] = True
    return run, f"{len(paths)} changed files; runs: {' '.join(job for job, on in run.items() if on) or 'no gated job'}"


def diff(base: str, head: str) -> list[str] | None:
    result = subprocess.run(["git", "-c", "core.quotePath=false", "diff", "--name-only", "--no-renames", base, head],
                            capture_output=True, text=True)
    return result.stdout.splitlines() if result.returncode == 0 else None


def describe(kind: str | frozenset[str]) -> str:
    return kind if isinstance(kind, str) else ",".join(sorted(kind)) or "none"


def main(argv: list[str]) -> int:
    ci_map = parse(MAP.read_text())
    if argv[:1] == ["github"] and len(argv) == 4:
        _, event, base, head = argv
        changed = diff(base, head) if event == "pull_request" else []
        run, reason = select(ci_map, event, changed)
        for path in changed or ():
            print(f"  {path}: {describe(classify(ci_map, path))}", file=sys.stderr)
        print(f"ci-changes: {reason}", file=sys.stderr)
        print("".join(f"{job}={str(on).lower()}\n" for job, on in run.items()), end="")
        return 0
    if argv == ["files"]:
        for path in sys.stdin.read().splitlines():
            if path:
                print(f"{path}\t{describe(classify(ci_map, path))}")
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
