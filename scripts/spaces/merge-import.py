#!/usr/bin/env python3
"""Import MonoCode 2's data into the one app as Space 2 (ADE-178).

    merge-import.py --space1 <data dir> --space2 <data dir> --report <merge-dry-run.json>
                    --claude-home <dir> [--codex-home <dir>] --backup <dir> [--apply] [--real] [--json]

Space 1 is the target, the one app's data folder (com.monocode.desktop); Space 2 is the source,
MonoCode 2's (com.monocode.second), only read through a /tmp snapshot. Quit both apps first, and
open the target once with a Spaces build before, so its DB has the Spaces tables. --report is the
ADE-169 merge-dry-run.json made after both apps quit; every count it holds must still be true.

Without --apply this is a dry run: it imports into a /tmp copy of the target DB, prints what an
apply would copy and register with the counts per table and folder, and writes nothing else.
With --apply it first backs up the target DB and every target folder it changes into a new folder
under --backup and checks the copies by sha256; no write happens unless that backup is complete.
Then it copies the data folders, links the login folders and imports the rows in one transaction,
which it rolls back when a count differs from the prediction. A second apply makes 0 changes.
Every conflict check (rows, folder entries, login links, Space 2's own name and login) runs in the
prediction, before the backup. Only a race after it can still stop an apply; the DB then rolls
back, the copied folders and login links stay, and a re-run converges.

What the ADE-169 report lists and the import leaves where it is is printed under "Not imported":
values that name MonoCode 2's data folder (transcript text, kept as written, so keep that folder),
MonoCode 2's browser storage, and its other data-folder entries.

The Spaces rows mirror the app's own writers (spaces::create, account::save_account and
account::set_login), since those run only inside the app and the app must be quit. Every path
inside the real home folder needs --real, so a test or a box never reaches real data.
"""

import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
import pwd
import re
import shutil
import sqlite3
import sys
import tempfile
from contextlib import ExitStack
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
_spec = importlib.util.spec_from_file_location("merge_dry_run", HERE / "merge-dry-run.py")
dry = importlib.util.module_from_spec(_spec)
sys.dont_write_bytecode = True
_spec.loader.exec_module(dry)
q = dry.q

SPACE_ID = "space-2"
SPACE_NAME = "Space 2"
LOGIN_ID = "monocode-2"
LOGIN_LABEL = "MonoCode 2"
DEFAULT_LOGIN = "default"

# Copied tables, parents first: table -> (stable key, active-clock columns).
# The key decides whether a row is there; the active-clock columns move onto
# Space 1's clock.
COPY = {
    "monos": (("id",), ()),
    "sessions": (("id",), ()),
    "mono_transcripts": (("session_id",), ()),
    "mono_blocks": (("session_id", "seq"), ()),
    "mono_turns": (("session_id", "seq"), ()),
    "mono_asks": (("id",), ("due_active_ms", "paused_at_active_ms", "next_due_active_ms")),
    "mono_ask_events": (("ask_id", "kind", "seq"), ("at_active_ms",)),
    "mono_outbox": (("key",), ("at_active_ms",)),
    # A ci.watch's wall-clock row; its wake is a mono_outbox row above.
    "ci_watches": (("id",), ()),
    "mono_messages": (("id",), ()),
    # The stall detector's rows move with their agents, as the asks do: an
    # import reads to the detector as a restart (alarms design section 8).
    "agent_activity": (("session_id",), (
        "turn_started_active_ms", "turn_ended_active_ms", "last_progress_active_ms",
        "background_ended_active_ms", "dialog_since_active_ms", "wake_pending_since_active_ms",
        "hold_ping_due_active_ms",
    )),
    "alarm_episodes": (("id",), (
        "stalled_since_active_ms", "opened_active_ms", "next_escalate_active_ms",
        "acted_active_ms", "closed_active_ms",
    )),
    "alarm_sends": (("episode_id", "level"), ("created_active_ms",)),
    "alarm_prompts": (("outbox_key",), ()),
    "alarm_dialogs": (("episode_id",), ()),
    "alarm_acts": (("episode_id", "seq"), ("at_active_ms",)),
    "agent_alarm_settings": (("actor",), ()),
    "alarm_repeat_limit": (("actor", "cause"), ("sent_active_ms",)),
    # Sleep holds are the hold log; an open one times out on Space 1's clock.
    "sleep_holds": (("id",), (
        "requested_active_ms", "deadline_active_ms", "decided_active_ms",
        "waiting_until_active_ms",
    )),
    "mono_session_ends": (("session_id",), ()),
    "mono_completion_deliveries": (("group_id",), ()),
    "notes": (("id",), ()),
    "milestones": (("id",), ()),
    "milestone_settings": (("project_path",), ()),
    "approvals": (("id",), ()),
    "automations": (("id",), ()),
    "automation_runs": (("id",), ()),
    "automation_event_claims": (("automation_id", "event_key"), ()),
    "session_reminders": (("session_id",), ()),
    "orchestration_runs": (("lead_id",), ()),
    "orchestration_sidebar": (("lead_id",), ()),
    "orchestration_workers": (("session_id",), ()),
    "worktree_removals": (("path",), ()),
}
# The integer key the target numbers anew.
RENUMBER = {"approvals": "number"}
# Tables whose rows are in a Space: imported rows join Space 2.
SPACED = {"monos", "sessions"}
# Per-app state the target keeps: Space 2's rows describe MonoCode 2's own
# clock, windows or run, which end with it.
KEEP = {
    "schema_migrations": "the target's own migrations",
    "mono_clock": "Space 1's active clock runs the one app",
    "mono_downtime": "MonoCode 2's downtime on its own clock",
    "monos_legacy_import": "a one-time marker of the target",
    "workspace_snapshot": "MonoCode 2's window layout",
    "workspace_window_snapshots": "MonoCode 2's window layout",
    "in_flight_sessions": "MonoCode 2's windows at quit",
    "mono_completions": "MonoCode 2's windows at quit",
    "space_settings": "the target's own settings",
    "alarm_settings": "the target's own alarm settings",
    "alarm_wake_retries": "one wake retry again on Space 1's clock costs nothing",
    "alarm_wake_items": "an item's wait counts again from Space 1's next pass",
    "checkout_leases": "the app takes a lease again on the next start",
}
# Spaces tables in a source that a Spaces build opened: rows matching the
# condition would be lost, so they stop the import.
SOURCE_SPACES = {
    "spaces": "id <> 'space-1'",
    "provider_accounts": f"id <> '{DEFAULT_LOGIN}'",
    "space_accounts": "1",
    "space_projects": "1",
    "space_project_links": "1",
    "space_project_accounts": "1",
    "account_route_grants": "1",
}
SKIPPED_FOLDERS = {"provider-accounts"}
ENCRYPTION_MARKER = "db-encryption-on"
LOGIN_HOMES = {"claude": ".claude", "codex": ".codex"}


class Stop(Exception):
    """The import cannot go on; nothing after the check that raised it ran."""


def real_home() -> Path:
    return Path(pwd.getpwuid(os.getuid()).pw_dir).resolve()


def inside(path: Path, root: Path) -> bool:
    return path == root or root in path.parents


def count(db, schema: str, table: str) -> int:
    return db.execute(f"SELECT count(*) FROM {schema}.{q(table)}").fetchone()[0]


def tree_digest(root: Path) -> str | None:
    """sha256 over every path, kind, link target and file content under root."""
    if not root.exists() and not root.is_symlink():
        return None
    digest = hashlib.sha256()
    paths = [root] + sorted(root.rglob("*")) if root.is_dir() and not root.is_symlink() else [root]
    for path in paths:
        digest.update(str(path.relative_to(root)).encode() + b"\0")
        if path.is_symlink():
            digest.update(b"L" + os.readlink(path).encode())
        elif path.is_dir():
            digest.update(b"D")
        else:
            digest.update(b"F" + hashlib.sha256(path.read_bytes()).digest())
    return digest.hexdigest()


def db_digest(path: Path) -> str:
    db = dry.read_only(path)
    try:
        return hashlib.sha256("\n".join(db.iterdump()).encode()).hexdigest()
    finally:
        db.close()


def hold_lock(stack: ExitStack, data: Path) -> None:
    """Holds the app's scheduler lock, so the run stops while an app runs. A
    held lock does not keep an app from starting (it only gives up the clock):
    both apps stay closed until the run ends."""
    lock = data / "scheduler.lock"
    if not lock.exists():
        return
    handle = stack.enter_context(open(lock, "rb"))
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise Stop(f"{data} is in use: quit its app first") from None


def check_report(report: dict, args, db, imported: bool) -> None:
    if report.get("format") != dry.FORMAT:
        raise Stop(f"--report is not a {dry.FORMAT} report")
    for key, data in (("space1", args.space1), ("space2", args.space2)):
        if Path(report["spaces"][key]["data_dir"]).resolve() != data:
            raise Stop(f"--report was made for another {key} folder")
    sides = [("space2", "src")] + ([] if imported else [("space1", "main")])
    for side, schema in sides:
        names = dry.tables(db, schema)
        for table in sorted(names.keys() | {t for t, e in report["tables"].items() if e["rows"][side] is not None}):
            expected = report["tables"].get(table, {"rows": {}})["rows"].get(side)
            actual = count(db, schema, table) if table in names else None
            if expected != actual:
                raise Stop(f"{side} {table} has {actual} rows, the report says {expected}: run merge-dry-run.py again")
    for name in dry.FOLDERS:
        listed = report["folders"][name]["space2"]["entries"]
        folder = args.space2 / name
        if (sorted(p.name for p in folder.iterdir()) if folder.is_dir() else []) != listed:
            raise Stop(f"space2 {name}/ changed since the report: run merge-dry-run.py again")


def check_schema(db) -> None:
    source, target = dry.tables(db, "src"), dry.tables(db, "main")
    if "spaces" not in target:
        raise Stop("The target DB has no Spaces tables: open it once with a Spaces build, then quit")
    unknown = sorted(source.keys() - COPY.keys() - KEEP.keys() - SOURCE_SPACES.keys())
    if unknown:
        raise Stop(f"No import rule for {', '.join(unknown)}")
    for table, where in SOURCE_SPACES.items():
        if table in source and db.execute(f"SELECT count(*) FROM src.{q(table)} WHERE {where}").fetchone()[0]:
            raise Stop(f"Space 2 already has its own {table} rows; importing them is not supported")
    for table in COPY.keys() & source.keys():
        missing = sorted(source[table].keys() - target.get(table, {}).keys() - {"space_id"})
        if missing:
            raise Stop(f"The target {table} lacks {', '.join(missing)}: migrate both DBs to one version first")


def clock(db, schema: str) -> int | None:
    row = db.execute(f"SELECT active_ms FROM {schema}.mono_clock WHERE id = 1").fetchone()
    return row[0] if row else None


def is_imported(db) -> bool:
    return db.execute(
        "SELECT 1 FROM main.space_accounts WHERE space_id = ? AND provider = 'claude' AND account_id = ?",
        (SPACE_ID, LOGIN_ID),
    ).fetchone() is not None


def import_rows(db, delta: int, imported: bool) -> dict:
    """Imports every copied table from src into main; counts per table."""
    source, target = dry.tables(db, "src"), dry.tables(db, "main")
    counts = {}
    for table, (key, active) in COPY.items():
        if table not in source:
            continue
        columns = [c for c in source[table] if c != "space_id" and c != RENUMBER.get(table)]
        values = [f"s.{q(c)} + {delta}" if c in active else f"s.{q(c)}" for c in columns]
        match = " AND ".join(f"m.{q(c)} = s.{q(c)}" for c in key)
        same = " AND ".join(f"m.{q(c)} IS {v}" for c, v in zip(columns, values))
        before = count(db, "main", table)
        conflicts = [] if imported else [r[0] for r in db.execute(
            f"SELECT {' || '.join(f's.{q(c)}' for c in key)} FROM src.{q(table)} s"
            f" JOIN main.{q(table)} m ON {match} WHERE NOT ({same}) ORDER BY 1"
        )]
        if conflicts:
            raise Stop(f"{table}: {len(conflicts)} Space 2 rows share a key with other Space 1 rows")
        spaced = table in SPACED and "space_id" in target[table]
        db.execute(
            f"INSERT INTO main.{q(table)} ({', '.join(map(q, columns))}{', space_id' if spaced else ''})"
            f" SELECT {', '.join(values)}{', ?' if spaced else ''} FROM src.{q(table)} s"
            f" WHERE NOT EXISTS (SELECT 1 FROM main.{q(table)} m WHERE {match})",
            (SPACE_ID,) if spaced else (),
        )
        inserted = db.execute("SELECT changes()").fetchone()[0]
        counts[table] = {
            "source": count(db, "src", table),
            "before": before,
            "insert": inserted,
            "after": count(db, "main", table),
        }
    return counts


def register_rows(db, logins: dict, now: int) -> list[str]:
    """Space 2, its logins and projects, as spaces::create, save_account, set_login and
    set_project_space write them."""
    done = []
    named = db.execute("SELECT name FROM main.spaces WHERE id = ?", (SPACE_ID,)).fetchone()
    if named and named[0] != SPACE_NAME and not is_imported(db):
        raise Stop(f"{SPACE_ID} already exists as {named[0]!r}: MonoCode 2's data would join that Space")
    if db.execute("INSERT OR IGNORE INTO main.spaces (id, name, color, created_at) VALUES (?, ?, NULL, ?)",
                  (SPACE_ID, SPACE_NAME, now)).rowcount:
        done.append(f"spaces {SPACE_ID}")
    for provider in logins:
        if db.execute("INSERT OR IGNORE INTO main.provider_accounts (provider, id, label, created_at)"
                      " VALUES (?, ?, ?, ?)", (provider, LOGIN_ID, LOGIN_LABEL, now)).rowcount:
            done.append(f"provider_accounts {provider}/{LOGIN_ID}")
        chosen = db.execute("SELECT account_id FROM main.space_accounts WHERE space_id = ? AND provider = ?",
                            (SPACE_ID, provider)).fetchone()
        if chosen is None:
            db.execute("INSERT INTO main.space_accounts (space_id, provider, account_id) VALUES (?, ?, ?)",
                       (SPACE_ID, provider, LOGIN_ID))
            done.append(f"space_accounts {SPACE_ID}/{provider}")
        elif chosen[0] != LOGIN_ID and not is_imported(db):
            raise Stop(f"{SPACE_NAME} already uses {provider} login {chosen[0]}")
    # A project with no row is assigned to Space 1, where a Space 2 session
    # would run in a copy of its checkout: a project only Space 2 used (its
    # sessions' folders and its Monos' projects) moves with it, keyed as
    # pathKey() (mono_gate::path_key) keys a POSIX or remote path. A project
    # Space 1 also uses stays assigned to Space 1, and is in Space 2 too
    # through the Space 2 Mono that lists it (spaces::project_spaces_in,
    # ADE-229), so no project leaves a Space that uses it.
    for (project,) in db.execute(
        "SELECT k FROM (SELECT CASE WHEN rtrim(p, '/') = '' THEN '/' ELSE rtrim(p, '/') END AS k, space_id"
        " FROM (SELECT cwd AS p, space_id FROM main.sessions"
        "       UNION ALL SELECT j.value, m.space_id FROM main.monos m,"
        "         json_each(CASE WHEN json_valid(m.data_json) THEN m.data_json END, '$.projects') j"
        "         WHERE j.type = 'text')"
        " WHERE p GLOB '/*' OR p GLOB 'remote://*')"
        " GROUP BY k HAVING sum(space_id <> ?) = 0 ORDER BY k", (SPACE_ID,)
    ).fetchall():
        if db.execute("INSERT OR IGNORE INTO main.space_projects (project_path, space_id) VALUES (?, ?)",
                      (project, SPACE_ID)).rowcount:
            done.append(f"space_projects {project}")
    return done


def import_db(db, logins: dict, now: int) -> dict:
    """The whole DB import inside the caller's transaction."""
    imported = is_imported(db)
    target, source = clock(db, "main"), clock(db, "src")
    delta = target - source if target is not None and source is not None else 0
    tables = import_rows(db, delta, imported)
    return {"imported_before": imported, "clock_delta_ms": delta, "tables": tables,
            "registered": register_rows(db, logins, now)}


def plan_folders(space1: Path, space2: Path, imported: bool) -> dict:
    plan = {}
    for name in dry.FOLDERS:
        source = space2 / name
        entries = sorted(source.iterdir()) if source.is_dir() else []
        if name in SKIPPED_FOLDERS:
            if entries:
                raise Stop(f"space2 {name}/ holds named logins; importing them is not supported")
            continue
        copy, present = [], []
        for entry in entries:
            dest = space1 / name / entry.name
            if not dest.exists() and not dest.is_symlink():
                copy.append(entry.name)
            elif imported or tree_digest(dest) == tree_digest(entry):
                present.append(entry.name)
            else:
                raise Stop(f"{name}/{entry.name} is in both Spaces with different contents")
        plan[name] = {"source": len(entries), "present": len(present), "copy": copy}
    return plan


def copy_folders(space1: Path, space2: Path, plan: dict) -> None:
    for name, entry in plan.items():
        for item in entry["copy"]:
            source, dest = space2 / name / item, space1 / name / item
            part = dest.with_name(f".importing-{item}")
            if part.is_dir() and not part.is_symlink():
                shutil.rmtree(part)
            elif part.exists() or part.is_symlink():
                part.unlink()
            dest.parent.mkdir(parents=True, exist_ok=True)
            if source.is_dir() and not source.is_symlink():
                shutil.copytree(source, part, symlinks=True)
            else:
                shutil.copy2(source, part, follow_symlinks=False)
            os.replace(part, dest)
            if tree_digest(dest) != tree_digest(source):
                raise Stop(f"{name}/{item} did not copy whole")


def check_login_home(provider: str, home: Path, apps: list[Path]) -> None:
    """The rule account_identity::login_folder applies to a linked login."""
    user = Path(os.environ.get("HOME", "/")).resolve()
    real = home.resolve()
    if not home.is_absolute() or ".." in home.parts or not real.is_dir():
        raise Stop(f"--{provider}-home must be an existing folder named by an absolute path")
    if real == user / LOGIN_HOMES[provider]:
        raise Stop(f"--{provider}-home is the first app's own {provider} login; it is never touched")
    def near(app: Path) -> bool:
        parts = real.relative_to(app.parent).parts if inside(real, app.parent) else ()
        return inside(real, app) or inside(app, real) or (bool(parts) and parts[0].startswith("com.monocode."))
    if real == user or not inside(real, user) or any(near(app) for app in apps):
        raise Stop(f"--{provider}-home must be inside the home folder and outside every app data folder")


def plan_links(space1: Path, logins: dict) -> dict:
    plan = {}
    for provider, home in logins.items():
        link = space1 / "provider-accounts" / provider / LOGIN_ID
        if link.is_symlink() and os.readlink(link) == str(home):
            plan[provider] = "present"
        elif link.exists() or link.is_symlink():
            raise Stop(f"{link} is already another login")
        else:
            plan[provider] = "link"
    return plan


def make_links(space1: Path, logins: dict, plan: dict) -> None:
    for provider, action in plan.items():
        if action == "link":
            link = space1 / "provider-accounts" / provider / LOGIN_ID
            link.parent.mkdir(parents=True, exist_ok=True)
            os.symlink(logins[provider], link)


def backup(space1: Path, root: Path, folders: list[str]) -> Path:
    """Copies the target DB and folders under root and checks every copy."""
    run = root / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    run.mkdir(parents=True)
    db = space1 / "monocode.db"
    source, target = dry.read_only(db), sqlite3.connect(run / "monocode.db")
    try:
        source.backup(target)
    finally:
        source.close()
        target.close()
    manifest = {"db": db_digest(db), "folders": {}}
    if db_digest(run / "monocode.db") != manifest["db"]:
        raise Stop("The DB backup does not match the target DB")
    for name in folders:
        live = space1 / name
        manifest["folders"][name] = tree_digest(live)
        if live.is_dir():
            shutil.copytree(live, run / "folders" / name, symlinks=True)
        if tree_digest(run / "folders" / name) != manifest["folders"][name]:
            raise Stop(f"The backup of {name}/ does not match the target")
    (run / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return run


def not_imported(report: dict, space2: Path) -> dict:
    """The ADE-169 inventory the import leaves where it is, with the reason."""
    paths = report["own_paths"]["space2"]
    storage = report["browser_storage"]
    others = report["other_entries"]["space2"]
    return {
        "own_paths": {
            "count": sum(paths.values()),
            "columns": paths,
            "reason": f"values naming {space2} stay as written (transcript text): keep that folder",
        },
        "browser_storage": {
            "count": len(storage["space2"]["keys"]),
            "differ": len(storage["keys_differ"]),
            "only_in_space2": len(storage["keys_only_in_space2"]),
            "reason": "browser storage stays with each app; the one app keeps Space 1's",
        },
        "other_entries": {
            "count": len(others),
            "entries": others,
            "reason": "MonoCode 2's own window state and lock files",
        },
    }


def render(result: dict) -> str:
    mode = "apply" if result["apply"] else "dry run (nothing written)"
    lines = [f"merge-import {mode}: MonoCode 2 -> {SPACE_NAME}"]
    if result.get("backup"):
        lines.append(f"backup: {result['backup']}")
    lines.append(f"already imported: {'yes' if result['imported_before'] else 'no'};"
                 f" active clock moved by {result['clock_delta_ms']} ms")
    lines += ["", f"  {'table':<28} {'space2':>8} {'before':>8} {'insert':>8} {'after':>8}"]
    lines += [f"  {t:<28} {c['source']:>8} {c['before']:>8} {c['insert']:>8} {c['after']:>8}"
              for t, c in result["tables"].items()]
    lines += ["", "Kept from the target:"]
    lines += [f"  {t}: {n} Space 2 rows not copied ({KEEP[t]})" for t, n in result["kept"].items()] or ["  none"]
    lines += ["", "Not imported:"]
    lines += [f"  {kind}: {n['count']} ({n['reason']})" for kind, n in result["not_imported"].items()]
    lines += ["", f"  {'folder':<28} {'space2':>8} {'present':>8} {'copy':>8}"]
    lines += [f"  {n + '/':<28} {f['source']:>8} {f['present']:>8} {len(f['copy']):>8}"
              for n, f in result["folders"].items()]
    lines += ["", "Logins:"]
    lines += [f"  {p}: {a} provider-accounts/{p}/{LOGIN_ID} -> {result['logins'][p]}" for p, a in result["links"].items()]
    lines += [f"  {r}" for r in result["registered"]]
    lines.append(f"\nchanges: {result['changes']}")
    return "\n".join(lines) + "\n"


def run(args) -> dict:
    space1, space2 = args.space1, args.space2
    for data in (space1, space2):
        if (data / ENCRYPTION_MARKER).exists():
            raise Stop(f"{data} has an encrypted DB; the importer reads plain SQLite only")
    logins = {"claude": args.claude_home} | ({"codex": args.codex_home} if args.codex_home else {})
    if len({str(h.resolve()) for h in logins.values()}) < len(logins):
        raise Stop("--claude-home and --codex-home must be different folders")
    for provider, home in logins.items():
        check_login_home(provider, home, [space1, space2])
    report = json.loads(args.report.read_text())
    now = int(datetime.now(timezone.utc).timestamp() * 1000)
    with ExitStack() as stack:
        for data in (space1, space2):
            hold_lock(stack, data)
        tmp = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="merge-import-", dir="/tmp")))
        dry.snapshot(space1 / "monocode.db", tmp / "predict.db")
        # uri=True, so ATTACH reads the source snapshot's file: URI on every SQLite build.
        db = sqlite3.connect((tmp / "predict.db").as_uri(), uri=True, isolation_level=None)
        stack.callback(db.close)
        source = dry.snapshot(space2 / "monocode.db", tmp / "space2.db")
        db.execute("ATTACH DATABASE ? AS src", (source,))
        db.execute("PRAGMA foreign_keys = ON")
        check_schema(db)
        imported = is_imported(db)
        check_report(report, args, db, imported)
        db.execute("BEGIN IMMEDIATE")
        db.execute("PRAGMA defer_foreign_keys = ON")
        result = import_db(db, logins, now)
        db.execute("COMMIT")
        sources = dry.tables(db, "src")
        result["kept"] = {t: count(db, "src", t) for t in KEEP if t in sources}
        result["not_imported"] = not_imported(report, space2)
        result["folders"] = plan_folders(space1, space2, result["imported_before"])
        result["links"] = plan_links(space1, logins)
        result["logins"] = {p: str(h) for p, h in logins.items()}
        result["apply"] = args.apply
        result["changes"] = (sum(c["insert"] for c in result["tables"].values()) + len(result["registered"])
                             + sum(len(f["copy"]) for f in result["folders"].values())
                             + sum(a == "link" for a in result["links"].values()))
        if not args.apply:
            return result
        changed = [n for n, f in result["folders"].items() if f["copy"]]
        changed += ["provider-accounts"] if "link" in result["links"].values() else []
        result["backup"] = str(backup(space1, args.backup, changed))
        copy_folders(space1, space2, result["folders"])
        make_links(space1, logins, result["links"])
        live = sqlite3.connect((space1 / "monocode.db").as_uri(), uri=True, isolation_level=None)
        stack.callback(live.close)
        live.execute("PRAGMA foreign_keys = ON")
        live.execute("ATTACH DATABASE ? AS src", (source,))
        live.execute("BEGIN IMMEDIATE")
        live.execute("PRAGMA defer_foreign_keys = ON")
        try:
            done = import_db(live, logins, now)
            if done["tables"] != result["tables"] or done["registered"] != result["registered"]:
                raise Stop("The import changed other counts than the dry run predicted: the DB rolled back;"
                           " the copied folders and the login link stay, and a re-run converges")
            live.execute("COMMIT")
        except BaseException:
            live.execute("ROLLBACK")
            raise
        (Path(result["backup"]) / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        return result


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--space1", type=Path, required=True, help="the one app's data folder (target)")
    parser.add_argument("--space2", type=Path, required=True, help="MonoCode 2's data folder (source)")
    parser.add_argument("--report", type=Path, required=True, help="merge-dry-run.json made after both apps quit")
    parser.add_argument("--claude-home", type=Path, required=True, help="Space 2's Claude login folder")
    parser.add_argument("--codex-home", type=Path, help="Space 2's Codex login folder, when it has one")
    parser.add_argument("--backup", type=Path, required=True, help="a new backup folder is made in it")
    parser.add_argument("--apply", action="store_true", help="write; without it nothing is written")
    parser.add_argument("--real", action="store_true", help="allow paths inside the real home folder")
    parser.add_argument("--json", action="store_true", help="print the result as JSON")
    args = parser.parse_args(argv)
    args.space1, args.space2 = args.space1.resolve(), args.space2.resolve()
    home = real_home()
    paths = [args.space1, args.space2, args.report, args.backup, args.claude_home, args.codex_home]
    if not args.real and any(p is not None and inside(p.resolve(), home) for p in paths):
        print(f"merge-import: a path is inside the real home folder {home}; pass --real to use real data",
              file=sys.stderr)
        return 2
    backup_root = args.backup.resolve()
    if hit := next((p for p in (REPO, args.space1, args.space2) if inside(backup_root, p)), None):
        print(f"merge-import: --backup must be outside {hit}", file=sys.stderr)
        return 2
    try:
        result = run(args)
    except (Stop, sqlite3.Error, OSError) as error:
        print(f"merge-import: {error}", file=sys.stderr)
        return 1
    sys.stdout.write(json.dumps(result, indent=2) + "\n" if args.json else render(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
