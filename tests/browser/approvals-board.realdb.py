#!/usr/bin/env python3
import json
import re
import sqlite3
import uuid
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HOME = str(Path.home())
APP_DB = Path(HOME, "Library/Application Support/com.monocode.desktop/monocode.db")
OUT = ROOT / "tests/browser/approvals-board.realdb.json"
NOW = int(datetime(2026, 10, 19, 22, 54, tzinfo=timezone.utc).timestamp() * 1000)
MIN = 60_000
SENDER = "approvals-board"
# decide.rs UNDELIVERED: null with no dropped row, 0 when requeue resends it, 1 when the user removed it.
DECIDE_RS_UNDELIVERED = f"""(SELECT notified_at IS NOT NULL FROM mono_messages
  WHERE from_session_id = '{SENDER}' AND request_id = approvals.id AND status = 'dropped') AS removed"""

rust = (ROOT / "src-tauri/src/approvals.rs").read_text()
ddl = re.search(r'execute_batch\(\s*"(CREATE TABLE IF NOT EXISTS approvals.*?)",\s*\)', rust, re.S).group(1)

db = sqlite3.connect(":memory:")
with sqlite3.connect(f"{APP_DB.as_uri()}?mode=ro", uri=True) as app:
    app.backup(db)
db.execute("PRAGMA foreign_keys = ON")
db.executescript(ddl)
db.execute("DELETE FROM approvals")

giga_id, giga_name = db.execute(
    "SELECT id, name FROM monos WHERE role = 'giga' ORDER BY sort_order, id LIMIT 1"
).fetchone()
mono_project = {
    name: json.loads(data)["projects"][0]
    for name, data in db.execute("SELECT name, data_json FROM monos WHERE role = 'mono'")
}
MONOCODE = mono_project["MonoCode Fork Orchestrator"]
MODELIST = mono_project["Modelist Orchestrator"]

GONE_ID = str(uuid.uuid4())
db.execute(
    "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at) VALUES (?, ?, 'giga', ?, 99, ?, ?)",
    (GONE_ID, "Modelist General",
     json.dumps({"name": "Modelist General", "mascot": "alien", "color": "hsl(211 92% 62%)", "projects": [MODELIST]}),
     NOW - 600 * MIN, NOW - 600 * MIN),
)


def option(label, description, recommended=False):
    return {"label": label, "description": description, "recommended": recommended}


def file_card(project, heading, summary, pros, cons, urgency, created, by=(giga_id, giga_name),
              options=(), links=(), origin=None, closed=None):
    columns = {
        "id": str(uuid.uuid4()),
        "project_path": project,
        "heading": heading,
        "summary": summary,
        "tradeoffs": json.dumps({"pros": pros, "cons": cons}),
        "options": json.dumps(list(options)),
        "urgency": urgency,
        "links": json.dumps(list(links)),
        "origin": origin,
        "requested_by": by[0],
        "requested_by_name": by[1],
        "created_at": created,
        "status": "open",
    }
    if closed:
        status, at, note, chosen = closed
        columns["status"] = status
        columns["withdrawn_at" if status == "withdrawn" else "decided_at"] = at
        columns["decision_note"] = note
        columns["chosen_options"] = json.dumps(chosen) if chosen else None
    names = ", ".join(columns)
    marks = ", ".join("?" for _ in columns)
    db.execute(f"INSERT INTO approvals ({names}) VALUES ({marks})", list(columns.values()))
    return columns["id"]


def at(day, hour, minute):
    return int(datetime(2026, 10, day, hour, minute, tzinfo=timezone.utc).timestamp() * 1000)


GATE = [
    option("Wait for #109, then build S2 on its caller identity",
           "S2 and S3 start when #109 merges. One gate, one identity path.", True),
    option("Build S2 on tyler/main now, rebase onto #109 later",
           "No wait. S2 reads the session id itself until #109 lands, then switches."),
    option("Split the identity part of #109 into its own small PR",
           "MonoCode 2 opens it today; both instances build on it."),
]
INDEX = [option("Index (project_id, status) only", None, True),
         option("Also index (requested_by, status)", None)]
CSV = [option("Drop CSV, ship JSON only", "Add CSV in v1.1 with a designed flattening.", True),
       option("Keep CSV, slip v1 a week", "Design the flattening first.")]

file_card(MONOCODE, "Bump Playwright to 1.49",
          "The 1.49 release fixes the WebKit focus bug two specs work around.",
          ["Two workarounds go"], ["A new browser download in CI"], "not_blocking", at(17, 9, 0),
          origin="MonoCode Fork Orchestrator", closed=("withdrawn", at(17, 16, 12), None, None))
unqueued = file_card(MONOCODE, "Store the Giga and Mono discussion on the card",
          "Keep the full back-and-forth between the Giga and the Mono on the card, so the board shows why it was filed.",
          ["The board explains itself"], ["The card grows without limit"], "not_blocking", at(18, 9, 30),
          origin="MonoCode Fork Orchestrator",
          closed=("rejected", at(18, 17, 51), "No. The Giga has the context; the board stays small.", None))
file_card(MONOCODE, "Index approvals on (project_id, status) only",
          "The ticket asks for one index. Withdraw and update also look up a card by its filer.",
          ["One index to keep"], ["Filer lookups scan the project"], "not_blocking", at(18, 11, 0),
          options=INDEX, origin="Mono Reviewer",
          closed=("changes_requested", at(18, 18, 5),
                  "Take option 2 as well, then file it again with the query plan from the copied database.",
                  ["Also index (requested_by, status)"]))
not_sent = file_card(MONOCODE, "Use a fake clock in the E2E suite",
                     "Two specs wait in real time for the badge to refresh.",
                     ["No real waits"], ["Every spec sets the clock"], "blocking", at(19, 15, 0),
                     origin="Mono Reviewer",
                     closed=("approved", NOW - 120 * MIN, None, None))
file_card(MONOCODE, "Show decided cards for 30 days, then hide them?",
          "Decided cards stay on the board for good, so after a month the Decided list gets long. Hidden cards stay in the table; only the board stops listing them.",
          ["The board stays short"], ["Old decisions need a second click"], "not_blocking", NOW - 60 * MIN,
          options=[option("Hide after 30 days", "A “Show older” row at the end of Decided.", True),
                   option("Keep them all", "Nothing hidden; the list grows.")],
          links=[{"kind": "linear", "title": "ADE-53", "url": "https://linear.app/adebuild/issue/ADE-53"}],
          origin="MonoCode Fork Orchestrator")
file_card(MONOCODE, "Keep Playwright at one worker in CI?",
          "CI runs the E2E suite with --workers=1 to match the machine rule. A full run takes 14 minutes. Two workers cut it to 8, but two specs share the copied app database and flake about one run in ten.",
          ["No flakes from the shared database copy", "Same setting locally and in CI"],
          ["14-minute runs on every push", "Slower feedback on the S6 E2E PRs"], "not_blocking", NOW - 5 * MIN,
          links=[{"kind": "pr", "title": "PR #112 CI config", "url": "https://github.com/tylers2222/Monocode-Unlocked/pull/112"}],
          origin="Mono Reviewer")
file_card(MONOCODE, "Wait for PR #109 before the approvals tool is built?",
          "The approvals_board tool must know which Giga called it. PR #109 adds that caller identity, but MonoCode 2’s Fork Orchestrator owns it and it is still in review. S1 is done; S2 and S3 wait on this.",
          ["One caller identity and one gate, as the ticket asks", "No rebase or second code path to delete later"],
          ["S2 and S3 slip until #109 merges, likely a day", "This instance idles on account 1 meanwhile"],
          "blocking", NOW - 12 * MIN, options=GATE,
          links=[{"kind": "pr", "title": "PR #109 caller identity", "url": "https://github.com/tylers2222/Monocode-Unlocked/pull/109"},
                 {"kind": "linear", "title": "ADE-51", "url": "https://linear.app/adebuild/issue/ADE-51"},
                 {"kind": "doop", "title": "Approvals board frames", "url": "https://doop.design/c/oRuRCkta7A"}],
          origin="MonoCode Fork Orchestrator")
gone = (GONE_ID, "Modelist General")
file_card(MODELIST, "Keep the model picker in v1?",
          "The picker works for flat configs only.",
          ["Users choose a model"], ["Nested configs fail"], "not_blocking", NOW - 600 * MIN, by=gone,
          options=[option("Keep it", None, True), option("Hide it", None)], origin="Modelist Orchestrator",
          closed=("approved", at(19, 13, 20), "Told the Modelist Orchestrator myself.", ["Keep it"]))
file_card(MODELIST, "Drop the CSV export from Modelist v1?",
          "The CSV export breaks on nested model configs and needs a flattening step nobody has designed. JSON export already works. Modelist Orchestrator’s worker stopped at the export screen.",
          ["v1 ships this week", "One export format to test"],
          ["Two early users asked for CSV by name", "CSV comes back as v1.1 work"],
          "blocking", at(18, 20, 0), by=gone, options=CSV,
          links=[{"kind": "linear", "title": "ADE-38", "url": "https://linear.app/adebuild/issue/ADE-38"}],
          origin="Modelist Orchestrator")
# A chat reset dropped this decision (it waits for the Giga's next chat), then the user deleted the Giga.
gone_waiting = file_card(MODELIST, "Pin the model list to one provider API version?",
                         "The model list changes whenever a provider ships a model, and two screenshots in the docs went stale.",
                         ["The docs stay right"], ["New models wait for a release"], "not_blocking", NOW - 580 * MIN,
                         by=gone, origin="Modelist Orchestrator", closed=("approved", at(19, 14, 10), None, None))
# The user removed this decision from the Giga's queue, then deleted the Giga.
gone_unqueued = file_card(MODELIST, "Rename presets to profiles in v1?",
                          "Two users called presets profiles in feedback.",
                          ["Matches what users say"], ["Every doc page changes"], "not_blocking", NOW - 560 * MIN,
                          by=gone, origin="Modelist Orchestrator",
                          closed=("rejected", at(19, 15, 30), "No. Presets is the word in the docs.", None))


def drop_decision(card, message_id, payload, to, decided, dropped, removed):
    """decide.rs drops the queued decision; drop_message sets notified_at when the user removed it."""
    db.execute(
        """INSERT INTO mono_messages (id, request_id, part, payload_hash, tool, from_session_id, to_mono_id,
             addressed, body, body_chars, wanting_reply, intent, hop, status, gate_result, handled_by_session, delivery,
             closed_at, notified_at, created_at, updated_at)
           VALUES (?, ?, 0, ?, 'send_message', ?, ?, ?, 'The user decided approval card.', 31, 0, 'new', 0,
                   'dropped', 'ok', ?, 'queue', ?, ?, ?, ?)""",
        (message_id, card, payload * 64, SENDER, to, to, str(uuid.uuid4()), dropped, dropped if removed else None,
         decided, dropped),
    )


drop_decision(gone_waiting, "m_gonewaitingz", "2", GONE_ID, at(19, 14, 10), at(19, 14, 40), removed=False)
drop_decision(gone_unqueued, "m_goneremovedz", "3", GONE_ID, at(19, 15, 30), at(19, 15, 40), removed=True)
db.execute("DELETE FROM monos WHERE id = ?", (GONE_ID,))
assert db.execute("SELECT COUNT(*) FROM approvals WHERE requested_by IS NULL").fetchone()[0] == 4
assert db.execute("SELECT COUNT(*) FROM mono_messages WHERE to_mono_id IS NULL AND request_id IN (?, ?)",
                  (gone_waiting, gone_unqueued)).fetchone()[0] == 2

drop_decision(not_sent, "m_abcdefghijkl", "0", giga_id, NOW - 120 * MIN, NOW - 100 * MIN, removed=False)
drop_decision(unqueued, "m_mnopqrstuvwx", "1", giga_id, at(18, 17, 51), at(18, 18, 0), removed=True)
db.commit()
assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
assert db.execute("PRAGMA foreign_key_check").fetchall() == []

db.row_factory = sqlite3.Row
cards = []
for row in db.execute(f"SELECT *, {DECIDE_RS_UNDELIVERED} FROM approvals ORDER BY number"):
    if row["status"] == "open":
        status = {"state": "open"}
    elif row["status"] == "withdrawn":
        status = {"state": "withdrawn", "withdrawnAt": row["withdrawn_at"]}
    else:
        status = {"state": "decided", "verdict": row["status"], "decidedAt": row["decided_at"],
                  "note": row["decision_note"], "chosen": json.loads(row["chosen_options"] or "[]"),
                  "undelivered": None if row["removed"] is None else ("removed" if row["removed"] else "waiting")}
    cards.append({
        "id": row["id"], "number": row["number"], "projectPath": row["project_path"],
        "heading": row["heading"], "summary": row["summary"], "tradeoffs": json.loads(row["tradeoffs"]),
        "options": json.loads(row["options"]), "urgency": row["urgency"], "links": json.loads(row["links"]),
        "origin": row["origin"], "requestedBy": {"id": row["requested_by"], "name": row["requested_by_name"]},
        "createdAt": row["created_at"], "status": status,
    })

monos = [
    {"id": r["id"], "role": r["role"], "sortOrder": r["sort_order"],
     "data": {k: v for k, v in json.loads(r["data_json"]).items()
              if k in {"name", "mascot", "color", "projects", "gigaSlug", "sessionId"}}}
    for r in db.execute("SELECT * FROM monos ORDER BY sort_order, id")
]
projects = list(dict.fromkeys(p for m in monos for p in m["data"].get("projects", [])))


def anonymize(value):
    if isinstance(value, str):
        return value.replace(HOME, "/Users/test")
    if isinstance(value, list):
        return [anonymize(v) for v in value]
    if isinstance(value, dict):
        return {anonymize(k): anonymize(v) for k, v in value.items()}
    return value


fixture = anonymize({"now": NOW, "monos": monos, "projects": projects, "approvals": cards})
OUT.write_text(json.dumps(fixture, indent=2, ensure_ascii=False) + "\n")
print(f"monos {len(monos)}, projects {len(projects)}, cards {len(cards)}, "
      f"open {sum(c['status']['state'] == 'open' for c in cards)}, "
      f"waiting {sum(c['status'].get('undelivered') == 'waiting' for c in cards)}, "
      f"removed from the queue {sum(c['status'].get('undelivered') == 'removed' for c in cards)}, "
      f"filer removed {sum(c['requestedBy']['id'] is None for c in cards)}")
