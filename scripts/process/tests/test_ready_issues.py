"""Tests for ready-issues.py on data recorded 2026-10-21 01:01 AEDT.

    python3 -m unittest discover -s scripts/process/tests

fixtures/linear-issues.json is the script's two Linear queries (the open ADE
issues, then the closed issues that block one). fixtures/merged-prs.json is 5
PRs of the script's gh query; none had a Ticket-part line yet.
fixtures/app-sessions.json holds, for each instance, the `monocode app
sessions.list` reply for the app's data at that time: the app's identifier,
then the monocode project's in-flight sessions plus a few idle ones. Busy and
archived flags and which branches name a ticket are as recorded; ids, titles,
the home folder and branch names are made up. The tests read it through a fake
`monocode` that prints the reply and logs each call.
"""
import copy
import io
import importlib.util
import json
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


class ReadyIssues(unittest.TestCase):
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
        return ri.ready_issues(self.issues, self.sessions, ri.final_parts(self.prs), instance)

    def ready(self, instance="2"):
        return {r.id: r for r in self.run_instance(instance)[0]}

    def issue(self, ident):
        return next(i for i in self.issues if i["identifier"] == ident)

    def pr(self, number):
        return next(pr for pr in self.prs if pr["number"] == number)

    def label(self, ident, *names):
        self.issue(ident)["labels"]["nodes"] += [{"name": name} for name in names]

    def event(self, ident, when, state=None, added=()):
        self.issue(ident)["history"]["nodes"].append(
            {"createdAt": when + "Z", "toState": state and {"type": state}, "addedLabels": [{"name": n} for n in added] or None})

    def set_state(self, ident, state, when):
        self.issue(ident)["state"]["type"] = state
        self.event(ident, when, state=state)

    def session(self, branch, busy=True, archived=0, instance="2", title="omp · a builder"):
        row = {"id": f"app-test-{len(self.sessions)}", "title": title, "branch": branch,
               "archived": archived, "instance": instance, "busy": busy}
        self.sessions.append(row)
        return row

    def test_recorded_data(self):
        ready, unlabelled = self.run_instance()
        self.assertEqual([r.id for r in ready], ["ADE-61", "ADE-64", "ADE-65", "ADE-66"])
        self.assertEqual(unlabelled, [])
        found = ri.unattributed(self.sessions)
        lines, code = ri.report(ready, unlabelled, found, RECORDED_AT)
        self.assertEqual(code, 1, "the four issues with no blockers have waited hours")
        self.assertIn("ready-issues: 4 ready with no builder, 4 waiting more than 60 min, 0 unlabelled, 15 unattributed.", lines)

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
        for case, reply, error in [
            ("a session, refused a project, lists its own", listed, None),
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
        self.assertEqual(ri.report([ready], [], [], ready.since + timedelta(minutes=60))[1], 0)
        self.assertEqual(ri.report([ready], [], [], ready.since + timedelta(minutes=61))[1], 1)

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
        writes = []
        # The sandbox of D1 denies the app data folder: no database opens.
        with mock.patch.object(ri, "APP_CLI", cli or self.cli), \
                mock.patch("sqlite3.connect", side_effect=PermissionError("Operation not permitted")), \
                mock.patch.object(ri, "linear_key", lambda: "key"), \
                mock.patch.object(ri, "open_issues_and_blockers", lambda key: copy.deepcopy(ISSUES)), \
                mock.patch.object(ri, "merged_prs", lambda: self.prs), \
                mock.patch.object(ri, "add_code_complete", lambda key, issues: writes.extend(i["identifier"] for i in issues)), \
                mock.patch.object(sys, "argv", ["ready-issues.py", "--instance", instance, *args]), \
                mock.patch("sys.stdout", new_callable=io.StringIO) as out, \
                mock.patch("sys.stderr", new_callable=io.StringIO) as err:
            code = ri.main()
        return code, out.getvalue() + err.getvalue(), writes

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
        self.assertIn("ready-issues: 5 ready with no builder, 5 waiting more than 60 min, 0 unlabelled, 11 unattributed.", out)
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
        self.assertNotIn("ready with no builder", out)

    def test_an_app_cli_that_fails_exits_2(self):
        for case, reply in [("no token", json.dumps({"ok": False, "error": "MONOCODE_APP_TOKEN is not set."})),
                            ("no JSON", "Cannot connect to MonoCode"), ("no output", "")]:
            with self.subTest(case):
                cli = fake_cli(Path(self.tmp) / "instance-2", {"": reply})
                code, out, writes = self.run_main(cli=cli)
                self.assertEqual((code, writes), (2, []), out)
                self.assertTrue(out.startswith("ready-issues: "), out)
        code, out, _ = self.run_main(cli="/nonexistent/monocode")
        self.assertEqual(code, 2, out)


class CommandLine(unittest.TestCase):
    def test_instance_is_required(self):
        result = subprocess.run([sys.executable, SCRIPT], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
