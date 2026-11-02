"""merge-dry-run.py on two made-up app data folders, one of them with a live writer.

    python3 -I scripts/spaces/merge_dry_run_test.py
"""

import hashlib
import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("merge-dry-run.py")
SECRET = "made-up-secret-token-7f3a"
LABEL = "made-up-person@example.com"
HIDDEN_TITLE = "Made-up title that stays private"

SCHEMA = """
CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, provider_account_id TEXT);
CREATE TABLE monos (id TEXT PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE notes (id TEXT PRIMARY KEY, slug TEXT NOT NULL UNIQUE, source_session_id TEXT);
CREATE TABLE approvals (number INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE);
CREATE TABLE mono_transcripts (session_id TEXT PRIMARY KEY REFERENCES sessions(id));
CREATE TABLE mono_asks (id TEXT PRIMARY KEY, parent_actor TEXT NOT NULL);
CREATE TABLE mono_clock (id INTEGER PRIMARY KEY CHECK (id = 1), active_ms INTEGER NOT NULL);
CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
"""

SPACE1_ROWS = """
INSERT INTO sessions VALUES ('s-same', 'Made-up A', NULL), ('s-diff', 'Made-up B', 'default'), ('s-one', 'Made-up C', NULL);
INSERT INTO monos VALUES ('m-shared', 'Made-up Mono'), ('m-one', 'Made-up One');
INSERT INTO notes VALUES ('n-1', 'plan', 's-one');
INSERT INTO approvals (id) VALUES ('a-1');
INSERT INTO mono_transcripts VALUES ('s-one');
INSERT INTO mono_clock VALUES (1, 100);
INSERT INTO schema_migrations VALUES (1, 0);
"""

SPACE2_ROWS = f"""
CREATE TABLE only_two (x TEXT);
INSERT INTO sessions VALUES ('s-same', 'Made-up A', NULL), ('s-diff', '{HIDDEN_TITLE}', 'default'),
  ('s-two', 'Made-up D', 'account-1'), ('s-lost-login', 'Made-up E', 'account-zzz'),
  ('s-path', '/Users/x/Library/Application Support/com.monocode.second/backgrounds/bg.png', NULL);
INSERT INTO monos VALUES ('m-shared', 'Made-up Other Mono');
INSERT INTO notes VALUES ('n-2', 'plan', 's-missing');
INSERT INTO approvals (id) VALUES ('a-2');
INSERT INTO mono_transcripts VALUES ('ghost');
INSERT INTO mono_asks VALUES ('k-1', 'mono:gone'), ('k-2', 'mono:m-shared'), ('k-3', 'session:s-two');
INSERT INTO mono_clock VALUES (1, 200);
INSERT INTO schema_migrations VALUES (1, 0);
"""


def make_db(path: Path, sql: str) -> sqlite3.Connection:
    db = sqlite3.connect(path, isolation_level=None)
    db.execute("PRAGMA journal_mode = WAL")
    db.execute("PRAGMA wal_autocheckpoint = 0")
    db.executescript(SCHEMA + sql)
    return db


def make_storage(webkit: Path, items: dict) -> None:
    folder = webkit / "WebsiteData/Default/origin/origin/LocalStorage"
    folder.mkdir(parents=True)
    db = sqlite3.connect(folder / "localstorage.sqlite3")
    db.execute("CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB NOT NULL ON CONFLICT FAIL)")
    db.executemany("INSERT INTO ItemTable VALUES (?, ?)", [(k, v.encode("utf-16-le")) for k, v in items.items()])
    db.commit()
    db.close()


def write(path: Path, data: bytes = b"made-up") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def fingerprint(root: Path) -> dict:
    return {
        str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in sorted(root.rglob("*"))
        if p.is_file() and not p.name.endswith("-shm")
    }


class MergeDryRun(unittest.TestCase):
    def test_report_on_made_up_spaces(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            one, two = root / "space1", root / "space2"
            one.mkdir()
            two.mkdir()
            make_db(one / "monocode.db", SPACE1_ROWS).close()
            writer = make_db(two / "monocode.db", SPACE2_ROWS)
            writer.execute("INSERT INTO sessions VALUES ('s-wal', 'Made-up WAL row', NULL)")
            self.assertGreater((two / "monocode.db-wal").stat().st_size, 0, "fixture keeps rows in the WAL")
            for path in ("monos/m-shared/SOUL.md", "monos/m-one/SOUL.md", "checkpoints/s-same/turn.json"):
                write(one / path)
            write(one / "backgrounds/chat-background.png", bytes(100))
            write(one / "linear-token", SECRET.encode())
            for path in ("monos/m-shared/SOUL.md", "monos/m-orphan/SOUL.md", "checkpoints/s-two/turn.json"):
                write(two / path)
            make_storage(root / "webkit1", {"monocode.colorScheme": "dark"})
            make_storage(root / "webkit2", {
                "monocode.colorScheme": "light",
                "monocode.providerAccounts.v1": json.dumps(
                    {"claude": [{"id": "account-1", "provider": "claude", "label": LABEL}]}),
                "monocode.providerAccountSelections.v1": json.dumps({"/made/up": {"claude": "account-sel-gone"}}),
            })
            sources = (one, two, root / "webkit1", root / "webkit2")
            before = [fingerprint(source) for source in sources]
            temps_before = set(Path("/tmp").glob("merge-dry-run-*"))
            out = root / "report"
            args = [sys.executable, "-I", str(SCRIPT), "--out", str(out),
                    "--space1", str(one), "--space2", str(two),
                    "--webkit1", str(root / "webkit1"), "--webkit2", str(root / "webkit2")]

            run = subprocess.run(args, capture_output=True, text=True)
            after = [fingerprint(source) for source in sources]
            writer.close()

            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(after, before, "no source file changed")
            report = json.loads((out / "merge-dry-run.json").read_text())
            text = (out / "merge-dry-run.txt").read_text()
            self.assertEqual(report["format"], "monocode-merge-dry-run/1")
            self.assertEqual(report["tables"]["sessions"]["rows"], {"space1": 3, "space2": 6}, "WAL row counted")
            self.assertEqual(report["tables"]["only_two"]["rows"], {"space1": None, "space2": 0})
            self.assertEqual(report["schema_diff"], {"only_two": "only in space2"})

            keys = {(t, tuple(k["columns"])): k for t, e in report["tables"].items() for k in e["keys"]}
            sessions = keys[("sessions", ("id",))]
            self.assertEqual((sessions["same"], sessions["different"], sessions["integer"]), (["s-same"], ["s-diff"], False))
            self.assertEqual(keys[("notes", ("slug",))]["different"], ["plan"], "UNIQUE index collision")
            self.assertEqual(keys[("monos", ("id",))]["different"], ["m-shared"])
            approvals = keys[("approvals", ("number",))]
            self.assertEqual((approvals["different"], approvals["integer"]), ([1], True))
            self.assertEqual(keys[("mono_clock", ("id",))]["different"], [1])

            refs = {(r["space"], r["table"], r["column"], r["target"]): r for r in report["broken_refs"]}
            self.assertEqual(refs[("space2", "mono_transcripts", "session_id", "sessions.id")]["values"], ["ghost"])
            self.assertTrue(refs[("space2", "mono_transcripts", "session_id", "sessions.id")]["declared"])
            self.assertEqual(refs[("space2", "notes", "source_session_id", "sessions.id")]["values"], ["s-missing"])
            self.assertEqual(refs[("space2", "mono_asks", "parent_actor", "mono:monos.id")]["values"], ["gone"])
            self.assertEqual(len(refs), 3, sorted(refs))
            self.assertEqual(report["own_paths"], {"space1": {}, "space2": {"sessions.title": 1}})

            folders = report["folders"]
            self.assertEqual(folders["monos"]["collisions"], ["m-shared"])
            self.assertEqual(folders["monos"]["without_row"], {"space1": [], "space2": ["m-orphan"]})
            self.assertEqual(folders["checkpoints"]["without_row"], {"space1": [], "space2": []})
            self.assertEqual((folders["backgrounds"]["space1"]["files"], folders["backgrounds"]["space1"]["bytes"]), (1, 100))
            self.assertFalse(folders["note-assets"]["space2"]["exists"])
            self.assertEqual(report["other_entries"], {"space1": ["linear-token"], "space2": []})

            storage = report["browser_storage"]
            self.assertEqual(storage["space2"]["accounts"], {"claude": ["account-1"]})
            self.assertEqual(storage["space2"]["selections"], 1)
            self.assertEqual(storage["space2"]["missing_logins"], ["account-sel-gone", "account-zzz"])
            self.assertEqual(storage["space2"]["sessions_by_login"], {"account-1": 1, "account-zzz": 1, "default": 4})
            self.assertEqual(storage["space1"]["accounts"], {})
            self.assertEqual(storage["keys_differ"], ["monocode.colorScheme"])
            self.assertEqual(report["spaces"]["space2"]["active_clock_ms"], 200)

            risks = report["risks"]
            self.assertTrue(risks[0].startswith("The schemas differ"), risks)
            self.assertTrue(risks[1].startswith("Text keys in both Spaces with different rows"), risks)
            self.assertIn("monos(id) 1", risks[1])
            self.assertTrue(any(r.startswith("4 Space 2 sessions run on login id 'default'") for r in risks), risks)
            self.assertTrue(any(r.startswith("1 Space 2 DB values hold a path") for r in risks), risks)
            self.assertIn("1. The schemas differ", text)
            self.assertRegex(text, r"\n  sessions +3 +6 +\(id\) 1 / 1\n")
            self.assertIn("monos              space1 2/2/14  space2 2/2/14; 1 collide; 1 without row", text)

            for output in (text, json.dumps(report)):
                for private in (SECRET, LABEL, HIDDEN_TITLE, "light", "dark"):
                    self.assertNotIn(private, output, "stored values never reach the report")
            self.assertEqual(set(Path("/tmp").glob("merge-dry-run-*")), temps_before, "the /tmp copies are deleted")

    def test_refuses_an_out_folder_inside_the_repo(self):
        out = SCRIPT.parent / "report-must-not-exist"
        missing = [f"--{flag}=/nonexistent/{flag}" for flag in ("space1", "space2", "webkit1", "webkit2")]
        run = subprocess.run([sys.executable, "-I", str(SCRIPT), "--out", str(out), *missing], capture_output=True, text=True)
        self.assertEqual(run.returncode, 2)
        self.assertIn("--out must be outside", run.stderr)
        self.assertFalse(out.exists())


if __name__ == "__main__":
    unittest.main()
