"""Checks for the ADE-183 cut-over job (cutover-ade183.sh): one subcommand per check.

    python3 -I cutover_checks.py <command> [args]

manifest <root> <out>          one line per entry (path, kind, size, sha256 or link target);
                               prints entries/files/bytes
sqlite-files <root>            every SQLite file under root, by its header
lock-held <data dir>           exit 0 if the app holds scheduler.lock (a try, released at once)
has-table <db> <table>         exit 0 if the table exists
clock <db>                     mono_clock.last_wall_ms, or 0
counts <result.json>           the importer's counts, one line per table
env-check <pid> <app>          exit 1 if the app's env has an agent var its LSEnvironment does not set
window <db before> <db after>  sessions that ran while the new app was open
check-dry <dry.json>           after == before + insert, insert <= space2 rows
check-apply <dry> <real> <db>  real == dry, live counts == after
json-field <file> <key>        one top-level field of a JSON file
"""
import hashlib
import json
import os
import plistlib
import re
import sqlite3
import subprocess
import sys
from pathlib import Path

AGENT_VAR = re.compile(r"(?:^|\s)((?:CLAUDE|CODEX|MONOCODE|ANTHROPIC|OPENAI)[A-Za-z0-9_]*)=")


def ro(db):
    return sqlite3.connect(f"{Path(db).resolve().as_uri()}?mode=ro", uri=True)


def manifest_lines(root: Path) -> list[str]:
    if not root.exists() and not root.is_symlink():
        return ["ABSENT"]
    lines = []
    for dirpath, dirs, files in os.walk(root):
        dirs.sort()
        for name in sorted(dirs + files):
            p = Path(dirpath, name)
            rel = p.relative_to(root)
            if p.is_symlink():
                lines.append(f"{rel}\tL\t-\t{os.readlink(p)}")
            elif p.is_dir():
                lines.append(f"{rel}\tD\t-\t-")
            elif p.is_file():  # sockets and pipes are skipped: ditto does not copy them
                h = hashlib.sha256()
                with open(p, "rb") as f:
                    for chunk in iter(lambda: f.read(1 << 20), b""):
                        h.update(chunk)
                lines.append(f"{rel}\tF\t{p.stat().st_size}\t{h.hexdigest()}")
    return lines


def env_verdict(env: str, allowed: dict) -> tuple[list[str], list[str]]:
    """(allowed KEY=value texts found, names NOT allowed). A name is allowed only when the exact
    text KEY=<its LSEnvironment value> stands in the env as a whole word, so a value with spaces
    is read whole, not cut at its first space."""
    ok, bad = [], []
    for name in dict.fromkeys(AGENT_VAR.findall(env)):
        text = f"{name}={allowed[name]}" if name in allowed else None
        if text and re.search(r"(?:^|\s)" + re.escape(text) + r"(?=\s|$)", env):
            ok.append(text)
        else:
            bad.append(name)
    return ok, bad


def apply_mismatches(dry: dict, real: dict, live_count) -> list[str]:
    bad = [t for t in dry["tables"].keys() | real["tables"].keys() if dry["tables"].get(t) != real["tables"].get(t)]
    if dry["registered"] != real["registered"]:
        bad.append("registered")
    bad += [f"{t}(live)" for t, c in real["tables"].items() if live_count(t) != c["after"]]
    return sorted(bad)


def dry_mismatches(dry: dict) -> list[str]:
    return [t for t, c in dry["tables"].items() if c["after"] != c["before"] + c["insert"] or c["insert"] > c["source"]]


def verdict(bad: list[str]) -> int:
    print("mismatch: " + ", ".join(bad) if bad else "ok")
    return 1 if bad else 0


def manifest(root, out):
    lines = manifest_lines(Path(root))
    Path(out).write_text("\n".join(lines) + "\n")
    files = [line.split("\t") for line in lines if "\tF\t" in line]
    print(f"entries={len(lines)} files={len(files)} bytes={sum(int(f[2]) for f in files)}")


def sqlite_files(root):
    for dirpath, _, files in os.walk(root):
        for name in files:
            p = Path(dirpath, name)
            if not p.is_symlink():
                with open(p, "rb") as f:
                    if f.read(16) == b"SQLite format 3\0":
                        print(p)


def lock_held(data):
    import fcntl
    try:
        with open(Path(data, "scheduler.lock"), "rb") as f:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        return 0
    except OSError:
        pass
    return 1


def has_table(db, table):
    try:
        return 0 if ro(db).execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (table,)).fetchone() else 1
    except sqlite3.Error:
        return 1


def clock(db):
    try:
        print(ro(db).execute("SELECT last_wall_ms FROM mono_clock").fetchone()[0])
    except Exception:
        print(0)


def counts(result):
    r = json.loads(Path(result).read_text())
    for t, c in r["tables"].items():
        print(f"  {t}: space2={c['source']} before={c['before']} insert={c['insert']} after={c['after']}")
    print(f"  registered={len(r['registered'])} folders_copy={sum(len(f['copy']) for f in r['folders'].values())}"
          f" links={sum(a == 'link' for a in r['links'].values())} changes={r['changes']}")


def env_check(pid, app):
    env = subprocess.run(["ps", "eww", "-o", "command=", "-p", pid], capture_output=True, text=True).stdout
    try:
        allowed = plistlib.loads(Path(app, "Contents/Info.plist").read_bytes()).get("LSEnvironment", {})
    except Exception:
        allowed = {}
    ok, bad = env_verdict(env, allowed)
    print(f"allowed (own LSEnvironment): {', '.join(ok) or 'none'}; NOT allowed: {', '.join(bad) or 'none'}")
    return 1 if bad or not env.strip() else 0


def window(before_db, after_db):
    a, b = ro(before_db), ro(after_db)

    def rows(db, sql):
        try:
            return dict(db.execute(sql).fetchall())
        except sqlite3.Error:
            return {}
    for label, sql in (("sessions.updated_at", "SELECT id, updated_at FROM sessions"),
                       ("mono_turns max seq", "SELECT session_id, max(seq) FROM mono_turns GROUP BY 1"),
                       ("mono_blocks max seq", "SELECT session_id, max(seq) FROM mono_blocks GROUP BY 1")):
        before, after = rows(a, sql), rows(b, sql)
        moved = sorted(k for k, v in after.items() if before.get(k) != v)
        print(f"  {label} changed: {len(moved)} {' '.join(moved)}")
    in_flight = ' '.join(sorted(rows(b, 'SELECT session_id, cwd FROM in_flight_sessions')))
    print(f"  in_flight_sessions after the quit: {in_flight or 'none'}")


def check_dry(dry):
    return verdict(dry_mismatches(json.loads(Path(dry).read_text())))


def check_apply(dry_path, real_path, live_db):
    dry, real = (json.loads(Path(a).read_text()) for a in (dry_path, real_path))
    db = ro(live_db)
    return verdict(apply_mismatches(dry, real, lambda t: db.execute(f'SELECT count(*) FROM "{t}"').fetchone()[0]))


def json_field(path, key):
    print(json.loads(Path(path).read_text())[key])


COMMANDS = {"manifest": manifest, "sqlite-files": sqlite_files, "lock-held": lock_held, "has-table": has_table,
            "clock": clock, "counts": counts, "env-check": env_check, "window": window, "check-dry": check_dry,
            "check-apply": check_apply, "json-field": json_field}


if __name__ == "__main__":
    sys.exit(COMMANDS[sys.argv[1]](*sys.argv[2:]) or 0)
