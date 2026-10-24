"""Tests for ready-issues.py on data recorded 2026-10-21 01:01 AEDT and 2026-10-22 21:02 AEDT.

    python3 -m unittest discover -s scripts/process/tests

fixtures/linear-issues.json is the script's two Linear queries (the open ADE
issues, then the closed issues that block one), recorded before the query asked
for description and priority; the tests add them as null and 0 (no priority).
fixtures/merged-prs.json is 5 PRs of the script's gh query; none had a
Ticket-part line yet. fixtures/app-sessions.json holds, for each instance, the
`monocode app sessions.list` reply for the app's data at that time: the app's
identifier, then the monocode project's in-flight sessions plus a few idle
ones. Busy and archived flags and which branches name a ticket are as
recorded; ids, titles, the home folder and branch names are made up. The tests
read it through a fake `monocode` that prints the reply and logs each call.

Recorded 2026-10-22: fixtures/linear-issues-2026-10-22.json (the same Linear
queries with description and priority; each description is made up and keeps
only the issue's Files line) and fixtures/open-prs-2026-10-22.json (the
script's gh query for open instance:2 PRs with their files).
fixtures/plan-v2.md is a made-up plan with the Change R tickets and their
Files. lines. StartNow runs them against a real temporary git repo with
worktrees.
"""
import copy
import dataclasses
import io
import importlib.util
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

HERE = Path(__file__).parent
SCRIPT = HERE.parent / "ready-issues.py"
_spec = importlib.util.spec_from_file_location("ready_issues", SCRIPT)
ri = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ri)
# The made-up project of fixtures/app-sessions.json.
PROJECT = "/Users/test/Desktop/monocode"
# ready-issues reads its project from MONOCODE_PROJECT before it asks git.
MADE_UP_PROJECT = mock.patch.dict("os.environ", {"MONOCODE_PROJECT": PROJECT})


def setUpModule():
    MADE_UP_PROJECT.start()


def tearDownModule():
    MADE_UP_PROJECT.stop()


RECORDED_AT = datetime(2026, 10, 20, 14, 1, tzinfo=timezone.utc)
ISSUES = json.loads((HERE / "fixtures/linear-issues.json").read_text())
for _issue in ISSUES:
    _issue.update(description=None, priority=0)
ISSUES_1022 = json.loads((HERE / "fixtures/linear-issues-2026-10-22.json").read_text())
OPEN_PRS_1022 = json.loads((HERE / "fixtures/open-prs-2026-10-22.json").read_text())
PLAN = HERE / "fixtures/plan-v2.md"
PRS = json.loads((HERE / "fixtures/merged-prs.json").read_text())
APP = json.loads((HERE / "fixtures/app-sessions.json").read_text())
FAKE_CLI = """\
#!/usr/bin/env python3
import json, sys
from pathlib import Path
me = Path(__file__)
with open(me.with_name("calls"), "a") as log:
    print(json.dumps(sys.argv[1:]), file=log)
replies = json.loads(me.with_name("replies.json").read_text())
payload = sys.argv[sys.argv.index("--json") + 1]
print(replies.get(payload, replies[""]))
"""


def at(text):
    return datetime.fromisoformat(text).replace(tzinfo=timezone.utc)


def fake_cli(directory, replies):
    """A `monocode` that prints replies[its --json payload], else replies[""], and logs its argv to `calls`."""
    cli = Path(directory) / "monocode"
    cli.write_text(FAKE_CLI)
    cli.chmod(cli.stat().st_mode | stat.S_IEXEC)
    (cli.parent / "replies.json").write_text(json.dumps(replies))
    return cli


def calls(cli):
    log = cli.with_name("calls")
    return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []


def recorded_cli(directory, instance):
    """A fake CLI answering the sessions.list that the app of this instance recorded."""
    folder = Path(directory) / f"instance-{instance}"
    folder.mkdir(exist_ok=True)
    return fake_cli(folder, {"": json.dumps(APP[instance])})


class EditIssues:
    def issue(self, ident):
        return next(i for i in self.issues if i["identifier"] == ident)

    def label(self, ident, *names):
        self.issue(ident)["labels"]["nodes"] += [{"name": name} for name in names]

    def event(self, ident, when, state=None, added=()):
        self.issue(ident)["history"]["nodes"].append(
            {"createdAt": when + "Z", "toState": state and {"type": state}, "addedLabels": [{"name": n} for n in added] or None})

    def set_state(self, ident, state, when):
        self.issue(ident)["state"]["type"] = state
        self.event(ident, when, state=state)


class ReadyIssues(EditIssues, unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.SESSIONS = []
        for instance in APP:
            with mock.patch.object(ri, "APP_CLI", recorded_cli(cls.tmp.name, instance)):
                cls.SESSIONS += ri.app_sessions(instance)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def setUp(self):
        self.issues = copy.deepcopy(ISSUES)
        self.sessions = copy.deepcopy(self.SESSIONS)
        self.prs = copy.deepcopy(PRS)

    def run_instance(self, instance="2"):
        return ri.ready_issues(self.issues, self.sessions, ri.final_parts(self.prs), instance, {}, [])

    def ready(self, instance="2"):
        """The issues whose blockers all unblocked, by id."""
        return {r.id: r for r in self.run_instance(instance)[0] if r.since}

    def pr(self, number):
        return next(pr for pr in self.prs if pr["number"] == number)

    def session(self, branch, busy=True, archived=0, instance="2", title="omp · a builder"):
        row = {"id": f"app-test-{len(self.sessions)}", "title": title, "branch": branch,
               "archived": archived, "instance": instance, "busy": busy}
        self.sessions.append(row)
        return row

    def test_recorded_data(self):
        todo, unlabelled = self.run_instance()
        self.assertEqual(list(self.ready()), ["ADE-61", "ADE-64", "ADE-65", "ADE-66"])
        self.assertEqual({t.waits for t in todo if t.since}, {("no Files list",)}, "nothing recorded has a Files list")
        self.assertEqual(unlabelled, [])
        found = ri.unattributed(self.sessions)
        lines, code = ri.report(todo, unlabelled, found, RECORDED_AT)
        self.assertEqual(code, 1, "four issues with no blocker wait for a human to add a Files list")
        no_files = lines[lines.index(next(line for line in lines if line.startswith("no Files list (blockers merged"))) + 1:][:4]
        self.assertEqual([line.split()[0] for line in no_files], ["ADE-61", "ADE-64", "ADE-65", "ADE-66"])
        self.assertIn("ready-issues: 0 start now, 0 waiting more than 60 min, 4 with no Files list, 27 wait, 0 unlabelled, 15 unattributed.", lines)

    def test_lists_the_builders_in_the_reply_of_its_instance(self):
        with tempfile.TemporaryDirectory() as tmp:
            cli = recorded_cli(tmp, "1")
            with mock.patch.object(ri, "APP_CLI", cli):
                sessions = ri.app_sessions("1")
            self.assertEqual(calls(cli), [["app", "sessions.list", "--json", json.dumps({"project": PROJECT})]])
        self.assertEqual({s["instance"] for s in sessions}, {"1"})
        self.assertEqual(ri.builder_tickets(sessions), {"ADE-52", "ADE-53"},
                         "instance 1's in-flight builders on feat/ade-52-* and feat/ade-53-*")
        self.assertEqual({s["id"] for s in ri.busy(sessions)},
                         {s["id"] for s in APP["1"]["result"]["sessions"] if s["busy"]})

    def test_a_session_lists_its_own_project_and_only_the_monocode_one_counts(self):
        listed = APP["2"]
        refused = {"ok": False, "error": "project is only for a Mono"}
        other = {"ok": True, "result": {"appIdentifier": "com.monocode.second", "cwd": "/tmp/vibtra", "sessions": []}}
        slashed = copy.deepcopy(listed)
        slashed["result"]["cwd"] += "/"
        for case, reply, error in [
            ("a session, refused a project, lists its own", listed, None),
            ("the app's path of the project ends in a slash", slashed, None),
            ("a session in another project", other, f"listed /tmp/vibtra, not {PROJECT}"),
        ]:
            with self.subTest(case), tempfile.TemporaryDirectory() as tmp:
                cli = fake_cli(tmp, {json.dumps({"project": PROJECT}): json.dumps(refused), "": json.dumps(reply)})
                with mock.patch.object(ri, "APP_CLI", cli):
                    if error:
                        self.assertRaisesRegex(RuntimeError, error, ri.app_sessions, "2")
                    else:
                        self.assertEqual(len(ri.app_sessions("2")), len(listed["result"]["sessions"]))
                self.assertEqual([call[-1] for call in calls(cli)], [json.dumps({"project": PROJECT}), "{}"])

    def test_builder_is_a_busy_session_on_the_ticket_branch(self):
        cases = [
            ({"branch": "fix/ade-61-new-window"}, False),
            ({"branch": "ADE-61"}, False),
            ({"branch": "feat/Ade-61"}, False),
            ({"branch": "fix/ade-61-new-window", "instance": "1"}, False),
            ({"branch": "fix/ade-6-steer"}, True),
            ({"branch": "fix/ade-610"}, True),
            ({"branch": "fix/xade-61-x"}, True),
            ({"branch": "fix/ade-61fix"}, True),
            ({"branch": "fix/ade-061-x"}, True),
            ({"branch": "main", "title": "omp · Linear ADE-61, PR 1. Read /Users/…"}, True),
            ({"branch": None, "title": "omp · Linear ADE-61"}, True),
            ({"branch": "fix/ade-61-new-window", "busy": False}, True),
            ({"branch": "fix/ade-61-new-window", "archived": 1}, True),
        ]
        for change, expect_ready in cases:
            with self.subTest(change=change):
                self.sessions = copy.deepcopy(self.SESSIONS)
                self.session(**change)
                self.assertEqual("ADE-61" in self.ready(), expect_ready)

    def test_blocker_with_a_builder_never_unblocks(self):
        self.label("ADE-11", "code-complete")
        self.event("ADE-11", "2026-10-20T12:30:00", added=["code-complete"])
        self.set_state("ADE-11", "completed", "2026-10-20T13:00:00")
        builder = self.session("fix/ade-11-pr-3-window-tabs")
        self.assertNotIn("ADE-62", self.ready())
        self.sessions.remove(builder)
        self.assertIn("ADE-62", self.ready())

    def test_recorded_instance_1_builders_hold_their_dependent(self):
        for blocker in ["ADE-50", "ADE-51", "ADE-52", "ADE-53", "ADE-54"]:
            self.set_state(blocker, "completed", "2026-10-20T13:00:00")
        self.assertNotIn("ADE-55", self.ready("1"), "ADE-52 and ADE-53 have busy builders on instance 1")
        self.sessions = [s for s in self.sessions if s["id"] not in ("app-test-1-03", "app-test-1-07")]
        self.assertEqual(self.ready("1")["ADE-55"].since, at("2026-10-20T13:00:00"))

    def test_final_part_needs_the_exact_line_and_n_equal_to_n(self):
        body = self.pr(161)["body"]
        cases = [("Ticket-part: 3/3", True), ("Ticket-part: 1/1", True), ("Ticket-part: 1/3", False),
                 ("Ticket-part: 0/0", False), ("Ticket-part: 4/3", False), ("Ticket-part:3/3", False),
                 ("ticket-part: 3/3", False), ("See Ticket-part: 3/3", False), ("Ticket-part: 3/3.", False)]
        for line, final in cases:
            with self.subTest(line=line):
                self.pr(161)["body"] = f"{body}\n{line}\n"
                self.assertEqual("ADE-11" in ri.final_parts(self.prs), final)
        self.pr(161)["title"] = "Remove the window-transfer path"
        self.assertEqual(ri.final_parts(self.prs), {}, "the ticket comes from the title prefix")

    def test_final_part_merged_labels_and_unblocks(self):
        self.assertEqual(ri.final_parts(self.prs), {}, "no recorded PR has a Ticket-part line yet")
        self.assertNotIn("ADE-62", self.ready(), "ADE-11 is In Progress, PR 1 merged")
        self.pr(161)["body"] += "\nTicket-part: 1/3\n"
        self.assertNotIn("ADE-62", self.ready())
        self.pr(161)["body"] = self.pr(161)["body"].replace("Ticket-part: 1/3", "Ticket-part: 3/3")
        finals = ri.final_parts(self.prs)
        self.assertEqual([i["identifier"] for i in ri.to_label(self.issues, finals)], ["ADE-11"])
        self.assertEqual(self.ready()["ADE-62"].since, ri.parse_time(self.pr(161)["mergedAt"]), "ready since the last part merged")
        self.label("ADE-11", "code-complete")
        self.assertEqual(ri.to_label(self.issues, finals), [], "already labelled: nothing to write")

    def test_add_code_complete_adds_the_label_by_issue_id(self):
        calls = []

        def fake_linear(key, query, variables):
            calls.append(variables)
            if "issueLabels" in query:
                return {"issueLabels": {"nodes": [{"id": "label-uuid"}]}}
            return {"issueAddLabel": {"success": True}}

        ade11 = self.issue("ADE-11")
        with mock.patch.object(ri, "linear", fake_linear):
            ri.add_code_complete("key", [ade11])
        self.assertEqual(calls[1], {"id": ade11["id"], "label": "label-uuid"})
        self.assertIn("code-complete", ri.label_names(ade11))
        self.assertIn("instance:2", ri.label_names(ade11), "other labels stay")

    def test_in_progress_blocker_blocks_until_code_complete(self):
        self.assertNotIn("ADE-62", self.ready())
        self.label("ADE-11", "code-complete")
        self.event("ADE-11", "2026-10-20T12:30:00", added=["code-complete"])
        self.assertEqual(self.ready()["ADE-62"].since, at("2026-10-20T12:30:00"))

    def test_code_complete_before_done_counts_from_the_label(self):
        self.label("ADE-11", "code-complete")
        self.event("ADE-11", "2026-10-20T12:30:00", added=["code-complete"])
        self.set_state("ADE-11", "completed", "2026-10-20T13:00:00")
        self.assertEqual(self.ready()["ADE-62"].since, at("2026-10-20T12:30:00"), "the earlier of the two")

    def test_done_canceled_and_duplicate_unblock_from_the_history_entry(self):
        for state in ["completed", "canceled", "duplicate"]:
            with self.subTest(state=state):
                self.setUp()
                self.set_state("ADE-11", state, "2026-10-20T11:40:00")
                self.assertEqual(self.ready()["ADE-62"].since, at("2026-10-20T11:40:00"))

    def test_blocker_reopened_then_done_counts_from_the_last_done(self):
        self.set_state("ADE-11", "completed", "2026-10-20T09:00:00")
        self.set_state("ADE-11", "started", "2026-10-20T10:00:00")
        self.set_state("ADE-11", "completed", "2026-10-20T11:00:00")
        self.assertEqual(self.ready()["ADE-62"].since, at("2026-10-20T11:00:00"))

    def test_closed_blocker_without_history_counts_from_updated_at(self):
        ade11 = self.issue("ADE-11")
        ade11.update(state={"type": "completed"}, updatedAt="2026-10-20T10:15:00Z")
        ade11["history"]["nodes"] = []
        self.assertEqual(self.ready()["ADE-62"].since, at("2026-10-20T10:15:00"))

    def test_ready_since_is_the_last_blocker(self):
        self.issue("ADE-63")["inverseRelations"]["nodes"].append({"type": "blocks", "issue": {"identifier": "ADE-11"}})
        self.set_state("ADE-23", "completed", "2026-10-20T12:00:00")
        self.assertNotIn("ADE-63", self.ready(), "ADE-11 still blocks")
        self.set_state("ADE-11", "completed", "2026-10-20T12:20:00")
        self.assertEqual(self.ready()["ADE-63"].since, at("2026-10-20T12:20:00"))

    def test_only_blocks_relations_block(self):
        self.issue("ADE-64")["inverseRelations"]["nodes"].append({"type": "related", "issue": {"identifier": "ADE-23"}})
        self.assertIn("ADE-64", self.ready())

    def test_no_blockers_ready_since_entering_todo(self):
        self.issue("ADE-64")["history"]["nodes"] = []
        self.assertEqual(self.ready()["ADE-64"].since, ri.parse_time(self.issue("ADE-64")["createdAt"]), "no history: createdAt")
        self.event("ADE-64", "2026-10-20T11:00:00", state="started")
        self.event("ADE-64", "2026-10-20T12:00:00", state="unstarted")
        ready = self.ready()["ADE-64"]
        self.assertEqual(ready.since, at("2026-10-20T12:00:00"))
        self.assertEqual(ri.report([ready], [], [], ready.since)[1], 1, "no Files list needs a human at once")
        blocked = dataclasses.replace(ready, since=None, waits=("blocker ADE-11", "no Files list"))
        self.assertEqual(ri.report([blocked], [], [], ready.since + timedelta(minutes=61))[1], 0, "the blocker comes first")
        start = dataclasses.replace(ready, waits=())
        self.assertEqual(ri.report([start], [], [], ready.since + timedelta(minutes=60))[1], 0)
        self.assertEqual(ri.report([start], [], [], ready.since + timedelta(minutes=61))[1], 1)

    def test_only_this_instances_issues_are_reported(self):
        for label in self.issue("ADE-61")["labels"]["nodes"]:
            label["name"] = "instance:1"
        self.assertNotIn("ADE-61", self.ready("2"))
        self.assertIn("ADE-61", self.ready("1"))

    def test_todo_issue_without_exactly_one_instance_label_is_unlabelled(self):
        self.label("ADE-61", "instance:1")
        self.issue("ADE-64")["labels"]["nodes"] = []
        ready, unlabelled = self.run_instance()
        self.assertEqual([i["identifier"] for i in unlabelled], ["ADE-64", "ADE-61"])
        self.assertNotIn("ADE-56", [i["identifier"] for i in unlabelled], "Backlog is not judged")
        self.assertEqual(ri.report([], unlabelled, [], RECORDED_AT)[1], 1)

    def test_busy_sessions_on_main_or_no_branch_are_unattributed(self):
        no_branch = self.session(None, title="omp · no branch")
        self.session("fix/ade-61-x")
        self.session("main", busy=False)
        found = ri.unattributed(self.sessions)
        self.assertEqual(len(found), 16, "15 recorded busy sessions on main, plus the one with no branch")
        self.assertTrue(all(s["busy"] and (s["branch"] or "main") == "main" for s in found))
        lines, code = ri.report([], [], found, RECORDED_AT)
        self.assertEqual(code, 0, "informational only")
        self.assertIn(f"instance 2  {no_branch['id'][-8:]}  <no branch>  omp · no branch", lines)


class Main(unittest.TestCase):
    """main() as checkin runs it, with no app database readable."""

    def setUp(self):
        self.prs = copy.deepcopy(PRS)
        next(pr for pr in self.prs if pr["number"] == 161)["body"] += "\nTicket-part: 3/3\n"
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = tmp.name
        self.cli = recorded_cli(tmp.name, "2")

    def run_main(self, *args, cli=None, instance="2"):
        return run_main(*args, issues=ISSUES, merged=self.prs, cli=cli or self.cli, instance=instance)

    def test_dry_run_prints_and_writes_nothing(self):
        code, out, writes = self.run_main("--dry-run")
        self.assertEqual(writes, [])
        self.assertIn("would label ADE-11 code-complete", out)
        code, out, writes = self.run_main()
        self.assertEqual(writes, ["ADE-11"])
        self.assertIn("ADE-11  labelled code-complete (PR #161 is its last part)", out)

    def test_builders_come_from_the_app_cli_when_no_database_opens(self):
        code, out, _ = self.run_main("--dry-run")
        self.assertEqual(code, 1, out)
        self.assertIn("ready-issues: 0 start now, 0 waiting more than 60 min, 5 with no Files list, 27 wait, 0 unlabelled, 11 unattributed.", out)
        self.assertEqual(calls(self.cli),
                         [["app", "sessions.list", "--json", json.dumps({"project": PROJECT})]])

    def test_a_reply_from_the_other_instance_exits_2(self):
        for instance, answered in [("2", "1"), ("1", "2")]:
            with self.subTest(instance=instance, answered=answered):
                cli = recorded_cli(self.tmp, answered)
                code, out, writes = self.run_main(cli=cli, instance=instance)
                self.assertEqual((code, writes), (2, []), out)
                self.assertIn(f"answered by {APP[answered]['result']['appIdentifier']}, not instance {instance}", out)

    def test_a_reply_from_an_app_built_before_268_exits_2(self):
        old = copy.deepcopy(APP["2"])
        del old["result"]["appIdentifier"]
        for session in old["result"]["sessions"]:
            del session["branch"]
        cli = fake_cli(Path(self.tmp) / "instance-2", {"": json.dumps(old)})
        code, out, writes = self.run_main("--dry-run", cli=cli)
        self.assertEqual((code, writes), (2, []), out)
        self.assertIn("no appIdentifier", out)
        self.assertNotIn("start now", out)

    def test_an_app_cli_that_fails_exits_2(self):
        not_json = "ready-issues: JSONDecodeError: Expecting value: line {} column 1 (char {})"
        for case, reply, line in [
            ("no token", json.dumps({"ok": False, "error": "MONOCODE_APP_TOKEN is not set."}),
             "ready-issues: RuntimeError: app sessions.list: MONOCODE_APP_TOKEN is not set."),
            ("no JSON", "Cannot connect to MonoCode", not_json.format(1, 0)),
            ("no output", "", not_json.format(2, 1)),
        ]:
            with self.subTest(case):
                cli = fake_cli(Path(self.tmp) / "instance-2", {"": reply})
                code, out, writes = self.run_main(cli=cli)
                self.assertEqual((code, writes), (2, []), out)
                self.assertIn(line, out.splitlines())
        code, out, _ = self.run_main(cli="/nonexistent/monocode")
        self.assertEqual(code, 2, out)


class Project(unittest.TestCase):
    """The monocode project ready-issues asks the app for, on any account or clone."""

    def load(self, script):
        spec = importlib.util.spec_from_file_location("ready_issues_copy", script)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    # Defect: the project is a fixed home path, or the worktree the script runs from.
    def test_the_project_is_the_main_checkout_of_the_scripts_repo(self):
        with tempfile.TemporaryDirectory() as tmp, mock.patch.dict("os.environ"):
            del os.environ["MONOCODE_PROJECT"]
            main, worktree, loose = Path(tmp) / "clone", Path(tmp) / "clone-worktree", Path(tmp) / "loose"
            git = ["git", "-c", "user.name=test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"]
            subprocess.run([*git, "init", "-q", main], check=True)
            (main / "ready-issues.py").write_text(SCRIPT.read_text())
            subprocess.run([*git, "-C", main, "add", "ready-issues.py"], check=True)
            subprocess.run([*git, "-C", main, "commit", "-qm", "script"], check=True)
            subprocess.run([*git, "-C", main, "worktree", "add", "-q", worktree], check=True, capture_output=True)
            for copy_in in (main, worktree):
                with self.subTest(copy_in.name):
                    project = self.load(copy_in / "ready-issues.py").monocode_project()
                    self.assertEqual(os.path.realpath(project), os.path.realpath(main))
            os.environ["MONOCODE_PROJECT"] = "/Users/test/elsewhere"
            self.assertEqual(self.load(worktree / "ready-issues.py").monocode_project(), "/Users/test/elsewhere")
            del os.environ["MONOCODE_PROJECT"]
            loose.mkdir()
            (loose / "ready-issues.py").write_text(SCRIPT.read_text())
            self.assertRaises(subprocess.CalledProcessError, self.load(loose / "ready-issues.py").monocode_project)


def run_main(*args, issues, merged, cli, instance="2", open_prs=(), repo=None, now=None):
    """ri.main() on recorded sources, worktrees of repo (none without it) and a fixed clock: (exit code, stdout and stderr, labelled)."""
    writes = []
    sources = dict(APP_CLI=cli, PLAN=PLAN, linear_key=lambda: "key", open_issues_and_blockers=lambda key: copy.deepcopy(issues),
                   merged_prs=lambda: merged, open_prs=lambda instance: list(open_prs),
                   add_code_complete=lambda key, found: writes.extend(i["identifier"] for i in found))
    if repo:
        sources["REPO_DIR"] = repo
    else:
        sources["worktree_claims"] = lambda path, pr_branches: []
    if now:
        sources["datetime"] = type("Clock", (datetime,), {"now": classmethod(lambda cls, tz=None: now)})
    # The sandbox of D1 denies the app data folder: no database opens.
    with mock.patch.multiple(ri, **sources), \
            mock.patch("sqlite3.connect", side_effect=PermissionError("Operation not permitted")), \
            mock.patch.object(sys, "argv", ["ready-issues.py", "--instance", instance, *args]), \
            mock.patch("sys.stdout", new_callable=io.StringIO) as out, \
            mock.patch("sys.stderr", new_callable=io.StringIO) as err:
        code = ri.main()
    return code, out.getvalue() + err.getvalue(), writes


GIT_ENV = {**os.environ, "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", "GIT_AUTHOR_NAME": "t",
           "GIT_AUTHOR_EMAIL": "t@example.com", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.com"}
PI_PROTOCOL = "src/integrations/harness/providers/pi/piProtocol.ts"

def git(cwd, *args):
    subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True, env=GIT_ENV)


def write(path, text="changed\n"):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


class StartNow(EditIssues, unittest.TestCase):
    """Today's Linear issues, open instance:2 PRs and plan, against a real repo whose tyler/main ref is `main`.

    Worktrees: feat/ade-26-ask-scheduler commits src-tauri/src/ask_scheduler.rs; feat/ade-31-catch-up edits it
    without a commit; feat/ade-91-scratch stages src-tauri/src/control.rs and adds two untracked files; a detached
    review checkout changes the same files; fix/ade-90-done changed CHANGELOG.md and is merged; assets/ade-92 is an
    orphan branch with no merge base; fix/ade-79-turn-end-with-background-job is the branch of open PR #255.
    """

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name).resolve()
        self.main, self.scratch = root / "main", root / "ade-91"
        git(root, "init", "-q", "-b", "main", "main")
        for name in ["src-tauri/src/ask_scheduler.rs", "src-tauri/src/control.rs", "CHANGELOG.md", "scripts/a.sh"]:
            write(self.main / name, "base\n")
        self.commit(self.main, "base")
        for branch, name in [("feat/ade-26-ask-scheduler", "src-tauri/src/ask_scheduler.rs"), ("fix/ade-90-done", "CHANGELOG.md"),
                             ("fix/ade-79-turn-end-with-background-job", PI_PROTOCOL)]:
            path = root / branch.replace("/", "-")
            git(self.main, "worktree", "add", "-q", "-b", branch, path)
            write(path / name)
            self.commit(path, branch)
        self.merge("fix/ade-90-done")
        git(self.main, "worktree", "add", "-q", "-b", "feat/ade-31-catch-up", root / "ade-31")
        write(root / "ade-31/src-tauri/src/ask_scheduler.rs")
        git(self.main, "worktree", "add", "-q", "-b", "feat/ade-91-scratch", self.scratch)
        git(self.main, "worktree", "add", "-q", "--detach", root / "review")
        for checkout in [self.scratch, root / "review"]:
            write(checkout / "src-tauri/src/control.rs")
            git(checkout, "add", "src-tauri/src/control.rs")
            write(checkout / "src/features/sessions/model/new.ts")
            write(checkout / "scripts/b.sh")
        git(self.main, "worktree", "add", "-q", "--detach", root / "assets")
        git(root / "assets", "checkout", "-q", "--orphan", "assets/ade-92")
        self.commit(root / "assets", "orphan")

        self.issues = copy.deepcopy(ISSUES_1022)
        self.files("ADE-64", "`src/features/sessions/model/`, `scripts/*.sh`, `CHANGELOG.md`")
        self.files("ADE-78", f"`{PI_PROTOCOL}`")
        self.files("ADE-65", "`src-tauri/src/pasteboard.rs`")
        self.files("ADE-82", "`src-tauri/src/{harness.rs, control.rs}`")
        self.set_state("ADE-79", "unstarted", "2026-10-22T09:30:00")
        self.files("ADE-79", f"`{PI_PROTOCOL}`")
        self.files("ADE-66", "the scenario harness, e.g. the world in src-tauri/src/scenario")
        self.files("ADE-81", "`harness_spawn` in `#[cfg(test)]`")
        self.files("ADE-69", "src/features/sessions, CHANGELOG.md.")
        self.files("ADE-74", "`src-tauri/src/{ask_scheduler,control}.{rs,ts}`")
        ade31 = self.issue("ADE-31")
        ade31["description"] = ade31["description"].replace("`src-tauri/src/ask_scheduler.rs`", "`src/features/unrelated.ts`")
        self.cli = recorded_cli(root, "2")

    def commit(self, cwd, message):
        git(cwd, "add", "-A")
        git(cwd, "commit", "-qm", message)

    def merge(self, branch):
        git(self.main, "merge", "-q", "--no-ff", "-m", f"Merge {branch}", branch)
        git(self.main, "update-ref", ri.BASE, "HEAD")

    def files(self, ident, line):
        self.issue(ident)["description"] += f"\nFiles: {line}\n"

    def todo(self):
        claims = ri.pr_claims(OPEN_PRS_1022) + ri.worktree_claims(self.main, {pr["headRefName"] for pr in OPEN_PRS_1022})
        with mock.patch.object(ri, "APP_CLI", self.cli):
            sessions = ri.app_sessions("2")
        todo, _ = ri.ready_issues(self.issues, sessions, {}, "2", ri.plan_files(PLAN.read_text()), claims)
        return {issue.id: issue.waits for issue in todo}

    def test_start_now_needs_merged_blockers_and_no_overlap(self):
        waits = self.todo()
        scratch = "worktree feat/ade-91-scratch"
        self.assertEqual(waits["ADE-31"], ("blocker ADE-29", "worktree feat/ade-26-ask-scheduler: src-tauri/src/ask_scheduler.rs"),
                         "the plan's Files line decides, not the description's; its own feat/ade-31 worktree does not block it")
        self.assertEqual(waits["ADE-34"], ("blocker ADE-27", "worktree feat/ade-26-ask-scheduler: src-tauri/src/ask_scheduler.rs",
                                           "worktree feat/ade-31-catch-up: src-tauri/src/ask_scheduler.rs",
                                           f"{scratch}: src-tauri/src/control.rs"),
                         "the detached review checkout does not count")
        self.assertEqual(waits["ADE-64"], (f"{scratch}: scripts/b.sh, src/features/sessions/model/new.ts",),
                         "untracked files count; the merged fix/ade-90 CHANGELOG.md does not")
        self.assertEqual(waits["ADE-78"], (f"PR #255: {PI_PROTOCOL}",), "the worktree of PR #255's branch counts through the PR")
        self.assertEqual(waits["ADE-79"], (), "PR #255 is on its own branch")
        self.assertEqual(waits["ADE-25"], ("blocker ADE-24",), "ADE-24 is In Progress")
        self.assertEqual(waits["ADE-65"], ())
        self.assertEqual(waits["ADE-82"], (f"{scratch}: src-tauri/src/control.rs",), "a/{b, c} lists a/b and a/c")
        self.assertEqual(waits["ADE-11"], ("no Files list",), "ADE-57, its blocker, is code-complete")
        self.assertEqual(waits["ADE-66"], ("no Files list",), "a list with prose in it has no paths")
        self.assertEqual(waits["ADE-81"], ("no Files list",), "a quoted word with no / or . is no path")
        self.assertEqual(waits["ADE-69"], (f"{scratch}: src/features/sessions/model/new.ts",),
                         "a plain comma list counts, and a path with no . is a directory")
        self.assertEqual(waits["ADE-74"], ("worktree feat/ade-26-ask-scheduler: src-tauri/src/ask_scheduler.rs",
                                           "worktree feat/ade-31-catch-up: src-tauri/src/ask_scheduler.rs",
                                           f"{scratch}: src-tauri/src/control.rs"), "every brace group expands")

        self.set_state("ADE-29", "completed", "2026-10-22T09:30:00")
        self.merge("feat/ade-26-ask-scheduler")
        git(self.main, "worktree", "lock", self.scratch)
        shutil.rmtree(self.scratch)
        ade78 = self.issue("ADE-78")
        ade78["description"] = ade78["description"].replace(PI_PROTOCOL, "src/features/monos/model/monoSettings.ts")
        code, out, _ = run_main(issues=self.issues, merged=[], cli=self.cli, open_prs=OPEN_PRS_1022, repo=self.main,
                                now=at("2026-10-22T10:03:00"))
        start = out[out.index("START NOW"):out.index("WAIT:")].splitlines()[1:]
        self.assertEqual([line.split()[0] for line in start], ["ADE-30", "ADE-31", "ADE-78", "ADE-79", "ADE-64", "ADE-65", "ADE-69", "ADE-82"],
                         "Change R, then High, then the rest, each by number")
        self.assertIn("(33 min)  R13: Catch-up reminders are spaced out", start[1], "ready since ADE-29 was done")
        self.assertIn("(33 min)", start[3], "ready since it entered Todo")
        self.assertIn("ADE-34  R16: Interrupted agents are woken to resume\n    blocker ADE-27\n"
                      "    worktree feat/ade-31-catch-up: src-tauri/src/ask_scheduler.rs\n", out,
                      "the merged ADE-26 work and the deleted, locked scratch worktree no longer block")
        no_files = out[out.index("no Files list (blockers merged"):out.index("unattributed")].splitlines()[1:]
        self.assertEqual([line.split()[0] for line in no_files], ["ADE-11", "ADE-66", "ADE-81"],
                         "named in the alarm: blockers merged, no Files list")
        self.assertIn("ready-issues: 8 start now, 5 waiting more than 60 min,", out)
        self.assertEqual(code, 1)


FAKE_GH = """\
#!/usr/bin/env python3
import sys
from pathlib import Path
here = Path(__file__).parent
with open(here / "calls", "a") as calls:
    print(" ".join(sys.argv[1:]), file=calls)
print((here / ("api.txt" if sys.argv[1] == "api" else "list.json")).read_text(), end="")
"""


class OpenPrs(unittest.TestCase):
    """rust-lang/rust #164662, recorded 2026-10-23 00:00 AEDT: `gh pr list --json files` gives 100 of its 144 files
    (fixtures/gh-pr-list-cut-164662.json); `gh api --paginate .../pulls/164662/files` gives all 144
    (fixtures/gh-api-files-164662.txt). A fake gh on PATH answers with them."""

    def test_a_file_past_the_first_100_still_blocks(self):
        late_file = "tests/run-make/const-destruct-stable-toolchain/rmake.rs"
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            shutil.copy(HERE / "fixtures/gh-pr-list-cut-164662.json", tmp / "list.json")
            shutil.copy(HERE / "fixtures/gh-api-files-164662.txt", tmp / "api.txt")
            (tmp / "gh").write_text(FAKE_GH)
            (tmp / "gh").chmod(0o755)
            with mock.patch.dict(os.environ, {"PATH": f"{tmp}{os.pathsep}{os.environ['PATH']}"}):
                claims = ri.pr_claims(ri.open_prs("2"))
            calls = (tmp / "calls").read_text().splitlines()
        self.assertEqual(len(claims[0].files), 144)
        self.assertEqual(ri.overlaps("ADE-1", (late_file,), claims), [f"PR #164662: {late_file}"])
        self.assertEqual(calls[1], "api --paginate repos/tylers2222/Monocode-Unlocked/pulls/164662/files --jq .[].filename")


class CommandLine(unittest.TestCase):
    def test_instance_is_required(self):
        result = subprocess.run([sys.executable, SCRIPT], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
