#!/usr/bin/env python3
"""Report how late each Orchestrator completion notice was.

Each `finished.log` line is a worker end. The script finds the worker turn the
Orchestrator monitored that ended at that logged time, then the first
completion-notice turn in the Orchestrator's chat that names that session.
The delay is from the turn end to the start of the notice turn.

The database is read from a `.backup` copy: pass one with --db, or the script
makes a fresh one of the MonoCode 2 database in a temporary folder.

Exit 0: every notice came within 10 minutes. Exit 1: a notice came later than
that (LATE), or a worker turn ended more than 10 minutes ago with no notice
(MISSING). A MISSING turn can also be one the Orchestrator never asked to be
notified about (a sessions.send without notifyOnComplete); read the session.
"""

from __future__ import annotations

import argparse
import json
import re
import sqlite3
import sys
import tempfile
from contextlib import closing
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

SYDNEY = ZoneInfo("Australia/Sydney")
LIVE_DB = Path.home() / "Library/Application Support/com.monocode.second/monocode.db"
FINISHED_LOG = (
    Path.home() / "MonoCode/gigas/monocode-general-2/projects/shared/finished.log"
)
ORCHESTRATOR = "MonoCode Fork Orchestrator"
NOTICE_PREFIX = "MonoCode completion notification"
LATE = timedelta(minutes=10)
FAILING = ("LATE", "MISSING")
# A log line is written just before the turn ends, often with minute precision.
MATCH_BEFORE = timedelta(minutes=1)
MATCH_AFTER = timedelta(minutes=5)
# The turn end is rounded from start + duration; its notice can start a moment before.
NOTICE_SLACK = timedelta(seconds=2)

LOG_TIME = re.compile(
    r"(\d{4}-\d{2}-\d{2})[ T](\d{2}:\d{2}(?::\d{2})?)(Z| ?[+-]\d{4})?"
)
# Head SHAs and PR numbers tie a log line to the turn whose report names them.
LOG_TOKEN = re.compile(r"\b[0-9a-f]{7,40}\b|#\d+")


@dataclass(frozen=True)
class LogEntry:
    at: datetime
    line: str


@dataclass(frozen=True)
class TurnEnd:
    session: str
    at: datetime
    report: str


@dataclass(frozen=True)
class Notice:
    at: datetime
    sessions: frozenset[str]


@dataclass(frozen=True)
class Gap:
    entry: LogEntry
    end: TurnEnd | None
    notice: Notice | None
    verdict: str

    @property
    def delay(self) -> timedelta | None:
        return self.notice.at - self.end.at if self.end and self.notice else None


def parse_log(text: str) -> list[LogEntry]:
    entries = []
    for line in text.splitlines():
        found = LOG_TIME.search(line)
        if not found:
            continue
        day, clock, zone = found.groups()
        at = datetime.fromisoformat(f"{day}T{clock}{(zone or '').strip()}")
        if at.tzinfo is None:
            at = at.replace(tzinfo=SYDNEY)
        entries.append(LogEntry(at, line.strip()))
    return entries


def ms(value: int) -> datetime:
    return datetime.fromtimestamp(value / 1000, timezone.utc)


def turn_ends(session: str, blocks: list[dict]) -> list[TurnEnd]:
    """One end per finished turn: its prompt block's start plus duration."""
    ends = []
    for index, block in enumerate(blocks):
        if block.get("role") != "user" or block.get("durationMs") is None:
            continue
        report = []
        for later in blocks[index + 1 :]:
            if later.get("role") == "user":
                break
            if later.get("role") == "assistant":
                report.append(later.get("text", ""))
        at = ms(block["startedAt"] + block["durationMs"])
        ends.append(TurnEnd(session, at, "\n".join(report)))
    return ends


def notice_sessions(text: str) -> frozenset[str]:
    payload, _ = json.JSONDecoder().raw_decode(text[text.index("\n\n{") + 2 :])
    reports = payload["sessions"] if "sessions" in payload else [payload]
    return frozenset(report["sessionId"] for report in reports)


def read_db(con: sqlite3.Connection, mono: str) -> tuple[list[TurnEnd], list[Notice]]:
    row = con.execute(
        "SELECT data_json FROM monos WHERE name = ?", (mono,)
    ).fetchone()
    if not row:
        raise SystemExit(f"notice-gap: no Mono named {mono!r} in the database")
    chat = json.loads(row[0])["sessionId"]
    notices, monitored = [], set()
    for (raw,) in con.execute(
        "SELECT block_json FROM mono_blocks WHERE session_id = ? AND is_user = 1"
        " ORDER BY seq",
        (chat,),
    ):
        block = json.loads(raw)
        monitored.update(s["sessionId"] for s in block.get("monoSpawnedSessions", []))
        if block.get("text", "").startswith(NOTICE_PREFIX) and "startedAt" in block:
            notices.append(Notice(ms(block["startedAt"]), notice_sessions(block["text"])))
    ends = []
    for session, raw in con.execute(
        f"SELECT id, blocks_json FROM sessions WHERE id IN ({','.join('?' * len(monitored))})",
        sorted(monitored),
    ):
        ends.extend(turn_ends(session, json.loads(raw)))
    return ends, notices


def match_end(entry: LogEntry, ends: list[TurnEnd]) -> TurnEnd | None:
    """The monitored turn that ended at the logged time; most shared SHAs or PRs win."""
    tokens = set(LOG_TOKEN.findall(entry.line))
    candidates = [
        end
        for end in ends
        if entry.at - MATCH_BEFORE <= end.at <= entry.at + MATCH_AFTER
    ]
    if not candidates:
        return None
    return min(
        candidates,
        key=lambda end: (
            -len(tokens & set(LOG_TOKEN.findall(end.report))),
            abs(end.at - entry.at),
        ),
    )


def notice_for(end: TurnEnd, ends: list[TurnEnd], notices: list[Notice]) -> Notice | None:
    """The first notice naming the session after this end and before its next end.

    A notice after the next end reports that later turn, so this one had none.
    """
    next_end = min(
        (e.at for e in ends if e.session == end.session and e.at > end.at),
        default=datetime.max.replace(tzinfo=timezone.utc),
    )
    return min(
        (
            n
            for n in notices
            if end.session in n.sessions and end.at - NOTICE_SLACK <= n.at < next_end
        ),
        key=lambda n: n.at,
        default=None,
    )


def gaps(
    entries: list[LogEntry], ends: list[TurnEnd], notices: list[Notice], now: datetime
) -> list[Gap]:
    result = []
    for entry in entries:
        end = match_end(entry, ends)
        if not end:
            result.append(Gap(entry, None, None, "unmatched"))
            continue
        notice = notice_for(end, ends, notices)
        if notice:
            verdict = "LATE" if notice.at - end.at > LATE else "ok"
        else:
            verdict = "MISSING" if now - end.at > LATE else "pending"
        result.append(Gap(entry, end, notice, verdict))
    return result


def clock(at: datetime | None) -> str:
    return at.astimezone(SYDNEY).strftime("%m-%d %H:%M:%S") if at else "-"


def render(rows: list[Gap]) -> str:
    lines = [f"{'logged':<14}  {'session':<8}  {'turn end':<14}  {'notice':<14}  {'delay':>7}  verdict    line"]
    for gap in rows:
        delay = f"{gap.delay.total_seconds() / 60:.1f}m" if gap.delay is not None else "-"
        session = gap.end.session[-8:] if gap.end else "-"
        lines.append(
            f"{clock(gap.entry.at)[:11]:<14}  {session:<8}  {clock(gap.end and gap.end.at):<14}  "
            f"{clock(gap.notice and gap.notice.at):<14}  {delay:>7}  {gap.verdict:<9}  {gap.entry.line[:70]}"
        )
    late = [gap for gap in rows if gap.verdict in FAILING]
    lines.append(
        f"{len(rows)} worker ends, {len(late)} late or missing notices (more than {LATE.seconds // 60} min)"
    )
    return "\n".join(lines)


def backup(source: Path, folder: str) -> Path:
    target = Path(folder) / "monocode.db"
    with closing(sqlite3.connect(f"file:{source}?mode=ro", uri=True)) as live, closing(
        sqlite3.connect(target)
    ) as copy:
        live.backup(copy)
    return target


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("log", nargs="?", type=Path, default=FINISHED_LOG)
    parser.add_argument("--db", type=Path, help="a .backup copy; default: back up the MonoCode 2 database")
    parser.add_argument("--mono", default=ORCHESTRATOR)
    parser.add_argument("--hours", type=float, help="only log lines from the last N hours")
    parser.add_argument("--now", type=datetime.fromisoformat, help="ISO time with offset; default: now")
    args = parser.parse_args(argv)
    now = args.now or datetime.now(timezone.utc)
    entries = parse_log(args.log.read_text())
    if args.hours is not None:
        entries = [e for e in entries if e.at >= now - timedelta(hours=args.hours)]
    with tempfile.TemporaryDirectory() as folder:
        db = args.db or backup(LIVE_DB, folder)
        with closing(sqlite3.connect(f"file:{db}?mode=ro", uri=True)) as con:
            ends, notices = read_db(con, args.mono)
    rows = gaps(entries, ends, notices, now)
    print(render(rows))
    return 1 if any(gap.verdict in FAILING for gap in rows) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
