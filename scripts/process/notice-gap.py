#!/usr/bin/env python3
"""Report how late each Orchestrator completion notice was.

Every turn of a session the Orchestrator monitors (its `monoSpawnedSessions`)
that ended since the first `finished.log` line (or in the last --hours) gets a
row: the first completion-notice turn in the Orchestrator's chat with a report
of that session that quotes the turn's prompt, and the delay between them.
A `finished.log` line is attached to a turn only when it names the session or
shares a head SHA or PR number with the turn's reports; the time decides only
between turns that share one. A line that matches no turn is `unmatched`.

The database is read from a `.backup` copy: pass one with --db, or the script
makes a fresh one of the MonoCode 2 database in a temporary folder.

Exit 0: every notice came within 10 minutes. Exit 1: a notice came later than
that (LATE), or a turn whose notice was asked for ended more than 10 minutes
ago with none (MISSING). Whether a turn was asked for:
- the session's first turn (`sessions.start`) is asked;
- the app's own continue (no prompt with an `appRequestId`) is `not-asked`;
- the `sessions.send` calls that quote one of the turn's prompts decide by
  `notifyOnComplete`;
- a requested turn that no readable send quotes is asked, except a queued
  follow-up: that is asked only when a send names the session with its prompt
  cut off (the app keeps a command's first line only).

Rows come from turns that ended. A send that the app accepted but never ran as
a turn, such as a queued follow-up (`sessions.send` with `whenBusy` `followUp`)
that was dropped (ADE-73), has no row, so the script cannot report it.
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
from typing import Iterable, Literal
from zoneinfo import ZoneInfo

SYDNEY = ZoneInfo("Australia/Sydney")
LIVE_DB = Path.home() / "Library/Application Support/com.monocode.second/monocode.db"
FINISHED_LOG = (
    Path.home() / "MonoCode/gigas/monocode-general-2/projects/shared/finished.log"
)
ORCHESTRATOR = "MonoCode Fork Orchestrator"
NOTICE_PREFIX = "MonoCode completion notification"
LATE = timedelta(minutes=10)
# A log line is written just before the turn ends, often with minute precision.
MATCH_BEFORE = timedelta(minutes=1)
MATCH_AFTER = timedelta(minutes=5)
# The turn end is rounded from start + duration; its notice can start a moment before.
NOTICE_SLACK = timedelta(seconds=2)
# A send quotes its prompt up to the first character that a shell or jq escapes.
# Briefs share long openings ("Read /Users/tylerstewart/MonoCode/gigas/..."), so
# the whole plain prefix is compared.
PLAIN = re.compile(r"[^'\"\\$`\n]*")
MIN_QUOTE = 12

LOG_TIME = re.compile(
    r"(\d{4}-\d{2}-\d{2})[ T](\d{2}:\d{2}(?::\d{2})?)(Z| ?[+-]\d{4})?"
)
# Head SHAs (compared on 7 characters) and PR numbers tie a log line to a turn.
IDENTITY = re.compile(r"\b(?=[0-9a-f]*\d)([0-9a-f]{7})[0-9a-f]{0,33}\b|(?:#|/pull/)(\d+)\b")
# The command text (with shell or jq quoting) or the CLI's answer.
NOTIFY = re.compile(r'notifyOnComplete\\*"?\s*:\s*true')

Verdict = Literal["ok", "LATE", "MISSING", "pending", "not-asked", "unmatched"]
FAILING: tuple[Verdict, ...] = ("LATE", "MISSING")


@dataclass(frozen=True)
class LogEntry:
    at: datetime
    line: str


@dataclass(frozen=True)
class TurnEnd:
    session: str
    start: datetime
    at: datetime
    report: str
    # The session's first prompt: a sessions.start, which notifies by default.
    first: bool
    # The opening prompt and every steer that joined the turn; a notice names one.
    prompts: tuple[str, ...]
    # Some prompt of the turn came from an app request (sessions.start or .send). The
    # app's own continue after a relaunch or a usage limit has none, and no watcher.
    requested: bool
    # The turn opened from the session's queue (a sessions.send whenBusy followUp).
    queued: bool


@dataclass(frozen=True)
class Notice:
    at: datetime
    # (sessionId, originalPrompt) for each report in the notice.
    reports: frozenset[tuple[str, str]]


@dataclass(frozen=True)
class Send:
    """A sessions.send tool call, timed by the start of the Orchestrator turn that made it."""

    start: datetime
    command: str
    notify: bool


@dataclass(frozen=True)
class Gap:
    end: TurnEnd | None
    lines: tuple[LogEntry, ...]
    notice: Notice | None
    verdict: Verdict

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
    """One end per finished turn: its prompt block's start plus duration.

    A steer joins the running turn as a user block with no startedAt.
    """
    ends = []
    prompts_at = [
        i for i, block in enumerate(blocks) if block.get("role") == "user" and "startedAt" in block
    ]
    for index, next_prompt in zip(prompts_at, prompts_at[1:] + [len(blocks)]):
        block = blocks[index]
        if block.get("durationMs") is None:
            continue
        turn = blocks[index:next_prompt]
        report = "\n".join(b.get("text", "") for b in turn if b.get("role") == "assistant")
        prompts = [b for b in turn if b.get("role") == "user"]
        start = ms(block["startedAt"])
        at = start + timedelta(milliseconds=block["durationMs"])
        ends.append(
            TurnEnd(
                session,
                start,
                at,
                report,
                index == prompts_at[0],
                tuple(b.get("text", "") for b in prompts),
                any("appRequestId" in b for b in prompts),
                (block.get("appDelivery") or {}).get("via") == "queued",
            )
        )
    return ends


def notice_reports(text: str) -> frozenset[tuple[str, str]]:
    payload, _ = json.JSONDecoder().raw_decode(text[text.index("\n\n{") + 2 :])
    reports = payload["sessions"] if "sessions" in payload else [payload]
    return frozenset((report["sessionId"], report.get("originalPrompt", "")) for report in reports)


def read_db(
    con: sqlite3.Connection, mono: str
) -> tuple[list[TurnEnd], list[Notice], list[Send]]:
    row = con.execute("SELECT data_json FROM monos WHERE name = ?", (mono,)).fetchone()
    if not row:
        raise SystemExit(f"notice-gap: no Mono named {mono!r} in the database")
    chat = json.loads(row[0])["sessionId"]
    notices, sends, monitored = [], [], set()
    turn: datetime | None = None
    for (raw,) in con.execute(
        "SELECT block_json FROM mono_blocks WHERE session_id = ? ORDER BY seq", (chat,)
    ):
        block = json.loads(raw)
        text = block.get("text", "")
        if block.get("role") == "user" and "startedAt" in block:
            turn = start = ms(block["startedAt"])
            monitored.update(s["sessionId"] for s in block.get("monoSpawnedSessions", []))
            if text.startswith(NOTICE_PREFIX):
                notices.append(Notice(start, notice_reports(text)))
        elif block.get("role") == "tool" and turn and "sessions.send" in text:
            answer = str((block.get("tool") or {}).get("detail", ""))
            sends.append(Send(turn, text, bool(NOTIFY.search(text) or NOTIFY.search(answer))))
    ends = []
    for session, raw in con.execute(
        f"SELECT id, blocks_json FROM sessions WHERE id IN ({','.join('?' * len(monitored))})",
        sorted(monitored),
    ):
        ends.extend(turn_ends(session, json.loads(raw)))
    return ends, notices, sends


def identity(text: str) -> set[str]:
    return {sha or f"#{pr}" for sha, pr in IDENTITY.findall(text)}


def short_ids(session: str) -> tuple[str, str]:
    """The two 8-character forms that logs and commands use: last and uuid-first."""
    return session[-8:], session[-36:-28]


def names(text: str, session: str) -> bool:
    return any(short in text for short in short_ids(session))


def match_end(entry: LogEntry, ends: list[TurnEnd]) -> TurnEnd | None:
    """The turn near the logged time that the line names (its session id) or shares
    the most head SHAs and PR numbers with; the time decides only between those."""
    tokens = identity(entry.line)
    scored = [
        (len(tokens & identity(end.report)) + names(entry.line, end.session), end)
        for end in ends
        if entry.at - MATCH_BEFORE <= end.at <= entry.at + MATCH_AFTER
    ]
    scored = [(shared, end) for shared, end in scored if shared]
    if not scored:
        return None
    return min(scored, key=lambda item: (-item[0], abs(item[1].at - entry.at)))[1]


def reports_turn(original: str, end: TurnEnd) -> bool:
    """A notice quotes the prompt it watched, cut to a budget; match on the shared prefix."""
    return bool(original) and any(
        prompt.startswith(original) or original.startswith(prompt) for prompt in end.prompts if prompt
    )


def notice_for(end: TurnEnd, notices: list[Notice]) -> Notice | None:
    """The first notice after this end with a report of this session's turn."""
    return min(
        (
            n
            for n in notices
            if n.at >= end.at - NOTICE_SLACK
            and any(session == end.session and reports_turn(original, end) for session, original in n.reports)
        ),
        key=lambda n: n.at,
        default=None,
    )


def quoted(prompt: str) -> str:
    """The prompt's text up to its first character that a shell or jq would escape."""
    return PLAIN.match(prompt).group(0)


def quotes(command: str, prompts: Iterable[str]) -> bool:
    return any(len(opening) >= MIN_QUOTE and opening in command for opening in map(quoted, prompts))


def asked(end: TurnEnd, sends: list[Send], session_prompts: tuple[str, ...]) -> bool:
    """Whether the Orchestrator asked to be told when this turn ends.

    A sessions.start asks by default. The app's own continue never asks. Otherwise
    the sends that quote one of the turn's prompts decide. With none, a direct turn
    is asked: its send may name the session on a line the app did not keep. A queued
    follow-up is asked only when a send names the session but quotes no prompt of
    any of its turns, so that send's prompt was cut off.
    """
    if end.first:
        return True
    if not end.requested:
        return False
    named = [send for send in sends if send.start <= end.at and names(send.command, end.session)]
    made = [send.notify for send in named if quotes(send.command, end.prompts)]
    if made:
        return any(made)
    if not end.queued:
        return True
    return any(not quotes(send.command, session_prompts) for send in named)


def verdict(
    end: TurnEnd, notice: Notice | None, sends: list[Send], session_prompts: tuple[str, ...], now: datetime
) -> Verdict:
    if notice:
        return "LATE" if notice.at - end.at > LATE else "ok"
    if not asked(end, sends, session_prompts):
        return "not-asked"
    return "MISSING" if now - end.at > LATE else "pending"


def gaps(
    entries: list[LogEntry],
    ends: list[TurnEnd],
    notices: list[Notice],
    sends: list[Send],
    since: datetime,
    now: datetime,
) -> list[Gap]:
    lines: dict[TurnEnd, list[LogEntry]] = {}
    unmatched = []
    for entry in entries:
        end = match_end(entry, ends)
        if end:
            lines.setdefault(end, []).append(entry)
        else:
            unmatched.append(Gap(None, (entry,), None, "unmatched"))
    prompts: dict[str, tuple[str, ...]] = {}
    for end in ends:
        prompts[end.session] = prompts.get(end.session, ()) + end.prompts
    rows = []
    for end in ends:
        if end.at < since and end not in lines:
            continue
        notice = notice_for(end, notices)
        rows.append(
            Gap(end, tuple(lines.get(end, ())), notice, verdict(end, notice, sends, prompts[end.session], now))
        )
    return sorted(
        rows + unmatched,
        key=lambda gap: gap.end.at if gap.end else gap.lines[0].at,
    )


def clock(at: datetime | None) -> str:
    return at.astimezone(SYDNEY).strftime("%m-%d %H:%M:%S") if at else "-"


def render(rows: list[Gap]) -> str:
    out = [f"{'logged':<11}  {'session':<8}  {'turn end':<14}  {'notice':<14}  {'delay':>7}  {'verdict':<9}  line"]
    for gap in rows:
        delay = f"{gap.delay.total_seconds() / 60:.1f}m" if gap.delay is not None else "-"
        logged = clock(gap.lines[0].at)[:11] if gap.lines else "-"
        line = gap.lines[0].line[:70] if gap.lines else "(no finished.log line)"
        out.append(
            f"{logged:<11}  {gap.end.session[-8:] if gap.end else '-':<8}  {clock(gap.end and gap.end.at):<14}  "
            f"{clock(gap.notice and gap.notice.at):<14}  {delay:>7}  {gap.verdict:<9}  {line}"
        )
    turns = [gap for gap in rows if gap.end]
    out.append(
        f"{len(turns)} monitored turn ends ({sum(1 for gap in turns if gap.lines)} with a finished.log line), "
        f"{len(rows) - len(turns)} log lines unmatched, "
        f"{sum(1 for gap in rows if gap.verdict in FAILING)} late or missing notices (more than {LATE.seconds // 60} min)"
    )
    return "\n".join(out)


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
    parser.add_argument("--hours", type=float, help="only turns and log lines from the last N hours")
    parser.add_argument("--now", type=datetime.fromisoformat, help="ISO time with offset; default: now")
    args = parser.parse_args(argv)
    now = args.now or datetime.now(timezone.utc)
    entries = [e for e in parse_log(args.log.read_text()) if e.at <= now]
    if args.hours is not None:
        since = now - timedelta(hours=args.hours)
        entries = [e for e in entries if e.at >= since]
    else:
        since = min((e.at for e in entries), default=now)
    with tempfile.TemporaryDirectory() as folder:
        db = args.db or backup(LIVE_DB, folder)
        with closing(sqlite3.connect(f"file:{db}?mode=ro", uri=True)) as con:
            ends, notices, sends = read_db(con, args.mono)
    ends = [end for end in ends if end.at <= now]
    notices = [notice for notice in notices if notice.at <= now]
    rows = gaps(entries, ends, notices, sends, since, now)
    print(render(rows))
    return 1 if any(gap.verdict in FAILING for gap in rows) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
