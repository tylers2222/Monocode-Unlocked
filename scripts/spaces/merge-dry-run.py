#!/usr/bin/env python3
"""Report what collides or breaks before MonoCode 2's data merges into Space 2.

    merge-dry-run.py --out <dir> [--space1 <data dir>] [--space2 <data dir>]
                     [--webkit1 <dir>] [--webkit2 <dir>]

Space 1 is MonoCode 1 (com.monocode.desktop), Space 2 is MonoCode 2 (com.monocode.second); the
defaults are their real macOS folders. Nothing is written there: each monocode.db and WebKit
localstorage.sqlite3 is copied with the SQLite backup API from a mode=ro connection (so the copy
holds the WAL) into a /tmp folder that is deleted at the end, and folders are only listed. The
report holds real ids and paths, so --out must be outside this repository and the source folders.
merge-dry-run.json (shape named by `format`) is the importer's input (ADE-178); merge-dry-run.txt
is the same for people. Stored values are never written out, only keys, counts, sizes and ids.
"""

import argparse
import json
import sqlite3
import sys
import tempfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

FORMAT = "monocode-merge-dry-run/1"
REPO = Path(__file__).resolve().parents[2]
ACCOUNTS_KEY = "monocode.providerAccounts.v1"
SELECTIONS_KEY = "monocode.providerAccountSelections.v1"
DEFAULT_ACCOUNT = "default"
DB_FILES = {"monocode.db", "monocode.db-wal", "monocode.db-shm"}
SIDES = ("space1", "space2")


@dataclass(frozen=True)
class Space:
    key: str
    app: str
    bundle: str
    data: Path
    webkit: Path
    schema: str


@dataclass(frozen=True)
class Ref:
    table: str
    column: str
    target: str
    target_column: str = "id"
    prefix: str = ""
    declared: bool = False

    def in_schema(self, schema_tables: dict) -> bool:
        return self.column in schema_tables.get(self.table, {}) and self.target_column in schema_tables.get(self.target, {})


# Id columns the schema does not declare as foreign keys. An actor column
# holds `mono:<id>` or `session:<id>`.
SOFT_REFS = (
    *(
        Ref(table, column, "sessions")
        for table, column in (
            ("notes", "source_session_id"),
            ("mono_session_ends", "session_id"),
            ("in_flight_sessions", "session_id"),
            ("orchestration_workers", "session_id"),
            ("orchestration_workers", "lead_id"),
            ("orchestration_runs", "lead_id"),
            ("orchestration_sidebar", "lead_id"),
            ("mono_messages", "from_session_id"),
            ("mono_messages", "to_session_id"),
            ("mono_messages", "handled_by_session"),
        )
    ),
    Ref("sessions", "automation_id", "automations"),
    *(
        Ref(table, column, target, prefix=f"{target[:-1]}:")
        for table, column in (
            ("mono_asks", "parent_actor"),
            ("mono_asks", "child_actor"),
            ("mono_outbox", "recipient"),
            ("milestones", "created_by"),
        )
        for target in ("monos", "sessions")
    ),
)

# Data folder -> the table whose ids name its entries ("" when a hash of the
# project path names them).
FOLDERS = {
    "monos": "monos",
    "checkpoints": "sessions",
    "note-assets": "notes",
    "project-logos": "",
    "backgrounds": "",
    "provider-accounts": "",
    "codex-monos": "",
    "attachments": "",
}


def q(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def snapshot(src: Path, dest: Path) -> str:
    # A mode=ro open of a WAL database creates an empty -wal file when there is
    # none. No -wal means no writer, so immutable reads it without touching it.
    live = Path(f"{src}-wal").exists()
    source = sqlite3.connect(f"{src.resolve().as_uri()}?mode=ro{'' if live else '&immutable=1'}", uri=True)
    target = sqlite3.connect(dest)
    try:
        source.backup(target)
    finally:
        source.close()
        target.close()
    return f"{dest.as_uri()}?mode=ro&immutable=1"


def rows_or_none(db, sql: str, args=()) -> list | None:
    """Rows of sql, or None when a table or column it names does not exist."""
    try:
        return db.execute(sql, args).fetchall()
    except sqlite3.OperationalError:
        return None


def tables(db, schema: str) -> dict[str, dict[str, str]]:
    names = db.execute(
        f"SELECT name FROM {schema}.sqlite_master"
        " WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    ).fetchall()
    return {
        name: {r[1]: r[2].upper() for r in db.execute(f"PRAGMA {schema}.table_info({q(name)})")}
        for (name,) in names
    }


def table_keys(db, table: str) -> list[tuple[str, ...]]:
    info = db.execute(f"PRAGMA main.table_info({q(table)})").fetchall()
    pk = tuple(r[1] for r in sorted((r for r in info if r[5]), key=lambda r: r[5]))
    keys = [pk] if pk else []
    for _, index, unique, origin, partial in db.execute(f"PRAGMA main.index_list({q(table)})"):
        cols = tuple(r[2] for r in db.execute(f"PRAGMA main.index_info({q(index)})"))
        if unique and origin != "pk" and not partial and None not in cols and cols not in keys:
            keys.append(cols)
    return keys


def collisions(db, table: str, key: tuple[str, ...], columns: list[str], integer: bool) -> dict:
    join = " AND ".join(f"a.{q(c)} = b.{q(c)}" for c in key)
    same = " AND ".join(f"a.{q(c)} IS b.{q(c)}" for c in columns)
    rows = db.execute(
        f"SELECT {', '.join(f'b.{q(c)}' for c in key)}, {same}"
        f" FROM s2.{q(table)} b JOIN main.{q(table)} a ON {join} ORDER BY 1"
    ).fetchall()
    value = (lambda r: r[0]) if len(key) == 1 else list
    return {
        "columns": list(key),
        "integer": integer,
        "same": [value(r[:-1]) for r in rows if r[-1]],
        "different": [value(r[:-1]) for r in rows if not r[-1]],
    }


def table_report(db, schemas: dict) -> dict:
    one, two = schemas["space1"], schemas["space2"]
    report = {}
    for table in sorted(one.keys() | two.keys()):
        entry = {
            "rows": {
                k: db.execute(f"SELECT count(*) FROM {s}.{q(table)}").fetchone()[0]
                if table in schemas[k] else None
                for k, s in zip(SIDES, ("main", "s2"))
            },
            "keys": [],
        }
        if table in one and table in two:
            columns = [c for c in one[table] if c in two[table]]
            entry["keys"] = [
                collisions(db, table, key, columns, len(key) == 1 and one[table][key[0]] == "INTEGER")
                for key in table_keys(db, table)
                if all(c in columns for c in key)
            ]
        report[table] = entry
    return report


def declared_refs(db, schema: str, table: str) -> list[Ref]:
    refs = []
    for row in db.execute(f"PRAGMA {schema}.foreign_key_list({q(table)})"):
        target, column, target_column = row[2], row[3], row[4]
        if target_column is None:
            info = db.execute(f"PRAGMA {schema}.table_info({q(target)})").fetchall()
            target_column = next(r[1] for r in info if r[5] == 1)
        refs.append(Ref(table, column, target, target_column, declared=True))
    return refs


def broken_refs(db, space: Space, schema_tables: dict) -> list[dict]:
    found = []
    soft = [r for r in SOFT_REFS if r.in_schema(schema_tables)]
    for ref in [r for t in schema_tables for r in declared_refs(db, space.schema, t)] + soft:
        col = f"c.{q(ref.column)}"
        value = f"substr({col}, {len(ref.prefix) + 1})" if ref.prefix else col
        match = f"{col} GLOB '{ref.prefix}*'" if ref.prefix else f"{col} IS NOT NULL"
        rows = rows_or_none(
            db,
            f"SELECT {value}, count(*) FROM {space.schema}.{q(ref.table)} c WHERE {match}"
            f" AND NOT EXISTS (SELECT 1 FROM {space.schema}.{q(ref.target)} p"
            f" WHERE p.{q(ref.target_column)} = {value}) GROUP BY 1 ORDER BY 1",
        )
        if rows:
            found.append({
                "space": space.key,
                "table": ref.table,
                "column": ref.column,
                "target": f"{ref.prefix}{ref.target}.{ref.target_column}",
                "declared": ref.declared,
                "rows": sum(n for _, n in rows),
                "values": [v for v, _ in rows],
            })
    return found


def own_paths(db, space: Space, schema_tables: dict) -> dict[str, int]:
    counts = {}
    for table, columns in schema_tables.items():
        for column in (c for c, kind in columns.items() if kind == "TEXT"):
            (n,) = db.execute(
                f"SELECT count(*) FROM {space.schema}.{q(table)} WHERE {q(column)} LIKE ?",
                (f"%/{space.bundle}/%",),
            ).fetchone()
            if n:
                counts[f"{table}.{column}"] = n
    return counts


def folder_stats(root: Path) -> dict:
    if not root.is_dir():
        return {"exists": False, "entries": [], "files": 0, "bytes": 0}
    files = [p for p in root.rglob("*") if p.is_file() and not p.is_symlink()]
    return {
        "exists": True,
        "entries": sorted(p.name for p in root.iterdir()),
        "files": len(files),
        "bytes": sum(p.stat().st_size for p in files),
    }


def folders(db, spaces) -> dict:
    report = {}
    for name, table in FOLDERS.items():
        stats = {s.key: folder_stats(s.data / name) for s in spaces}
        entries = {k: set(v["entries"]) for k, v in stats.items()}
        named = {
            s.key: {r[0] for r in rows_or_none(db, f"SELECT id FROM {s.schema}.{q(table)}") or []}
            if table else entries[s.key]
            for s in spaces
        }
        report[name] = {
            **stats,
            "collisions": sorted(entries["space1"] & entries["space2"]),
            "without_row": {k: sorted(entries[k] - named[k]) for k in SIDES},
        }
    return report


def other_entries(space: Space) -> list[str]:
    return sorted(
        p.name + ("/" if p.is_dir() else "")
        for p in (space.data.iterdir() if space.data.is_dir() else [])
        if p.name not in DB_FILES and p.name not in FOLDERS
    )


def schema_diff(one: dict, two: dict) -> dict:
    diff = {}
    for table in sorted(one.keys() | two.keys()):
        if table not in two or table not in one:
            diff[table] = f"only in {'space1' if table in one else 'space2'}"
        elif one[table] != two[table]:
            diff[table] = sorted(set(one[table].items()) ^ set(two[table].items()))
    return diff


def read_storage(space: Space, tmp: Path) -> tuple[int, dict[str, str]]:
    files = sorted(space.webkit.glob("WebsiteData/Default/*/*/LocalStorage/localstorage.sqlite3"))
    items = {}
    for i, path in enumerate(files):
        db = sqlite3.connect(snapshot(path, tmp / f"{space.key}-storage{i}.sqlite3"), uri=True)
        for key, value in db.execute("SELECT key, value FROM ItemTable"):
            items[key] = value.decode("utf-16-le", "replace") if isinstance(value, bytes) else str(value)
        db.close()
    return len(files), items


def parse_record(raw: str | None) -> dict:
    try:
        value = json.loads(raw) if raw else {}
    except json.JSONDecodeError:
        return {}
    return value if isinstance(value, dict) else {}


def storage_report(db, spaces, tmp: Path) -> dict:
    report, values = {}, {}
    for s in spaces:
        count, values[s.key] = read_storage(s, tmp)
        stored = parse_record(values[s.key].get(ACCOUNTS_KEY))
        accounts = {
            provider: [a["id"] for a in rows if isinstance(a, dict) and isinstance(a.get("id"), str)]
            for provider, rows in stored.items()
            if isinstance(rows, list)
        }
        known = {DEFAULT_ACCOUNT, *(a for ids in accounts.values() for a in ids)}
        selections = parse_record(values[s.key].get(SELECTIONS_KEY))
        picked = {a for c in selections.values() if isinstance(c, dict) for a in c.values() if isinstance(a, str)}
        sessions = dict(rows_or_none(
            db,
            f"SELECT coalesce(provider_account_id, ?), count(*) FROM {s.schema}.sessions GROUP BY 1",
            (DEFAULT_ACCOUNT,),
        ) or [])
        report[s.key] = {
            "files": count,
            "keys": {k: len(v) for k, v in sorted(values[s.key].items())},
            "accounts": accounts,
            "selections": len(selections),
            "sessions_by_login": sessions,
            "missing_logins": sorted((picked | sessions.keys()) - known),
        }
    one, two = values["space1"], values["space2"]
    report["keys_differ"] = sorted(k for k in one.keys() & two.keys() if one[k] != two[k])
    report["keys_only_in_space2"] = sorted(two.keys() - one.keys())
    return report


def scalar(db, sql: str):
    rows = rows_or_none(db, sql)
    return rows[0][0] if rows else None


def build(spaces: tuple[Space, Space], tmp: Path) -> dict:
    db = sqlite3.connect(snapshot(spaces[0].data / "monocode.db", tmp / "space1.db"), uri=True)
    db.execute("ATTACH DATABASE ? AS s2", (snapshot(spaces[1].data / "monocode.db", tmp / "space2.db"),))
    schemas = {s.key: tables(db, s.schema) for s in spaces}
    report = {
        "format": FORMAT,
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "spaces": {
            s.key: {
                "app": s.app,
                "data_dir": str(s.data),
                "schema_version": scalar(db, f"SELECT max(version) FROM {s.schema}.schema_migrations"),
                "active_clock_ms": scalar(db, f"SELECT active_ms FROM {s.schema}.mono_clock WHERE id = 1"),
            }
            for s in spaces
        },
        "schema_diff": schema_diff(schemas["space1"], schemas["space2"]),
        "tables": table_report(db, schemas),
        "broken_refs": [r for s in spaces for r in broken_refs(db, s, schemas[s.key])],
        "skipped_refs": {
            s.key: [f"{r.table}.{r.column} -> {r.prefix}{r.target}" for r in SOFT_REFS if not r.in_schema(schemas[s.key])]
            for s in spaces
        },
        "own_paths": {s.key: own_paths(db, s, schemas[s.key]) for s in spaces},
        "folders": folders(db, spaces),
        "other_entries": {s.key: other_entries(s) for s in spaces},
        "browser_storage": storage_report(db, spaces, tmp),
    }
    db.close()
    report["risks"] = risks(report)
    return report


def key_hits(report: dict, integer: bool, *fields: str) -> list[str]:
    return [
        f"{table}({', '.join(k['columns'])}) {n}"
        for table, entry in report["tables"].items()
        for k in entry["keys"]
        if k["integer"] == integer and (n := sum(len(k[f]) for f in fields))
    ]


def joined(prefix: str, parts: list[str]) -> str:
    return f"{prefix}: {'; '.join(parts)}."


def risks(report: dict) -> list[str]:
    spaces, storage, refs = report["spaces"], report["browser_storage"], report["broken_refs"]
    clocks = {spaces[k]["active_clock_ms"] for k in SIDES}
    folder_hits = [f"{n} {len(f['collisions'])}" for n, f in report["folders"].items() if f["collisions"]]
    declared = [f"{r['space']} {r['table']}.{r['column']} {r['rows']}" for r in refs if r["declared"]]
    soft = [f"{r['space']} {r['table']}.{r['column']} {r['rows']}" for r in refs if not r["declared"]]
    without_row = sum(len(v) for f in report["folders"].values() for v in f["without_row"].values())
    defaults = storage["space2"]["sessions_by_login"].get(DEFAULT_ACCOUNT, 0)
    own = sum(report["own_paths"]["space2"].values())
    missing = storage["space1"]["missing_logins"] + storage["space2"]["missing_logins"]
    text_diff, text_same = key_hits(report, False, "different"), key_hits(report, False, "same")
    integer = key_hits(report, True, "same", "different")
    rules = [
        (report["schema_diff"] or spaces["space1"]["schema_version"] != spaces["space2"]["schema_version"],
         joined("The schemas differ; migrate both DBs to one version first", list(report["schema_diff"]))),
        (text_diff, joined("Text keys in both Spaces with different rows (one would be lost)", text_diff)),
        (folder_hits, joined("Data folder entries with the same name in both Spaces", folder_hits)),
        (defaults, f"{defaults} Space 2 sessions run on login id 'default'; in one window that id"
                   " names one login, so map them to Space 2's own login."),
        (own, f"{own} Space 2 DB values hold a path into MonoCode 2's data folder; rewrite them."),
        (None not in clocks and len(clocks) == 2,
         "The active clocks differ; rebase Space 2's active-time columns (asks, outbox, ask events)."),
        (integer, joined("Integer keys in both Spaces (renumber Space 2 rows)", integer)),
        (declared, joined("Rows that break a declared foreign key", declared)),
        (missing, f"{len(missing)} login ids in sessions or selections are not in browser storage."),
        (storage["keys_differ"],
         f"{len(storage['keys_differ'])} browser-storage keys hold different values in each Space; pick a rule per key."),
        (text_same, joined("Rows identical in both Spaces (skip on import)", text_same)),
        (soft, joined("Rows that name an id that is not there (undeclared references)", soft)),
        (without_row, f"{without_row} data folder entries have no row in their table."),
    ]
    return [message for hit, message in rules if hit]


def render(report: dict) -> str:
    lines = [f"Merge dry run {report['generated_at']} ({report['format']})", ""]
    lines += [f"{k}: {v['app']}, schema {v['schema_version']}, {v['data_dir']}" for k, v in report["spaces"].items()]
    lines += ["", "Risks, most severe first:"]
    lines += [f"  {i}. {r}" for i, r in enumerate(report["risks"], 1)] or ["  none"]
    lines += ["", f"  {'rows per table':<34} {'space1':>8} {'space2':>8}   key collisions (same / different)"]
    for table, entry in report["tables"].items():
        counts = " ".join(f"{'-' if n is None else n:>8}" for n in entry["rows"].values())
        hits = [
            f"({', '.join(k['columns'])}) {len(k['same'])} / {len(k['different'])}"
            for k in entry["keys"] if k["same"] or k["different"]
        ]
        lines.append(f"  {table:<34} {counts}   {' '.join(hits)}".rstrip())
    lines += ["", "Broken references:"]
    lines += [
        f"  {r['space']} {r['table']}.{r['column']} -> {r['target']}"
        f" ({'declared' if r['declared'] else 'undeclared'}): {r['rows']} rows, {len(r['values'])} ids"
        for r in report["broken_refs"]
    ] or ["  none"]
    lines += [f"  {k} not checked (table or column absent): {', '.join(v)}" for k, v in report["skipped_refs"].items() if v]
    lines += ["", "Values holding a path into the Space's own data folder:"]
    lines += [f"  {k} {col}: {n}" for k, cols in report["own_paths"].items() for col, n in cols.items()] or ["  none"]
    lines += ["", "Data folders that move (entries/files/bytes per Space; collisions; entries with no row):"]
    for name, f in report["folders"].items():
        sides = "  ".join(
            f"{k} {len(f[k]['entries'])}/{f[k]['files']}/{f[k]['bytes']}" if f[k]["exists"] else f"{k} absent"
            for k in SIDES
        )
        missing = sum(len(v) for v in f["without_row"].values())
        lines.append(f"  {name:<18} {sides}; {len(f['collisions'])} collide; {missing} without row")
    lines += [f"  other {k}: {', '.join(v) or 'none'}" for k, v in report["other_entries"].items()]
    storage = report["browser_storage"]
    lines += ["", "Browser storage:"]
    for k in SIDES:
        s = storage[k]
        accounts = ", ".join(f"{p} {len(a)}" for p, a in s["accounts"].items()) or "none saved"
        by_login = ", ".join(f"{a} {n}" for a, n in s["sessions_by_login"].items()) or "none"
        lines.append(f"  {k}: {s['files']} files, {len(s['keys'])} keys; saved logins: {accounts};"
                     f" {s['selections']} selections; sessions by login: {by_login};"
                     f" missing logins: {len(s['missing_logins'])}")
    lines.append(f"  keys with different values: {len(storage['keys_differ'])};"
                 f" only in space2: {len(storage['keys_only_in_space2'])}")
    return "\n".join(lines) + "\n"


def main(argv=None) -> int:
    home = Path.home()
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--space1", type=Path, default=home / "Library/Application Support/com.monocode.desktop")
    parser.add_argument("--space2", type=Path, default=home / "Library/Application Support/com.monocode.second")
    parser.add_argument("--webkit1", type=Path, default=home / "Library/WebKit/com.monocode.desktop")
    parser.add_argument("--webkit2", type=Path, default=home / "Library/WebKit/com.monocode.second")
    args = parser.parse_args(argv)
    out = args.out.resolve()
    guarded = [REPO] + [p.resolve() for p in (args.space1, args.space2, args.webkit1, args.webkit2)]
    if inside := next((p for p in guarded if out == p or p in out.parents), None):
        print(f"merge-dry-run: --out must be outside {inside}: the report holds real ids", file=sys.stderr)
        return 2
    spaces = (
        Space("space1", "MonoCode 1", "com.monocode.desktop", args.space1, args.webkit1, "main"),
        Space("space2", "MonoCode 2", "com.monocode.second", args.space2, args.webkit2, "s2"),
    )
    try:
        with tempfile.TemporaryDirectory(prefix="merge-dry-run-", dir="/tmp") as tmp:
            report = build(spaces, Path(tmp))
    except sqlite3.Error as error:
        print(f"merge-dry-run: {error}", file=sys.stderr)
        return 1
    out.mkdir(parents=True, exist_ok=True)
    (out / "merge-dry-run.json").write_text(json.dumps(report, indent=2) + "\n")
    (out / "merge-dry-run.txt").write_text(render(report))
    print(f"merge-dry-run: {len(report['risks'])} risks; report in {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
